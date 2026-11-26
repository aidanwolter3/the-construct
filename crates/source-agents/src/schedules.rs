//! Schedules owned by the pod (decision `agents-cap-v3`): a schedule is a
//! work item. Its definition (name, cron, time zone, workspace, model, max
//! runtime, prompt, on or paused) lives in `agents_schedules`, and the
//! timer ([`tick`]) sends an ordinary `start_run` for each due occurrence:
//! `origin: schedule`, `work_item_id` the schedule's id, `occurrence` the
//! time it is for (RFC 3339 with its offset), and a `request_id` made from both ([`request_id`]), so a
//! resend after a crash or a retry never makes a second run.
//!
//! An occurrence is taken up (recorded) before it is sent. One the runner
//! can't take within [`MISS_AFTER_MS`] of its time (the pod or the runner
//! was down) is skipped as missed, and you are told. Pausing drops what
//! waits to be sent; resuming, or changing the cron or zone, starts from
//! now (nothing paused is "missed").
//!
//! The timer runs in the follower (every pass, connected or not) and in
//! the source's notification poll, so it runs from server start for every
//! pod signed in to a runner (`Source::keeps_open`).

use pod_core::Pod;
use pod_core::tz::jiff::{Timestamp, tz::TimeZone};
use serde_json::{Value, json};

use crate::cap::{self, CapError, Clients, Command, Runner, StartRun};
use crate::cron::{self, Cron};
use crate::db::{self, Occurrence, Schedule};

/// An occurrence not sent within this long of its time is missed (ms).
pub const MISS_AFTER_MS: i64 = 2 * 3600 * 1000;
/// The default and the most of a schedule's max runtime, in minutes.
pub const DEFAULT_RUNTIME_MIN: i64 = 240;
/// The most occurrences recorded in one look (the newest; older ones are
/// counted as missed).
const LOOK_LIMIT: usize = 100;
/// The most schedules a pod keeps.
pub const MAX_SCHEDULES: usize = 50;
const PROMPT_MAX: usize = 20_000;

/// The request id of schedule `id`'s `occurrence` (`manual`: Run now):
/// the same inputs always give the same id, e.g. `sch_ab_20261115T035200m0700`
/// for `2026-11-15T03:52:00-07:00` (the offset's sign kept as `p` or `m`).
pub fn request_id(id: &str, occurrence: &str, manual: bool) -> String {
    let (when, offset) = occurrence.split_at(occurrence.len().min(19));
    let mut compact: String = when.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    for c in offset.chars() {
        match c {
            '+' => compact.push('p'),
            '-' => compact.push('m'),
            c if c.is_ascii_alphanumeric() => compact.push(c),
            _ => {}
        }
    }
    format!("{id}_{compact}{}", if manual { "_now" } else { "" })
}

/// The zone a schedule reads its cron in.
pub fn zone(s: &Schedule) -> TimeZone {
    pod_core::tz::parse(&s.timezone).unwrap_or(TimeZone::UTC)
}

/// The next occurrence of `s` after `now` (ms): its name ([`cron::name`]) and
/// instant, `None` when paused or its cron is unreadable.
pub fn next_run(s: &Schedule, now: i64) -> Option<(String, i64)> {
    if !s.enabled {
        return None;
    }
    let c = Cron::parse(&s.cron).ok()?;
    let at = Timestamp::from_millisecond(now.max(s.looked_until)).ok()?;
    c.next_after(&zone(s), at)
        .map(|(n, t)| (n, t.as_millisecond()))
}

/// What the timer owes the person: notifications, sent by the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Occurrences of a schedule were skipped: not sent in time.
    Missed {
        schedule: String,
        count: usize,
        last: String,
        key: String,
    },
    /// A background agent's credential was refused by its grant again and
    /// again (decision `mcp-call-log`): once a day per credential.
    GrantRefused {
        credential: String,
        count: i64,
        key: String,
    },
    /// The runner refused an occurrence for good.
    Refused {
        schedule: String,
        occurrence: String,
        reason: String,
        key: String,
    },
}

/// One pass's result: the notices owed and the runs the runner answered
/// (for the mirror).
#[derive(Debug, Default)]
pub struct Pass {
    pub notices: Vec<Notice>,
    pub runs: Vec<Value>,
}

/// One pod's timer lock: one pass at a time.
#[derive(Debug, Default)]
pub struct Timer(tokio::sync::Mutex<()>);

/// Take up the due occurrences of every schedule (pure on the store; no
/// runner): those past [`MISS_AFTER_MS`] are missed, the others wait to be
/// sent. Blocking.
pub fn look(store: &pod_core::store::Store, now: i64) -> rusqlite::Result<Vec<Notice>> {
    let mut notices = Vec::new();
    for s in db::schedules(store)? {
        if !s.enabled || s.looked_until >= now {
            continue;
        }
        let (Ok(c), Ok(from), Ok(to)) = (
            Cron::parse(&s.cron),
            Timestamp::from_millisecond(s.looked_until),
            Timestamp::from_millisecond(now),
        ) else {
            continue;
        };
        let due = c.between(&zone(&s), from, to, usize::MAX);
        let total = due.len();
        let kept = &due[total.saturating_sub(LOOK_LIMIT)..];
        let mut missed = total - kept.len();
        let mut last_missed = String::new();
        let mut occurrences = Vec::new();
        for (name, at) in kept {
            let at = at.as_millisecond();
            let late = now - at > MISS_AFTER_MS;
            if late {
                missed += 1;
                last_missed = name.clone();
            }
            occurrences.push(Occurrence {
                schedule_id: s.id.clone(),
                occurrence: name.clone(),
                due_at: at,
                state: if late {
                    db::OCC_MISSED
                } else {
                    db::OCC_PENDING
                }
                .into(),
                request_id: request_id(&s.id, name, false),
                run_id: String::new(),
                manual: false,
                updated_at: now,
            });
        }
        if missed > 0 && last_missed.is_empty() {
            last_missed = due.first().map(|(n, _)| n.clone()).unwrap_or_default();
        }
        db::take_up(store, &s.id, &occurrences, now)?;
        if missed > 0 {
            notices.push(Notice::Missed {
                key: format!("missed:{}:{last_missed}", s.id),
                schedule: s.name.clone(),
                count: missed,
                last: last_missed,
            });
        }
    }
    Ok(notices)
}

/// The `start_run` of occurrence `o` of schedule `s`.
pub fn command(s: &Schedule, o: &Occurrence) -> Command {
    Command::StartRun(StartRun {
        request_id: o.request_id.clone(),
        workspace: s.workspace.clone(),
        work_item_id: s.id.clone(),
        occurrence: Some(o.occurrence.clone()),
        origin: cap::ORIGIN_SCHEDULE.into(),
        title: s.name.clone(),
        prompt: s.prompt.clone(),
        model: (!s.model.is_empty()).then(|| s.model.clone()),
        max_runtime_min: Some(s.max_runtime_min.clamp(1, cap::MAX_RUNTIME_MIN as i64) as u32),
        resume_from: None,
    })
}

/// One pass of the timer: take up due occurrences, then send what waits to
/// be sent (oldest first) until the runner can't be reached. Without a
/// runner, the schedules only move on (nothing can be sent, so nothing is
/// missed).
pub async fn tick(pod: &Pod, clients: &Clients, runner: Option<&Runner>, now: i64) -> Pass {
    let timer = pod.local::<Timer>();
    let _held = timer.0.lock().await;
    let mut pass = Pass::default();
    let Some(runner) = runner else {
        let _ = pod
            .blocking(move |s| {
                for sc in db::schedules(s)? {
                    if sc.enabled && sc.looked_until < now {
                        db::take_up(s, &sc.id, &[], now)?;
                    }
                }
                Ok(())
            })
            .await;
        return pass;
    };
    let Ok((notices, pending, schedules)) = pod
        .blocking(move |s| Ok((look(s, now)?, db::pending(s)?, db::schedules(s)?)))
        .await
    else {
        return pass;
    };
    pass.notices = notices;
    // Repeated refusals of a background agent's credential (the pod's call
    // log): a runner warning, at most once a day each.
    if let Ok(refused) = pod
        .blocking(move |s| s.with_conn(|c| pod_core::call_log::refused_recently(c, now / 1000)))
        .await
    {
        for (credential, count) in refused {
            let short: String = credential.chars().take(80).collect();
            pass.notices.push(Notice::GrantRefused {
                key: format!("grant_refused:{short}:{}", now / 86_400_000),
                credential,
                count,
            });
        }
    }
    let mut changed = false;
    for o in pending {
        let Some(s) = schedules.iter().find(|s| s.id == o.schedule_id) else {
            continue;
        };
        if now - o.due_at > MISS_AFTER_MS {
            let o2 = o.clone();
            let _ = pod
                .blocking(move |st| db::set_occurrence(st, &o2, db::OCC_MISSED, "", now))
                .await;
            pass.notices.push(Notice::Missed {
                key: format!("missed:{}:{}", s.id, o.occurrence),
                schedule: s.name.clone(),
                count: 1,
                last: o.occurrence.clone(),
            });
            changed = true;
            continue;
        }
        match cap::command(clients, runner, &command(s, &o)).await {
            Ok(answer) => {
                let run_id = cap::run_id_of(&answer).unwrap_or_default();
                let o2 = o.clone();
                let _ = pod
                    .blocking(move |st| db::set_occurrence(st, &o2, db::OCC_SENT, &run_id, now))
                    .await;
                if let Some(r) = answer.get("run").filter(|r| r.is_object()) {
                    pass.runs.push(r.clone());
                }
                changed = true;
            }
            Err(CapError::Status(code @ (400 | 404 | 409), detail)) => {
                let o2 = o.clone();
                let _ = pod
                    .blocking(move |st| db::set_occurrence(st, &o2, db::OCC_REFUSED, "", now))
                    .await;
                pass.notices.push(Notice::Refused {
                    key: format!("refused:{}:{}", s.id, o.occurrence),
                    schedule: s.name.clone(),
                    occurrence: o.occurrence.clone(),
                    reason: if detail.is_empty() {
                        format!("the runner answered {code}")
                    } else {
                        detail
                    },
                });
                changed = true;
            }
            Err(e) => {
                tracing::info!(error = %e, "a scheduled run waits for the runner");
                break;
            }
        }
    }
    if changed || !pass.notices.is_empty() {
        pod.publish_data_all(crate::NAME);
    }
    pass
}

/// Run schedule `s` now: an occurrence named by this minute, sent at
/// once (a second tap in the same minute answers the same run). The run's
/// answer.
pub async fn run_now(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    s: &Schedule,
    now: i64,
) -> Result<Value, CapError> {
    let tz = zone(s);
    let at = Timestamp::from_millisecond(now).unwrap_or(Timestamp::UNIX_EPOCH);
    let minute = cron::minute_of(&tz, at);
    let o = Occurrence {
        schedule_id: s.id.clone(),
        request_id: request_id(&s.id, &minute, true),
        occurrence: minute,
        due_at: now,
        state: db::OCC_PENDING.into(),
        run_id: String::new(),
        manual: true,
        updated_at: now,
    };
    let (o2, id) = (o.clone(), s.id.clone());
    let _ = pod
        .blocking(move |st| db::take_up(st, &id, std::slice::from_ref(&o2), 0))
        .await;
    let answer = cap::command(clients, runner, &command(s, &o)).await;
    let (state, run_id) = match &answer {
        Ok(a) => (db::OCC_SENT, cap::run_id_of(a).unwrap_or_default()),
        Err(CapError::Status(400 | 404 | 409, _)) => (db::OCC_REFUSED, String::new()),
        // Not sent: not retried by the timer either (a tap is now or never).
        Err(_) => (db::OCC_MISSED, String::new()),
    };
    let _ = pod
        .blocking(move |st| db::set_occurrence(st, &o, state, &run_id, now))
        .await;
    pod.publish_data_all(crate::NAME);
    answer
}

// ---------- definitions ----------

fn bad(detail: impl Into<String>) -> Value {
    json!({"error": "invalid_request", "detail": detail.into()})
}

/// A text input: `None` when absent or blank (keep it, on an edit).
fn input<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn number(args: &Value, k: &str) -> Option<Result<i64, ()>> {
    match args.get(k) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(v) => Some(
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .ok_or(()),
        ),
    }
}

fn flag(args: &Value, k: &str) -> Option<bool> {
    match args.get(k)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "true" | "on" | "yes" => Some(true),
            "false" | "off" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A schedule name: 1 to 60 letters, digits, spaces, `-`, `_` or `.`.
fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.chars().count() <= 60
        && n.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// A schedule from `args` (`name, cron, timezone, workspace, model,
/// max_runtime_min, enabled, prompt`), on top of `existing` for an edit
/// (blank or absent fields keep their value; `new_name` renames).
/// `pod_zone`: the zone a new schedule without one takes.
pub fn from_input(
    args: &Value,
    existing: Option<&Schedule>,
    pod_zone: &str,
    now: i64,
) -> Result<Schedule, Value> {
    let mut s = existing.cloned().unwrap_or_else(|| Schedule {
        id: source_collections::db::new_id("sch_"),
        timezone: pod_zone.to_owned(),
        max_runtime_min: DEFAULT_RUNTIME_MIN,
        enabled: true,
        looked_until: now,
        created_at: now,
        ..Default::default()
    });
    let name_key = if existing.is_some() {
        "new_name"
    } else {
        "name"
    };
    if let Some(n) = input(args, name_key) {
        if !valid_name(n) {
            return Err(bad(
                "a schedule's name is 1 to 60 letters, digits, spaces, - _ or .",
            ));
        }
        s.name = n.to_owned();
    }
    let mut moved = false;
    if let Some(c) = input(args, "cron") {
        Cron::parse(c).map_err(bad)?;
        moved |= s.cron != c;
        s.cron = c.to_owned();
    }
    if let Some(z) = input(args, "timezone") {
        let tz = pod_core::tz::parse(z).map_err(bad)?;
        let z = tz.iana_name().unwrap_or(z).to_owned();
        moved |= s.timezone != z;
        s.timezone = z;
    }
    if let Some(w) = input(args, "workspace") {
        if w.chars().count() > 100 {
            return Err(bad("\"workspace\" is at most 100 characters"));
        }
        s.workspace = w.to_owned();
    }
    if let Some(m) = args.get("model").and_then(Value::as_str) {
        let m = m.trim();
        if m.len() > 100 || !m.chars().all(|c| c.is_ascii_graphic()) {
            return Err(bad("\"model\" is one word of at most 100 characters"));
        }
        // An edit's blank model keeps it; "default" clears it.
        if !m.is_empty() || existing.is_none() {
            s.model = if m == "default" {
                String::new()
            } else {
                m.to_owned()
            };
        }
    }
    match number(args, "max_runtime_min") {
        Some(Ok(n @ 1..=480)) => s.max_runtime_min = n,
        Some(_) => return Err(bad("\"max_runtime_min\" is 1 to 480 minutes")),
        None => {}
    }
    if let Some(p) = input(args, "prompt") {
        if p.chars().count() > PROMPT_MAX {
            return Err(bad("\"prompt\" is at most 20000 characters"));
        }
        s.prompt = p.to_owned();
    }
    if let Some(e) = flag(args, "enabled") {
        moved |= e && !s.enabled;
        s.enabled = e;
    }
    for (k, v) in [
        ("name", &s.name),
        ("cron", &s.cron),
        ("workspace", &s.workspace),
        ("prompt", &s.prompt),
    ] {
        if v.is_empty() {
            return Err(bad(format!("a schedule needs \"{k}\"")));
        }
    }
    if s.timezone.is_empty() {
        s.timezone = "UTC".into();
    }
    if moved {
        // A new time, or back on: from now, with nothing missed.
        s.looked_until = now;
    }
    s.updated_at = now;
    Ok(s)
}

/// A schedule as the MCP tool and the routes answer it.
pub fn describe(s: &Schedule, now: i64) -> Value {
    json!({
        "id": s.id, "name": s.name, "cron": s.cron, "timezone": s.timezone,
        "workspace": s.workspace, "model": s.model, "max_runtime_min": s.max_runtime_min,
        "enabled": s.enabled, "prompt": s.prompt,
        "next_run": next_run(s, now).map(|(n, _)| n),
    })
}

/// What a schedule operation was refused with: (status, body).
pub type Refusal = (u16, Value);

fn not_found(key: &str) -> Refusal {
    (
        404,
        json!({"error": "not_found", "detail": format!("no schedule '{key}'")}),
    )
}

fn store_failed() -> Refusal {
    (
        500,
        json!({"error": "store_failed", "detail": "the schedules can't be written"}),
    )
}

/// The schedule `args` names (`name` or `id`).
async fn named(pod: &Pod, args: &Value) -> Result<Schedule, Refusal> {
    let key = input(args, "name")
        .or_else(|| input(args, "id"))
        .ok_or((400, bad("\"name\" names a schedule")))?
        .to_owned();
    let k = key.clone();
    pod.blocking(move |s| db::schedule(s, &k))
        .await
        .map_err(|_| store_failed())?
        .ok_or_else(|| not_found(&key))
}

/// Create a schedule from `args`.
pub async fn create(pod: &Pod, args: &Value) -> Result<Value, Refusal> {
    let now = crate::follower::now_ms();
    let (zone, count) = pod
        .blocking(|s| {
            Ok((
                pod_core::tz::time_zone_name(s)?.unwrap_or_else(|| "UTC".into()),
                db::schedules(s)?.len(),
            ))
        })
        .await
        .map_err(|_| store_failed())?;
    if count >= MAX_SCHEDULES {
        return Err((
            409,
            json!({"error": "too_many", "detail": "a pod keeps at most 50 schedules"}),
        ));
    }
    let s = from_input(args, None, &zone, now).map_err(|e| (400, e))?;
    put(pod, s, now).await
}

async fn put(pod: &Pod, s: Schedule, now: i64) -> Result<Value, Refusal> {
    let s2 = s.clone();
    let ok = pod
        .blocking(move |st| db::put_schedule(st, &s2))
        .await
        .map_err(|_| store_failed())?;
    if !ok {
        return Err((
            409,
            json!({"error": "name_taken", "detail": format!("a schedule is already named '{}'", s.name)}),
        ));
    }
    pod.publish_data_all(crate::NAME);
    Ok(json!({"ok": true, "schedule": describe(&s, now)}))
}

/// Edit the schedule `args` names (blank fields keep their value).
pub async fn update(pod: &Pod, args: &Value) -> Result<Value, Refusal> {
    let now = crate::follower::now_ms();
    let old = named(pod, args).await?;
    let s = from_input(args, Some(&old), "UTC", now).map_err(|e| (400, e))?;
    put(pod, s, now).await
}

/// Pause or resume the schedule `args` names. Pausing drops what waits to
/// be sent.
pub async fn set_enabled(pod: &Pod, args: &Value, enabled: bool) -> Result<Value, Refusal> {
    let now = crate::follower::now_ms();
    let mut s = named(pod, args).await?;
    if s.enabled != enabled {
        s.enabled = enabled;
        s.looked_until = now;
        s.updated_at = now;
    }
    let id = s.id.clone();
    if !enabled {
        let _ = pod.blocking(move |st| db::drop_pending(st, &id)).await;
    }
    put(pod, s, now).await
}

/// Pause every schedule: the kill switch (decision `mcp-call-log`). Runs
/// going now go on and nothing is revoked; each schedule resumes by name.
pub async fn pause_all(pod: &Pod) -> Result<Value, Refusal> {
    let now = crate::follower::now_ms();
    let list = pod
        .blocking(db::schedules)
        .await
        .map_err(|_| store_failed())?;
    let mut paused = Vec::new();
    for mut s in list.into_iter().filter(|s| s.enabled) {
        s.enabled = false;
        s.looked_until = now;
        s.updated_at = now;
        let id = s.id.clone();
        let _ = pod.blocking(move |st| db::drop_pending(st, &id)).await;
        paused.push(s.name.clone());
        let _ = put(pod, s, now).await?;
    }
    pod.publish_data_all(crate::NAME);
    Ok(json!({"ok": true, "paused": paused}))
}

/// Delete the schedule `args` names (its runs go on).
pub async fn delete(pod: &Pod, args: &Value) -> Result<Value, Refusal> {
    let s = named(pod, args).await?;
    let id = s.id.clone();
    pod.blocking(move |st| db::delete_schedule(st, &id))
        .await
        .map_err(|_| store_failed())?;
    pod.publish_data_all(crate::NAME);
    Ok(json!({"ok": true, "deleted": s.name}))
}

/// Find the schedule `args` names, for Run now.
pub async fn find(pod: &Pod, args: &Value) -> Result<Schedule, Refusal> {
    named(pod, args).await
}

/// `agents_schedules {op, ...}`: the MCP tool.
pub async fn tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
    let out = match op {
        "list" => {
            let now = crate::follower::now_ms();
            let list = pod
                .blocking(db::schedules)
                .await
                .map_err(|_| store_failed().1)?;
            Ok(json!({"schedules": list.iter().map(|s| describe(s, now)).collect::<Vec<_>>()}))
        }
        "create" => create(pod, args).await,
        "update" => update(pod, args).await,
        "delete" => delete(pod, args).await,
        "pause" => set_enabled(pod, args, false).await,
        "pause_all" => pause_all(pod).await,
        "resume" => set_enabled(pod, args, true).await,
        "run" => {
            let s = named(pod, args).await.map_err(|(_, e)| e)?;
            let Some(a) = pod.adapter::<crate::AgentsAdapter>() else {
                return Err(bad("no agents source"));
            };
            let Some(runner) = pod.blocking(db::runner).await.ok().flatten() else {
                return Err(json!({"error": "not_signed_in",
                    "detail": "the pod is not signed in to an agent runner"}));
            };
            run_now(pod, &a.clients, &runner, &s, crate::follower::now_ms())
                .await
                .map(|answer| json!({"ok": true, "run_id": cap::run_id_of(&answer)}))
                .map_err(|e| {
                    (
                        502,
                        json!({"error": "runner_refused", "detail": e.to_string()}),
                    )
                })
        }
        _ => Err((
            400,
            bad("\"op\" is list, create, update, delete, pause, pause_all, resume or run"),
        )),
    };
    out.map_err(|(_, e)| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_deterministic() {
        assert_eq!(
            request_id("sch_ab", "2026-11-15T03:52:00-07:00", false),
            "sch_ab_20261115T035200m0700"
        );
        assert_eq!(
            request_id("sch_ab", "2026-11-15T03:52:00+07:00", true),
            "sch_ab_20261115T035200p0700_now"
        );
    }

    #[test]
    fn inputs_make_a_schedule() {
        let args = json!({"name": "nightly-deps", "cron": "0 3 * * *", "workspace": "sample-repo",
            "prompt": "Bump the dependencies.", "max_runtime_min": "45"});
        let s = from_input(&args, None, "America/Denver", 1000).unwrap();
        assert!(s.id.starts_with("sch_"));
        assert_eq!(
            (s.timezone.as_str(), s.max_runtime_min),
            ("America/Denver", 45)
        );
        assert!(s.enabled && s.model.is_empty());
        // An edit keeps what it leaves blank.
        let edit = json!({"name": "nightly-deps", "cron": "", "prompt": " ", "model": "a-model",
            "new_name": "nightly"});
        let e = from_input(&edit, Some(&s), "UTC", 2000).unwrap();
        assert_eq!((e.name.as_str(), e.cron.as_str()), ("nightly", "0 3 * * *"));
        assert_eq!(
            (e.prompt.as_str(), e.model.as_str()),
            ("Bump the dependencies.", "a-model")
        );
        assert_eq!(e.looked_until, 1000, "the same time: nothing moves");
        let e = from_input(&json!({"cron": "0 4 * * *"}), Some(&s), "UTC", 3000).unwrap();
        assert_eq!(e.looked_until, 3000, "a new time starts from now");
        for bad in [
            json!({"name": "x", "cron": "nope", "workspace": "w", "prompt": "p"}),
            json!({"name": "x", "cron": "0 3 * * *", "workspace": "w"}),
            json!({"name": "a/b", "cron": "0 3 * * *", "workspace": "w", "prompt": "p"}),
            json!({"name": "x", "cron": "0 3 * * *", "workspace": "w", "prompt": "p", "timezone": "Mars/Base"}),
            json!({"name": "x", "cron": "0 3 * * *", "workspace": "w", "prompt": "p", "max_runtime_min": 481}),
        ] {
            assert!(from_input(&bad, None, "UTC", 0).is_err(), "{bad}");
        }
    }
}
