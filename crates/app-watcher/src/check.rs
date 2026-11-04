//! Checking targets: fetch (conditionally), skip when unchanged, extract,
//! store and match, notify. Runs in the background, one run per pod at a
//! time, started by the notification poller, by opening the app when a
//! target is overdue, and by "refresh now".

use std::time::Duration;

use pod_core::{NewNotification, Pod};
use reqwest::{StatusCode, header};

use crate::db::{self, CheckResult, Target};
use crate::extract::{self, Kind, RawEvent};
use crate::html;
use crate::when::{Zone, civil_from_days};
use crate::{APP_ID, CHANNEL, NAME, WatcherAdapter};

/// Largest page read.
pub const MAX_BODY: usize = 3 * 1024 * 1024;
/// One fetch's timeout.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// One model call's timeout.
pub const LLM_TIMEOUT: Duration = Duration::from_secs(180);
/// Event pages fetched per check (targets with `details`).
pub const MAX_DETAIL_FETCHES: usize = 10;
/// Targets checked per run (the rest wait for the next run).
const MAX_PER_RUN: usize = 50;
/// More notifications than this from one run arrive quietly.
const LOUD_PER_RUN: usize = 10;

/// Pod setting: `0` turns fetching off (temporary pods, which anyone can
/// make, must not make the server fetch URLs of their choosing).
pub const FETCH_SETTING: &str = "watcher.fetch";

/// A fetch that went wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchError {
    pub message: String,
    /// The server asked to wait this long (`Retry-After`, seconds).
    pub retry_after: Option<i64>,
}

fn fail(message: impl Into<String>) -> FetchError {
    FetchError {
        message: message.into(),
        retry_after: None,
    }
}

/// What a fetch answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// GET `url`, sending the validators of the last fetch.
pub async fn fetch(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<Fetched, FetchError> {
    let mut req = client.get(url).header(
        header::ACCEPT,
        "text/html,application/xhtml+xml,text/calendar,application/ld+json;q=0.9,*/*;q=0.8",
    );
    if let Some(e) = etag {
        req = req.header(header::IF_NONE_MATCH, e);
    }
    if let Some(m) = last_modified {
        req = req.header(header::IF_MODIFIED_SINCE, m);
    }
    let mut resp = req.send().await.map_err(|e| {
        let e = if e.is_timeout() {
            "timed out".to_owned()
        } else if e.is_connect() {
            "could not connect".to_owned()
        } else {
            e.to_string()
        };
        fail(e)
    })?;
    let status = resp.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|s| *s > 0);
        return Err(FetchError {
            message: format!("the site answered {status}"),
            retry_after,
        });
    }
    let header_of = |h: header::HeaderName| {
        resp.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header_of(header::ETAG);
    let last_modified = header_of(header::LAST_MODIFIED);
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| fail(e.to_string()))? {
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err(fail(format!("the page is over {} MiB", MAX_BODY >> 20)));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
    })
}

/// Today's date (`YYYY-MM-DD`) in `zone` (else UTC).
fn today(now: i64, zone: Option<Zone>) -> String {
    let local = now + i64::from(zone.map_or(0, |z| z.offset_at(now))) * 60;
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Run `extractor` (`auto` or a [`Kind`]) over a fetched page: the events
/// and the extractor that found them. `auto` reads an iCalendar body as
/// `ics`, else tries `jsonld`, then `llm` when the pod has a model.
pub async fn extract(
    adapter: &WatcherAdapter,
    extractor: &str,
    body: &str,
    url: &str,
    hint: &str,
    zone: Option<Zone>,
    now: i64,
) -> Result<(Vec<RawEvent>, &'static str), String> {
    let kind = match Kind::parse(extractor) {
        Some(k) => k,
        None if body.trim_start().starts_with("BEGIN:VCALENDAR") => Kind::Ics,
        None => {
            let found = extract::jsonld(body, url);
            if !found.is_empty() || adapter.config().llm.is_none() {
                return Ok((found, Kind::Jsonld.name()));
            }
            Kind::Llm
        }
    };
    let events = match kind {
        Kind::Ics => extract::ics(body),
        Kind::Jsonld => extract::jsonld(body, url),
        Kind::Llm => {
            let Some(cfg) = &adapter.config().llm else {
                return Err(
                    "the llm extractor needs a model: set CONSTRUCT_INFER_API_KEY on the server"
                        .into(),
                );
            };
            let text = html::to_text(body);
            extract::llm(
                adapter.llm_client(),
                cfg,
                &text,
                url,
                hint,
                &today(now, zone),
            )
            .await?
        }
    };
    Ok((events, kind.name()))
}

/// Fetch each new event's own page (at most [`MAX_DETAIL_FETCHES`]) for
/// its full description: the page's JSON-LD event's, else its text.
async fn fill_details(client: &reqwest::Client, events: &mut [RawEvent], new: &[bool]) {
    let mut fetched = 0;
    for (e, is_new) in events.iter_mut().zip(new) {
        if !is_new || e.url.is_empty() || fetched >= MAX_DETAIL_FETCHES {
            continue;
        }
        fetched += 1;
        let Ok(Fetched::Body { bytes, .. }) = fetch(client, &e.url, None, None).await else {
            continue;
        };
        let page = String::from_utf8_lossy(&bytes);
        let found = extract::jsonld(&page, &e.url);
        let same = found
            .iter()
            .find(|f| {
                f.start.year == e.start.year
                    && f.start.month == e.start.month
                    && f.start.day == e.start.day
            })
            .or(found.first());
        let description = match same {
            Some(f) if !f.description.is_empty() => {
                if e.venue.is_empty() {
                    e.venue = f.venue.clone();
                }
                f.description.clone()
            }
            _ => html::to_text(&page),
        };
        if description.len() > e.description.len() {
            e.description = html::truncate(&description, extract::MAX_DESCRIPTION);
        }
    }
}

/// When to try again after `failures` failures in a row.
fn backoff(t: &Target, failures: i64, retry_after: Option<i64>, now: i64) -> i64 {
    let exp = (t.interval_secs.max(900)) << (failures.clamp(1, 6) - 1);
    let wait = exp.min(86_400).max(retry_after.unwrap_or(0)).max(60);
    now + wait
}

/// Check target `t` once and record how it went.
pub async fn check_target(pod: &Pod, adapter: &WatcherAdapter, t: &Target) -> CheckResult {
    let now = pod_core::util::now_secs();
    let result = check_inner(pod, adapter, t, now).await;
    let (t2, r) = (t.clone(), result.clone());
    let _ = pod
        .blocking(move |s| s.with_conn(|c| db::record_check(c, &t2, &r, now)))
        .await;
    pod.publish_data_all(NAME);
    result
}

async fn fetching_allowed(pod: &Pod) -> bool {
    pod.blocking(|s| s.pod_setting(FETCH_SETTING))
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v != "0")
}

/// The message when fetching is off.
pub const FETCH_OFF: &str =
    "fetching websites is turned off on this pod (temporary pods can't fetch)";

async fn check_inner(pod: &Pod, adapter: &WatcherAdapter, t: &Target, now: i64) -> CheckResult {
    let failed = |error: String, retry_after: Option<i64>| CheckResult::Failed {
        retry_at: backoff(t, t.failures + 1, retry_after, now),
        error: html::truncate(&error, 300),
    };
    if !fetching_allowed(pod).await {
        return CheckResult::Failed {
            error: FETCH_OFF.into(),
            retry_at: now + 86_400,
        };
    }
    let client = adapter.fetch_client();
    let fetched = match fetch(
        client,
        &t.url,
        t.etag.as_deref(),
        t.last_modified.as_deref(),
    )
    .await
    {
        Ok(f) => f,
        Err(e) => return failed(e.message, e.retry_after),
    };
    let Fetched::Body {
        bytes,
        etag,
        last_modified,
    } = fetched
    else {
        return CheckResult::Unchanged;
    };
    let hash = db::body_hash(&bytes);
    if t.content_hash.as_deref() == Some(hash.as_str()) {
        return CheckResult::Unchanged;
    }
    let body = String::from_utf8_lossy(&bytes);
    let (mut events, used) =
        match extract(adapter, &t.extractor, &body, &t.url, &t.hint, t.zone(), now).await {
            Ok(x) => x,
            Err(e) => return failed(e, None),
        };
    if t.details {
        let ids: Vec<String> = events.iter().map(|e| db::event_id(&t.id, e)).collect();
        let lookup = ids.clone();
        let existing = pod
            .blocking(move |s| s.with_conn(|c| db::existing_events(c, &lookup)))
            .await
            .unwrap_or_default();
        let new: Vec<bool> = ids.iter().map(|i| !existing.contains(i)).collect();
        fill_details(client, &mut events, &new).await;
    }
    let count = events.len();
    let t2 = t.clone();
    let stored = pod
        .blocking(move |s| s.transaction(|tx| db::store_events(tx, &t2, &events, now)))
        .await;
    if stored.is_err() {
        return failed("the events could not be stored".into(), None);
    }
    CheckResult::Extracted {
        content_hash: hash,
        etag,
        last_modified,
        events: count,
        extractor: used,
    }
}

/// Per-pod: one run at a time.
#[derive(Default)]
pub struct RunLock(tokio::sync::Mutex<()>);

/// Check every due target, then notify new matches. Returns at once when a
/// run is already going.
pub async fn run_due(pod: &Pod) {
    let lock = pod.local::<RunLock>();
    let Ok(_guard) = lock.0.try_lock() else {
        return;
    };
    let Some(adapter) = pod.adapter::<WatcherAdapter>() else {
        return;
    };
    let now = pod_core::util::now_secs();
    let _ = pod
        .blocking(move |s| s.with_conn(|c| db::prune(c, now)))
        .await;
    let due = pod
        .blocking(move |s| s.with_conn(|c| db::due_targets(c, now, MAX_PER_RUN)))
        .await
        .unwrap_or_default();
    for t in due {
        let budget = if t.extractor == "ics" || t.extractor == "jsonld" {
            FETCH_TIMEOUT * 2
        } else {
            LLM_TIMEOUT + FETCH_TIMEOUT * 2
        };
        if tokio::time::timeout(budget * 2, check_target(pod, adapter, &t))
            .await
            .is_err()
        {
            let r = CheckResult::Failed {
                error: "the check timed out".into(),
                retry_at: backoff(&t, t.failures + 1, None, now),
            };
            let _ = pod
                .blocking(move |s| s.with_conn(|c| db::record_check(c, &t, &r, now)))
                .await;
        }
    }
    deliver(pod).await;
}

/// Start [`run_due`] in the background.
pub fn spawn_run(pod: &Pod) {
    let pod = pod.clone();
    tokio::spawn(async move { run_due(&pod).await });
}

/// Notify every match not notified yet: one notification per event.
pub async fn deliver(pod: &Pod) {
    let now = pod_core::util::now_secs();
    let pending = match pod
        .blocking(move |s| s.transaction(|tx| db::take_pending(tx, now)))
        .await
    {
        Ok(p) => p,
        Err(_) => return,
    };
    if pending.is_empty() {
        return;
    }
    let items: Vec<NewNotification> = pending
        .into_iter()
        .enumerate()
        .map(|(i, p)| NewNotification {
            app_id: APP_ID.into(),
            channel: CHANNEL.into(),
            dedupe_key: format!("watcher:{}", p.event_id),
            title: p.title,
            body: if p.rules.is_empty() {
                p.meta
            } else {
                format!("{} — {}", p.meta, p.rules.join(", "))
            },
            image_url: None,
            target_uri: Some(db::event_uri(&p.event_id)),
            group_key: Some("watcher".into()),
            created_at: now,
            quiet: i >= LOUD_PER_RUN,
        })
        .collect();
    if let Err(e) = pod.notify(items).await {
        tracing::error!(status = ?e.0, "storing watcher notifications failed");
    }
    pod.publish_data_all(NAME);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_backoff() {
        assert_eq!(today(0, Some(Zone::Fixed(-420))), "1969-12-31");
        let t = Target {
            id: "t".into(),
            title: String::new(),
            url: String::new(),
            extractor: "auto".into(),
            hint: String::new(),
            details: false,
            interval_secs: 3600,
            utc_offset: None,
            time_zone: None,
            archived: false,
            created_at: 0,
            next_fetch_at: 0,
            etag: None,
            last_modified: None,
            content_hash: None,
            last_fetch_at: None,
            last_ok_at: None,
            last_error: None,
            failures: 0,
            empty_runs: 0,
            last_extractor: None,
        };
        assert_eq!(backoff(&t, 1, None, 0), 3600);
        assert_eq!(backoff(&t, 3, None, 0), 4 * 3600);
        assert_eq!(backoff(&t, 9, None, 0), 86_400);
        assert_eq!(backoff(&t, 1, Some(7200), 0), 7200);
    }
}
