//! Creating and changing targets and rules: the app's routes and the MCP
//! tools share these. Input is read leniently, the way forms send it
//! (`"30"` for a number, `"true"` for a bool, commas for lists); a value
//! that can't be read is refused (422, so a client rolls back).

use axum::http::StatusCode;
use pod_core::Pod;
use serde_json::{Value, json};

use crate::db::{self, Rule, Target};
use crate::extract::Kind;
use crate::rules;
use crate::when::parse_utc_offset;
use crate::{NAME, WatcherAdapter};

/// Live targets per pod.
pub const MAX_TARGETS: i64 = 50;
/// Live rules per pod.
pub const MAX_RULES: i64 = 100;
/// Default and allowed check intervals, minutes.
pub const DEFAULT_INTERVAL_MIN: i64 = 360;
pub const MIN_INTERVAL_MIN: i64 = 15;
pub const MAX_INTERVAL_MIN: i64 = 7 * 24 * 60;

/// A refusal: status and body (`{"error", "detail"}`).
pub type Refusal = (StatusCode, Value);

pub fn invalid(detail: impl Into<String>) -> Refusal {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({ "error": "invalid_request", "detail": detail.into() }),
    )
}

fn not_found(what: &str, id: &str) -> Refusal {
    (
        StatusCode::NOT_FOUND,
        json!({ "error": format!("{what}_not_found"), "id": id }),
    )
}

fn store_failed() -> Refusal {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": "store_error" }),
    )
}

fn text(body: &Value, key: &str, max: usize) -> Result<Option<String>, Refusal> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.chars().count() > max {
                return Err(invalid(format!("\"{key}\" is at most {max} characters")));
            }
            Ok(Some(s.to_owned()))
        }
        Some(_) => Err(invalid(format!("\"{key}\" is text"))),
    }
}

fn boolean(body: &Value, key: &str) -> Result<Option<bool>, Refusal> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => match s.trim() {
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => Err(invalid(format!("\"{key}\" is true or false"))),
        },
        Some(_) => Err(invalid(format!("\"{key}\" is true or false"))),
    }
}

fn integer(body: &Value, key: &str) -> Result<Option<i64>, Refusal> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .map(Some)
            .ok_or_else(|| invalid(format!("\"{key}\" is a whole number"))),
    }
}

/// Check a target URL: `http(s)://`, and a public host unless the server
/// allows private ones.
pub fn check_url(adapter: &WatcherAdapter, url: &str) -> Result<String, Refusal> {
    let parsed =
        reqwest::Url::parse(url.trim()).map_err(|_| invalid(format!("\"{url}\" is not a URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(invalid("the URL must start with http:// or https://"));
    }
    let host = parsed.host_str().unwrap_or("");
    if !adapter.config().allow_private_hosts && !pod_core::net::is_public_host(host) {
        return Err(invalid(format!("\"{host}\" is not a public website")));
    }
    if url.len() > 2_000 {
        return Err(invalid("the URL is at most 2000 characters"));
    }
    Ok(parsed.to_string())
}

/// Apply the target fields present in `body` to `t`.
pub fn apply_target(adapter: &WatcherAdapter, t: &mut Target, body: &Value) -> Result<(), Refusal> {
    if let Some(url) = text(body, "url", 2_000)?.filter(|u| !u.is_empty()) {
        t.url = check_url(adapter, &url)?;
    }
    if let Some(title) = text(body, "title", 100)? {
        t.title = title;
    }
    if let Some(x) = text(body, "extractor", 20)?.filter(|x| !x.is_empty()) {
        if x != "auto" && Kind::parse(&x).is_none() {
            return Err(invalid(format!(
                "\"extractor\" is auto, {}",
                Kind::ALL.join(", ")
            )));
        }
        t.extractor = x;
    }
    if let Some(m) = integer(body, "interval_minutes")? {
        if !(MIN_INTERVAL_MIN..=MAX_INTERVAL_MIN).contains(&m) {
            return Err(invalid(format!(
                "\"interval_minutes\" is {MIN_INTERVAL_MIN}-{MAX_INTERVAL_MIN}"
            )));
        }
        t.interval_secs = m * 60;
    }
    if let Some(h) = text(body, "hint", 1_000)? {
        t.hint = h;
    }
    if let Some(d) = boolean(body, "details")? {
        t.details = d;
    }
    if let Some(o) = text(body, "utc_offset", 10)? {
        t.utc_offset = parse_utc_offset(&o).map_err(invalid)?;
    }
    if let Some(a) = boolean(body, "archived")? {
        t.archived = a;
    }
    if t.title.is_empty() {
        t.title = reqwest::Url::parse(&t.url)
            .ok()
            .and_then(|u| {
                u.host_str()
                    .map(|h| h.trim_start_matches("www.").to_owned())
            })
            .unwrap_or_else(|| "Website".into());
    }
    Ok(())
}

/// Create a target (`id` absent) or change one; answers its entity. A
/// repeated `client_id` answers the target its create made.
pub async fn save_target(pod: &Pod, body: Value) -> Result<Value, Refusal> {
    let adapter = pod
        .adapter::<WatcherAdapter>()
        .expect("the pod has the watcher adapter");
    let now = pod_core::util::now_secs();
    let id = text(&body, "id", 64)?.filter(|i| !i.is_empty());
    let client_id = text(&body, "client_id", 64)?.filter(|i| !i.is_empty());
    // Read what exists first (blocking), then validate, then write.
    let (lookup_id, lookup_client) = (id.clone(), client_id.clone());
    let existing = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let by_id = match lookup_id.as_deref() {
                    Some(i) => match i.strip_prefix("pending-") {
                        Some(cl) => db::target_by_client(c, cl)?,
                        None => db::target(c, i)?,
                    },
                    None => None,
                };
                let by_client = match lookup_client.as_deref() {
                    Some(cl) => db::target_by_client(c, cl)?,
                    None => None,
                };
                Ok((by_id, by_client, db::live_target_count(c)?))
            })
        })
        .await
        .map_err(|_| store_failed())?;
    let (by_id, by_client, live) = existing;
    let creating = id.is_none();
    let mut t = match (id.as_deref(), by_id, by_client) {
        (None, _, Some(made)) => return entity_of_target(pod, made.id).await,
        (Some(i), None, _) => return Err(not_found("target", i)),
        (Some(_), Some(t), _) => t,
        (None, _, None) => {
            if text(&body, "url", 2_000)?.is_none_or(|u| u.is_empty()) {
                return Err(invalid("\"url\" is required"));
            }
            Target {
                id: db::new_id('t'),
                title: String::new(),
                url: String::new(),
                extractor: "auto".into(),
                hint: String::new(),
                details: false,
                interval_secs: DEFAULT_INTERVAL_MIN * 60,
                utc_offset: None,
                archived: false,
                created_at: now,
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
            }
        }
    };
    let was_archived = t.archived;
    apply_target(adapter, &mut t, &body)?;
    if !t.archived && (creating || was_archived) && live >= MAX_TARGETS {
        return Err((
            StatusCode::CONFLICT,
            json!({ "error": "too_many_targets", "detail": format!("a pod watches at most {MAX_TARGETS} websites") }),
        ));
    }
    let (saved, cl) = (t.clone(), client_id.clone());
    pod.blocking(move |s| {
        s.transaction(|tx| {
            if creating {
                db::insert_target(tx, &saved, cl.as_deref())
            } else {
                db::update_target(tx, &saved, now)
            }
        })
    })
    .await
    .map_err(|_| store_failed())?;
    pod.publish_data_all(NAME);
    crate::check::spawn_run(pod);
    entity_of_target(pod, t.id).await
}

async fn entity_of_target(pod: &Pod, id: String) -> Result<Value, Refusal> {
    let now = pod_core::util::now_secs();
    let found = pod
        .blocking(move |s| {
            s.with_conn(|c| match db::target(c, &id)? {
                Some(t) => db::target_entity(c, &t, now).map(Some),
                None => Ok(None),
            })
        })
        .await
        .map_err(|_| store_failed())?;
    found.map(|e| Value::Object(e.0)).ok_or_else(store_failed)
}

/// Create a rule (`id` absent) or change one; answers its entity.
pub async fn save_rule(pod: &Pod, body: Value) -> Result<Value, Refusal> {
    let now = pod_core::util::now_secs();
    let id = text(&body, "id", 64)?.filter(|i| !i.is_empty());
    let client_id = text(&body, "client_id", 64)?.filter(|i| !i.is_empty());
    let (lookup_id, lookup_client) = (id.clone(), client_id.clone());
    let (by_id, by_client, live) = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let by_id = match lookup_id.as_deref() {
                    Some(i) => match i.strip_prefix("pending-") {
                        Some(cl) => db::rule_by_client(c, cl)?,
                        None => db::rule(c, i)?,
                    },
                    None => None,
                };
                let by_client = match lookup_client.as_deref() {
                    Some(cl) => db::rule_by_client(c, cl)?,
                    None => None,
                };
                Ok((by_id, by_client, db::live_rule_count(c)?))
            })
        })
        .await
        .map_err(|_| store_failed())?;
    let creating = id.is_none();
    let mut r = match (id.as_deref(), by_id, by_client) {
        (None, _, Some(made)) => return Ok(Value::Object(db::rule_entity(&made).0)),
        (Some(i), None, _) => return Err(not_found("rule", i)),
        (Some(_), Some(r), _) => r,
        (None, _, None) => Rule {
            id: db::new_id('r'),
            title: String::new(),
            spec: Default::default(),
            archived: false,
        },
    };
    let was_archived = r.archived;
    if let Some(t) = text(&body, "title", 100)? {
        r.title = t;
    }
    rules::apply(&mut r.spec, &body).map_err(invalid)?;
    if let Some(a) = boolean(&body, "archived")? {
        r.archived = a;
    }
    if r.title.is_empty() {
        r.title = r
            .spec
            .any
            .first()
            .or(r.spec.all.first())
            .cloned()
            .unwrap_or_else(|| "Every event".into());
    }
    if !r.archived && (creating || was_archived) && live >= MAX_RULES {
        return Err((
            StatusCode::CONFLICT,
            json!({ "error": "too_many_rules", "detail": format!("a pod has at most {MAX_RULES} rules") }),
        ));
    }
    let saved = r.clone();
    pod.blocking(move |s| s.transaction(|tx| db::save_rule(tx, &saved, client_id.as_deref(), now)))
        .await
        .map_err(|_| store_failed())?;
    pod.publish_data_all(NAME);
    // Matches of a new or changed rule are notified at once.
    let p = pod.clone();
    tokio::spawn(async move { crate::check::deliver(&p).await });
    Ok(Value::Object(db::rule_entity(&r).0))
}
