//! The feeds' data: the [`State`] as entities with display text worked out
//! (status words and colours, "12 min ago", "$0.42"), so layouts bind
//! fields and need no logic.

use pod_core::tz::jiff::{Timestamp, tz::TimeZone};
use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

use crate::db::Health;
use crate::state::{self, Run, State};
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

pub const PURPLE: &str = "#7B4FC9";

/// A run's word and colour, from its state, end reason and outcome.
pub fn run_look(state: &str, end_reason: &str, outcome: &str) -> (&'static str, &'static str) {
    match state {
        state::QUEUED => ("Queued", GRAY),
        state::STARTING => ("Starting", BLUE),
        state::WORKING => ("Working", BLUE),
        state::NEEDS_YOU => ("Needs you", ORANGE),
        state::ENDED => match (outcome, end_reason) {
            ("done", _) => ("Done", GREEN),
            ("review", _) => (NEEDS_REVIEW, PURPLE),
            ("blocked", _) => ("Blocked", RED),
            ("failed", _) => ("Failed", RED),
            (_, "stopped") => ("Stopped", GRAY),
            (_, "lost") => ("Lost", RED),
            (_, "start_failed") => ("Didn't start", RED),
            (_, "timed_out") => ("Timed out", RED),
            (_, "expired") => ("Expired", GRAY),
            // Ended by itself without a report: a person should look.
            _ => (NEEDS_REVIEW, PURPLE),
        },
        "" => ("", GRAY),
        _ => ("Unknown", GRAY),
    }
}

/// Whether a run in this look waits for a person to review it: the ones
/// "Mark reviewed" applies to.
pub fn needs_review(state: &str, end_reason: &str, outcome: &str) -> bool {
    run_look(state, end_reason, outcome).0 == NEEDS_REVIEW
}

pub const NEEDS_REVIEW: &str = "Needs review";

/// A run's word and colour as this pod shows it: [`run_look`], with a run
/// that needs review and was marked reviewed here reading Reviewed.
pub fn shown_look(
    state: &str,
    end_reason: &str,
    outcome: &str,
    reviewed: bool,
) -> (&'static str, &'static str) {
    if reviewed && needs_review(state, end_reason, outcome) {
        ("Reviewed", GREEN)
    } else {
        run_look(state, end_reason, outcome)
    }
}

/// What a waiting run waits for, e.g. `Wants permission`.
pub fn needs_you_label(reason: &str) -> &'static str {
    match reason {
        "permission" => "Wants permission",
        "question" => "Has a question",
        _ => "Waiting for your reply",
    }
}

/// Where `r` sorts in `agents:sessions`: waiting for you, then starting or
/// working, then queued, then ended.
fn rank(r: &Run) -> u8 {
    match r.state.as_str() {
        state::NEEDS_YOU => 0,
        state::STARTING | state::WORKING => 1,
        state::QUEUED => 2,
        state::ENDED => 4,
        _ => 3,
    }
}

fn count(st: &State, states: &[&str]) -> usize {
    st.runs
        .values()
        .filter(|r| states.contains(&r.state.as_str()))
        .count()
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
            "summary_text": "", "working_count": 0, "needs_you_count": 0, "queued_count": 0,
            "color": GRAY, "updated_text": "",
        }));
    };
    let working = count(st, &[state::STARTING, state::WORKING]);
    let needs = count(st, &[state::NEEDS_YOU]);
    let queued = count(st, &[state::QUEUED]);
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if needs > 0 {
        parts.push(format!("{needs} needs you"));
    }
    if queued > 0 {
        parts.push(format!("{queued} queued"));
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
        "queued_count": queued,
        "color": if !h.connected { GRAY } else if needs > 0 { ORANGE } else if working > 0 { BLUE } else { GREEN },
        "updated_text": updated,
    }))
}

/// One run's entity (`agents://sessions/<run id>`). `reviewed`: marked
/// reviewed on this pod.
pub fn session(r: &Run, reviewed: bool, now_ms: i64) -> EntityRecord {
    let waits = needs_review(&r.state, &r.end_reason, &r.outcome);
    let reviewed = reviewed && waits;
    let (status_text, color) = shown_look(&r.state, &r.end_reason, &r.outcome, reviewed);
    let active = r.is_active();
    let elapsed = match (r.started_at, r.ended_at) {
        (s, _) if s <= 0 => String::new(),
        (s, e) if e > 0 => text::duration((e - s) / 1000),
        (s, _) => text::duration((now_ms - s) / 1000),
    };
    let tokens = r.input_tokens + r.output_tokens;
    let needs_you = r.state == state::NEEDS_YOU;
    record(json!({
        "id": r.id,
        "title": if r.title.is_empty() { "Untitled run" } else { &r.title },
        "state": r.state,
        "status": r.state,
        "status_text": status_text,
        "status_color": color,
        "active": active,
        "needs_you": needs_you,
        "needs_review": waits && !reviewed,
        "reviewed": reviewed,
        "needs_you_reason": if needs_you { r.needs_you_reason.as_str() } else { "" },
        "needs_you_label": if needs_you { needs_you_label(&r.needs_you_reason) } else { "" },
        "needs_you_text": if needs_you { r.needs_you_text.as_str() } else { "" },
        "end_reason": r.end_reason,
        "outcome": r.outcome,
        "outcome_summary": r.outcome_summary,
        "summary": r.summary,
        "error": r.error,
        "pr_url": r.pr(),
        "asleep": r.asleep && active,
        "current_tool": if active { r.current_tool.as_str() } else { "" },
        "cost_text": text::cost(r.cost_usd),
        "tokens_text": if tokens > 0 { format!("{tokens} tokens") } else { String::new() },
        "elapsed_text": elapsed,
        "updated_text": if r.last_at() > 0 { text::ago(r.last_at(), now_ms) } else { String::new() },
        "task_id": r.task_id,
        "schedule": state::schedule_of_task(&r.task_id).unwrap_or(""),
        "sandbox": r.sprite,
        "workspace": r.workspace,
        "origin": r.origin,
        "session_id": r.session_id,
        "resumed_from": r.resumed_from,
        // The runner's link, else one built from a claude.ai session id.
        "claude_url": if r.claude_url.is_empty() {
            text::claude_url(&r.session_id).unwrap_or_default()
        } else {
            r.claude_url.clone()
        },
    }))
}

/// The `sessions` feed: `agents:status` and `agents:sessions` (runs that
/// need you, then starting or working, then queued, then ended ones, each
/// newest first).
pub fn sessions(st: Option<&(State, Health)>, host: &str, now_ms: i64) -> DataBundle {
    let mut b = DataBundle::default();
    b.entities
        .insert(STATUS_URI.into(), status(st, host, now_ms));
    b.queries
        .insert(crate::STATUS_QUERY.into(), vec![STATUS_URI.into()]);
    let mut list: Vec<&Run> = st
        .map(|(s, _)| s.runs.values().collect())
        .unwrap_or_default();
    list.sort_by_key(|r| (rank(r), -r.last_at(), r.id.clone()));
    let mut order = Vec::new();
    for r in list {
        let uri = format!("agents://sessions/{}", r.id);
        let reviewed = st.is_some_and(|(s, _)| s.reviewed.contains(&r.id));
        b.entities.insert(uri.clone(), session(r, reviewed, now_ms));
        order.push(uri);
    }
    b.queries.insert(crate::SESSIONS_QUERY.into(), order);
    b
}

/// A sandbox state's word and colour.
pub fn sprite_look(state: &str) -> (&'static str, &'static str) {
    match state {
        "awake" => ("Awake", GREEN),
        "asleep" => ("Asleep", GRAY),
        "waking" => ("Waking", BLUE),
        "updating" => ("Updating", BLUE),
        "unreachable" => ("Unreachable", ORANGE),
        "unprovisioned" => ("Not set up", GRAY),
        _ => ("Unknown", GRAY),
    }
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
        let (state_text, color) = sprite_look(&x.state);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "name": x.name,
                "state": x.state,
                "awake": x.state == "awake",
                "state_text": state_text,
                "color": color,
                "last_seen_text": if x.last_heartbeat_at > 0 { text::ago(x.last_heartbeat_at, now_ms) } else { String::new() },
                "agentd_version": x.agentd_version,
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
        let run = st.and_then(|(s, _)| s.runs.get(&last.run_id));
        let reviewed = st.is_some_and(|(s, _)| s.reviewed.contains(&last.run_id));
        let (last_text, last_color) =
            shown_look(&last.state, &last.end_reason, &last.outcome, reviewed);
        let last_summary = if !last.outcome_summary.is_empty() {
            last.outcome_summary.clone()
        } else {
            run.map(|r| r.summary.clone()).unwrap_or_default()
        };
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
                "last_status": if last.state == state::ENDED && !last.outcome.is_empty() { &last.outcome } else if last.state == state::ENDED { &last.end_reason } else { &last.state },
                "last_status_text": last_text,
                "last_status_color": last_color,
                "last_summary": last_summary,
                "last_error": run.map(|r| r.error.as_str()).unwrap_or(""),
                "last_cost_text": run.map(|r| text::cost(r.cost_usd)).unwrap_or_default(),
                "last_session_id": last.run_id,
                "last_run_id": last.run_id,
                "last_run_text": last.due_at.map(|t| text::ago(t, now_ms)).unwrap_or_default(),
            })),
        );
        order.push(uri);
    }
    b.queries.insert(crate::SCHEDULES_QUERY.into(), order);
    b
}

/// The run-log data path: run `id` and `agents:timeline:<id>`, its log
/// (`GET /v2/runs/<id>/log`'s `{entries: [{ts, kind, text}]}`), oldest
/// first. `log` is `None` when the runner couldn't be asked.
pub fn timeline(
    st: Option<&(State, Health)>,
    id: &str,
    log: Option<&Value>,
    now_ms: i64,
) -> DataBundle {
    let mut b = DataBundle::default();
    if let Some((s, _)) = st
        && let Some(r) = s.runs.get(id)
    {
        b.entities.insert(
            format!("agents://sessions/{id}"),
            session(r, s.reviewed.contains(id), now_ms),
        );
    }
    let entries: Vec<&Value> = log
        .and_then(|l| l.get("entries"))
        .and_then(Value::as_array)
        .map(|a| a.iter().take(LOG_MAX).collect())
        .unwrap_or_default();
    let mut order = Vec::new();
    for (n, e) in entries.into_iter().enumerate() {
        let uri = format!("agents://events/{id}-{n}");
        let ts = e.get("ts").and_then(Value::as_i64).unwrap_or(0);
        let kind = text::clean(e.get("kind").and_then(Value::as_str).unwrap_or(""), 40);
        let line = text::clean(e.get("text").and_then(Value::as_str).unwrap_or(""), 500);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "seq": n,
                "type": kind,
                "text": if line.is_empty() { kind.clone() } else { line },
                "time_text": if ts > 0 { text::ago(ts, now_ms) } else { String::new() },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(format!("agents:timeline:{id}"), order);
    b
}

/// The most log lines shown.
pub const LOG_MAX: usize = 500;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Schedule;

    fn st() -> (State, Health) {
        let mut s = State::default();
        let runs = [
            ("ended_old", "ended", 5, ("reported", "done")),
            ("ended_new", "ended", 8, ("lost", "")),
            ("work", "working", 1, ("", "")),
            ("start", "starting", 3, ("", "")),
            ("wait", "needs_you", 2, ("", "")),
            ("queue", "queued", 9, ("", "")),
        ];
        for (id, state, at, (end, outcome)) in runs {
            s.runs.insert(
                id.into(),
                Run {
                    id: id.into(),
                    state: state.into(),
                    end_reason: end.into(),
                    outcome: outcome.into(),
                    needs_you_reason: if state == "needs_you" {
                        "question".into()
                    } else {
                        String::new()
                    },
                    needs_you_text: if state == "needs_you" {
                        "Merge now?".into()
                    } else {
                        String::new()
                    },
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
    fn runs_sort_by_what_they_need() {
        let s = st();
        let b = sessions(Some(&s), "runner.example", 120_000);
        assert_eq!(
            b.queries[crate::SESSIONS_QUERY],
            [
                "agents://sessions/wait",
                "agents://sessions/start",
                "agents://sessions/work",
                "agents://sessions/queue",
                "agents://sessions/ended_new",
                "agents://sessions/ended_old",
            ]
        );
        let status = &b.entities[STATUS_URI];
        assert_eq!(
            status.get_str("summary_text"),
            Some("2 working · 1 needs you · 1 queued")
        );
        assert_eq!(status.get("queued_count"), Some(&json!(1)));
        let wait = &b.entities["agents://sessions/wait"];
        assert_eq!(wait.get_str("status_text"), Some("Needs you"));
        assert_eq!(wait.get_str("needs_you_label"), Some("Has a question"));
        assert_eq!(wait.get_str("needs_you_text"), Some("Merge now?"));
        assert_eq!(wait.get("active"), Some(&json!(true)));
        let lost = &b.entities["agents://sessions/ended_new"];
        assert_eq!(lost.get_str("status_text"), Some("Lost"));
        assert_eq!(lost.get("active"), Some(&json!(false)));
        assert_eq!(
            b.entities["agents://sessions/ended_old"].get_str("status_text"),
            Some("Done")
        );
    }

    #[test]
    fn every_run_state_has_words() {
        for (state, end, outcome, want) in [
            ("queued", "", "", "Queued"),
            ("starting", "", "", "Starting"),
            ("working", "", "", "Working"),
            ("needs_you", "", "", "Needs you"),
            ("ended", "reported", "done", "Done"),
            ("ended", "reported", "review", "Needs review"),
            ("ended", "exited", "", "Needs review"),
            ("ended", "reported", "blocked", "Blocked"),
            ("ended", "lost", "failed", "Failed"),
            ("ended", "stopped", "", "Stopped"),
            ("ended", "lost", "", "Lost"),
            ("ended", "start_failed", "", "Didn't start"),
            ("ended", "timed_out", "", "Timed out"),
            ("ended", "expired", "", "Expired"),
        ] {
            assert_eq!(
                run_look(state, end, outcome).0,
                want,
                "{state} {end} {outcome}"
            );
        }
    }

    #[test]
    fn a_run_marked_reviewed_reads_reviewed() {
        let mut s = st();
        for (id, outcome, end) in [
            ("ended_old", "review", "reported"),
            ("ended_new", "", "lost"),
        ] {
            let r = s.0.runs.get_mut(id).unwrap();
            r.outcome = outcome.into();
            r.end_reason = end.into();
        }
        let look = |s: &(State, Health), id: &str| {
            let b = sessions(Some(s), "r", 0);
            let e = b.entities[&format!("agents://sessions/{id}")].clone();
            (
                e.get_str("status_text").unwrap().to_owned(),
                e.get("needs_review").cloned(),
                e.get("reviewed").cloned(),
            )
        };
        assert_eq!(
            look(&s, "ended_old"),
            ("Needs review".into(), Some(json!(true)), Some(json!(false)))
        );
        s.0.reviewed
            .extend(["ended_old".into(), "ended_new".into(), "work".into()]);
        assert_eq!(
            look(&s, "ended_old"),
            ("Reviewed".into(), Some(json!(false)), Some(json!(true)))
        );
        // Only a run that needs review reads Reviewed.
        assert_eq!(
            look(&s, "ended_new"),
            ("Lost".into(), Some(json!(false)), Some(json!(false)))
        );
        assert_eq!(look(&s, "work").0, "Working");
        assert!(shown_look("ended", "exited", "", true).0 == "Reviewed");
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
        let idle = status(
            Some(&(
                State::default(),
                Health {
                    connected: true,
                    ..Default::default()
                },
            )),
            "r",
            0,
        );
        assert_eq!(idle.get_str("summary_text"), Some("Nothing running"));
        assert_eq!(idle.get_str("color"), Some(GREEN));
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
                last_run: Some(crate::state::LastRun {
                    run_id: "run_9".into(),
                    state: "ended".into(),
                    end_reason: "expired".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let h = Health::default();
        let b = schedules(Some(&(s, h)), 1_800_000_000_000, &TimeZone::UTC);
        let e = &b.entities["agents://schedules/nightly"];
        assert_eq!(e.get_str("next_run_text"), Some("in 3 h · Fri 11:00"));
        assert_eq!(e.get_str("state_text"), Some("Active"));
        assert_eq!(e.get("can_pause"), Some(&json!(true)));
        assert_eq!(e.get_str("last_status_text"), Some("Expired"));
        assert_eq!(e.get_str("last_run_id"), Some("run_9"));
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

    #[test]
    fn sandboxes_say_their_state() {
        let mut s = State::default();
        for (name, state) in [("a", "asleep"), ("b", "awake"), ("c", "unprovisioned")] {
            s.sprites.insert(
                name.into(),
                crate::state::Sprite {
                    name: name.into(),
                    state: state.into(),
                    ..Default::default()
                },
            );
        }
        let b = sandboxes(Some(&(s, Health::default())), 0);
        assert_eq!(b.queries[crate::SANDBOXES_QUERY][0], "agents://sandboxes/b");
        assert_eq!(
            b.entities["agents://sandboxes/c"].get_str("state_text"),
            Some("Not set up")
        );
    }

    #[test]
    fn a_run_log_reads_as_lines() {
        let log = json!({"entries": [{"ts": 1000, "kind": "state", "text": "Working"},
            {"ts": 2000, "kind": "tool", "text": "Bash: cargo test\u{7}"}]});
        let b = timeline(None, "run_1", Some(&log), 61_000);
        let order = &b.queries["agents:timeline:run_1"];
        assert_eq!(order.len(), 2);
        assert_eq!(
            b.entities[&order[1]].get_str("text"),
            Some("Bash: cargo test")
        );
        assert_eq!(
            b.entities[&order[0]].get_str("time_text"),
            Some("1 min ago")
        );
        assert!(timeline(None, "run_1", None, 0).queries["agents:timeline:run_1"].is_empty());
    }
}
