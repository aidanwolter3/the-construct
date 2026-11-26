//! The feeds' data: the [`State`] and the pod's schedules as entities with
//! display text worked out (status words and colours, "12 min ago",
//! "$0.42"), so layouts bind fields and need no logic.

use pod_core::tz::jiff::{Timestamp, tz::TimeZone};
use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

use crate::db::{self, Health, Occurrence, Schedule};
use crate::state::{self, Run, Sprite, State};
use crate::text;

pub const STATUS_URI: &str = "agents://runner/status";

pub const BLUE: &str = "#1E66D0";
pub const ORANGE: &str = "#B26A00";
pub const GREEN: &str = "#2E7D32";
pub const RED: &str = "#C62828";
pub const GRAY: &str = "#6B6B76";
pub const PURPLE: &str = "#7B4FC9";

pub const NEEDS_REVIEW: &str = "Needs review";

fn record(v: Value) -> EntityRecord {
    match v {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord::new(),
    }
}

/// What a pending outcome waits on, e.g. `Waiting on CI`.
pub fn stage_text(stage: &str) -> &'static str {
    match stage {
        "ci" => "Waiting on CI",
        "review" => "Waiting on review",
        "deploy" => "Waiting on deploy",
        _ => "Waiting",
    }
}

/// Why a failed outcome failed, as its word.
pub fn failure_text(reason: &str) -> &'static str {
    match reason {
        "ci" => "CI failed",
        "closed_unmerged" => "Closed unmerged",
        "deploy" => "Deploy failed",
        "blocked" => "Blocked",
        _ => "Failed",
    }
}

/// Why a queued run waits, in words.
pub fn queue_text(reason: &str) -> &'static str {
    match reason {
        "waiting_for_slot" => "Waiting for a free slot",
        "sprite_waking" => "Its sandbox is waking",
        "sprite_degraded" => "Its sandbox is degraded",
        "sprite_down" => "Its sandbox is down",
        "" => "",
        _ => "Waiting",
    }
}

/// A run's word and colour, from its execution and its outcome.
pub fn run_look(r: &Run) -> (&'static str, &'static str) {
    match r.state.as_str() {
        state::QUEUED => ("Queued", GRAY),
        state::LAUNCHING => ("Starting", BLUE),
        state::WORKING => ("Working", BLUE),
        state::NEEDS_YOU => ("Needs you", ORANGE),
        state::PARKED => ("Parked", ORANGE),
        state::STALLED => ("Stalled", ORANGE),
        state::INTERRUPTED => ("Restarting", ORANGE),
        state::STOPPING => ("Stopping", GRAY),
        state::CLOSING => ("Finishing", BLUE),
        // Timed out reads as such even when its outcome needs review: the
        // limit, not the review, is what happened.
        state::ENDED if r.timed_out() => ("Timed out", RED),
        state::ENDED => match r.outcome_state() {
            state::OUTCOME_DONE if r.outcome.reviewed => ("Reviewed", GREEN),
            state::OUTCOME_DONE => ("Done", GREEN),
            state::OUTCOME_FAILED => (failure_text(&r.outcome.reason), RED),
            state::OUTCOME_NEEDS_REVIEW => (NEEDS_REVIEW, PURPLE),
            state::OUTCOME_PENDING if r.outcome.stage == "review" => {
                (stage_text(&r.outcome.stage), PURPLE)
            }
            state::OUTCOME_PENDING => (stage_text(&r.outcome.stage), BLUE),
            _ => match r.end_reason.as_str() {
                "stopped" => ("Stopped", GRAY),
                "lost" => ("Lost", RED),
                "start_failed" => ("Didn't start", RED),
                "timed_out" => ("Timed out", RED),
                "expired" => ("Expired", GRAY),
                _ => ("Ended", GRAY),
            },
        },
        "" => ("", GRAY),
        _ => ("Unknown", GRAY),
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

/// Counts as running now.
fn running(r: &Run) -> bool {
    matches!(
        r.state.as_str(),
        state::LAUNCHING
            | state::WORKING
            | state::STALLED
            | state::INTERRUPTED
            | state::STOPPING
            | state::CLOSING
    )
}

/// Where `r` sorts in `agents:sessions`: waiting for you, then running,
/// then queued, then ended ones waiting on their outcome, then the rest.
fn rank(r: &Run) -> u8 {
    if r.waits_for_you() {
        0
    } else if running(r) {
        1
    } else if r.state == state::QUEUED {
        2
    } else if r.outcome_state() == state::OUTCOME_PENDING || r.needs_review() {
        3
    } else if r.is_active() {
        2
    } else {
        4
    }
}

/// The zone the pod's times read in for a schedule.
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
            "waiting_count": 0, "color": GRAY, "updated_text": "",
        }));
    };
    let count = |f: &dyn Fn(&Run) -> bool| st.runs.values().filter(|r| f(r)).count();
    let working = count(&running);
    let needs = count(&Run::waits_for_you);
    let queued = count(&|r| r.state == state::QUEUED);
    let waiting = count(&|r| !r.is_active() && r.outcome_state() == state::OUTCOME_PENDING);
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
    if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
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
        "waiting_count": waiting,
        "color": if !h.connected { GRAY } else if needs > 0 { ORANGE } else if working > 0 { BLUE } else { GREEN },
        "updated_text": updated,
    }))
}

/// One run's entity (`agents://sessions/<run id>`). `schedule`: the name of
/// the pod's schedule it runs for, if any; `continued_by`: the run that
/// continues it (Reopen or Continue), if any.
pub fn session(r: &Run, schedule: &str, continued_by: &str, now_ms: i64) -> EntityRecord {
    let (status_text, color) = run_look(r);
    let active = r.is_active();
    let elapsed = match (r.started_at, r.ended_at) {
        (s, _) if s <= 0 => String::new(),
        (s, e) if e > 0 => text::duration((e - s) / 1000),
        (s, _) => text::duration((now_ms - s) / 1000),
    };
    let tokens = r.input_tokens + r.output_tokens;
    let waits = r.waits_for_you();
    let claim = r.claim.clone().unwrap_or_default();
    let pending = !active && r.outcome_state() == state::OUTCOME_PENDING;
    let attempt = if r.attempt > 1 && r.max_attempts > 0 {
        format!("Attempt {} of {}", r.attempt, r.max_attempts)
    } else {
        String::new()
    };
    let explain = match r.state.as_str() {
        state::PARKED => {
            "Parked: it waited 30 min, so its sandbox may sleep. Answer it in the Claude app and it picks up again."
        }
        state::STALLED => {
            "Stalled: its sandbox is suspended or degraded. It carries on when the sandbox is back."
        }
        state::INTERRUPTED => "Its process stopped; it is being started again.",
        _ => "",
    };
    let mut v = json!({
        "id": r.id,
        "title": if r.title.is_empty() { "Untitled run" } else { &r.title },
        "state": r.state,
        "status": r.state,
        "status_text": status_text,
        "status_color": color,
        "state_detail": if active { r.state_detail.as_str() } else { "" },
        "explain_text": explain,
        "active": active,
        "needs_you": waits,
        "parked": r.state == state::PARKED,
        "stalled": r.state == state::STALLED,
        // Older layouts show an Asleep pill: a parked run's sandbox may sleep.
        "asleep": r.state == state::PARKED,
        "queue_reason": if r.state == state::QUEUED { r.queue_reason.as_str() } else { "" },
        "queue_text": if r.state == state::QUEUED { queue_text(&r.queue_reason) } else { "" },
        "needs_review": r.needs_review(),
        "reviewed": r.outcome.reviewed,
        "waiting": pending,
        "stage": if pending { r.outcome.stage.as_str() } else { "" },
        "stage_text": if pending { stage_text(&r.outcome.stage) } else { "" },
        "needs_you_reason": if waits { r.needs_you_reason.as_str() } else { "" },
        "needs_you_label": if waits { needs_you_label(&r.needs_you_reason) } else { "" },
        "needs_you_text": if waits { r.needs_you_text.as_str() } else { "" },
    });
    let more = json!({
        "end_reason": r.end_reason,
        "outcome": r.outcome_state(),
        "outcome_reason": r.outcome.reason,
        "outcome_text": if !active { status_text } else { "" },
        "outcome_summary": claim.summary,
        "claim": claim.status,
        "claim_mismatch": r.outcome.claim_mismatch,
        "claim_mismatch_text": if r.outcome.claim_mismatch {
            "What the agent reported disagrees with GitHub; GitHub's state is shown."
        } else { "" },
        "follow_up": claim.follow_up,
        "summary": r.summary,
        "error": r.error,
        "pr_url": r.pr(),
        "deploy_url": r.outcome.deploy_url,
        "attempt_text": attempt,
        "current_tool": if active { r.current_tool.as_str() } else { "" },
        "cost_text": text::cost(r.cost_usd),
        "tokens_text": if tokens > 0 { format!("{tokens} tokens") } else { String::new() },
        "elapsed_text": elapsed,
        "updated_text": if r.last_at() > 0 { text::ago(r.last_at(), now_ms) } else { String::new() },
        "work_item_id": r.work_item_id,
        "task_id": r.work_item_id,
        "occurrence": r.occurrence,
        "schedule": schedule,
        "sandbox": r.sprite,
        "workspace": r.workspace,
        "model": r.model,
        "branch": r.branch,
        "origin": r.origin,
        "session_id": r.session_id,
        "resumed_from": r.resumed_from,
        "continued_by": continued_by,
        // Continue: a run that timed out, never continued, whose Claude
        // session the runner can resume.
        "can_continue": r.timed_out() && continued_by.is_empty() && !r.session_id.is_empty(),
        // The runner's link, else one built from a claude.ai session id.
        "claude_url": if r.claude_url.is_empty() {
            text::claude_url(&r.session_id).unwrap_or_default()
        } else {
            r.claude_url.clone()
        },
    });
    if let (Value::Object(a), Value::Object(b)) = (&mut v, more) {
        a.extend(b);
    }
    record(v)
}

/// The id of the run that continues `r`, or empty.
fn next_of<'a>(st: Option<&'a (State, Health)>, r: &Run) -> &'a str {
    st.and_then(|(s, _)| s.continuation_of(&r.id))
        .map_or("", |n| n.id.as_str())
}

/// The schedule name of run `r`, from the pod's schedules.
fn schedule_name<'a>(r: &Run, schedules: &'a [Schedule]) -> &'a str {
    if r.origin != crate::cap::ORIGIN_SCHEDULE {
        return "";
    }
    schedules
        .iter()
        .find(|s| s.id == r.work_item_id)
        .map_or("", |s| s.name.as_str())
}

/// The `sessions` feed: `agents:status`, `agents:sessions` (runs that
/// need you, then running, then queued, then ended ones waiting on their
/// outcome, then the rest, each newest first) and, for each schedule,
/// `agents:schedule_runs:<name>` (its runs, the latest started first).
pub fn sessions(
    st: Option<&(State, Health)>,
    schedules: &[Schedule],
    host: &str,
    now_ms: i64,
) -> DataBundle {
    let mut b = DataBundle::default();
    let mut st_rec = status(st, host, now_ms);
    // The kill switch shows while any schedule would run (decision
    // `mcp-call-log`).
    st_rec.0.insert(
        "can_pause_all".into(),
        json!(st.is_some() && schedules.iter().any(|s| s.enabled)),
    );
    b.entities.insert(STATUS_URI.into(), st_rec);
    b.queries
        .insert(crate::STATUS_QUERY.into(), vec![STATUS_URI.into()]);
    let mut list: Vec<&Run> = st
        .map(|(s, _)| s.runs.values().collect())
        .unwrap_or_default();
    list.sort_by_key(|r| (rank(r), -r.last_at(), r.id.clone()));
    let mut order = Vec::new();
    for r in &list {
        let uri = format!("agents://sessions/{}", r.id);
        b.entities.insert(
            uri.clone(),
            session(r, schedule_name(r, schedules), next_of(st, r), now_ms),
        );
        order.push(uri);
    }
    b.queries.insert(crate::SESSIONS_QUERY.into(), order);
    // Each schedule's runs, the latest started first (a schedule with no
    // runs gets an empty list, so a screen can bind it either way).
    let mut by_start = list;
    by_start.sort_by_key(|r| (-r.queued_at.max(r.started_at), r.id.clone()));
    for sc in schedules {
        let runs = by_start
            .iter()
            .filter(|r| r.origin == crate::cap::ORIGIN_SCHEDULE && r.work_item_id == sc.id)
            .map(|r| format!("agents://sessions/{}", r.id))
            .collect();
        b.queries.insert(schedule_runs_query(&sc.name), runs);
    }
    b
}

/// The query of schedule `name`'s runs, latest first: `agents:schedule_runs:<name>`.
pub fn schedule_runs_query(name: &str) -> String {
    format!("agents:schedule_runs:{name}")
}

/// A sandbox state's word and colour.
pub fn sprite_look(state: &str) -> (&'static str, &'static str) {
    match state {
        "awake" => ("Awake", GREEN),
        "asleep" => ("Asleep", GRAY),
        "waking" => ("Waking", BLUE),
        "updating" => ("Updating", BLUE),
        "provisioning" => ("Setting up", BLUE),
        "degraded" => ("Degraded", ORANGE),
        "suspended" => ("Suspended", ORANGE),
        "down" => ("Down", RED),
        "unprovisioned" => ("Not set up", GRAY),
        _ => ("Unknown", GRAY),
    }
}

fn sprite_entity(x: &Sprite, now_ms: i64) -> Value {
    let (state_text, color) = sprite_look(&x.state);
    let troubled = matches!(x.state.as_str(), "degraded" | "suspended" | "down");
    // A launch refused for memory alone leaves an awake sandbox awake (its
    // run waits for a slot): that isn't trouble to show.
    let failures: Vec<&str> = x
        .preflight_failures
        .iter()
        .map(String::as_str)
        .filter(|f| troubled || *f != "memory")
        .collect();
    let preflight = if failures.is_empty() {
        String::new()
    } else {
        format!("Preflight failed: {}", failures.join(", "))
    };
    json!({
        "name": x.name,
        "state": x.state,
        "awake": x.state == "awake",
        "state_text": state_text,
        "color": color,
        "reason": if troubled { x.reason.as_str() } else { "" },
        "troubled": troubled,
        "down": x.state == "down",
        "can_wake": matches!(x.state.as_str(), "asleep" | "suspended"),
        "slots_text": if x.slots > 0 { format!("{} of {} slots in use", x.slots_used, x.slots) } else { String::new() },
        "preflight_text": preflight,
        "disk_warning": x.disk_failures_7d >= state::REPEAT_DISK_FAILURES,
        "disk_text": if x.disk_failures_7d >= state::REPEAT_DISK_FAILURES {
            format!("Its disk failed {} times in 7 days: it likely needs replacing.", x.disk_failures_7d)
        } else { String::new() },
        "last_seen_text": if x.last_heartbeat_at > 0 { text::ago(x.last_heartbeat_at, now_ms) } else { String::new() },
        "agentd_version": x.agentd_version,
        "workspace": x.workspace,
    })
}

/// The `sandboxes` feed: `agents:sandboxes`, troubled ones first, then
/// awake.
pub fn sandboxes(st: Option<&(State, Health)>, now_ms: i64) -> DataBundle {
    let mut b = DataBundle::default();
    let mut list: Vec<_> = st
        .map(|(s, _)| s.sprites.values().collect())
        .unwrap_or_default();
    let order_of = |s: &str| match s {
        "down" => 0,
        "degraded" | "suspended" => 1,
        "awake" => 2,
        _ => 3,
    };
    list.sort_by_key(|x| (order_of(&x.state), x.name.clone()));
    let mut order = Vec::new();
    for x in list {
        let uri = format!("agents://sandboxes/{}", x.name);
        b.entities
            .insert(uri.clone(), record(sprite_entity(x, now_ms)));
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

/// A pod schedule with what its runs did: the schedule, its newest
/// occurrence, and the newest run the runner lists for it.
pub struct ScheduleView<'a> {
    pub schedule: &'a Schedule,
    pub last: Option<&'a Occurrence>,
}

/// The `schedules` feed: `agents:schedules`, by name, from the pod's own
/// store. Times read in each schedule's zone, else the pod's `tz`.
pub fn schedules(
    st: Option<&(State, Health)>,
    list: &[ScheduleView<'_>],
    now_ms: i64,
    tz: &TimeZone,
) -> DataBundle {
    let mut b = DataBundle::default();
    let mut order = Vec::new();
    for v in list {
        let sc = v.schedule;
        let uri = format!("agents://schedules/{}", sc.name);
        let next = match crate::schedules::next_run(sc, now_ms) {
            _ if !sc.enabled => "Paused".to_owned(),
            Some((_, t)) => format!(
                "{} · {}",
                text::until(t, now_ms),
                local(t, &schedule_zone(&sc.timezone, tz))
            ),
            None => "Not scheduled".to_owned(),
        };
        let mirror = st.map(|(s, _)| s);
        // The run the newest occurrence answered, else the newest run of it.
        let run = v
            .last
            .filter(|o| !o.run_id.is_empty())
            .and_then(|o| mirror.and_then(|m| m.runs.get(&o.run_id)))
            .or_else(|| mirror.and_then(|m| m.latest_run(&sc.id)))
            // A run continued (Reopen, Continue) is carried on by the last
            // run of its chain.
            .map(|r| mirror.map_or(r, |m| m.latest_of(r)));
        let (last_status, last_text, last_color) = match (v.last, run) {
            (Some(o), _) if o.state == db::OCC_MISSED => ("missed".to_owned(), "Missed", ORANGE),
            (Some(o), _) if o.state == db::OCC_REFUSED => ("refused".to_owned(), "Refused", RED),
            (Some(o), None) if o.state == db::OCC_PENDING => {
                ("sending".to_owned(), "Sending", GRAY)
            }
            (_, Some(r)) => {
                let (t, c) = run_look(r);
                let s = if r.is_active() {
                    r.state.clone()
                } else {
                    r.outcome_state().to_owned()
                };
                (s, t, c)
            }
            _ => (String::new(), "", GRAY),
        };
        let ran = v.last.is_some() || run.is_some();
        let last_at = v
            .last
            .map(|o| o.due_at)
            .or_else(|| run.map(|r| r.queued_at))
            .unwrap_or(0);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "id": sc.id,
                "name": sc.name,
                "cron": sc.cron,
                "timezone": sc.timezone,
                "workspace": sc.workspace,
                "model": sc.model,
                "model_text": if sc.model.is_empty() { "Default model" } else { sc.model.as_str() },
                "max_runtime_min": sc.max_runtime_min,
                "max_runtime_minutes": sc.max_runtime_min,
                "prompt": sc.prompt,
                "enabled": sc.enabled,
                "paused": !sc.enabled,
                "can_pause": sc.enabled,
                "state_text": if sc.enabled { "Active" } else { "Paused" },
                "next_run_text": next,
                "ran": ran,
                "last_status": last_status,
                "last_status_text": last_text,
                "last_status_color": last_color,
                "last_summary": run.map(|r| r.said().to_owned()).unwrap_or_default(),
                "last_error": run.map(|r| r.error.as_str()).unwrap_or(""),
                "last_cost_text": run.map(|r| text::cost(r.cost_usd)).unwrap_or_default(),
                "last_run_id": run.map(|r| r.id.as_str()).unwrap_or(""),
                "last_session_id": run.map(|r| r.id.as_str()).unwrap_or(""),
                "last_run_text": if last_at > 0 { text::ago(last_at, now_ms) } else { String::new() },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(crate::SCHEDULES_QUERY.into(), order);
    b
}

/// The run-log data path: run `id` and `agents:timeline:<id>`, its log
/// (`GET /v3/runs/<id>/log`'s `{entries: [{ts, kind, text}]}`), oldest
/// first. `log` is `None` when the runner couldn't be asked.
pub fn timeline(
    st: Option<&(State, Health)>,
    schedules: &[Schedule],
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
            session(r, schedule_name(r, schedules), next_of(st, r), now_ms),
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

/// What the pod keeps of a run's log (up to 4 MiB from the runner): the
/// first [`LOG_MAX`] entries, each only the time, kind and text it shows,
/// so a log is kept and cached at a size the timeline needs.
pub fn kept_log(log: &Value) -> Value {
    let entries: Vec<Value> = log
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(LOG_MAX)
        .map(|e| {
            json!({
                "ts": e.get("ts").and_then(Value::as_i64).unwrap_or(0),
                "kind": text::clean(e.get("kind").and_then(Value::as_str).unwrap_or(""), 40),
                "text": text::clean(e.get("text").and_then(Value::as_str).unwrap_or(""), 500),
            })
        })
        .collect();
    json!({ "entries": entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Outcome;

    fn r(id: &str, state: &str, at: i64, end: &str, outcome: &str, stage: &str) -> Run {
        Run {
            id: id.into(),
            state: state.into(),
            end_reason: end.into(),
            outcome: Outcome {
                state: outcome.into(),
                stage: stage.into(),
                ..Default::default()
            },
            started_at: at * 1000,
            updated_at: at * 1000,
            ..Default::default()
        }
    }

    fn st() -> (State, Health) {
        let mut s = State::default();
        for run in [
            r("ended_old", "ended", 5, "reported", "done", ""),
            r("ended_new", "ended", 8, "lost", "", ""),
            r("ci", "ended", 7, "reported", "pending", "ci"),
            r("work", "working", 1, "", "", ""),
            r("start", "launching", 3, "", "", ""),
            r("wait", "needs_you", 2, "", "", ""),
            r("park", "parked", 1, "", "", ""),
            r("queue", "queued", 9, "", "", ""),
        ] {
            s.runs.insert(run.id.clone(), run);
        }
        let q = s.runs.get_mut("queue").unwrap();
        q.queue_reason = "sprite_waking".into();
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
    fn a_schedules_runs_list_latest_first() {
        let (mut s, h) = st();
        for (id, at) in [
            ("ended_old", 5),
            ("park", 1),
            ("queue", 9),
            ("ended_new", 8),
        ] {
            let run = s.runs.get_mut(id).unwrap();
            run.origin = crate::cap::ORIGIN_SCHEDULE.into();
            run.work_item_id = if id == "ended_new" { "sch_b" } else { "sch_a" }.into();
            run.queued_at = at * 1000;
        }
        let sc = |id: &str, name: &str| Schedule {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        };
        let list = [
            sc("sch_a", "nightly"),
            sc("sch_b", "weekly"),
            sc("sch_c", "never"),
        ];
        let b = sessions(Some(&(s, h)), &list, "runner.example", 120_000);
        // Not by what they need (park first in agents:sessions): by start.
        assert_eq!(
            b.queries["agents:schedule_runs:nightly"],
            [
                "agents://sessions/queue",
                "agents://sessions/ended_old",
                "agents://sessions/park",
            ]
        );
        assert_eq!(
            b.queries["agents:schedule_runs:weekly"],
            ["agents://sessions/ended_new"]
        );
        assert!(b.queries["agents:schedule_runs:never"].is_empty());
    }

    #[test]
    fn runs_sort_by_what_they_need() {
        let s = st();
        let b = sessions(Some(&s), &[], "runner.example", 120_000);
        assert_eq!(
            b.queries[crate::SESSIONS_QUERY],
            [
                "agents://sessions/wait",
                "agents://sessions/park",
                "agents://sessions/start",
                "agents://sessions/work",
                "agents://sessions/queue",
                "agents://sessions/ci",
                "agents://sessions/ended_new",
                "agents://sessions/ended_old",
            ]
        );
        let status = &b.entities[STATUS_URI];
        assert_eq!(
            status.get_str("summary_text"),
            Some("2 working · 2 needs you · 1 queued · 1 waiting")
        );
        let e = |id: &str| b.entities[&format!("agents://sessions/{id}")].clone();
        assert_eq!(e("park").get_str("status_text"), Some("Parked"));
        assert_eq!(e("park").get("needs_you"), Some(&json!(true)));
        assert_eq!(
            e("queue").get_str("queue_text"),
            Some("Its sandbox is waking")
        );
        assert_eq!(e("ci").get_str("status_text"), Some("Waiting on CI"));
        assert_eq!(e("ci").get("waiting"), Some(&json!(true)));
        assert_eq!(e("ended_new").get_str("status_text"), Some("Lost"));
        assert_eq!(e("ended_old").get_str("status_text"), Some("Done"));
    }

    #[test]
    fn every_run_state_has_words() {
        for (state, end, outcome, stage, want) in [
            ("queued", "", "", "", "Queued"),
            ("launching", "", "", "", "Starting"),
            ("working", "", "", "", "Working"),
            ("needs_you", "", "", "", "Needs you"),
            ("parked", "", "", "", "Parked"),
            ("stalled", "", "", "", "Stalled"),
            ("interrupted", "", "", "", "Restarting"),
            ("stopping", "", "", "", "Stopping"),
            ("closing", "", "", "", "Finishing"),
            ("ended", "reported", "pending", "ci", "Waiting on CI"),
            (
                "ended",
                "reported",
                "pending",
                "review",
                "Waiting on review",
            ),
            ("ended", "lost", "pending", "deploy", "Waiting on deploy"),
            ("ended", "reported", "done", "", "Done"),
            ("ended", "reported", "needs_review", "", "Needs review"),
            ("ended", "reported", "failed", "", "Failed"),
            ("ended", "stopped", "", "", "Stopped"),
            ("ended", "lost", "", "", "Lost"),
            ("ended", "start_failed", "", "", "Didn't start"),
            ("ended", "timed_out", "", "", "Timed out"),
            // Timed out wins over a review, not over a settled outcome.
            ("ended", "timed_out", "needs_review", "", "Timed out"),
            ("ended", "timed_out", "done", "", "Done"),
            ("ended", "timed_out", "pending", "ci", "Waiting on CI"),
            ("ended", "expired", "", "", "Expired"),
        ] {
            assert_eq!(
                run_look(&r("x", state, 0, end, outcome, stage)).0,
                want,
                "{state} {end} {outcome}"
            );
        }
        let mut x = r("x", "ended", 0, "reported", "failed", "");
        for (reason, want) in [
            ("ci", "CI failed"),
            ("closed_unmerged", "Closed unmerged"),
            ("deploy", "Deploy failed"),
            ("blocked", "Blocked"),
        ] {
            x.outcome.reason = reason.into();
            assert_eq!(run_look(&x).0, want);
        }
        x.outcome = Outcome {
            state: "done".into(),
            reviewed: true,
            ..Default::default()
        };
        assert_eq!(run_look(&x).0, "Reviewed");
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
    fn schedules_read_in_their_zone_with_their_last_run() {
        // 2026-11-08 13:12 UTC, a Sunday: 06:12 in Denver.
        let next: Timestamp = "2026-11-08T13:12:00Z".parse().unwrap();
        let now = next.as_millisecond() - (14 * 3600 + 3 * 60) * 1000;
        let sc = |name: &str, zone: &str| Schedule {
            id: format!("sch_{name}"),
            name: name.into(),
            cron: "12 6 * * *".into(),
            timezone: zone.into(),
            workspace: "w".into(),
            enabled: true,
            prompt: "p".into(),
            max_runtime_min: 30,
            looked_until: now,
            ..Default::default()
        };
        let denver = sc("denver", "America/Denver");
        let mut paused = sc("paused", "UTC");
        paused.enabled = false;
        let mut s = State::default();
        let mut run = r("run_9", "ended", 1, "reported", "failed", "");
        run.work_item_id = "sch_denver".into();
        run.origin = "schedule".into();
        run.outcome.reason = "deploy".into();
        s.runs.insert("run_9".into(), run);
        let views = [
            ScheduleView {
                schedule: &denver,
                last: None,
            },
            ScheduleView {
                schedule: &paused,
                last: None,
            },
        ];
        let berlin = pod_core::tz::parse("Europe/Berlin").unwrap();
        let both = (s, Health::default());
        let b = schedules(Some(&both), &views, now, &berlin);
        let e = |n: &str| b.entities[&format!("agents://schedules/{n}")].clone();
        assert_eq!(
            e("denver").get_str("next_run_text"),
            Some("in 14 h 3 min · Sun 06:12")
        );
        assert_eq!(
            e("denver").get_str("last_status_text"),
            Some("Deploy failed")
        );
        assert_eq!(e("denver").get_str("last_run_id"), Some("run_9"));
        assert_eq!(e("paused").get_str("next_run_text"), Some("Paused"));
        assert_eq!(e("paused").get("can_pause"), Some(&json!(false)));
        // A missed occurrence reads Missed.
        let missed = Occurrence {
            schedule_id: "sch_denver".into(),
            occurrence: "2026-11-07T06:12".into(),
            due_at: now - 1000,
            state: db::OCC_MISSED.into(),
            request_id: String::new(),
            run_id: String::new(),
            manual: false,
            updated_at: now,
        };
        let views = [ScheduleView {
            schedule: &denver,
            last: Some(&missed),
        }];
        let b = schedules(Some(&both), &views, now, &berlin);
        assert_eq!(
            b.entities["agents://schedules/denver"].get_str("last_status_text"),
            Some("Missed")
        );
        // The run's entity names its schedule.
        let sb = sessions(Some(&both), std::slice::from_ref(&denver), "r", now);
        assert_eq!(
            sb.entities["agents://sessions/run_9"].get_str("schedule"),
            Some("denver")
        );
    }

    #[test]
    fn sandboxes_say_their_state() {
        let mut s = State::default();
        for (name, state) in [
            ("a", "asleep"),
            ("b", "awake"),
            ("c", "down"),
            ("d", "degraded"),
        ] {
            s.sprites.insert(
                name.into(),
                Sprite {
                    name: name.into(),
                    state: state.into(),
                    reason: "no heartbeat for 15 min".into(),
                    disk_failures_7d: if name == "d" { 2 } else { 0 },
                    ..Default::default()
                },
            );
        }
        let b = sandboxes(Some(&(s, Health::default())), 0);
        assert_eq!(
            b.queries[crate::SANDBOXES_QUERY],
            [
                "agents://sandboxes/c",
                "agents://sandboxes/d",
                "agents://sandboxes/b",
                "agents://sandboxes/a"
            ]
        );
        let c = &b.entities["agents://sandboxes/c"];
        assert_eq!(c.get_str("state_text"), Some("Down"));
        assert_eq!(c.get("down"), Some(&json!(true)));
        assert_eq!(c.get_str("reason"), Some("no heartbeat for 15 min"));
        let d = &b.entities["agents://sandboxes/d"];
        assert_eq!(d.get("disk_warning"), Some(&json!(true)));
        assert_eq!(
            b.entities["agents://sandboxes/b"].get_str("reason"),
            Some("")
        );
        // Memory alone on an awake sandbox: not shown as a failure.
        let mut s = State::default();
        s.sprites.insert(
            "m".into(),
            Sprite {
                name: "m".into(),
                state: "awake".into(),
                preflight_ok: false,
                preflight_failures: vec!["memory".into()],
                ..Default::default()
            },
        );
        let b = sandboxes(Some(&(s, Health::default())), 0);
        let m = &b.entities["agents://sandboxes/m"];
        assert_eq!(m.get_str("preflight_text"), Some(""));
        assert_eq!(m.get_str("state_text"), Some("Awake"));
    }

    #[test]
    fn a_run_log_reads_as_lines() {
        let log = json!({"entries": [{"ts": 1000, "kind": "state", "text": "Working"},
            {"ts": 2000, "kind": "tool", "text": "Bash: cargo test\u{7}"}]});
        let b = timeline(None, &[], "run_1", Some(&log), 61_000);
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
        assert!(timeline(None, &[], "run_1", None, 0).queries["agents:timeline:run_1"].is_empty());
    }

    #[test]
    fn a_run_log_is_kept_at_what_the_timeline_shows() {
        let entry =
            json!({"ts": 1, "kind": "tool", "text": "x".repeat(10_000), "extra": [1, 2, 3]});
        let log = json!({"entries": vec![entry; LOG_MAX + 100], "other": "y".repeat(10_000)});
        let kept = kept_log(&log);
        let entries = kept["entries"].as_array().unwrap();
        assert_eq!(entries.len(), LOG_MAX);
        assert_eq!(entries[0]["text"].as_str().unwrap().chars().count(), 500);
        assert!(entries[0].get("extra").is_none() && kept.get("other").is_none());
        let b = timeline(None, &[], "run_1", Some(&kept), 0);
        assert_eq!(b.queries["agents:timeline:run_1"].len(), LOG_MAX);
    }
}
