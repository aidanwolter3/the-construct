//! The feeds' data: the [`State`] as entities with display text worked out
//! (status words and colours, "12 min ago", "$0.42"), so layouts bind
//! fields and need no logic.

use pod_core::tz::jiff::{Timestamp, tz::TimeZone};
use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

use crate::db::Health;
use crate::state::{self, Session, State};
use crate::text;

pub const STATUS_URI: &str = "agents://runner/status";

pub const BLUE: &str = "#1E66D0";
pub const ORANGE: &str = "#B26A00";
pub const GREEN: &str = "#2E7D32";
pub const RED: &str = "#C62828";
pub const GRAY: &str = "#6B6B76";

fn record(v: Value) -> EntityRecord {
    match v {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord::new(),
    }
}

/// A session status's word and colour.
pub fn status_look(status: &str) -> (&'static str, &'static str) {
    match status {
        state::STARTING => ("Starting", BLUE),
        state::WORKING => ("Working", BLUE),
        state::WAITING_PERMISSION => ("Needs you", ORANGE),
        state::WAITING_INPUT => ("Waiting for you", ORANGE),
        state::IDLE => ("Idle", GRAY),
        state::FINISHED => ("Finished", GREEN),
        state::FAILED => ("Failed", RED),
        state::STALE => ("Lost", GRAY),
        "missed" => ("Missed", RED),
        "running" => ("Running", BLUE),
        "" => ("", GRAY),
        _ => ("Unknown", GRAY),
    }
}

fn needs_you(s: &Session) -> bool {
    matches!(
        s.status.as_str(),
        state::WAITING_PERMISSION | state::WAITING_INPUT
    )
}

/// Whether `s` is doing something right now (starting or working). Idle
/// sessions are not: a sandbox's Remote Control keeps an idle standby
/// session open, and a finished run's session stays open idle.
fn is_working(s: &Session) -> bool {
    matches!(s.status.as_str(), state::STARTING | state::WORKING)
}

/// Where `s` sorts in `agents:sessions`: waiting for you, then working,
/// then idle, then ended.
fn rank(s: &Session) -> u8 {
    if needs_you(s) {
        0
    } else if is_working(s) {
        1
    } else if s.is_active() {
        2
    } else {
        3
    }
}

/// The zone a schedule's times read in: its own `timezone`, else the pod's.
fn schedule_zone(name: &str, pod: &TimeZone) -> TimeZone {
    pod_core::tz::parse(name).unwrap_or_else(|_| pod.clone())
}

/// A time in zone `tz`, e.g. `Sat 03:00`.
fn local(ms: i64, tz: &TimeZone) -> String {
    Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(tz.clone()).strftime("%a %H:%M").to_string())
        .unwrap_or_default()
}

/// What the runner connection looks like: the `agents:status` entity.
pub fn status(st: Option<&(State, Health)>, host: &str, now_ms: i64) -> EntityRecord {
    let Some((st, h)) = st else {
        return record(json!({
            "signed_in": false, "connected": false, "stale": false, "host": "",
            "status_text": "Not signed in to an agent runner",
            "summary_text": "", "working_count": 0, "needs_you_count": 0, "color": GRAY,
            "updated_text": "",
        }));
    };
    let needs = st.sessions.values().filter(|s| needs_you(s)).count();
    let working = st.sessions.values().filter(|s| is_working(s)).count();
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if needs > 0 {
        parts.push(format!("{needs} needs you"));
    }
    let summary = if parts.is_empty() {
        "Nothing running".to_owned()
    } else {
        parts.join(" · ")
    };
    let updated = h
        .last_ok_at
        .map(|t| text::ago(t, now_ms))
        .unwrap_or_default();
    let status_text = if h.connected {
        format!("Connected to {host}")
    } else {
        format!(
            "Can't reach {host}{}; showing what it last said{}",
            h.last_error
                .as_deref()
                .map(|e| format!(" ({})", text::clean(e, 120)))
                .unwrap_or_default(),
            if updated.is_empty() {
                String::new()
            } else {
                format!(" {updated}")
            }
        )
    };
    record(json!({
        "signed_in": true,
        "connected": h.connected,
        "stale": !h.connected,
        "host": host,
        "status_text": status_text,
        "summary_text": summary,
        "working_count": working,
        "needs_you_count": needs,
        "color": if !h.connected { GRAY } else if needs > 0 { ORANGE } else if working > 0 { BLUE } else { GREEN },
        "updated_text": updated,
    }))
}

/// One session's entity.
pub fn session(s: &Session, now_ms: i64) -> EntityRecord {
    let (status_text, color) = status_look(&s.status);
    let elapsed = if s.started_at <= 0 {
        String::new()
    } else if s.is_active() {
        text::duration((now_ms - s.started_at) / 1000)
    } else if s.duration_s > 0.0 {
        text::duration(s.duration_s as i64)
    } else {
        text::duration((s.updated_at - s.started_at) / 1000)
    };
    let tokens = s.input_tokens + s.output_tokens;
    record(json!({
        "id": s.id,
        "title": if s.title.is_empty() { "Untitled session" } else { &s.title },
        "status": s.status,
        "status_text": status_text,
        "status_color": color,
        "active": s.is_active(),
        "needs_you": needs_you(s),
        "current_tool": state::tool_text(&s.current_tool, &s.tool_detail),
        "summary": s.summary,
        "pr_url": s.pr_url,
        "error": s.error,
        "cost_text": text::cost(s.cost_usd),
        "tokens_text": if tokens > 0 { format!("{tokens} tokens") } else { String::new() },
        "elapsed_text": elapsed,
        "updated_text": if s.updated_at > 0 { text::ago(s.updated_at, now_ms) } else { String::new() },
        "task_id": s.task_id,
        "schedule": state::schedule_of_task(&s.task_id).unwrap_or(""),
        "sandbox": s.sprite,
        "workspace": s.workspace,
        "claude_url": text::claude_url(&s.id).unwrap_or_default(),
    }))
}

/// The `sessions` feed: `agents:status` and `agents:sessions` (the ones
/// that need you, then working, idle and ended ones, each newest first).
pub fn sessions(st: Option<&(State, Health)>, host: &str, now_ms: i64) -> DataBundle {
    let mut b = DataBundle::default();
    b.entities
        .insert(STATUS_URI.into(), status(st, host, now_ms));
    b.queries
        .insert(crate::STATUS_QUERY.into(), vec![STATUS_URI.into()]);
    let mut list: Vec<&Session> = st
        .map(|(s, _)| s.sessions.values().collect())
        .unwrap_or_default();
    list.sort_by_key(|s| (rank(s), -s.updated_at.max(s.started_at)));
    let mut order = Vec::new();
    for s in list {
        let uri = format!("agents://sessions/{}", s.id);
        b.entities.insert(uri.clone(), session(s, now_ms));
        order.push(uri);
    }
    b.queries.insert(crate::SESSIONS_QUERY.into(), order);
    b
}

/// The `sandboxes` feed: `agents:sandboxes`, awake first.
pub fn sandboxes(st: Option<&(State, Health)>, now_ms: i64) -> DataBundle {
    let mut b = DataBundle::default();
    let mut list: Vec<_> = st
        .map(|(s, _)| s.sprites.values().collect())
        .unwrap_or_default();
    list.sort_by_key(|x| (x.state != "awake", x.name.clone()));
    let mut order = Vec::new();
    for x in list {
        let uri = format!("agents://sandboxes/{}", x.name);
        let awake = x.state == "awake";
        b.entities.insert(
            uri.clone(),
            record(json!({
                "name": x.name,
                "state": x.state,
                "awake": awake,
                "state_text": match x.state.as_str() {
                    "awake" => "Awake",
                    "asleep" => "Asleep",
                    "" => "Unknown",
                    _ => "Unknown",
                },
                "color": if awake { GREEN } else { GRAY },
                "last_seen_text": if x.last_seen > 0 { text::ago(x.last_seen, now_ms) } else { String::new() },
                "workspace": x.workspace,
            })),
        );
        order.push(uri);
    }
    b.queries.insert(crate::SANDBOXES_QUERY.into(), order);
    b
}

/// The `checks` feed: `agents:checks`, warnings first.
pub fn checks(st: Option<&(State, Health)>) -> DataBundle {
    let mut b = DataBundle::default();
    let mut list: Vec<_> = st
        .map(|(s, _)| s.checks.values().collect())
        .unwrap_or_default();
    list.sort_by_key(|c| (c.status != "warn", c.id.clone()));
    let mut order = Vec::new();
    for c in list {
        let uri = format!("agents://checks/{}", c.id);
        let warn = c.status == "warn";
        b.entities.insert(
            uri.clone(),
            record(json!({
                "id": c.id,
                "title": c.title,
                "status": if warn { "warn" } else { "ok" },
                "ok": !warn,
                "status_text": if warn { "Warning" } else { "OK" },
                "detail": c.detail,
                "color": if warn { ORANGE } else { GREEN },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(crate::CHECKS_QUERY.into(), order);
    b
}

/// The `schedules` feed: `agents:schedules`, by name. Times read in each
/// schedule's own zone, else the pod's `tz`.
pub fn schedules(st: Option<&(State, Health)>, now_ms: i64, tz: &TimeZone) -> DataBundle {
    let mut b = DataBundle::default();
    let list: Vec<_> = st
        .map(|(s, _)| s.schedules.values().collect())
        .unwrap_or_default();
    let mut order = Vec::new();
    for sc in list {
        let uri = format!("agents://schedules/{}", sc.name);
        let state_text = if !sc.enabled {
            "Off"
        } else if sc.paused {
            "Paused"
        } else {
            "Active"
        };
        let next = match sc.next_run {
            _ if !sc.enabled => "Off".to_owned(),
            _ if sc.paused => "Paused".to_owned(),
            Some(t) => format!(
                "{} · {}",
                text::until(t, now_ms),
                local(t, &schedule_zone(&sc.timezone, tz))
            ),
            None => "Not scheduled".to_owned(),
        };
        let last = sc.last_run.clone().unwrap_or_default();
        let (last_text, last_color) = status_look(&last.status);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "name": sc.name,
                "cron": sc.cron,
                "timezone": sc.timezone,
                "workspace": sc.workspace,
                "model": sc.model,
                "max_runtime_minutes": sc.max_runtime_minutes,
                "enabled": sc.enabled,
                "paused": sc.paused,
                "can_pause": sc.enabled && !sc.paused,
                "state_text": state_text,
                "next_run_text": next,
                "ran": sc.last_run.is_some(),
                "last_status": last.status,
                "last_status_text": last_text,
                "last_status_color": last_color,
                "last_summary": last.summary,
                "last_error": last.error,
                "last_cost_text": text::cost(last.cost_usd),
                "last_session_id": last.session_id,
                "last_run_text": last.due_at.map(|t| text::ago(t, now_ms)).unwrap_or_default(),
            })),
        );
        order.push(uri);
    }
    b.queries.insert(crate::SCHEDULES_QUERY.into(), order);
    b
}

/// One timeline line for a stored event.
pub fn event_text(e: &Value) -> String {
    let d = &e["data"];
    let t = |k: &str, max| text::clean(d[k].as_str().unwrap_or(""), max);
    let status = e["status"].as_str().unwrap_or("");
    match e["type"].as_str().unwrap_or("") {
        "task.launched" => "Launched".to_owned(),
        "session.started" => "Started".to_owned(),
        "session.status" => {
            let (word, _) = status_look(status);
            if word.is_empty() {
                "Status changed".to_owned()
            } else {
                word.to_owned()
            }
        }
        "tool.used" => match state::tool_text(&t("tool", 80), &t("detail", 200)) {
            s if s.is_empty() => "Used a tool".to_owned(),
            s => format!("Used {s}"),
        },
        "permission.requested" => match state::tool_text(&t("tool", 80), &t("detail", 200)) {
            s if s.is_empty() => "Asked for permission".to_owned(),
            s => format!("Asked to use {s}"),
        },
        "session.finished" => match t("summary", 300) {
            s if s.is_empty() => "Finished".to_owned(),
            s => format!("Finished: {s}"),
        },
        "session.failed" => match t("error", 300) {
            s if s.is_empty() => "Failed".to_owned(),
            s => format!("Failed: {s}"),
        },
        other => text::clean(other, 60),
    }
}

/// The timeline data path: session `id` and `agents:timeline:<id>`.
pub fn timeline(
    st: Option<&(State, Health)>,
    id: &str,
    events: &[Value],
    now_ms: i64,
) -> DataBundle {
    let mut b = DataBundle::default();
    if let Some(s) = st.and_then(|(s, _)| s.sessions.get(id)) {
        b.entities
            .insert(format!("agents://sessions/{id}"), session(s, now_ms));
    }
    let mut order = Vec::new();
    for e in events {
        let Some(seq) = e["seq"].as_i64() else {
            continue;
        };
        let uri = format!("agents://events/{seq}");
        let ts = e["ts"].as_i64().unwrap_or(0);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "seq": seq,
                "type": e["type"],
                "text": event_text(e),
                "time_text": if ts > 0 { text::ago(ts, now_ms) } else { String::new() },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(format!("agents:timeline:{id}"), order);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Schedule;

    fn st() -> (State, Health) {
        let mut s = State::default();
        for (id, status, at) in [
            ("a", state::FINISHED, 5),
            ("b", state::WORKING, 1),
            ("c", state::WAITING_PERMISSION, 2),
        ] {
            s.sessions.insert(
                id.into(),
                Session {
                    id: id.into(),
                    status: status.into(),
                    started_at: at * 1000,
                    updated_at: at * 1000,
                    ..Default::default()
                },
            );
        }
        (
            s,
            Health {
                connected: true,
                last_error: None,
                last_ok_at: Some(60_000),
            },
        )
    }

    #[test]
    fn active_sessions_come_first() {
        let s = st();
        let b = sessions(Some(&s), "runner.example", 120_000);
        assert_eq!(
            b.queries[crate::SESSIONS_QUERY],
            [
                "agents://sessions/c",
                "agents://sessions/b",
                "agents://sessions/a"
            ]
        );
        let status = &b.entities[STATUS_URI];
        assert_eq!(
            status.get_str("summary_text"),
            Some("1 working · 1 needs you")
        );
        assert_eq!(
            status.get_str("status_text"),
            Some("Connected to runner.example")
        );
        let c = &b.entities["agents://sessions/c"];
        assert_eq!(c.get_str("status_text"), Some("Needs you"));
        assert_eq!(c.get_str("elapsed_text"), Some("1 min"));
    }

    #[test]
    fn an_unreachable_runner_says_so() {
        let (s, mut h) = st();
        h.connected = false;
        h.last_error = Some("the runner can't be reached (no connection)".into());
        let e = status(Some(&(s, h)), "runner.example", 600_000);
        assert_eq!(e.get("stale"), Some(&json!(true)));
        assert!(
            e.get_str("status_text")
                .unwrap()
                .starts_with("Can't reach runner.example"),
            "{e:?}"
        );
        let signed_out = status(None, "", 0);
        assert_eq!(signed_out.get("signed_in"), Some(&json!(false)));
    }

    #[test]
    fn schedules_read_well() {
        let mut s = State::default();
        s.schedules.insert(
            "nightly".into(),
            Schedule {
                name: "nightly".into(),
                enabled: true,
                next_run: Some(1_800_000_000_000 + 3 * 3_600_000),
                ..Default::default()
            },
        );
        let h = Health::default();
        let b = schedules(Some(&(s, h)), 1_800_000_000_000, &TimeZone::UTC);
        let e = &b.entities["agents://schedules/nightly"];
        assert_eq!(e.get_str("next_run_text"), Some("in 3 h · Fri 11:00"));
        assert_eq!(e.get_str("state_text"), Some("Active"));
        assert_eq!(e.get("can_pause"), Some(&json!(true)));
    }

    #[test]
    fn idle_sessions_are_not_working() {
        let (mut s, h) = st();
        // A sandbox's idle standby session and a finished run's open one.
        for (id, at) in [("idle-standby", 9), ("idle-run", 3)] {
            s.sessions.insert(
                id.into(),
                Session {
                    id: id.into(),
                    status: state::IDLE.into(),
                    started_at: at * 1000,
                    updated_at: at * 1000,
                    ..Default::default()
                },
            );
        }
        let b = sessions(Some(&(s.clone(), h.clone())), "runner.example", 120_000);
        let st_e = &b.entities[STATUS_URI];
        assert_eq!(st_e.get("working_count"), Some(&json!(1)));
        assert_eq!(st_e.get("needs_you_count"), Some(&json!(1)));
        assert_eq!(
            st_e.get_str("summary_text"),
            Some("1 working · 1 needs you")
        );
        assert_eq!(
            b.queries[crate::SESSIONS_QUERY],
            [
                "agents://sessions/c",
                "agents://sessions/b",
                "agents://sessions/idle-standby",
                "agents://sessions/idle-run",
                "agents://sessions/a",
            ]
        );

        // Only idle and ended sessions: nothing is running.
        s.sessions
            .retain(|_, x| !matches!(x.id.as_str(), "b" | "c"));
        let e = status(Some(&(s, h)), "runner.example", 120_000);
        assert_eq!(e.get("working_count"), Some(&json!(0)));
        assert_eq!(e.get("needs_you_count"), Some(&json!(0)));
        assert_eq!(e.get_str("summary_text"), Some("Nothing running"));
        assert_eq!(e.get_str("color"), Some(GREEN));
    }

    #[test]
    fn schedule_times_read_in_the_schedules_zone() {
        // 2026-11-08 13:12 UTC, a Sunday: 06:12 in Denver.
        let next: Timestamp = "2026-11-08T13:12:00Z".parse().unwrap();
        let next = next.as_millisecond();
        let now = next - (14 * 3600 + 3 * 60) * 1000;
        let mut s = State::default();
        for (name, zone) in [
            ("denver", "America/Denver"),
            ("unset", ""),
            ("bad", "Nowhere/Land"),
        ] {
            s.schedules.insert(
                name.into(),
                Schedule {
                    name: name.into(),
                    cron: "12 6 * * *".into(),
                    timezone: zone.into(),
                    enabled: true,
                    next_run: Some(next),
                    ..Default::default()
                },
            );
        }
        let berlin = pod_core::tz::parse("Europe/Berlin").unwrap();
        let b = schedules(Some(&(s, Health::default())), now, &berlin);
        let text = |n: &str| {
            b.entities[&format!("agents://schedules/{n}")]
                .get_str("next_run_text")
                .map(str::to_owned)
        };
        assert_eq!(text("denver").as_deref(), Some("in 14 h 3 min · Sun 06:12"));
        // No zone of its own (or an unknown one): the pod's.
        assert_eq!(text("unset").as_deref(), Some("in 14 h 3 min · Sun 14:12"));
        assert_eq!(text("bad").as_deref(), Some("in 14 h 3 min · Sun 14:12"));
    }
}
