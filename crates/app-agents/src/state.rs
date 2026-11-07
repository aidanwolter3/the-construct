//! What the pod knows of the runner: the last snapshot with every event
//! since applied ([`State`]). Applying a snapshot or an event is pure and
//! returns the [`Effect`]s it owes (notifications, task-row changes), so the
//! rules are tested without a runner.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cap::Event;
use crate::text;

/// At most this many sessions are kept (the oldest ended ones go first).
pub const MAX_SESSIONS: usize = 200;

/// Session statuses.
pub const STARTING: &str = "starting";
pub const WORKING: &str = "working";
pub const WAITING_PERMISSION: &str = "waiting_permission";
pub const WAITING_INPUT: &str = "waiting_input";
pub const IDLE: &str = "idle";
pub const FINISHED: &str = "finished";
pub const FAILED: &str = "failed";
pub const STALE: &str = "stale";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The last event applied (the stream resumes after it).
    pub cursor: i64,
    pub sprites: BTreeMap<String, Sprite>,
    pub sessions: BTreeMap<String, Session>,
    pub checks: BTreeMap<String, Check>,
    pub schedules: BTreeMap<String, Schedule>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sprite {
    pub name: String,
    /// `awake`, `asleep`, or whatever the runner says.
    pub state: String,
    /// Milliseconds.
    pub last_seen: i64,
    /// The workspace `wake` takes (the sandbox's name unless the runner says).
    pub workspace: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub status: String,
    pub sprite: String,
    pub workspace: String,
    pub task_id: String,
    pub current_tool: String,
    pub tool_detail: String,
    pub summary: String,
    pub pr_url: String,
    pub error: String,
    pub reason: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_s: f64,
    /// Milliseconds.
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub title: String,
    /// `ok` or `warn`.
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub cron: String,
    pub timezone: String,
    pub workspace: String,
    pub model: String,
    pub max_runtime_minutes: i64,
    pub enabled: bool,
    pub paused: bool,
    /// Milliseconds.
    pub next_run: Option<i64>,
    pub last_run: Option<LastRun>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub run_key: String,
    pub due_at: Option<i64>,
    pub status: String,
    pub error: String,
    pub session_id: String,
    pub summary: String,
    pub cost_usd: f64,
}

/// What applying something owes.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A session started waiting for a permission.
    NeedsYou {
        session_id: String,
        title: String,
        detail: String,
    },
    Finished {
        session_id: String,
        title: String,
        summary: String,
        pr_url: String,
    },
    Failed {
        session_id: String,
        title: String,
        error: String,
    },
    /// A security check turned to `warn`.
    CheckWarn {
        id: String,
        title: String,
        detail: String,
    },
    ScheduleMissed {
        name: String,
        error: String,
    },
    ScheduleTimeout {
        name: String,
        minutes: i64,
        stopped: bool,
    },
    /// A session for task `task_id` (a Tasks row) started.
    TaskStarted {
        task_id: String,
        session_id: String,
    },
    TaskFinished {
        task_id: String,
        summary: String,
        pr_url: String,
    },
    TaskFailed {
        task_id: String,
        error: String,
    },
}

// ---------- reading the runner's JSON ----------

fn s(v: &Value, keys: &[&str], max: usize) -> String {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(|x| text::clean(x, max))
        .unwrap_or_default()
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// A time in milliseconds (seconds are taken for small values).
fn ms(v: &Value, keys: &[&str]) -> Option<i64> {
    let n = keys
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_i64))?;
    Some(if n.abs() < 100_000_000_000 {
        n * 1000
    } else {
        n
    })
}

fn summary(v: &Value) -> String {
    v.get("summary")
        .and_then(Value::as_str)
        .map(|x| text::clean_lines(x, 2000))
        .unwrap_or_default()
}

fn pr(v: &Value) -> String {
    v.get("pr_url")
        .and_then(Value::as_str)
        .and_then(text::pr_url)
        .unwrap_or_default()
}

fn sprite_of(v: &Value) -> Option<Sprite> {
    let name = s(v, &["name", "id", "sprite"], 100);
    if name.is_empty() {
        return None;
    }
    let workspace = s(v, &["workspace"], 100);
    Some(Sprite {
        workspace: if workspace.is_empty() {
            name.clone()
        } else {
            workspace
        },
        state: s(v, &["state", "status"], 40),
        last_seen: ms(v, &["last_seen", "last_seen_at", "updated_at", "ts"]).unwrap_or(0),
        name,
    })
}

fn session_of(v: &Value) -> Option<Session> {
    let id = s(v, &["id", "session_id"], 120);
    if id.is_empty() {
        return None;
    }
    Some(Session {
        title: s(v, &["title"], 200),
        status: s(v, &["status"], 40),
        sprite: s(v, &["sprite"], 100),
        workspace: s(v, &["workspace"], 100),
        task_id: s(v, &["task_id"], 200),
        current_tool: s(v, &["current_tool", "tool"], 80),
        tool_detail: s(v, &["tool_detail", "detail"], 300),
        summary: summary(v),
        pr_url: pr(v),
        error: s(v, &["error"], 500),
        reason: s(v, &["reason"], 100),
        cost_usd: f(v, "cost_usd"),
        input_tokens: i(v, "input_tokens"),
        output_tokens: i(v, "output_tokens"),
        duration_s: f(v, "duration_s"),
        started_at: ms(v, &["started_at", "created_at"]).unwrap_or(0),
        updated_at: ms(v, &["updated_at", "started_at", "created_at"]).unwrap_or(0),
        id,
    })
}

fn check_of(v: &Value) -> Option<Check> {
    let id = s(v, &["id"], 100);
    if id.is_empty() {
        return None;
    }
    Some(Check {
        title: s(v, &["title"], 200),
        status: s(v, &["status"], 20),
        detail: s(v, &["detail"], 500),
        id,
    })
}

fn last_run_of(v: &Value) -> Option<LastRun> {
    if !v.is_object() {
        return None;
    }
    Some(LastRun {
        run_key: s(v, &["run_key"], 200),
        due_at: ms(v, &["due_at"]),
        status: s(v, &["status"], 40),
        error: s(v, &["error"], 500),
        session_id: s(v, &["session_id"], 120),
        summary: summary(v),
        cost_usd: f(v, "cost_usd"),
    })
}

fn schedule_of(v: &Value) -> Option<Schedule> {
    let name = s(v, &["name"], 100);
    if name.is_empty() {
        return None;
    }
    Some(Schedule {
        cron: s(v, &["cron"], 100),
        timezone: s(v, &["timezone"], 64),
        workspace: s(v, &["workspace"], 100),
        model: s(v, &["model"], 100),
        max_runtime_minutes: i(v, "max_runtime_minutes"),
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        paused: v.get("paused").and_then(Value::as_bool).unwrap_or(false),
        next_run: ms(v, &["next_run"]),
        last_run: v.get("last_run").and_then(last_run_of),
        name,
    })
}

fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

/// The schedule a scheduled run's task id names: `schedule/<name>/...`.
pub fn schedule_of_task(task_id: &str) -> Option<&str> {
    let rest = task_id.strip_prefix("schedule/")?;
    let name = rest.split('/').next()?;
    (!name.is_empty()).then_some(name)
}

// ---------- applying ----------

impl Session {
    /// Running or waiting (not finished, failed or stale).
    pub fn is_active(&self) -> bool {
        !matches!(self.status.as_str(), FINISHED | FAILED | STALE)
    }
}

impl State {
    /// Replace everything with `snap` (`GET /v1/snapshot`). Owes a
    /// notification for each session now waiting for a permission and each
    /// check now warning that weren't before.
    pub fn apply_snapshot(&mut self, snap: &Value) -> Vec<Effect> {
        let mut effects = Vec::new();
        let sessions: BTreeMap<String, Session> = list(snap, "sessions")
            .filter_map(session_of)
            .map(|x| (x.id.clone(), x))
            .collect();
        for x in sessions.values() {
            let before = self.sessions.get(&x.id).map(|o| o.status.as_str());
            if x.status == WAITING_PERMISSION && before != Some(WAITING_PERMISSION) {
                effects.push(Effect::NeedsYou {
                    session_id: x.id.clone(),
                    title: x.title.clone(),
                    detail: tool_text(&x.current_tool, &x.tool_detail),
                });
            }
        }
        let checks: BTreeMap<String, Check> = list(snap, "checks")
            .filter_map(check_of)
            .map(|x| (x.id.clone(), x))
            .collect();
        for x in checks.values() {
            let before = self.checks.get(&x.id).map(|o| o.status.as_str());
            if x.status == "warn" && before != Some("warn") {
                effects.push(Effect::CheckWarn {
                    id: x.id.clone(),
                    title: x.title.clone(),
                    detail: x.detail.clone(),
                });
            }
        }
        self.sessions = sessions;
        self.checks = checks;
        self.sprites = list(snap, "sprites")
            .filter_map(sprite_of)
            .map(|x| (x.name.clone(), x))
            .collect();
        self.schedules = list(snap, "schedules")
            .filter_map(schedule_of)
            .map(|x| (x.name.clone(), x))
            .collect();
        if let Some(c) = snap.get("cursor").and_then(Value::as_i64) {
            self.cursor = c;
        }
        self.trim();
        effects
    }

    fn session(&mut self, e: &Event) -> Option<&mut Session> {
        let id = e.session_id.clone()?;
        let s = self.sessions.entry(id.clone()).or_insert_with(|| Session {
            id,
            status: STARTING.into(),
            started_at: e.ts,
            ..Default::default()
        });
        s.updated_at = s.updated_at.max(e.ts);
        if let Some(t) = &e.task_id {
            s.task_id = text::clean(t, 200);
        }
        if let Some(sp) = &e.sprite {
            s.sprite = text::clean(sp, 100);
        }
        for (k, field) in [("title", &mut s.title), ("workspace", &mut s.workspace)] {
            if let Some(v) = e.text(k) {
                *field = text::clean(&v, 200);
            }
        }
        Some(s)
    }

    /// Apply one event. Events at or before the cursor are skipped (a
    /// replay after a reconnect).
    pub fn apply(&mut self, e: &Event) -> Vec<Effect> {
        if e.seq <= self.cursor {
            return Vec::new();
        }
        self.cursor = e.seq;
        let mut out = Vec::new();
        let status = e.status.as_deref().map(|x| text::clean(x, 40));
        match e.kind.as_str() {
            "task.launched" => {
                if let Some(s) = self.session(e) {
                    s.status = status.unwrap_or_else(|| STARTING.into());
                }
            }
            "session.started" => {
                if let Some(s) = self.session(e) {
                    s.status = status.unwrap_or_else(|| WORKING.into());
                    if s.started_at == 0 {
                        s.started_at = e.ts;
                    }
                    if !s.task_id.is_empty() {
                        out.push(Effect::TaskStarted {
                            task_id: s.task_id.clone(),
                            session_id: s.id.clone(),
                        });
                    }
                }
            }
            "session.status" | "permission.requested" => {
                let is_perm = e.kind == "permission.requested";
                let tool = e.text("tool").map(|t| text::clean(&t, 80));
                let detail = e.text("detail").map(|t| text::clean(&t, 300));
                if let Some(s) = self.session(e) {
                    let before = std::mem::take(&mut s.status);
                    s.status = if is_perm {
                        WAITING_PERMISSION.into()
                    } else {
                        status.unwrap_or_else(|| before.clone())
                    };
                    if let Some(t) = tool {
                        s.current_tool = t;
                        s.tool_detail = detail.unwrap_or_default();
                    }
                    if s.status == WAITING_PERMISSION && before != WAITING_PERMISSION {
                        out.push(Effect::NeedsYou {
                            session_id: s.id.clone(),
                            title: s.title.clone(),
                            detail: tool_text(&s.current_tool, &s.tool_detail),
                        });
                    }
                }
            }
            "tool.used" => {
                let tool = e.text("tool").map(|t| text::clean(&t, 80));
                let detail = e.text("detail").map(|t| text::clean(&t, 300));
                if let Some(s) = self.session(e) {
                    if let Some(t) = tool {
                        s.current_tool = t;
                        s.tool_detail = detail.unwrap_or_default();
                    }
                    if s.status == STARTING || s.status == WAITING_PERMISSION {
                        s.status = WORKING.into();
                    }
                }
            }
            "session.finished" => {
                let d = e.data.clone();
                if let Some(s) = self.session(e) {
                    s.status = FINISHED.into();
                    s.summary = summary(&d);
                    s.pr_url = pr(&d);
                    s.cost_usd = f(&d, "cost_usd");
                    s.input_tokens = i(&d, "input_tokens");
                    s.output_tokens = i(&d, "output_tokens");
                    s.duration_s = f(&d, "duration_s");
                    s.reason = text::clean(d["reason"].as_str().unwrap_or(""), 100);
                    s.current_tool.clear();
                    s.tool_detail.clear();
                    let s = s.clone();
                    out.push(Effect::Finished {
                        session_id: s.id.clone(),
                        title: s.title.clone(),
                        summary: s.summary.clone(),
                        pr_url: s.pr_url.clone(),
                    });
                    if let Some(name) = schedule_of_task(&s.task_id)
                        && let Some(sc) = self.schedules.get_mut(name)
                    {
                        sc.last_run = Some(LastRun {
                            status: FINISHED.into(),
                            session_id: s.id.clone(),
                            summary: s.summary.clone(),
                            cost_usd: s.cost_usd,
                            ..sc.last_run.clone().unwrap_or_default()
                        });
                    } else if !s.task_id.is_empty() {
                        out.push(Effect::TaskFinished {
                            task_id: s.task_id,
                            summary: s.summary,
                            pr_url: s.pr_url,
                        });
                    }
                }
            }
            "session.failed" => {
                let error = text::clean(e.data["error"].as_str().unwrap_or(""), 500);
                if let Some(s) = self.session(e) {
                    s.status = FAILED.into();
                    s.error = error.clone();
                    s.current_tool.clear();
                    s.tool_detail.clear();
                    let s = s.clone();
                    out.push(Effect::Failed {
                        session_id: s.id.clone(),
                        title: s.title.clone(),
                        error: error.clone(),
                    });
                    if let Some(name) = schedule_of_task(&s.task_id)
                        && let Some(sc) = self.schedules.get_mut(name)
                    {
                        sc.last_run = Some(LastRun {
                            status: FAILED.into(),
                            session_id: s.id.clone(),
                            error,
                            ..sc.last_run.clone().unwrap_or_default()
                        });
                    } else if !s.task_id.is_empty() {
                        out.push(Effect::TaskFailed {
                            task_id: s.task_id,
                            error,
                        });
                    }
                }
            }
            "sprite.state" => {
                if let Some(name) = e.sprite.as_deref().map(|n| text::clean(n, 100))
                    && !name.is_empty()
                {
                    let sp = self.sprites.entry(name.clone()).or_insert_with(|| Sprite {
                        workspace: name.clone(),
                        name,
                        ..Default::default()
                    });
                    if let Some(st) = status {
                        sp.state = st;
                    }
                    sp.last_seen = e.ts;
                }
            }
            "security.check" => {
                let id = e
                    .text("id")
                    .map(|x| text::clean(&x, 100))
                    .unwrap_or_default();
                if !id.is_empty() {
                    let c = self.checks.entry(id.clone()).or_insert_with(|| Check {
                        id: id.clone(),
                        ..Default::default()
                    });
                    let before = std::mem::take(&mut c.status);
                    c.status = status.unwrap_or_else(|| before.clone());
                    if let Some(t) = e.text("title") {
                        c.title = text::clean(&t, 200);
                    }
                    if let Some(d) = e.text("detail") {
                        c.detail = text::clean(&d, 500);
                    }
                    if c.status == "warn" && before != "warn" {
                        out.push(Effect::CheckWarn {
                            id,
                            title: c.title.clone(),
                            detail: c.detail.clone(),
                        });
                    }
                }
            }
            "schedule.state" => {
                if let Some(sc) = self.schedule(e) {
                    match status.as_deref() {
                        Some("paused") => sc.paused = true,
                        Some("active") => sc.paused = false,
                        _ => {}
                    }
                }
            }
            "schedule.missed" => {
                let error = text::clean(e.data["error"].as_str().unwrap_or(""), 500);
                if let Some(sc) = self.schedule(e) {
                    sc.last_run = Some(LastRun {
                        status: "missed".into(),
                        error: error.clone(),
                        ..Default::default()
                    });
                    out.push(Effect::ScheduleMissed {
                        name: sc.name.clone(),
                        error,
                    });
                }
            }
            "schedule.timeout" => {
                let minutes = i(&e.data, "max_runtime_minutes");
                let stopped = e.data["stopped"].as_bool().unwrap_or(false);
                if let Some(sc) = self.schedule(e) {
                    out.push(Effect::ScheduleTimeout {
                        name: sc.name.clone(),
                        minutes: if minutes > 0 {
                            minutes
                        } else {
                            sc.max_runtime_minutes
                        },
                        stopped,
                    });
                }
            }
            _ => {}
        }
        self.trim();
        out
    }

    fn schedule(&mut self, e: &Event) -> Option<&mut Schedule> {
        let name = text::clean(&e.text("schedule")?, 100);
        if name.is_empty() {
            return None;
        }
        Some(
            self.schedules
                .entry(name.clone())
                .or_insert_with(|| Schedule {
                    name,
                    enabled: true,
                    ..Default::default()
                }),
        )
    }

    /// Keep at most [`MAX_SESSIONS`], dropping the oldest ended ones.
    fn trim(&mut self) {
        if self.sessions.len() <= MAX_SESSIONS {
            return;
        }
        let mut ended: Vec<(i64, String)> = self
            .sessions
            .values()
            .filter(|s| !s.is_active())
            .map(|s| (s.updated_at, s.id.clone()))
            .collect();
        ended.sort();
        let extra = self.sessions.len() - MAX_SESSIONS;
        for (_, id) in ended.into_iter().take(extra) {
            self.sessions.remove(&id);
        }
    }
}

/// `Bash: npm test`, or the tool alone.
pub fn tool_text(tool: &str, detail: &str) -> String {
    match (tool.is_empty(), detail.is_empty()) {
        (true, _) => String::new(),
        (false, true) => tool.to_owned(),
        (false, false) => format!("{tool}: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, kind: &str, rest: Value) -> Event {
        let mut v = json!({"v": 1, "seq": seq, "ts": seq * 1000, "type": kind});
        for (k, x) in rest.as_object().unwrap() {
            v[k] = x.clone();
        }
        Event::parse(&v).unwrap()
    }

    #[test]
    fn a_session_runs_to_a_pull_request() {
        let mut st = State::default();
        assert!(
            st.apply(&ev(
                1,
                "task.launched",
                json!({"session_id": "s1", "task_id": "r1",
                "data": {"title": "Fix the login page"}})
            ))
            .is_empty()
        );
        assert_eq!(
            st.apply(&ev(
                2,
                "session.started",
                json!({"session_id": "s1", "sprite": "box-1"})
            )),
            [Effect::TaskStarted {
                task_id: "r1".into(),
                session_id: "s1".into()
            }]
        );
        st.apply(&ev(
            3,
            "tool.used",
            json!({"session_id": "s1", "data": {"tool": "Bash", "detail": "npm test"}}),
        ));
        assert_eq!(st.sessions["s1"].current_tool, "Bash");
        let got = st.apply(&ev(
            4,
            "permission.requested",
            json!({"session_id": "s1", "data": {"tool": "Bash", "detail": "rm -rf build"}}),
        ));
        assert_eq!(
            got,
            [Effect::NeedsYou {
                session_id: "s1".into(),
                title: "Fix the login page".into(),
                detail: "Bash: rm -rf build".into()
            }]
        );
        // The status event for the same wait owes nothing more.
        assert!(
            st.apply(&ev(
                5,
                "session.status",
                json!({"session_id": "s1", "status": "waiting_permission"})
            ))
            .is_empty()
        );
        let got = st.apply(&ev(6, "session.finished", json!({"session_id": "s1", "data": {
            "summary": "Fixed it.", "pr_url": "https://github.com/owner/repo/pull/7", "cost_usd": 0.5}})));
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[1],
            Effect::TaskFinished {
                task_id: "r1".into(),
                summary: "Fixed it.".into(),
                pr_url: "https://github.com/owner/repo/pull/7".into()
            }
        );
        assert_eq!(st.cursor, 6);
        // A replay is skipped.
        assert!(
            st.apply(&ev(6, "session.failed", json!({"session_id": "s1"})))
                .is_empty()
        );
        assert_eq!(st.sessions["s1"].status, FINISHED);
    }

    #[test]
    fn a_bad_pr_link_is_dropped() {
        let mut st = State::default();
        st.apply(&ev(
            1,
            "session.finished",
            json!({"session_id": "s1", "data": {
            "summary": "x", "pr_url": "https://evil.example/pull/1"}}),
        ));
        assert_eq!(st.sessions["s1"].pr_url, "");
    }

    #[test]
    fn a_check_warns_once_until_ok() {
        let mut st = State::default();
        let warn = |seq| {
            ev(
                seq,
                "security.check",
                json!({"status": "warn", "data": {"id": "egress", "title": "Egress"}}),
            )
        };
        assert_eq!(st.apply(&warn(1)).len(), 1);
        assert!(st.apply(&warn(2)).is_empty());
        st.apply(&ev(
            3,
            "security.check",
            json!({"status": "ok", "data": {"id": "egress"}}),
        ));
        assert_eq!(st.apply(&warn(4)).len(), 1);
        // A snapshot with the check still warning owes nothing.
        let snap =
            json!({"cursor": 9, "checks": [{"id": "egress", "status": "warn", "title": "Egress"}]});
        assert!(st.apply_snapshot(&snap).is_empty());
        assert_eq!(st.cursor, 9);
    }

    #[test]
    fn schedules_follow_their_events() {
        let mut st = State::default();
        st.apply_snapshot(
            &json!({"cursor": 1, "schedules": [{"name": "nightly", "cron": "0 3 * * *",
            "timezone": "UTC", "workspace": "repo", "max_runtime_minutes": 30, "enabled": true,
            "paused": false, "next_run": 1_800_000_000_000_i64, "last_run": null}]}),
        );
        st.apply(&ev(
            2,
            "schedule.state",
            json!({"status": "paused", "data": {"schedule": "nightly"}}),
        ));
        assert!(st.schedules["nightly"].paused);
        assert_eq!(
            st.apply(&ev(
                3,
                "schedule.timeout",
                json!({"data": {"schedule": "nightly", "stopped": true}})
            )),
            [Effect::ScheduleTimeout {
                name: "nightly".into(),
                minutes: 30,
                stopped: true
            }]
        );
        // A scheduled run's session updates the schedule, not a task row.
        let got = st.apply(&ev(4, "session.finished", json!({"session_id": "s9",
            "task_id": "schedule/nightly/2026-11-07T03:00", "data": {"summary": "Done", "cost_usd": 0.2}})));
        assert_eq!(got.len(), 1);
        let last = st.schedules["nightly"].last_run.clone().unwrap();
        assert_eq!(
            (last.status.as_str(), last.summary.as_str()),
            (FINISHED, "Done")
        );
        assert_eq!(
            st.apply(&ev(
                5,
                "schedule.missed",
                json!({"data": {"schedule": "nightly", "error": "runner was down"}})
            )),
            [Effect::ScheduleMissed {
                name: "nightly".into(),
                error: "runner was down".into()
            }]
        );
    }

    #[test]
    fn old_ended_sessions_are_dropped() {
        let mut st = State::default();
        for n in 0..(MAX_SESSIONS as i64 + 5) {
            st.apply(&ev(
                n + 1,
                "session.failed",
                json!({"session_id": format!("s{n}")}),
            ));
        }
        assert_eq!(st.sessions.len(), MAX_SESSIONS);
        assert!(!st.sessions.contains_key("s0"));
    }
}
