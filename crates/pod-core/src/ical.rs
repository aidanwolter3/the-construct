//! Calendar feeds (decision `ical-feeds`): a private `.ics` URL a calendar
//! app subscribes to (Proton Calendar's "Add calendar from link"), made
//! from rows the owner chose. Only where the host serves public links
//! (`CONSTRUCT_PUBLIC_WEB=1`).
//!
//! Privacy is storage (decision `sharing`): a subscriber never reaches the
//! pod. The pod renders each feed and copies the text to the host when its
//! data settles; the host answers `GET /ical/<token>.ics` from that copy.
//! The core knows no source: a feed's `sources` are read by the source
//! that answers [`crate::Source::calendar_events`] (Collections), which
//! maps each row to an [`Event`] and lets nothing else of the row out. This
//! module stores the definitions (a pod setting), writes RFC 5545, and runs
//! the MCP tools, which review every text a subscriber would read before a
//! feed goes out (as `share_publicly` does).

use std::collections::BTreeSet;

use axum::http::HeaderMap;
use jiff::civil::Date;
use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::Pod;
use crate::construct::share::Shelf;

/// Most feeds a pod keeps.
pub const MAX_FEEDS: usize = 5;
/// Most sources (collections, each with its own mapping) one feed reads.
pub const MAX_SOURCES: usize = 10;
/// Most events one feed carries (the earliest, after [`PAST_DAYS`]).
pub const MAX_EVENTS: usize = 3000;
/// Most bytes of one feed's text: the host keeps it in `host.db`, outside
/// every pod's storage quota (as a public link's copy,
/// [`crate::public_copy::MAX_COPY_BYTES`]). Events past it are left out.
pub const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
/// How far back a feed reaches: events that ended in the last 30 days stay.
pub const PAST_DAYS: i64 = 30;
/// Events in a review's `next`.
const NEXT: usize = 10;
/// The pod setting holding the feeds' definitions (JSON).
const SETTING: &str = "calendar_feeds";
/// Wait this long after a data change before rendering again.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(3);
/// Render again at least this often (the 30-day window moves; the pod's
/// time zone may change).
const EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// What a subscriber's app is told about refreshing (Proton may ignore it).
const TTL: &str = "PT1H";

/// When an event starts or ends: a whole day (all-day events, `VALUE=DATE`)
/// or a moment (written in UTC, never floating).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventTime {
    Day(Date),
    At(i64),
}

impl EventTime {
    /// The instant it starts (a day: its midnight in `tz`).
    pub fn instant(&self, tz: &TimeZone) -> i64 {
        match self {
            EventTime::At(t) => *t,
            EventTime::Day(d) => d
                .to_zoned(tz.clone())
                .map(|z| z.timestamp().as_second())
                .unwrap_or(0),
        }
    }

    /// `2026-12-03` or `2026-12-03 19:00` in `tz`, for reviews.
    pub fn text(&self, tz: &TimeZone) -> String {
        match self {
            EventTime::Day(d) => d.to_string(),
            EventTime::At(t) => Timestamp::from_second(*t)
                .map(|ts| {
                    ts.to_zoned(tz.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default(),
        }
    }
}

/// One event of a feed, as a source mapped it from one of its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// What the event is of, stable across edits (`<collection>/<row id>`):
    /// with the pod, its UID, so an edited row updates in the subscriber's
    /// calendar rather than appearing twice.
    pub key: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: EventTime,
    /// Exclusive, as RFC 5545's `DTEND` (an all-day event on one day ends
    /// the next day).
    pub end: EventTime,
    /// When the row was made and last changed (unix seconds).
    pub created: i64,
    pub modified: i64,
}

/// A feed's rendered text on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCopy {
    /// `cal-` and 32 base32 characters: unguessable; the URL is
    /// `/ical/<token>.ics`.
    pub token: String,
    pub owner_pod: String,
    pub name: String,
    pub ics: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub revoked_at: Option<i64>,
}

/// A new feed token.
pub fn new_token() -> String {
    let mut buf = [0u8; 20];
    getrandom::fill(&mut buf).expect("OS random number generator");
    format!("cal-{}", construct_elements::canonical::base32(&buf))
}

/// Whether `s` looks like a feed token.
pub fn is_token(s: &str) -> bool {
    s.strip_prefix("cal-").is_some_and(|r| {
        (16..=64).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

// ---------- RFC 5545 ----------

/// A TEXT value: `\`, `;`, `,` and newlines escaped, carriage returns and
/// other control characters dropped.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// One content line, folded so no line is longer than 75 octets (a CRLF
/// and a space before each continuation, never inside a character), with
/// its CRLF.
pub fn fold(line: &str, out: &mut String) {
    let mut width = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        // The first line holds 75 octets; continuations 74 after their space.
        if width + n > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out.push_str("\r\n");
}

fn utc(t: i64) -> String {
    Timestamp::from_second(t)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn property(name: &str, t: &EventTime) -> String {
    match t {
        EventTime::Day(d) => format!("{name};VALUE=DATE:{}", d.strftime("%Y%m%d")),
        EventTime::At(t) => format!("{name}:{}", utc(*t)),
    }
}

/// The part of every UID that names the pod, without showing its id.
pub fn uid_domain(pod_id: &str) -> String {
    let h = Sha256::digest(format!("construct-calendar:{pod_id}").as_bytes());
    format!(
        "{}.calendar.the-construct",
        construct_elements::canonical::base32(&h[..10])
    )
}

/// The feed as RFC 5545 text (CRLF line ends, folded at 75 octets): a
/// `VCALENDAR` named `title` with one `VEVENT` per event, in order. Times
/// are UTC (`…Z`) or whole days, never floating; `zone` is the owner's
/// time zone, said for apps that show it (`X-WR-TIMEZONE`). Only the
/// events that fit in [`MAX_FEED_BYTES`] are written, from the first; the
/// answer says how many.
pub fn write(title: &str, zone: Option<&str>, domain: &str, events: &[Event]) -> (String, usize) {
    const END: &str = "END:VCALENDAR\r\n";
    let mut out = String::new();
    let mut line = |l: String| fold(&l, &mut out);
    line("BEGIN:VCALENDAR".into());
    line("VERSION:2.0".into());
    line("PRODID:-//The Construct//Calendar feed//EN".into());
    line("CALSCALE:GREGORIAN".into());
    line("METHOD:PUBLISH".into());
    line(format!("X-WR-CALNAME:{}", escape(title)));
    if let Some(z) = zone {
        line(format!("X-WR-TIMEZONE:{}", escape(z)));
    }
    line(format!("REFRESH-INTERVAL;VALUE=DURATION:{TTL}"));
    line(format!("X-PUBLISHED-TTL:{TTL}"));
    let mut kept = 0;
    for e in events {
        let event = vevent(e, domain);
        if out.len() + event.len() + END.len() > MAX_FEED_BYTES {
            break;
        }
        out.push_str(&event);
        kept += 1;
    }
    out.push_str(END);
    (out, kept)
}

/// One event's `VEVENT`, folded, with its CRLFs.
fn vevent(e: &Event, domain: &str) -> String {
    let mut out = String::new();
    let mut line = |l: String| fold(&l, &mut out);
    let uid: String = e
        .key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    line("BEGIN:VEVENT".into());
    line(format!("UID:{uid}@{domain}"));
    // The row's last change: an edited row is a newer version of the
    // same event.
    line(format!("DTSTAMP:{}", utc(e.modified)));
    line(format!("CREATED:{}", utc(e.created)));
    line(format!("LAST-MODIFIED:{}", utc(e.modified)));
    line(format!("SEQUENCE:{}", (e.modified - e.created).max(0)));
    line(property("DTSTART", &e.start));
    line(property("DTEND", &e.end));
    line(format!("SUMMARY:{}", escape(&e.summary)));
    if let Some(l) = e.location.as_deref().filter(|l| !l.is_empty()) {
        line(format!("LOCATION:{}", escape(l)));
    }
    if let Some(d) = e.description.as_deref().filter(|d| !d.is_empty()) {
        line(format!("DESCRIPTION:{}", escape(d)));
    }
    if matches!(e.start, EventTime::Day(_)) {
        line("TRANSP:TRANSPARENT".into());
    }
    line("END:VEVENT".into());
    out
}

// ---------- definitions ----------

/// A feed as the pod keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    /// `[a-z][a-z0-9_]*`, unique in the pod.
    pub name: String,
    /// The calendar's name in the subscriber's app (`X-WR-CALNAME`).
    pub title: String,
    /// What each source reads, as given (the source checks and reads it).
    pub sources: Vec<Value>,
    pub token: String,
    pub created_at: i64,
    pub updated_at: i64,
}

async fn load(pod: &Pod) -> Result<Vec<Feed>, Value> {
    pod.blocking(|s| s.pod_setting(SETTING))
        .await
        .map_err(|_| refuse("store_error", "the feeds could not be read"))
        .map(|v| {
            v.and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        })
}

/// The collections the pod's calendar feeds read (each source's
/// `collection`): others subscribe to them, so no grant writes their rows
/// (`crate::grant::for_call`).
pub async fn feed_row_sets(pod: &Pod) -> BTreeSet<String> {
    load(pod)
        .await
        .unwrap_or_default()
        .iter()
        .flat_map(|f| f.sources.iter())
        .filter_map(|s| s["collection"].as_str().map(str::to_owned))
        .collect()
}

async fn save(pod: &Pod, feeds: Vec<Feed>) -> Result<(), Value> {
    let text = serde_json::to_string(&feeds).unwrap_or_else(|_| "[]".into());
    pod.blocking(move |s| s.set_pod_setting(SETTING, &text))
        .await
        .map_err(|_| refuse("store_error", "the feeds could not be saved"))
}

fn valid_name(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

/// The pod's zone and its name.
async fn zone(pod: &Pod) -> (TimeZone, Option<String>) {
    let name = pod.blocking(crate::tz::time_zone_name).await.ok().flatten();
    let tz = name
        .as_deref()
        .and_then(|n| crate::tz::parse(n).ok())
        .unwrap_or(TimeZone::UTC);
    (tz, name)
}

/// A feed's events: what the sources map, from [`PAST_DAYS`] ago on, in
/// order of their start, at most [`MAX_EVENTS`].
pub async fn events(pod: &Pod, sources: &[Value], now: i64) -> Result<Vec<Event>, Value> {
    let since = now - PAST_DAYS * 86_400;
    for a in pod.catalog().adapters() {
        if let Some(f) = a.calendar_events(pod, sources) {
            let (tz, _) = zone(pod).await;
            let mut out: Vec<Event> = f
                .await?
                .into_iter()
                .filter(|e| e.end.instant(&tz) > since)
                .collect();
            out.sort_by(|a, b| (a.start.instant(&tz), &a.key).cmp(&(b.start.instant(&tz), &b.key)));
            out.truncate(MAX_EVENTS);
            return Ok(out);
        }
    }
    Err(refuse(
        "no_calendar_source",
        "no data source of this pod makes calendar events",
    ))
}

/// A feed's text, with its pod id's UID domain.
async fn render(
    pod: &Pod,
    pod_id: &str,
    title: &str,
    sources: &[Value],
) -> Result<(String, Vec<Event>), Value> {
    let mut evs = events(pod, sources, crate::util::now_secs()).await?;
    let (_, zone_name) = zone(pod).await;
    let (ics, kept) = write(title, zone_name.as_deref(), &uid_domain(pod_id), &evs);
    // The review and the count say only what goes out.
    evs.truncate(kept);
    Ok((ics, evs))
}

// ---------- the tools ----------

fn shelf(pod: &Pod) -> Result<(String, std::sync::Arc<dyn Shelf>), Value> {
    let Some((id, shelf)) = pod.shelf() else {
        return Err(refuse(
            "no_shelf",
            "this server runs the pod without a host",
        ));
    };
    if !shelf.public_links_enabled() {
        return Err(refuse(
            "public_links_off",
            "this pod can't publish calendar feeds: the server doesn't serve public links (CONSTRUCT_PUBLIC_WEB), or the pod is a temporary one",
        ));
    }
    Ok((id.to_owned(), shelf.clone()))
}

fn url(base: Option<&str>, token: &str) -> String {
    let path = format!("/ical/{token}.ics");
    base.map_or(path.clone(), |b| {
        format!("{}{path}", b.trim_end_matches('/'))
    })
}

/// The MCP tools (listed only while the host serves public links).
pub fn tool_specs() -> Vec<Value> {
    let source = json!({ "type": "object", "description": "{collection, where?, start, end?, time?, end_time?, duration_minutes?, title, location?, description?}",
        "properties": {
            "collection": { "type": "string" },
            "where": { "description": "{field, equals} or a list of them, all of which must match (as a view's where)" },
            "start": { "type": "string", "description": "A date or datetime field" },
            "end": { "type": "string", "description": "A date or datetime field (optional)" },
            "time": { "type": "string", "description": "A text field with the start time (\"8:00 AM\", \"15:45\"), for a date start" },
            "end_time": { "type": "string", "description": "A text field with the end time" },
            "duration_minutes": { "description": "Minutes, or a number field, when there is no end" },
            "title": { "type": "string", "description": "A template: \"{area} day\"" },
            "location": { "type": "string", "description": "A template: \"{venue}\"" },
            "description": { "type": "string", "description": "A template" } },
        "required": ["collection", "start", "title"] });
    vec![
        json!({ "name": "set_calendar_feed",
            "description": "Create or change a calendar feed: a private .ics URL a calendar app subscribes to (Proton Calendar: Add calendar → From link), made from rows the pod keeps (decision ical-feeds). A pod keeps at most 5. Each source maps a collection's rows to events: where filters rows (as a view's where); start is a date or datetime field (a date with no time is an all-day event); end, time and end_time (text such as \"8:00 AM\" or \"15:45\") and duration_minutes are optional (a timed event with none lasts an hour); title, location and description are templates of the row's fields (\"{area} day\"). Only what a source maps leaves the pod: no other field of a row is written to the feed. Events from 30 days ago on are included, in the pod's time zone (set_time_zone). Without confirm nothing is published: the answer is a review of every title, location and description a subscriber would read and the next events; show it to the user and ask. With confirm (the review_id) the feed goes out and answers its url; it is rendered again whenever the rows change. Changing a live feed keeps its url.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string", "description": "[a-z][a-z0-9_]*: the feed's name in the pod" },
                "title": { "type": "string", "description": "The calendar's name in the app (default: the name)" },
                "sources": { "type": "array", "items": source },
                "confirm": { "type": "string", "description": "The review_id of the review the user approved" } },
                "required": ["name", "sources"] } }),
        json!({ "name": "list_calendar_feeds",
            "description": "The pod's calendar feeds (set_calendar_feed): each one's url, title, sources and how many events it carries.",
            "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "revoke_calendar_feed",
            "description": "Delete a calendar feed: its url stops working at once and its copy leaves the server (subscribers' apps keep what they last fetched until they refresh).",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string" } }, "required": ["name"] } }),
        json!({ "name": "rotate_calendar_feed",
            "description": "Give a calendar feed a new url (if the old one leaked): the old url stops working at once, and the user subscribes again with the new one.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string" } }, "required": ["name"] } }),
    ]
}

/// Runs a calendar feed tool; `None` when `name` isn't one.
pub async fn tool(
    pod: &Pod,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> Option<Result<Value, Value>> {
    if !tool_specs().iter().any(|t| t["name"] == name) {
        return None;
    }
    let (pod_id, shelf) = match shelf(pod) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let base = crate::public_copy::base_url(headers);
    let base = base.as_deref();
    // The feed list is read, checked against MAX_FEEDS and saved again
    // around slow renders: one change at a time.
    let _one_at_a_time = match name {
        "set_calendar_feed" | "revoke_calendar_feed" | "rotate_calendar_feed" => {
            Some(pod.sharing.lock().await)
        }
        _ => None,
    };
    Some(match name {
        "set_calendar_feed" => set(pod, &pod_id, shelf.as_ref(), args, base).await,
        "list_calendar_feeds" => list(pod, base).await,
        "revoke_calendar_feed" => revoke(pod, &pod_id, shelf.as_ref(), args).await,
        "rotate_calendar_feed" => rotate(pod, &pod_id, shelf.as_ref(), args, base).await,
        _ => return None,
    })
}

fn review_id(name: &str, title: &str, sources: &[Value], ics: &str) -> String {
    let mut h = Sha256::new();
    h.update(json!([name, title, sources]).to_string().as_bytes());
    h.update(ics.as_bytes());
    format!(
        "rev-{}",
        &construct_elements::canonical::base32(&h.finalize())[..20]
    )
}

fn texts(evs: &[Event], f: impl Fn(&Event) -> Option<&str>) -> Vec<String> {
    evs.iter()
        .filter_map(f)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn set(
    pod: &Pod,
    pod_id: &str,
    shelf: &dyn Shelf,
    args: &Value,
    base: Option<&str>,
) -> Result<Value, Value> {
    let name = args["name"].as_str().unwrap_or("").trim().to_owned();
    if !valid_name(&name) {
        return Err(refuse(
            "invalid_name",
            "a feed's name is [a-z][a-z0-9_]*, at most 40 characters",
        ));
    }
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&name)
        .chars()
        .take(120)
        .collect::<String>();
    let sources = match &args["sources"] {
        Value::Array(a) if !a.is_empty() && a.len() <= MAX_SOURCES => a.clone(),
        _ => {
            return Err(refuse(
                "invalid_sources",
                format!(
                    "sources is a list of 1 to {MAX_SOURCES} {{collection, start, title, ...}}"
                ),
            ));
        }
    };
    let mut feeds = load(pod).await?;
    let existing = feeds.iter().position(|f| f.name == name);
    if existing.is_none() && feeds.len() >= MAX_FEEDS {
        return Err(refuse(
            "too_many_feeds",
            format!(
                "a pod keeps at most {MAX_FEEDS} calendar feeds: revoke one or add sources to one"
            ),
        ));
    }
    let (ics, evs) = render(pod, pod_id, &title, &sources).await?;
    let id = review_id(&name, &title, &sources, &ics);
    if args["confirm"].as_str() != Some(id.as_str()) {
        let (tz, zone_name) = zone(pod).await;
        let now = crate::util::now_secs();
        let next: Vec<Value> = evs
            .iter()
            .filter(|e| e.end.instant(&tz) > now)
            .take(NEXT)
            .map(|e| {
                let mut v = json!({ "start": e.start.text(&tz), "title": e.summary });
                if let EventTime::At(_) = e.end {
                    v["end"] = json!(e.end.text(&tz));
                } else {
                    v["all_day"] = json!(true);
                }
                if let Some(l) = &e.location {
                    v["location"] = json!(l);
                }
                v
            })
            .collect();
        return Ok(json!({
            "review_id": id,
            "published": "nothing yet: confirm with this review_id once the user approves",
            "calendar": title,
            "time_zone": zone_name.unwrap_or_else(|| "UTC (set_time_zone to change)".into()),
            "events": evs.len(),
            "a_subscriber_reads": {
                "titles": texts(&evs, |e| Some(e.summary.as_str())),
                "locations": texts(&evs, |e| e.location.as_deref()),
                "descriptions": texts(&evs, |e| e.description.as_deref()),
                "and": "each event's start and end; nothing else of the rows",
            },
            "next": next,
            "updates": "the feed is rendered again whenever these rows change; the calendar app fetches it on its own schedule (Proton: every few hours)",
        }));
    }
    let now = crate::util::now_secs();
    let feed = match existing {
        Some(i) => {
            let f = &mut feeds[i];
            f.title = title;
            f.sources = sources;
            f.updated_at = now;
            f.clone()
        }
        None => {
            let f = Feed {
                name: name.clone(),
                title,
                sources,
                token: new_token(),
                created_at: now,
                updated_at: now,
            };
            feeds.push(f.clone());
            f
        }
    };
    shelf
        .put_calendar(CalendarCopy {
            token: feed.token.clone(),
            owner_pod: pod_id.to_owned(),
            name: feed.name.clone(),
            ics,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        })
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    save(pod, feeds).await?;
    tracing::info!("a calendar feed was published");
    Ok(json!({
        "name": feed.name,
        "title": feed.title,
        "url": url(base, &feed.token),
        "events": evs.len(),
        "detail": "anyone with this url can read these events: give it only to the user's calendar app (Proton Calendar: Add calendar → From link). revoke_calendar_feed stops it; rotate_calendar_feed gives it a new url",
    }))
}

async fn list(pod: &Pod, base: Option<&str>) -> Result<Value, Value> {
    let feeds = load(pod).await?;
    let mut out = Vec::new();
    for f in feeds {
        let events = events(pod, &f.sources, crate::util::now_secs()).await;
        let mut v = json!({ "name": f.name, "title": f.title, "url": url(base, &f.token),
            "sources": f.sources, "updated_at": f.updated_at });
        match events {
            Ok(e) => v["events"] = json!(e.len()),
            Err(e) => v["problem"] = e,
        }
        out.push(v);
    }
    Ok(json!({ "feeds": out }))
}

async fn take(pod: &Pod, args: &Value) -> Result<(Vec<Feed>, usize), Value> {
    let name = args["name"].as_str().unwrap_or("");
    let feeds = load(pod).await?;
    let i = feeds
        .iter()
        .position(|f| f.name == name)
        .ok_or_else(|| refuse("unknown_feed", "this pod has no calendar feed of that name"))?;
    Ok((feeds, i))
}

async fn revoke(pod: &Pod, pod_id: &str, shelf: &dyn Shelf, args: &Value) -> Result<Value, Value> {
    let (mut feeds, i) = take(pod, args).await?;
    let f = feeds.remove(i);
    shelf
        .revoke_calendar(pod_id, &f.token)
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    save(pod, feeds).await?;
    Ok(json!({ "ok": true, "name": f.name,
        "detail": "the feed's url no longer answers and its copy left the server; remove the calendar from the app too" }))
}

async fn rotate(
    pod: &Pod,
    pod_id: &str,
    shelf: &dyn Shelf,
    args: &Value,
    base: Option<&str>,
) -> Result<Value, Value> {
    let (mut feeds, i) = take(pod, args).await?;
    let old = feeds[i].token.clone();
    let (ics, _) = render(pod, pod_id, &feeds[i].title, &feeds[i].sources).await?;
    let token = new_token();
    shelf
        .put_calendar(CalendarCopy {
            token: token.clone(),
            owner_pod: pod_id.to_owned(),
            name: feeds[i].name.clone(),
            ics,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        })
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    feeds[i].token = token.clone();
    feeds[i].updated_at = crate::util::now_secs();
    let name = feeds[i].name.clone();
    save(pod, feeds).await?;
    shelf
        .revoke_calendar(pod_id, &old)
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    Ok(json!({ "name": name, "url": url(base, &token),
        "detail": "the old url stopped working: subscribe again in the calendar app with this one (and remove the old calendar)" }))
}

// ---------- keeping feeds current ----------

/// Renders every feed of the pod again and copies what changed to the host.
pub async fn refresh(pod: &Pod) {
    let Ok((pod_id, shelf)) = shelf(pod) else {
        return;
    };
    let Ok(feeds) = load(pod).await else {
        return;
    };
    for f in feeds {
        let ics = match render(pod, &pod_id, &f.title, &f.sources).await {
            Ok((ics, _)) => ics,
            // A collection or field it reads went: the feed keeps its last copy.
            Err(e) => {
                tracing::warn!(error = %e["error"], "a calendar feed couldn't be rendered");
                continue;
            }
        };
        let current = shelf.calendar_by_token(&f.token).ok().flatten();
        if current.as_ref().is_some_and(|c| c.ics == ics) {
            continue;
        }
        let _ = shelf.put_calendar(CalendarCopy {
            token: f.token.clone(),
            owner_pod: pod_id.clone(),
            name: f.name.clone(),
            ics,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        });
    }
}

/// Keeps the pod's feeds current: after its data changes (and settles), and
/// every hour. Started with the pod.
pub fn watch(pod: &Pod) {
    let Some((_, shelf)) = pod.shelf() else {
        return;
    };
    if !shelf.public_links_enabled() {
        return;
    }
    let mut rx = pod.subscribe_data();
    let weak = std::sync::Arc::downgrade(pod);
    tokio::spawn(async move {
        loop {
            // The pod's own data (a session's feed, such as Pixelfed's,
            // never makes events).
            let changed = tokio::time::timeout(EVERY, async {
                loop {
                    match rx.recv().await {
                        Ok(e) if e.token_hash.is_none() => return true,
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return true,
                        Err(_) => return false,
                    }
                }
            })
            .await;
            if changed == Ok(false) {
                return;
            }
            tokio::time::sleep(SETTLE).await;
            while rx.try_recv().is_ok() {}
            let Some(pod) = weak.upgrade() else { return };
            if load(&pod).await.is_ok_and(|f| !f.is_empty()) {
                refresh(&pod).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> EventTime {
        EventTime::Day(s.parse().unwrap())
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape("Skate, eat; repeat\\\nback\r"),
            "Skate\\, eat\\; repeat\\\\\\nback"
        );
    }

    #[test]
    fn long_lines_fold_at_75_octets_without_splitting_characters() {
        let long = format!("SUMMARY:{}", "é".repeat(60));
        let mut out = String::new();
        fold(&long, &mut out);
        assert!(out.ends_with("\r\n"));
        let lines: Vec<&str> = out.trim_end_matches("\r\n").split("\r\n").collect();
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(l.len() <= 75, "{} octets: {l}", l.len());
        }
        for l in &lines[1..] {
            assert!(l.starts_with(' '));
        }
        // Unfolding gives the line back.
        let unfolded = lines.join("\r\n").replace("\r\n ", "");
        assert_eq!(unfolded, long);
    }

    #[test]
    fn a_calendar_has_crlf_lines_utc_times_and_whole_days() {
        let evs = vec![
            Event {
                key: "focus_days/r1".into(),
                summary: "House day".into(),
                location: None,
                description: None,
                start: day("2026-12-05"),
                end: day("2026-12-06"),
                created: 1_700_000_000,
                modified: 1_700_000_100,
            },
            Event {
                key: "events/r2".into(),
                summary: "Skating, at night".into(),
                location: Some("Rink; Main St".into()),
                description: Some("Line one\nLine two".into()),
                start: EventTime::At(1_796_000_000),
                end: EventTime::At(1_796_003_600),
                created: 1_700_000_000,
                modified: 1_700_000_000,
            },
        ];
        let (ics, kept) = write(
            "My plans",
            Some("America/Denver"),
            "x.calendar.the-construct",
            &evs,
        );
        assert_eq!(kept, 2);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(!ics.replace("\r\n", "").contains('\n'));
        assert!(ics.contains("X-WR-CALNAME:My plans\r\n"));
        assert!(ics.contains("REFRESH-INTERVAL;VALUE=DURATION:PT1H\r\n"));
        assert!(ics.contains("X-PUBLISHED-TTL:PT1H\r\n"));
        assert!(ics.contains("DTSTART;VALUE=DATE:20261205\r\nDTEND;VALUE=DATE:20261206\r\n"));
        assert!(ics.contains("UID:focus_days-r1@x.calendar.the-construct\r\n"));
        assert!(ics.contains("SEQUENCE:100\r\n"));
        assert!(
            ics.contains(&format!("DTSTART:{}\r\n", utc(1_796_000_000))),
            "{ics}"
        );
        assert!(ics.contains("SUMMARY:Skating\\, at night\r\n"));
        assert!(ics.contains("LOCATION:Rink\\; Main St\r\n"));
        assert!(ics.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        // Never a floating time: every DTSTART/DTEND is a date or ends in Z.
        for l in ics
            .split("\r\n")
            .filter(|l| l.starts_with("DTSTART") || l.starts_with("DTEND"))
        {
            assert!(l.contains("VALUE=DATE:") || l.ends_with('Z'), "{l}");
        }
    }

    /// The host keeps a feed's text outside every pod's quota: a feed of
    /// long texts keeps the earliest events that fit, each whole.
    #[test]
    fn a_feed_stops_at_its_byte_cap() {
        let evs: Vec<Event> = (0..MAX_EVENTS as i64)
            .map(|i| Event {
                key: format!("notes/r{i}"),
                summary: "😀".repeat(1000),
                location: Some("😀".repeat(1000)),
                description: Some("😀".repeat(1000)),
                start: EventTime::At(1_796_000_000 + i * 3600),
                end: EventTime::At(1_796_003_600 + i * 3600),
                created: 1_700_000_000,
                modified: 1_700_000_000,
            })
            .collect();
        let (ics, kept) = write("Long", None, "x.calendar.the-construct", &evs);
        assert!(ics.len() <= MAX_FEED_BYTES, "{} bytes", ics.len());
        assert!(0 < kept && kept < evs.len(), "{kept}");
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), kept);
        assert_eq!(ics.matches("END:VEVENT").count(), kept);
        assert!(ics.contains("UID:notes-r0@"));
        assert!(ics.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
    }

    #[test]
    fn tokens_and_uid_domains() {
        let t = new_token();
        assert!(is_token(&t), "{t}");
        assert!(!is_token("pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(!is_token("cal-../../etc"));
        assert_eq!(uid_domain("p1"), uid_domain("p1"));
        assert_ne!(uid_domain("p1"), uid_domain("p2"));
        assert!(!uid_domain("p1").contains("p1"));
    }
}
