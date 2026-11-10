//! What the pod knows of the runner: a mirror of its records (decision
//! `agents-runs`). A snapshot replaces everything; an event's record
//! replaces the stored one when its `version` is higher. The pod decides
//! nothing about a run's state: it copies it. Applying is pure and returns
//! the [`Effect`]s (notifications) owed by comparing each record before and
//! after, so the rules are tested without a runner.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cap::{Event, Kind};
use crate::text;

/// How many notified transitions [`State::notified`] remembers.
pub const MAX_NOTIFIED: usize = 200;
/// At most this many runs are kept (the oldest ended ones go first); the
/// runner's snapshot holds at most 200.
pub const MAX_RUNS: usize = 300;

/// Run states.
pub const QUEUED: &str = "queued";
pub const STARTING: &str = "starting";
pub const WORKING: &str = "working";
pub const NEEDS_YOU: &str = "needs_you";
pub const ENDED: &str = "ended";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The last event applied (the stream resumes after it).
    pub cursor: i64,
    pub runs: BTreeMap<String, Run>,
    pub sprites: BTreeMap<String, Sprite>,
    pub schedules: BTreeMap<String, Schedule>,
    pub checks: BTreeMap<String, Check>,
    /// Transitions already notified (`needs:<run>:<version>`,
    /// `ended:<run>`, `check:<id>:<version>`), newest last and at most
    /// [`MAX_NOTIFIED`], so nothing is notified twice across restarts.
    pub notified: Vec<String>,
}

/// One attempt at one piece of work on one Sprite, as the runner sent it
/// (cleaned). Empty strings and zero times stand for the runner's nulls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: String,
    pub version: i64,
    /// The Tasks row (or `schedule/<name>/<minute>`), empty for phone runs.
    pub task_id: String,
    pub title: String,
    /// `task`, `schedule` or `phone`.
    pub origin: String,
    pub workspace: String,
    pub sprite: String,
    /// [`QUEUED`], [`STARTING`], [`WORKING`], [`NEEDS_YOU`] or [`ENDED`].
    pub state: String,
    /// When `needs_you`: `permission`, `question` or `stopped`, and the text.
    pub needs_you_reason: String,
    pub needs_you_text: String,
    /// When ended: `reported`, `exited`, `stopped`, `timed_out`, `lost`,
    /// `start_failed` or `expired`.
    pub end_reason: String,
    /// The agent's report: `done`, `review`, `blocked` or `failed` (empty
    /// without one), its summary and pull request.
    pub outcome: String,
    pub outcome_summary: String,
    pub outcome_pr_url: String,
    pub error: String,
    pub asleep: bool,
    pub current_tool: String,
    pub summary: String,
    pub pr_url: String,
    /// Checked by [`text::claude_link`].
    pub claude_url: String,
    pub session_id: String,
    pub resumed_from: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Milliseconds.
    pub queued_at: i64,
    pub started_at: i64,
    pub updated_at: i64,
    pub ended_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sprite {
    pub name: String,
    pub workspace: String,
    pub version: i64,
    /// `unprovisioned`, `asleep`, `waking`, `awake`, `unreachable` or
    /// `updating`.
    pub state: String,
    pub agentd_version: String,
    /// Milliseconds.
    pub last_heartbeat_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    pub id: String,
    pub version: i64,
    pub title: String,
    /// `ok` or `warn`.
    pub status: String,
    pub detail: String,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub name: String,
    pub version: i64,
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

/// A schedule's last run: `{run_id, due_at, state, end_reason, outcome}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastRun {
    pub run_id: String,
    pub due_at: Option<i64>,
    pub state: String,
    pub end_reason: String,
    pub outcome: String,
    pub outcome_summary: String,
}

/// What applying something owes: notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A run entered `needs_you` (`key`: the transition, for dedupe).
    NeedsYou {
        run_id: String,
        title: String,
        reason: String,
        text: String,
        key: String,
    },
    /// A run ended.
    Ended { run: Box<Run> },
    /// A security check turned to `warn`.
    CheckWarn {
        id: String,
        title: String,
        detail: String,
        key: String,
    },
}

// ---------- reading the runner's JSON ----------

fn s(v: &Value, k: &str, max: usize) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(|x| text::clean(x, max))
        .unwrap_or_default()
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k)
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// A time in milliseconds (seconds are taken for small values).
fn ms(v: &Value, k: &str) -> Option<i64> {
    let n = v.get(k).and_then(Value::as_i64)?;
    Some(if n.abs() < 100_000_000_000 {
        n * 1000
    } else {
        n
    })
}

fn lines(v: &Value, k: &str) -> String {
    v.get(k)
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

fn word(v: &Value, k: &str) -> String {
    s(v, k, 40)
}

/// A run record, or `None` without a usable id.
pub fn run_of(v: &Value) -> Option<Run> {
    let id = v.get("id").and_then(Value::as_str)?;
    if !crate::cap::valid_id(id) {
        return None;
    }
    let needs = v.get("needs_you").filter(|n| n.is_object());
    let outcome = v.get("outcome").filter(|o| o.is_object());
    Some(Run {
        id: id.to_owned(),
        version: i(v, "version"),
        task_id: s(v, "task_id", 200),
        title: s(v, "title", 200),
        origin: word(v, "origin"),
        workspace: s(v, "workspace", 100),
        sprite: s(v, "sprite", 100),
        state: word(v, "state"),
        needs_you_reason: needs.map(|n| word(n, "reason")).unwrap_or_default(),
        needs_you_text: needs.map(|n| lines(n, "text")).unwrap_or_default(),
        end_reason: word(v, "end_reason"),
        outcome: outcome.map(|o| word(o, "status")).unwrap_or_default(),
        outcome_summary: outcome.map(|o| lines(o, "summary")).unwrap_or_default(),
        outcome_pr_url: outcome.map(pr).unwrap_or_default(),
        error: s(v, "error", 500),
        asleep: v.get("asleep").and_then(Value::as_bool).unwrap_or(false),
        current_tool: s(v, "current_tool", 300),
        summary: lines(v, "summary"),
        pr_url: pr(v),
        claude_url: v
            .get("claude_url")
            .and_then(Value::as_str)
            .and_then(text::claude_link)
            .unwrap_or_default(),
        session_id: s(v, "session_id", 120),
        resumed_from: s(v, "resumed_from", 120),
        cost_usd: f(v, "cost_usd"),
        input_tokens: i(v, "input_tokens"),
        output_tokens: i(v, "output_tokens"),
        queued_at: ms(v, "queued_at").unwrap_or(0),
        started_at: ms(v, "started_at").unwrap_or(0),
        updated_at: ms(v, "updated_at").unwrap_or(0),
        ended_at: ms(v, "ended_at").unwrap_or(0),
    })
}

pub fn sprite_of(v: &Value) -> Option<Sprite> {
    let name = s(v, "name", 100);
    if name.is_empty() {
        return None;
    }
    let workspace = s(v, "workspace", 100);
    Some(Sprite {
        workspace: if workspace.is_empty() {
            name.clone()
        } else {
            workspace
        },
        version: i(v, "version"),
        state: word(v, "state"),
        agentd_version: s(v, "agentd_version", 40),
        last_heartbeat_at: ms(v, "last_heartbeat_at").unwrap_or(0),
        updated_at: ms(v, "updated_at").unwrap_or(0),
        name,
    })
}

pub fn check_of(v: &Value) -> Option<Check> {
    let id = s(v, "id", 100);
    if id.is_empty() {
        return None;
    }
    Some(Check {
        version: i(v, "version"),
        title: s(v, "title", 200),
        status: s(v, "status", 20),
        detail: s(v, "detail", 500),
        checked_at: ms(v, "checked_at").unwrap_or(0),
        id,
    })
}

fn last_run_of(v: &Value) -> Option<LastRun> {
    if !v.is_object() {
        return None;
    }
    let outcome = v.get("outcome").filter(|o| o.is_object());
    Some(LastRun {
        run_id: s(v, "run_id", 120),
        due_at: ms(v, "due_at"),
        state: word(v, "state"),
        end_reason: word(v, "end_reason"),
        outcome: outcome.map(|o| word(o, "status")).unwrap_or_default(),
        outcome_summary: outcome.map(|o| lines(o, "summary")).unwrap_or_default(),
    })
}

pub fn schedule_of(v: &Value) -> Option<Schedule> {
    let name = s(v, "name", 100);
    if name.is_empty() {
        return None;
    }
    Some(Schedule {
        version: i(v, "version"),
        cron: s(v, "cron", 100),
        timezone: s(v, "timezone", 64),
        workspace: s(v, "workspace", 100),
        model: s(v, "model", 100),
        max_runtime_minutes: i(v, "max_runtime_minutes"),
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        paused: v.get("paused").and_then(Value::as_bool).unwrap_or(false),
        next_run: ms(v, "next_run"),
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

impl Run {
    /// Not ended.
    pub fn is_active(&self) -> bool {
        self.state != ENDED
    }

    /// The pull request: the outcome's, else the last one seen.
    pub fn pr(&self) -> &str {
        if self.outcome_pr_url.is_empty() {
            &self.pr_url
        } else {
            &self.outcome_pr_url
        }
    }

    /// When it last changed, for sorting newest first.
    pub fn last_at(&self) -> i64 {
        self.updated_at
            .max(self.ended_at)
            .max(self.started_at)
            .max(self.queued_at)
    }
}

impl State {
    /// Replace everything with `snap` (`GET /v2/snapshot`). Owes what each
    /// record's change from the one stored owes; a run seen for the first
    /// time owes a needs-you notification only (an ended run that is new to
    /// the pod is history, not news).
    pub fn apply_snapshot(&mut self, snap: &Value) -> Vec<Effect> {
        let mut effects = Vec::new();
        let runs: BTreeMap<String, Run> = list(snap, "runs")
            .filter_map(run_of)
            .map(|x| (x.id.clone(), x))
            .collect();
        for r in runs.values() {
            let before = self.runs.get(&r.id).cloned();
            self.run_effects(before.as_ref(), r, false, &mut effects);
        }
        let checks: BTreeMap<String, Check> = list(snap, "checks")
            .filter_map(check_of)
            .map(|x| (x.id.clone(), x))
            .collect();
        for c in checks.values() {
            let before = self.checks.get(&c.id).cloned();
            self.check_effects(before.as_ref(), c, &mut effects);
        }
        self.runs = runs;
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

    /// Apply one event: its record replaces the stored one when its version
    /// is higher (or there is none). An older or equal version changes
    /// nothing, so a replay is harmless.
    pub fn apply(&mut self, e: &Event) -> Vec<Effect> {
        self.cursor = self.cursor.max(e.seq);
        let mut out = Vec::new();
        match e.kind {
            Kind::Run => {
                let Some(r) = run_of(&e.record) else {
                    return out;
                };
                let before = self.runs.get(&r.id).cloned();
                if before.as_ref().is_some_and(|b| b.version >= r.version) {
                    return out;
                }
                self.run_effects(before.as_ref(), &r, true, &mut out);
                self.runs.insert(r.id.clone(), r);
                self.trim();
            }
            Kind::Sprite => {
                if let Some(x) = sprite_of(&e.record)
                    && self
                        .sprites
                        .get(&x.name)
                        .is_none_or(|b| b.version < x.version)
                {
                    self.sprites.insert(x.name.clone(), x);
                }
            }
            Kind::Schedule => {
                if let Some(x) = schedule_of(&e.record)
                    && self
                        .schedules
                        .get(&x.name)
                        .is_none_or(|b| b.version < x.version)
                {
                    self.schedules.insert(x.name.clone(), x);
                }
            }
            Kind::Check => {
                if let Some(c) = check_of(&e.record) {
                    let before = self.checks.get(&c.id).cloned();
                    if before.as_ref().is_none_or(|b| b.version < c.version) {
                        self.check_effects(before.as_ref(), &c, &mut out);
                        self.checks.insert(c.id.clone(), c);
                    }
                }
            }
        }
        out
    }

    /// What run `after` owes compared with `before`. `news`: it came on the
    /// stream (a run new to the pod that already ended is still news).
    fn run_effects(
        &mut self,
        before: Option<&Run>,
        after: &Run,
        news: bool,
        out: &mut Vec<Effect>,
    ) {
        let was = before.map(|b| b.state.as_str());
        if after.state == NEEDS_YOU && was != Some(NEEDS_YOU) {
            let key = format!("needs:{}:{}", after.id, after.version);
            if self.first_notice(&key) {
                out.push(Effect::NeedsYou {
                    run_id: after.id.clone(),
                    title: after.title.clone(),
                    reason: after.needs_you_reason.clone(),
                    text: after.needs_you_text.clone(),
                    key,
                });
            }
        }
        let ended_now = after.state == ENDED && was != Some(ENDED) && (was.is_some() || news);
        if ended_now && self.first_notice(&format!("ended:{}", after.id)) {
            out.push(Effect::Ended {
                run: Box::new(after.clone()),
            });
        }
    }

    fn check_effects(&mut self, before: Option<&Check>, after: &Check, out: &mut Vec<Effect>) {
        let was = before.map(|b| b.status.as_str());
        if after.status == "warn" && was != Some("warn") {
            let key = format!("check:{}:{}", after.id, after.version);
            if self.first_notice(&key) {
                out.push(Effect::CheckWarn {
                    id: after.id.clone(),
                    title: after.title.clone(),
                    detail: after.detail.clone(),
                    key,
                });
            }
        }
    }

    /// Whether `key` hasn't been notified yet (and remember it now).
    fn first_notice(&mut self, key: &str) -> bool {
        if self.notified.iter().any(|k| k == key) {
            return false;
        }
        self.notified.push(key.to_owned());
        if self.notified.len() > MAX_NOTIFIED {
            let extra = self.notified.len() - MAX_NOTIFIED;
            self.notified.drain(..extra);
        }
        true
    }

    /// Keep at most [`MAX_RUNS`], dropping the oldest ended ones.
    fn trim(&mut self) {
        if self.runs.len() <= MAX_RUNS {
            return;
        }
        let mut ended: Vec<(i64, String)> = self
            .runs
            .values()
            .filter(|r| !r.is_active())
            .map(|r| (r.last_at(), r.id.clone()))
            .collect();
        ended.sort();
        let extra = self.runs.len() - MAX_RUNS;
        for (_, id) in ended.into_iter().take(extra) {
            self.runs.remove(&id);
        }
    }

    /// The current run of a task row: run `run_id` when the row names one
    /// (`None` while the pod hasn't seen it), else the newest run whose
    /// `task_id` is the row's id.
    pub fn current_run(&self, row_id: &str, run_id: &str) -> Option<&Run> {
        if !run_id.is_empty() {
            return self.runs.get(run_id);
        }
        self.runs
            .values()
            .filter(|r| r.task_id == row_id)
            .max_by_key(|r| (r.queued_at, r.last_at(), r.id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: &str, version: i64, state: &str, extra: Value) -> Value {
        let mut v = json!({"id": id, "version": version, "state": state, "title": "Fix the footer",
            "task_id": "r1", "queued_at": 1_800_000_000_000_i64});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn ev(seq: i64, kind: &str, record: Value) -> Event {
        Event::parse(&json!({"v": 2, "seq": seq, "ts": seq, "type": kind, "record": record}))
            .unwrap()
    }

    #[test]
    fn a_record_with_a_higher_version_replaces_the_stored_one() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 5, "runs": [run("run_1", 2, "working", json!({}))]}));
        assert_eq!(st.cursor, 5);
        // An older version (a replay, or a late copy) changes nothing.
        assert!(
            st.apply(&ev(6, "run.updated", run("run_1", 1, "queued", json!({}))))
                .is_empty()
        );
        assert_eq!(st.runs["run_1"].state, WORKING);
        assert_eq!(st.cursor, 6);
        let got = st.apply(&ev(
            7,
            "run.updated",
            run(
                "run_1",
                3,
                "needs_you",
                json!({"needs_you": {"reason": "question",
                "text": "Open the PR now?"}}),
            ),
        ));
        assert_eq!(
            got,
            [Effect::NeedsYou {
                run_id: "run_1".into(),
                title: "Fix the footer".into(),
                reason: "question".into(),
                text: "Open the PR now?".into(),
                key: "needs:run_1:3".into(),
            }]
        );
        // Still waiting (a new version, e.g. asleep): nothing more.
        assert!(
            st.apply(&ev(
                8,
                "run.updated",
                run("run_1", 4, "needs_you", json!({"asleep": true}))
            ))
            .is_empty()
        );
        assert!(st.runs["run_1"].asleep);
        // Answered, then waiting again: a second notice.
        st.apply(&ev(9, "run.updated", run("run_1", 5, "working", json!({}))));
        assert_eq!(
            st.apply(&ev(
                10,
                "run.updated",
                run("run_1", 6, "needs_you", json!({}))
            ))
            .len(),
            1
        );
        // Ending: once, whatever repeats it.
        let end = run(
            "run_1",
            7,
            "ended",
            json!({"end_reason": "reported",
            "outcome": {"status": "review", "summary": "Opened a PR.",
            "pr_url": "https://github.com/sample-owner/sample-repo/pull/4"}}),
        );
        let got = st.apply(&ev(11, "run.updated", end.clone()));
        assert!(
            matches!(&got[..], [Effect::Ended { run }] if run.outcome == "review"
            && run.pr() == "https://github.com/sample-owner/sample-repo/pull/4")
        );
        let mut again = end;
        again["version"] = json!(8);
        assert!(st.apply(&ev(12, "run.updated", again)).is_empty());
    }

    #[test]
    fn a_snapshot_owes_only_the_changes_it_shows() {
        let mut st = State::default();
        let snap = json!({"cursor": 3, "runs": [
            run("run_old", 4, "ended", json!({"end_reason": "reported", "outcome": {"status": "done"}})),
            run("run_wait", 2, "needs_you", json!({"needs_you": {"reason": "permission", "text": "Bash: git push"}})),
            run("run_work", 2, "working", json!({}))],
            "checks": [{"id": "egress", "status": "warn", "title": "Egress", "version": 1}]});
        let got = st.apply_snapshot(&snap);
        // A run ended before the pod saw it is history; one waiting is news.
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(matches!(&got[0], Effect::NeedsYou { run_id, .. } if run_id == "run_wait"));
        assert!(matches!(&got[1], Effect::CheckWarn { .. }));
        // The same snapshot again owes nothing.
        assert!(st.apply_snapshot(&snap).is_empty());
        // A run that ended while the pod was away is news.
        let snap = json!({"cursor": 9, "runs": [
            run("run_work", 5, "ended", json!({"end_reason": "lost", "error": "the process is gone"}))]});
        let got = st.apply_snapshot(&snap);
        assert!(matches!(&got[..], [Effect::Ended { run }] if run.end_reason == "lost"));
        // Replaced: runs the runner no longer lists are gone.
        assert_eq!(st.runs.len(), 1);
        assert!(st.checks.is_empty());
    }

    #[test]
    fn notices_are_remembered_across_restarts() {
        let mut st = State::default();
        st.apply(&ev(
            1,
            "run.updated",
            run("run_1", 1, "needs_you", json!({})),
        ));
        let mut back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        // The pod lost the record (e.g. a re-sign-in kept the memory) but
        // not the notice.
        back.runs.clear();
        assert!(
            back.apply(&ev(
                2,
                "run.updated",
                run("run_1", 1, "needs_you", json!({}))
            ))
            .is_empty()
        );
        // A state stored before a field existed still reads.
        let old: State = serde_json::from_value(json!({"cursor": 3})).unwrap();
        assert!(old.runs.is_empty());
        for n in 0..(MAX_NOTIFIED as i64 + 10) {
            back.apply(&ev(
                10 + n,
                "run.updated",
                run(&format!("run_{n}x"), 1, "needs_you", json!({})),
            ));
        }
        assert_eq!(back.notified.len(), MAX_NOTIFIED);
    }

    #[test]
    fn runner_text_is_cleaned_and_links_checked() {
        let r = run_of(
            &json!({"id": "run_1", "title": "a\u{7}b", "pr_url": "https://evil.example/pull/1",
            "claude_url": "https://claude.ai/code/session_01Ab", "outcome": {"status": "done",
            "pr_url": "https://github.com/o/r/pull/2"}}),
        )
        .unwrap();
        assert_eq!(r.title, "ab");
        assert_eq!(r.pr_url, "");
        assert_eq!(r.pr(), "https://github.com/o/r/pull/2");
        assert_eq!(r.claude_url, "https://claude.ai/code/session_01Ab");
        assert!(run_of(&json!({"id": "../etc"})).is_none());
        assert!(run_of(&json!({"title": "no id"})).is_none());
    }

    #[test]
    fn sprites_schedules_and_checks_keep_the_newest_version() {
        let mut st = State::default();
        st.apply(&ev(
            1,
            "sprite.updated",
            json!({"name": "box-1", "version": 2, "state": "awake"}),
        ));
        st.apply(&ev(
            2,
            "sprite.updated",
            json!({"name": "box-1", "version": 1, "state": "asleep"}),
        ));
        assert_eq!(st.sprites["box-1"].state, "awake");
        assert_eq!(st.sprites["box-1"].workspace, "box-1");
        st.apply(&ev(
            3,
            "schedule.updated",
            json!({"name": "nightly", "version": 1, "paused": true,
            "last_run": {"run_id": "run_9", "state": "ended", "end_reason": "expired"}}),
        ));
        let last = st.schedules["nightly"].last_run.clone().unwrap();
        assert_eq!(
            (last.run_id.as_str(), last.end_reason.as_str()),
            ("run_9", "expired")
        );
        let warn = |v: i64| {
            ev(
                4 + v,
                "check.updated",
                json!({"id": "egress", "version": v, "status": "warn"}),
            )
        };
        assert_eq!(st.apply(&warn(1)).len(), 1);
        assert!(st.apply(&warn(1)).is_empty());
        assert!(st.apply(&warn(2)).is_empty());
    }

    #[test]
    fn a_tasks_current_run() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"runs": [
            run("run_a", 1, "ended", json!({"queued_at": 1_000})),
            run("run_b", 1, "working", json!({"queued_at": 2_000})),
            run("run_c", 1, "working", json!({"task_id": "r2"}))]}));
        assert_eq!(st.current_run("r1", "").unwrap().id, "run_b");
        assert_eq!(st.current_run("r1", "run_a").unwrap().id, "run_a");
        // A run the row names that the pod hasn't seen yet: none.
        assert!(st.current_run("r1", "run_new").is_none());
        assert!(st.current_run("r9", "").is_none());
    }
}
