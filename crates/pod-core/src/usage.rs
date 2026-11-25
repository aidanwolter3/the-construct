//! Usage trails on the pod (decision `usage-trails`): what the owner's
//! devices say about how they use their screens (`protocol::usage`), kept
//! only while the `usage` jack is plugged (off by default), and read by the
//! Anticipate job as totals, never as raw trails.
//!
//! - `POST /v1/usage {events: [...]}`: the owner's own device sessions only
//!   (not an MCP credential, a public visitor or the pod's own requests), at
//!   most [`protocol::usage::MAX_BATCH`] events; events that aren't valid
//!   are dropped. With the jack unplugged: 204 and nothing kept.
//! - Kept: the raw events [`KEEP_RAW_DAYS`] days (a device's go with it once
//!   it is forgotten), daily totals [`KEEP_DAILY_DAYS`] days.
//! - Read: `usage_report` and `usage_paths` (MCP, `edit`): opens, starts,
//!   completions, the step people drop off at, median time, backtracks
//!   (Back within 3 s) and dead taps (a tap that did nothing); and the most
//!   frequent screen sequences.

use std::collections::BTreeMap;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::Pod;
use protocol::usage::UsageEvent;

/// The jack that keeps usage.
pub const JACK: &str = "usage";
/// Raw events are kept this long.
pub const KEEP_RAW_DAYS: i64 = 14;
/// Daily totals are kept this long.
pub const KEEP_DAILY_DAYS: i64 = 180;
/// A Back this soon after a screen opened is a backtrack.
const BACKTRACK_MS: u64 = 3_000;
/// A device's screens more than this apart are separate visits.
const VISIT_GAP_SECS: i64 = 30 * 60;

pub(crate) fn router() -> axum::Router<Pod> {
    axum::Router::new().route(protocol::usage::USAGE_PATH, axum::routing::post(post_usage))
}

async fn post_usage(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(r) = crate::refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    if pod.internal_caller(&headers).is_some() || pod.host_credential(&headers).is_some() {
        return crate::json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "owner_devices_only", "detail": "usage comes from the owner's own devices" }),
        );
    }
    let Some(device) = headers
        .get(protocol::DEVICE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|d| !d.is_empty() && d.len() <= 128)
        .map(str::to_owned)
    else {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "no_device", "detail": "usage names the device that sends it" }),
        );
    };
    #[derive(serde::Deserialize)]
    struct Batch {
        events: Vec<Value>,
    }
    let Ok(batch) = serde_json::from_slice::<Batch>(&body) else {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": "the body is {events: [...]}" }),
        );
    };
    if batch.events.len() > protocol::usage::MAX_BATCH {
        return crate::json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({ "error": "too_many_events", "detail": format!("at most {} a batch", protocol::usage::MAX_BATCH) }),
        );
    }
    let events: Vec<UsageEvent> = batch
        .events
        .into_iter()
        .filter_map(|v| serde_json::from_value::<UsageEvent>(v).ok())
        .filter(UsageEvent::valid)
        .collect();
    let r = crate::blocking(&pod, move |s| {
        if !crate::jacks::core_plugged(s, JACK) {
            return Ok(None);
        }
        s.with_conn(|c| store(c, &device, &events)).map(Some)
    })
    .await;
    match r {
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Ok(Some(n)) => axum::Json(json!({ "ok": true, "kept": n })).into_response(),
        Err(e) => e.into_response(),
    }
}

fn day(at: i64) -> String {
    jiff::Timestamp::from_second(at)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Keep `events` from `device`, with their daily totals; prune.
pub fn store(c: &Connection, device: &str, events: &[UsageEvent]) -> rusqlite::Result<usize> {
    let now = crate::util::now_secs();
    let mut n = 0;
    for e in events {
        // A device's clock may be off; nothing from the future or the far past.
        if e.at > now + 3_600 || e.at < now - KEEP_RAW_DAYS * 86_400 {
            continue;
        }
        c.execute(
            "INSERT INTO usage_events (device, at, kind, layout, route, from_layout, node, intent,
                                       target, collection, effect, dwell_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                device,
                e.at,
                e.kind,
                e.layout,
                e.route,
                e.from,
                e.node,
                e.intent,
                e.target,
                e.collection,
                e.effect,
                e.dwell_ms.map(|d| d as i64)
            ],
        )?;
        c.execute(
            "INSERT INTO usage_daily (day, kind, layout, node, target, effect, n)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT (day, kind, layout, node, target, effect) DO UPDATE SET n = n + 1",
            params![
                day(e.at),
                e.kind,
                e.layout,
                e.node.clone().unwrap_or_default(),
                e.target.clone().unwrap_or_default(),
                e.effect.clone().unwrap_or_default()
            ],
        )?;
        n += 1;
    }
    c.execute(
        "DELETE FROM usage_events WHERE at < ?1",
        [now - KEEP_RAW_DAYS * 86_400],
    )?;
    c.execute(
        "DELETE FROM usage_events WHERE at < ?1 AND device NOT IN (SELECT id FROM client_devices)",
        [now - 86_400],
    )?;
    c.execute(
        "DELETE FROM usage_daily WHERE day < ?1",
        [day(now - KEEP_DAILY_DAYS * 86_400)],
    )?;
    Ok(n)
}

struct Ev {
    device: String,
    at: i64,
    kind: String,
    layout: String,
    node: Option<String>,
    target: Option<String>,
    collection: Option<String>,
    effect: Option<String>,
    dwell_ms: Option<i64>,
}

fn events_since(c: &Connection, since: i64) -> rusqlite::Result<Vec<Ev>> {
    let mut stmt = c.prepare(
        "SELECT device, at, kind, layout, node, target, collection, effect, dwell_ms
         FROM usage_events WHERE at >= ?1 ORDER BY device, at, rowid",
    )?;
    stmt.query_map([since], |r| {
        Ok(Ev {
            device: r.get(0)?,
            at: r.get(1)?,
            kind: r.get(2)?,
            layout: r.get(3)?,
            node: r.get(4)?,
            target: r.get(5)?,
            collection: r.get(6)?,
            effect: r.get(7)?,
            dwell_ms: r.get(8)?,
        })
    })?
    .collect()
}

fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// `usage_report {layout? | start_layout + done_collection, days?}`.
pub fn report(c: &Connection, args: &Value) -> Result<Value, Value> {
    let days = args["days"].as_i64().unwrap_or(7).clamp(1, KEEP_RAW_DAYS);
    let since = crate::util::now_secs() - days * 86_400;
    let evs = events_since(c, since)
        .map_err(|e| json!({ "error": "store_error", "detail": e.to_string() }))?;
    if let (Some(start), Some(done)) = (
        args["start_layout"].as_str(),
        args["done_collection"].as_str(),
    ) {
        // A journey: a start is its first screen opening; a completion is a
        // write to its collection that went through within 30 minutes.
        let (mut starts, mut done_n, mut times) = (0, 0, Vec::new());
        let mut open: Option<(String, i64)> = None;
        for e in &evs {
            if e.kind == "screen" && e.layout == start {
                starts += 1;
                open = Some((e.device.clone(), e.at));
            } else if e.kind == "result"
                && e.collection.as_deref() == Some(done)
                && e.effect.as_deref() == Some("ok")
                && let Some((d, t)) = &open
                && *d == e.device
                && e.at - t <= VISIT_GAP_SECS
            {
                done_n += 1;
                times.push(e.at - t);
                open = None;
            }
        }
        return Ok(json!({
            "days": days, "starts": starts, "completions": done_n,
            "completion_rate": if starts > 0 { json!((done_n as f64) / (starts as f64)) } else { Value::Null },
            "median_secs": median(times),
            "note": "Small numbers mean little: judge a change only on 20 or more starts each side."
        }));
    }
    let layout = args["layout"].as_str().ok_or_else(|| {
        json!({ "error": "invalid_arguments", "detail": "pass layout, or start_layout and done_collection" })
    })?;
    let mut opens = 0;
    let mut from: BTreeMap<String, i64> = BTreeMap::new();
    let mut taps: BTreeMap<(String, String), (i64, i64)> = BTreeMap::new();
    let (mut backs, mut backtracks) = (0, 0);
    for e in evs.iter().filter(|e| e.layout == layout) {
        match e.kind.as_str() {
            "screen" => opens += 1,
            "tap" => {
                let k = (
                    e.node.clone().unwrap_or_default(),
                    e.target.clone().unwrap_or_default(),
                );
                let t = taps.entry(k).or_default();
                t.0 += 1;
                if e.effect.as_deref() == Some("none") {
                    t.1 += 1;
                }
            }
            "back" => {
                backs += 1;
                if e.dwell_ms.is_some_and(|d| (d as u64) < BACKTRACK_MS) {
                    backtracks += 1;
                }
            }
            _ => {}
        }
    }
    let mut stmt = c
        .prepare("SELECT from_layout, COUNT(*) FROM usage_events WHERE at >= ?1 AND kind = 'screen' AND layout = ?2 GROUP BY from_layout ORDER BY 2 DESC LIMIT 10")
        .map_err(|e| json!({ "error": "store_error", "detail": e.to_string() }))?;
    for r in stmt
        .query_map(params![since, layout], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(|e| json!({ "error": "store_error", "detail": e.to_string() }))?
        .flatten()
    {
        from.insert(r.0.unwrap_or_default(), r.1);
    }
    let mut tap_list: Vec<Value> = taps
        .into_iter()
        .map(|((node, target), (n, dead))| json!({ "node": node, "target": target, "taps": n, "dead_taps": dead }))
        .collect();
    tap_list.sort_by_key(|t| -t["taps"].as_i64().unwrap_or(0));
    tap_list.truncate(30);
    Ok(json!({
        "layout": layout, "days": days, "opens": opens, "opened_from": from,
        "backs": backs, "backtracks": backtracks, "taps": tap_list,
        "note": "Totals over the owner's devices. Labels are the layout's own names and may hold text an author wrote."
    }))
}

/// `usage_paths {days?, min_count?}`: the most frequent sequences of 2 to 4
/// screens within a visit.
pub fn paths(c: &Connection, args: &Value) -> Result<Value, Value> {
    let days = args["days"].as_i64().unwrap_or(14).clamp(1, KEEP_RAW_DAYS);
    let min = args["min_count"].as_i64().unwrap_or(3).max(3);
    let since = crate::util::now_secs() - days * 86_400;
    let evs = events_since(c, since)
        .map_err(|e| json!({ "error": "store_error", "detail": e.to_string() }))?;
    let mut counts: BTreeMap<Vec<String>, i64> = BTreeMap::new();
    let mut visit: Vec<String> = Vec::new();
    let mut last: Option<(String, i64)> = None;
    let flush = |v: &mut Vec<String>, counts: &mut BTreeMap<Vec<String>, i64>| {
        for len in 2..=4 {
            for w in v.windows(len) {
                *counts.entry(w.to_vec()).or_default() += 1;
            }
        }
        v.clear();
    };
    for e in evs.iter().filter(|e| e.kind == "screen") {
        let new_visit = last
            .as_ref()
            .is_none_or(|(d, t)| *d != e.device || e.at - t > VISIT_GAP_SECS);
        if new_visit {
            flush(&mut visit, &mut counts);
        }
        if visit.last() != Some(&e.layout) {
            visit.push(e.layout.clone());
        }
        last = Some((e.device.clone(), e.at));
    }
    flush(&mut visit, &mut counts);
    let mut out: Vec<(Vec<String>, i64)> = counts.into_iter().filter(|(_, n)| *n >= min).collect();
    out.sort_by_key(|(p, n)| (-n, -(p.len() as i64)));
    out.truncate(30);
    Ok(json!({
        "days": days,
        "paths": out.into_iter().map(|(p, n)| json!({ "screens": p, "count": n })).collect::<Vec<_>>(),
    }))
}

pub(crate) fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "usage_report",
            "description": "How the owner really uses a screen or a journey, from their devices (decision usage-trails; the pod's usage jack must be plugged). layout: opens, where it is opened from, taps per node (and dead taps), Backs and backtracks (Back within 3 s). start_layout + done_collection: a journey's starts, completions (a write to that collection within 30 minutes), completion rate and median time. days: 1 to 14 (default 7). Totals only; small numbers mean little.",
            "inputSchema": { "type": "object", "properties": {
                "layout": { "type": "string" }, "start_layout": { "type": "string" },
                "done_collection": { "type": "string" }, "days": { "type": "integer" } } }
        }),
        json!({
            "name": "usage_paths",
            "description": "The owner's most frequent sequences of 2 to 4 screens within one visit (days 1 to 14, default 14; min_count at least 3): paths no journey covers are journeys to propose.",
            "inputSchema": { "type": "object", "properties": {
                "days": { "type": "integer" }, "min_count": { "type": "integer" } } }
        }),
    ]
}
