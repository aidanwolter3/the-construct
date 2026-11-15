//! What the pod knows of the runner: a mirror of its records (decision
//! `agents-cap-v3`). Every record carries a `version`; the pod keeps the
//! higher one, from a snapshot or from an event, so a replay or a late copy
//! changes nothing. The pod decides nothing about a run's execution or its
//! outcome: it copies them. Applying is pure and returns the [`Effect`]s
//! (notifications) owed by comparing each record before and after, so the
//! rules are tested without a runner.
//!
//! A run's **execution** (`state`) and its **outcome** (`outcome.state`,
//! verified by the runner against GitHub) are separate: an ended run's
//! outcome keeps changing until it is `done` or `failed`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cap::{Event, Kind};
use crate::text;

/// How many notified transitions [`State::notified`] remembers.
pub const MAX_NOTIFIED: usize = 400;
/// At most this many runs are kept (the oldest settled ones go first).
pub const MAX_RUNS: usize = 300;
/// An ended run whose outcome is settled is dropped this long after it
/// ended, as the runner's snapshot drops it (milliseconds).
pub const KEEP_ENDED_MS: i64 = 7 * 24 * 3600 * 1000;

/// Execution states.
pub const QUEUED: &str = "queued";
pub const LAUNCHING: &str = "launching";
pub const WORKING: &str = "working";
pub const NEEDS_YOU: &str = "needs_you";
pub const PARKED: &str = "parked";
pub const STALLED: &str = "stalled";
pub const INTERRUPTED: &str = "interrupted";
pub const STOPPING: &str = "stopping";
pub const CLOSING: &str = "closing";
pub const ENDED: &str = "ended";

/// Outcome states.
pub const OUTCOME_NONE: &str = "none";
pub const OUTCOME_PENDING: &str = "pending";
pub const OUTCOME_DONE: &str = "done";
pub const OUTCOME_FAILED: &str = "failed";
pub const OUTCOME_NEEDS_REVIEW: &str = "needs_review";

/// End reasons that read as a failure when the outcome says nothing more.
pub const BAD_ENDS: [&str; 4] = ["lost", "start_failed", "timed_out", "expired"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The last event applied: catching up and the stream resume after it.
    /// Stored in its own column (`agents_state.cursor`).
    pub cursor: i64,
    pub runs: BTreeMap<String, Run>,
    pub sprites: BTreeMap<String, Sprite>,
    pub checks: BTreeMap<String, Check>,
    /// Transitions already notified (`needs:<run>:<version>`,
    /// `ended:<run>`, `done:<run>`, `sprite_down:<name>:<version>`, ...),
    /// newest last and at most [`MAX_NOTIFIED`], so nothing is notified
    /// twice across restarts, replays or snapshots.
    pub notified: Vec<String>,
}

/// A run's claim: what the agent said with `foreman report`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Claim {
    /// `done`, `review`, `blocked` or `failed`.
    pub status: String,
    pub summary: String,
    pub pr_url: String,
    pub follow_up: String,
}

/// A run's outcome, owned by the runner (verified against GitHub).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outcome {
    /// `none`, `pending`, `done`, `failed` or `needs_review` (empty: none).
    pub state: String,
    /// When pending: `ci`, `review` or `deploy`.
    pub stage: String,
    /// When failed: `ci`, `closed_unmerged`, `deploy`, `blocked` or `failed`.
    pub reason: String,
    /// Checked by [`text::pr_url`].
    pub pr_url: String,
    /// Checked by [`text::deploy_url`].
    pub deploy_url: String,
    pub claim_mismatch: bool,
    pub reviewed: bool,
    pub deadline_at: i64,
    pub updated_at: i64,
}

/// One attempt at one piece of work on one Sprite, as the runner sent it
/// (cleaned). Empty strings and zero times stand for the runner's nulls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: String,
    pub version: i64,
    pub request_id: String,
    /// The pod's work item: a Tasks row's id or a schedule's id; empty for
    /// phone runs.
    pub work_item_id: String,
    /// A schedule's occurrence (RFC 3339 with its offset).
    pub occurrence: String,
    /// `task`, `schedule` or `phone`.
    pub origin: String,
    pub title: String,
    pub workspace: String,
    pub sprite: String,
    pub model: String,
    pub max_runtime_min: i64,
    /// An execution state ([`QUEUED`] ... [`ENDED`]).
    pub state: String,
    pub state_detail: String,
    /// When queued: `waiting_for_slot`, `sprite_waking`, `sprite_degraded`
    /// or `sprite_down`.
    pub queue_reason: String,
    /// When needs_you or parked: `permission`, `question` or `stopped`,
    /// and the text.
    pub needs_you_reason: String,
    pub needs_you_text: String,
    /// When ended: `reported`, `stopped`, `timed_out`, `lost`,
    /// `start_failed` or `expired`.
    pub end_reason: String,
    pub error: String,
    pub deadline_at: i64,
    pub attempt: i64,
    pub max_attempts: i64,
    pub current_tool: String,
    pub summary: String,
    pub claim: Option<Claim>,
    pub outcome: Outcome,
    /// Checked by [`text::claude_link`].
    pub claude_url: String,
    pub session_id: String,
    pub resumed_from: String,
    pub branch: String,
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
    /// `unprovisioned`, `provisioning`, `asleep`, `waking`, `awake`,
    /// `degraded`, `suspended`, `down` or `updating`.
    pub state: String,
    /// Why degraded, suspended or down.
    pub reason: String,
    pub slots: i64,
    pub slots_used: i64,
    pub preflight_ok: bool,
    /// `storage`, `disk`, `memory`, `agentd`, `sccache`, `git`.
    pub preflight_failures: Vec<String>,
    /// Degraded-for-disk episodes in the last 7 days.
    pub disk_failures_7d: i64,
    pub agentd_version: String,
    /// Milliseconds.
    pub last_heartbeat_at: i64,
    pub deadline_at: i64,
    pub updated_at: i64,
}

/// A second disk episode within 7 days: the Sprite likely needs replacing.
pub const REPEAT_DISK_FAILURES: i64 = 2;

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

/// What applying something owes: notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A run started waiting for you (`key`: the transition, for dedupe).
    NeedsYou { run: Box<Run>, key: String },
    /// A run's outcome still waits for your review after 24 h: inbox only.
    ReviewReminder { run: Box<Run>, key: String },
    /// A run's outcome turned done.
    Done { run: Box<Run>, key: String },
    /// A run ended needing your review (no claim, no pull request).
    NeedsReview { run: Box<Run>, key: String },
    /// A run went wrong: ended lost, didn't start, timed out or expired, or
    /// its outcome failed.
    RunWarning { run: Box<Run>, key: String },
    /// A Sprite went down, or failed its disk again within 7 days.
    SpriteWarning {
        sprite: Box<Sprite>,
        disk: bool,
        key: String,
    },
    /// A security check turned to `warn`.
    CheckWarn {
        id: String,
        title: String,
        detail: String,
        key: String,
    },
}

impl Effect {
    /// The run this effect is about, if any.
    pub fn run(&self) -> Option<&Run> {
        match self {
            Self::NeedsYou { run, .. }
            | Self::ReviewReminder { run, .. }
            | Self::Done { run, .. }
            | Self::NeedsReview { run, .. }
            | Self::RunWarning { run, .. } => Some(run),
            _ => None,
        }
    }
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

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// A time in milliseconds (seconds are taken for small values).
fn ms(v: &Value, k: &str) -> i64 {
    let Some(n) = v.get(k).and_then(Value::as_i64) else {
        return 0;
    };
    if n.abs() < 100_000_000_000 {
        n * 1000
    } else {
        n
    }
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

fn claim_of(v: &Value) -> Option<Claim> {
    let c = v.get("claim").filter(|c| c.is_object())?;
    Some(Claim {
        status: word(c, "status"),
        summary: lines(c, "summary"),
        pr_url: pr(c),
        follow_up: lines(c, "follow_up"),
    })
}

fn outcome_of(v: &Value) -> Outcome {
    let Some(o) = v.get("outcome").filter(|o| o.is_object()) else {
        return Outcome::default();
    };
    Outcome {
        state: word(o, "state"),
        stage: word(o, "stage"),
        reason: word(o, "reason"),
        pr_url: pr(o),
        deploy_url: o
            .get("deploy_url")
            .and_then(Value::as_str)
            .and_then(text::deploy_url)
            .unwrap_or_default(),
        claim_mismatch: b(o, "claim_mismatch"),
        reviewed: b(o, "reviewed"),
        deadline_at: ms(o, "deadline_at"),
        updated_at: ms(o, "updated_at"),
    }
}

/// A run record, or `None` without a usable id.
pub fn run_of(v: &Value) -> Option<Run> {
    let id = v.get("id").and_then(Value::as_str)?;
    if !crate::cap::valid_id(id) {
        return None;
    }
    let needs = v.get("needs_you").filter(|n| n.is_object());
    Some(Run {
        id: id.to_owned(),
        version: i(v, "version"),
        request_id: s(v, "request_id", 120),
        work_item_id: s(v, "work_item_id", 200),
        occurrence: s(v, "occurrence", 40),
        origin: word(v, "origin"),
        title: s(v, "title", 200),
        workspace: s(v, "workspace", 100),
        sprite: s(v, "sprite", 100),
        model: s(v, "model", 100),
        max_runtime_min: i(v, "max_runtime_min"),
        state: word(v, "state"),
        state_detail: s(v, "state_detail", 300),
        queue_reason: word(v, "queue_reason"),
        needs_you_reason: needs.map(|n| word(n, "reason")).unwrap_or_default(),
        needs_you_text: needs.map(|n| lines(n, "text")).unwrap_or_default(),
        end_reason: word(v, "end_reason"),
        error: s(v, "error", 500),
        deadline_at: ms(v, "deadline_at"),
        attempt: i(v, "attempt"),
        max_attempts: i(v, "max_attempts"),
        current_tool: s(v, "current_tool", 300),
        summary: lines(v, "summary"),
        claim: claim_of(v),
        outcome: outcome_of(v),
        claude_url: v
            .get("claude_url")
            .and_then(Value::as_str)
            .and_then(text::claude_link)
            .unwrap_or_default(),
        session_id: s(v, "session_id", 120),
        resumed_from: s(v, "resumed_from", 120),
        branch: s(v, "branch", 200),
        cost_usd: f(v, "cost_usd"),
        input_tokens: i(v, "input_tokens"),
        output_tokens: i(v, "output_tokens"),
        queued_at: ms(v, "queued_at"),
        started_at: ms(v, "started_at"),
        updated_at: ms(v, "updated_at"),
        ended_at: ms(v, "ended_at"),
    })
}

pub fn sprite_of(v: &Value) -> Option<Sprite> {
    let name = s(v, "name", 100);
    if name.is_empty() {
        return None;
    }
    let workspace = s(v, "workspace", 100);
    let preflight = v.get("preflight").filter(|p| p.is_object());
    Some(Sprite {
        workspace: if workspace.is_empty() {
            name.clone()
        } else {
            workspace
        },
        version: i(v, "version"),
        state: word(v, "state"),
        reason: s(v, "reason", 300),
        slots: i(v, "slots"),
        slots_used: i(v, "slots_used"),
        preflight_ok: preflight.is_none_or(|p| p.get("ok").and_then(Value::as_bool) != Some(false)),
        preflight_failures: preflight
            .and_then(|p| p.get("failures"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|x| text::clean(x, 40))
            .filter(|x| !x.is_empty())
            .take(10)
            .collect(),
        disk_failures_7d: i(v, "disk_failures_7d"),
        agentd_version: s(v, "agentd_version", 40),
        last_heartbeat_at: ms(v, "last_heartbeat_at"),
        deadline_at: ms(v, "deadline_at"),
        updated_at: ms(v, "updated_at"),
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
        checked_at: ms(v, "checked_at"),
        id,
    })
}

fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

// ---------- reading a run ----------

impl Run {
    /// Not ended.
    pub fn is_active(&self) -> bool {
        self.state != ENDED
    }

    /// Waiting for you: `needs_you`, or `parked` (its lease dropped).
    pub fn waits_for_you(&self) -> bool {
        matches!(self.state.as_str(), NEEDS_YOU | PARKED)
    }

    /// The outcome's state, `none` when the runner sent none.
    pub fn outcome_state(&self) -> &str {
        if self.outcome.state.is_empty() {
            OUTCOME_NONE
        } else {
            &self.outcome.state
        }
    }

    /// Ended and waiting for a person's review.
    pub fn needs_review(&self) -> bool {
        !self.is_active() && self.outcome_state() == OUTCOME_NEEDS_REVIEW
    }

    /// Went wrong: its outcome failed, or it ended lost, unstarted, timed
    /// out or expired with no outcome that says otherwise.
    pub fn failed(&self) -> bool {
        match self.outcome_state() {
            OUTCOME_FAILED => true,
            OUTCOME_NONE => !self.is_active() && BAD_ENDS.contains(&self.end_reason.as_str()),
            _ => false,
        }
    }

    /// The pull request: the outcome's (GitHub's), else the claim's.
    pub fn pr(&self) -> &str {
        if !self.outcome.pr_url.is_empty() {
            &self.outcome.pr_url
        } else {
            self.claim.as_ref().map_or("", |c| c.pr_url.as_str())
        }
    }

    /// What it did, in its own words: the claim's summary, else the last
    /// thing Claude said.
    pub fn said(&self) -> &str {
        match &self.claim {
            Some(c) if !c.summary.is_empty() => &c.summary,
            _ => &self.summary,
        }
    }

    /// When it last changed, for sorting newest first.
    pub fn last_at(&self) -> i64 {
        self.updated_at
            .max(self.ended_at)
            .max(self.started_at)
            .max(self.queued_at)
            .max(self.outcome.updated_at)
    }

    /// Whether its outcome is settled (`done` or `failed`, or nothing left
    /// to wait for).
    fn settled(&self) -> bool {
        !self.is_active() && !matches!(self.outcome_state(), OUTCOME_PENDING | OUTCOME_NEEDS_REVIEW)
    }
}

// ---------- applying ----------

impl State {
    /// Take `snap` (`GET /v3/snapshot`): it lists every run that matters
    /// now, so runs it doesn't list are dropped; each record listed is kept
    /// at the higher version of the stored and the sent one. Owes what each
    /// record's change from the stored one owes; a record seen for the
    /// first time owes only a needs-you notice (an ended run new to the pod
    /// is history, not news).
    pub fn apply_snapshot(&mut self, snap: &Value) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut runs = BTreeMap::new();
        for r in list(snap, "runs").filter_map(run_of) {
            let before = self.runs.get(&r.id).cloned();
            match before {
                Some(b) if b.version >= r.version => {
                    runs.insert(b.id.clone(), b);
                }
                _ => {
                    self.run_effects(before.as_ref(), &r, false, false, &mut effects);
                    runs.insert(r.id.clone(), r);
                }
            }
        }
        let mut checks = BTreeMap::new();
        for c in list(snap, "checks").filter_map(check_of) {
            let before = self.checks.get(&c.id).cloned();
            match before {
                Some(b) if b.version >= c.version => {
                    checks.insert(b.id.clone(), b);
                }
                _ => {
                    self.check_effects(before.as_ref(), &c, &mut effects);
                    checks.insert(c.id.clone(), c);
                }
            }
        }
        let mut sprites = BTreeMap::new();
        for x in list(snap, "sprites").filter_map(sprite_of) {
            let before = self.sprites.get(&x.name).cloned();
            match before {
                Some(b) if b.version >= x.version => {
                    sprites.insert(b.name.clone(), b);
                }
                _ => {
                    self.sprite_effects(before.as_ref(), &x, false, &mut effects);
                    sprites.insert(x.name.clone(), x);
                }
            }
        }
        self.runs = runs;
        self.checks = checks;
        self.sprites = sprites;
        if let Some(c) = snap.get("cursor").and_then(Value::as_i64) {
            self.cursor = c;
        }
        // The runner's snapshot drops old runs itself: none by age here.
        self.trim(i64::MIN);
        effects
    }

    /// Apply events in log order (a page of `/v3/events`, or what the
    /// stream sent): each record replaces the stored one when its version
    /// is higher (or there is none), so a replay is harmless. A run new to
    /// the pod that already ended is news here: it started and ended while
    /// the pod was away. A needs-you notice whose run no longer waits by
    /// the end of the batch is dropped (it was answered meanwhile). `now`
    /// (milliseconds) trims runs long settled.
    pub fn apply_events(&mut self, events: &[Event], now: i64) -> Vec<Effect> {
        let mut out = Vec::new();
        for e in events {
            self.cursor = self.cursor.max(e.seq);
            match e.kind {
                Kind::Run => {
                    let Some(r) = run_of(&e.record) else { continue };
                    let before = self.runs.get(&r.id).cloned();
                    if before.as_ref().is_some_and(|b| b.version >= r.version) {
                        // An older or equal copy; a reminder rides on it
                        // all the same.
                        if e.reminder
                            && let Some(b) = before
                        {
                            self.reminder(&b, &mut out);
                        }
                        continue;
                    }
                    self.run_effects(before.as_ref(), &r, true, e.reminder, &mut out);
                    self.runs.insert(r.id.clone(), r);
                }
                Kind::Sprite => {
                    let Some(x) = sprite_of(&e.record) else {
                        continue;
                    };
                    let before = self.sprites.get(&x.name).cloned();
                    if before.as_ref().is_none_or(|b| b.version < x.version) {
                        self.sprite_effects(before.as_ref(), &x, true, &mut out);
                        self.sprites.insert(x.name.clone(), x);
                    }
                }
                Kind::Check => {
                    let Some(c) = check_of(&e.record) else {
                        continue;
                    };
                    let before = self.checks.get(&c.id).cloned();
                    if before.as_ref().is_none_or(|b| b.version < c.version) {
                        self.check_effects(before.as_ref(), &c, &mut out);
                        self.checks.insert(c.id.clone(), c);
                    }
                }
            }
        }
        out.retain(|e| match e {
            Effect::NeedsYou { run, .. } => self.runs.get(&run.id).is_some_and(Run::waits_for_you),
            _ => true,
        });
        self.trim(now);
        out
    }

    /// Apply one run record the runner answered a command with.
    pub fn apply_run(&mut self, record: &Value) -> Vec<Effect> {
        let Some(r) = run_of(record) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let before = self.runs.get(&r.id).cloned();
        if before.as_ref().is_none_or(|b| b.version < r.version) {
            self.run_effects(before.as_ref(), &r, true, false, &mut out);
            self.runs.insert(r.id.clone(), r);
        }
        out
    }

    fn reminder(&mut self, r: &Run, out: &mut Vec<Effect>) {
        if !r.needs_review() && r.outcome_state() != OUTCOME_PENDING {
            return;
        }
        let key = format!("reminder:{}:{}", r.id, r.version);
        if self.first_notice(&key) {
            out.push(Effect::ReviewReminder {
                run: Box::new(r.clone()),
                key,
            });
        }
    }

    /// What run `after` owes compared with `before`. `news`: it came as an
    /// event (a run new to the pod that already ended is still news).
    fn run_effects(
        &mut self,
        before: Option<&Run>,
        after: &Run,
        news: bool,
        reminder: bool,
        out: &mut Vec<Effect>,
    ) {
        let boxed = || Box::new(after.clone());
        let waited = before.is_some_and(Run::waits_for_you);
        if after.waits_for_you() && !waited {
            let key = format!("needs:{}:{}", after.id, after.version);
            if self.first_notice(&key) {
                out.push(Effect::NeedsYou { run: boxed(), key });
            }
        }
        if reminder {
            self.reminder(after, out);
        }
        // An ended run first seen in a snapshot is history.
        if before.is_none() && !news && !after.is_active() {
            return;
        }
        let was_failed = before.is_some_and(Run::failed);
        if after.failed() && !was_failed {
            // `ended:` keeps the key a v2 pod used for a bad ending.
            let key = if after.outcome_state() == OUTCOME_FAILED {
                format!("failed:{}", after.id)
            } else {
                format!("ended:{}", after.id)
            };
            if self.first_notice(&key) {
                out.push(Effect::RunWarning { run: boxed(), key });
            }
        }
        let was = |s: &str| before.is_some_and(|b| b.outcome_state() == s);
        if after.outcome_state() == OUTCOME_DONE && !was(OUTCOME_DONE) && !after.outcome.reviewed {
            let key = format!("done:{}", after.id);
            if self.first_notice(&key) {
                out.push(Effect::Done { run: boxed(), key });
            }
        }
        if after.needs_review() && !before.is_some_and(Run::needs_review) {
            let key = format!("review:{}", after.id);
            if self.first_notice(&key) {
                out.push(Effect::NeedsReview { run: boxed(), key });
            }
        }
    }

    fn sprite_effects(
        &mut self,
        before: Option<&Sprite>,
        after: &Sprite,
        news: bool,
        out: &mut Vec<Effect>,
    ) {
        if before.is_none() && !news {
            return;
        }
        if after.state == "down" && before.is_none_or(|b| b.state != "down") {
            let key = format!("sprite_down:{}:{}", after.name, after.version);
            if self.first_notice(&key) {
                out.push(Effect::SpriteWarning {
                    sprite: Box::new(after.clone()),
                    disk: false,
                    key,
                });
            }
        }
        let disk = |s: &Sprite| s.disk_failures_7d >= REPEAT_DISK_FAILURES;
        if disk(after) && !before.is_some_and(disk) {
            let key = format!("sprite_disk:{}:{}", after.name, after.version);
            if self.first_notice(&key) {
                out.push(Effect::SpriteWarning {
                    sprite: Box::new(after.clone()),
                    disk: true,
                    key,
                });
            }
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

    /// Drop settled runs that ended over [`KEEP_ENDED_MS`] before `now`,
    /// then keep at most [`MAX_RUNS`], dropping the oldest settled ones.
    fn trim(&mut self, now: i64) {
        self.runs.retain(|_, r| {
            !(r.settled() && r.ended_at > 0 && now.saturating_sub(r.ended_at) > KEEP_ENDED_MS)
        });
        if self.runs.len() <= MAX_RUNS {
            return;
        }
        let mut old: Vec<(i64, String)> = self
            .runs
            .values()
            .filter(|r| r.settled())
            .map(|r| (r.last_at(), r.id.clone()))
            .collect();
        old.sort();
        let extra = self.runs.len() - MAX_RUNS;
        for (_, id) in old.into_iter().take(extra) {
            self.runs.remove(&id);
        }
    }

    /// The current run of a work item: run `run_id` when the item names one
    /// (`None` while the pod hasn't seen it), else the newest run whose
    /// `work_item_id` is the item's id.
    pub fn current_run(&self, item_id: &str, run_id: &str) -> Option<&Run> {
        if !run_id.is_empty() {
            return self.runs.get(run_id);
        }
        self.latest_run(item_id)
    }

    /// The newest run of work item `item_id`.
    pub fn latest_run(&self, item_id: &str) -> Option<&Run> {
        self.runs
            .values()
            .filter(|r| r.work_item_id == item_id)
            .max_by_key(|r| (r.queued_at, r.last_at(), r.id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: &str, version: i64, state: &str, extra: Value) -> Value {
        let mut v = json!({"id": id, "version": version, "state": state, "title": "Fix the footer",
            "work_item_id": "r1", "queued_at": 1_800_000_000_000_i64});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn ev(seq: i64, kind: &str, record: Value) -> Event {
        Event::parse(&json!({"v": 3, "seq": seq, "ts": seq, "type": kind, "record": record}))
            .unwrap()
    }

    fn kinds(effects: &[Effect]) -> Vec<&'static str> {
        effects
            .iter()
            .map(|e| match e {
                Effect::NeedsYou { .. } => "needs_you",
                Effect::ReviewReminder { .. } => "reminder",
                Effect::Done { .. } => "done",
                Effect::NeedsReview { .. } => "review",
                Effect::RunWarning { .. } => "warning",
                Effect::SpriteWarning { .. } => "sprite",
                Effect::CheckWarn { .. } => "check",
            })
            .collect()
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn a_record_with_a_higher_version_replaces_the_stored_one() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 5, "runs": [run("run_1", 2, "working", json!({}))]}));
        assert_eq!(st.cursor, 5);
        // An older version (a replay, or a late copy) changes nothing.
        let old = ev(6, "run.updated", run("run_1", 1, "queued", json!({})));
        assert!(st.apply_events(&[old], NOW).is_empty());
        assert_eq!(st.runs["run_1"].state, WORKING);
        assert_eq!(st.cursor, 6);
        let ask = run(
            "run_1",
            3,
            "needs_you",
            json!({"needs_you": {"reason": "question", "text": "Open the PR now?"}}),
        );
        let got = st.apply_events(&[ev(7, "run.updated", ask)], NOW);
        assert!(
            matches!(&got[..], [Effect::NeedsYou { run, key }] if key == "needs:run_1:3"
                && run.needs_you_text == "Open the PR now?")
        );
        // Parked after 30 min: still the same wait, nothing more.
        let parked = ev(8, "run.updated", run("run_1", 4, "parked", json!({})));
        assert!(st.apply_events(&[parked], NOW).is_empty());
        // Answered, then waiting again: a second notice.
        let back = [
            ev(9, "run.updated", run("run_1", 5, "working", json!({}))),
            ev(10, "run.updated", run("run_1", 6, "needs_you", json!({}))),
        ];
        assert_eq!(kinds(&st.apply_events(&back, NOW)), ["needs_you"]);
    }

    #[test]
    fn outcomes_notify_as_they_settle() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 1, "runs": [run("run_1", 1, "working", json!({}))]}));
        let pr = "https://github.com/sample-owner/sample-repo/pull/4";
        let steps = [
            json!({"state": "closing", "claim": {"status": "done", "summary": "Fixed.", "pr_url": pr}}),
            json!({"state": "ended", "end_reason": "reported", "outcome": {"state": "pending", "stage": "ci", "pr_url": pr}}),
            json!({"state": "ended", "outcome": {"state": "pending", "stage": "deploy", "pr_url": pr}}),
            json!({"state": "ended", "outcome": {"state": "done", "pr_url": pr}}),
            json!({"state": "ended", "outcome": {"state": "done", "pr_url": pr}, "cost_usd": 1.0}),
        ];
        let mut got = Vec::new();
        // Records are whole: each step is the last one changed.
        let mut r = run("run_1", 1, "working", json!({}));
        for (n, fields) in steps.iter().enumerate() {
            r["version"] = json!(2 + n as i64);
            for (k, v) in fields.as_object().unwrap() {
                r[k] = v.clone();
            }
            got.extend(st.apply_events(&[ev(2 + n as i64, "run.updated", r.clone())], NOW));
        }
        assert_eq!(kinds(&got), ["done"]);
        assert_eq!(got[0].run().unwrap().pr(), pr);
        assert_eq!(got[0].run().unwrap().said(), "Fixed.");
        // Failing outcomes and bad endings warn, once each.
        let fail = run(
            "run_2",
            3,
            "ended",
            json!({"end_reason": "reported", "outcome": {"state": "failed", "reason": "ci"}}),
        );
        let lost = run(
            "run_3",
            2,
            "ended",
            json!({"end_reason": "lost", "error": "gone"}),
        );
        let got = st.apply_events(
            &[
                ev(10, "run.updated", fail.clone()),
                ev(11, "run.updated", lost),
                ev(12, "run.updated", {
                    let mut f = fail;
                    f["version"] = json!(4);
                    f
                }),
            ],
            NOW,
        );
        assert_eq!(kinds(&got), ["warning", "warning"]);
        // A run lost whose PR then counts: pending, then done.
        let saved = run(
            "run_3",
            3,
            "ended",
            json!({"end_reason": "lost", "outcome": {"state": "done"}}),
        );
        assert_eq!(
            kinds(&st.apply_events(&[ev(13, "run.updated", saved)], NOW)),
            ["done"]
        );
        // Needing review notifies; marked reviewed (done, reviewed) doesn't.
        let review = run(
            "run_4",
            2,
            "ended",
            json!({"end_reason": "stopped", "outcome": {"state": "needs_review"}}),
        );
        assert_eq!(
            kinds(&st.apply_events(&[ev(14, "run.updated", review)], NOW)),
            ["review"]
        );
        let reviewed = run(
            "run_4",
            3,
            "ended",
            json!({"end_reason": "stopped", "outcome": {"state": "done", "reviewed": true}}),
        );
        assert!(
            st.apply_events(&[ev(15, "run.updated", reviewed)], NOW)
                .is_empty()
        );
    }

    #[test]
    fn a_run_that_started_and_ended_while_away_notifies_on_replay() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 3, "runs": []}));
        // Replayed from the cursor after a gap: the whole life of a run.
        let page = [
            ev(4, "run.updated", run("run_x", 1, "queued", json!({}))),
            ev(5, "run.updated", run("run_x", 2, "working", json!({}))),
            ev(6, "run.updated", run("run_x", 3, "needs_you", json!({}))),
            ev(7, "run.updated", run("run_x", 4, "working", json!({}))),
            ev(
                8,
                "run.updated",
                run(
                    "run_x",
                    5,
                    "ended",
                    json!({"end_reason": "reported", "outcome": {"state": "done"}}),
                ),
            ),
        ];
        // The question was answered meanwhile: only the ending notifies.
        let got = st.apply_events(&page, NOW);
        assert_eq!(kinds(&got), ["done"]);
        assert_eq!(st.cursor, 8);
        // The same page again (a replay after a crash): nothing.
        assert!(st.apply_events(&page, NOW).is_empty());
        // An ended run first seen in a snapshot is history.
        let mut fresh = State::default();
        let got = fresh.apply_snapshot(&json!({"cursor": 9, "runs": [
            run("run_old", 4, "ended", json!({"end_reason": "lost"})),
            run("run_wait", 2, "parked", json!({"needs_you": {"reason": "permission"}}))]}));
        assert_eq!(kinds(&got), ["needs_you"]);
    }

    #[test]
    fn a_snapshot_keeps_the_newer_record_and_drops_the_unlisted() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 3, "runs": [
            run("run_a", 5, "working", json!({})), run("run_b", 1, "working", json!({}))]}));
        let got = st.apply_snapshot(&json!({"cursor": 9, "runs": [
            run("run_a", 4, "queued", json!({})),
            run("run_b", 2, "ended", json!({"end_reason": "timed_out"}))]}));
        assert_eq!(
            st.runs["run_a"].state, WORKING,
            "the stored version is newer"
        );
        assert_eq!(kinds(&got), ["warning"]);
        let got = st.apply_snapshot(&json!({"cursor": 10, "runs": []}));
        assert!(got.is_empty() && st.runs.is_empty());
    }

    #[test]
    fn sprites_warn_when_down_or_failing_their_disk_again() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"cursor": 1, "sprites": [
            {"name": "box-1", "version": 1, "state": "down", "reason": "no heartbeat"}]}));
        let sprite = |v: i64, state: &str, disk: i64| {
            ev(
                v,
                "sprite.updated",
                json!({"name": "box-1", "version": v, "state": state,
                    "disk_failures_7d": disk, "reason": "disk error"}),
            )
        };
        // Down when first seen in a snapshot: history.
        assert!(st.sprites["box-1"].state == "down");
        let got = st.apply_events(
            &[
                sprite(2, "awake", 0),
                sprite(3, "degraded", 1),
                sprite(4, "awake", 1),
                sprite(5, "degraded", 2),
                sprite(6, "down", 2),
                sprite(6, "down", 2),
            ],
            NOW,
        );
        assert_eq!(kinds(&got), ["sprite", "sprite"]);
        assert!(matches!(&got[0], Effect::SpriteWarning { disk: true, .. }));
        assert!(matches!(&got[1], Effect::SpriteWarning { disk: false, .. }));
    }

    #[test]
    fn a_review_reminder_is_its_own_notice() {
        let mut st = State::default();
        let review = run(
            "run_r",
            2,
            "ended",
            json!({"end_reason": "reported", "outcome": {"state": "pending", "stage": "review"}}),
        );
        st.apply_snapshot(&json!({"cursor": 1, "runs": [review.clone()]}));
        let mut again = review;
        again["version"] = json!(3);
        let reminder = Event::parse(&json!({"v": 3, "seq": 2, "type": "outcome.updated",
            "reminder": true, "record": again}))
        .unwrap();
        assert_eq!(
            kinds(&st.apply_events(std::slice::from_ref(&reminder), NOW)),
            ["reminder"]
        );
        assert!(st.apply_events(&[reminder], NOW).is_empty());
    }

    #[test]
    fn notices_are_remembered_across_restarts() {
        let mut st = State::default();
        st.apply_events(
            &[ev(
                1,
                "run.updated",
                run("run_1", 1, "needs_you", json!({})),
            )],
            NOW,
        );
        let mut back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        back.runs.clear();
        assert!(
            back.apply_events(
                &[ev(
                    2,
                    "run.updated",
                    run("run_1", 1, "needs_you", json!({}))
                )],
                NOW
            )
            .is_empty()
        );
        let old: State = serde_json::from_value(json!({"cursor": 3})).unwrap();
        assert!(old.runs.is_empty());
        for n in 0..(MAX_NOTIFIED as i64 + 10) {
            back.apply_events(
                &[ev(
                    10 + n,
                    "run.updated",
                    run(&format!("run_{n}x"), 1, "needs_you", json!({})),
                )],
                NOW,
            );
        }
        assert_eq!(back.notified.len(), MAX_NOTIFIED);
    }

    #[test]
    fn runner_text_is_cleaned_and_links_checked() {
        let r = run_of(&json!({"id": "run_1", "title": "a\u{7}b",
            "claim": {"status": "done", "pr_url": "https://evil.example/pull/1"},
            "claude_url": "https://claude.ai/code/session_01Ab",
            "outcome": {"state": "pending", "pr_url": "https://github.com/o/r/pull/2",
                "deploy_url": "https://github.com/o/r/actions/runs/77"}}))
        .unwrap();
        assert_eq!(r.title, "ab");
        assert_eq!(r.claim.as_ref().unwrap().pr_url, "");
        assert_eq!(r.pr(), "https://github.com/o/r/pull/2");
        assert_eq!(
            r.outcome.deploy_url,
            "https://github.com/o/r/actions/runs/77"
        );
        assert_eq!(r.claude_url, "https://claude.ai/code/session_01Ab");
        let bad = run_of(&json!({"id": "run_1", "outcome": {
            "pr_url": "javascript:x", "deploy_url": "https://evil.example/actions/runs/1"}}))
        .unwrap();
        assert_eq!((bad.pr(), bad.outcome.deploy_url.as_str()), ("", ""));
        assert!(run_of(&json!({"id": "../etc"})).is_none());
    }

    #[test]
    fn settled_runs_are_dropped_after_a_week() {
        let mut st = State::default();
        let ended = |id: &str, outcome: &str| {
            ev(
                1,
                "run.updated",
                run(
                    id,
                    1,
                    "ended",
                    json!({"ended_at": NOW - KEEP_ENDED_MS - 1, "outcome": {"state": outcome}}),
                ),
            )
        };
        st.apply_events(
            &[ended("run_done", "done"), ended("run_wait", "needs_review")],
            NOW,
        );
        assert_eq!(st.runs.keys().collect::<Vec<_>>(), ["run_wait"]);
    }

    #[test]
    fn a_work_items_current_run() {
        let mut st = State::default();
        st.apply_snapshot(&json!({"runs": [
            run("run_a", 1, "ended", json!({"queued_at": 1_000})),
            run("run_b", 1, "working", json!({"queued_at": 2_000})),
            run("run_c", 1, "working", json!({"work_item_id": "r2"}))]}));
        assert_eq!(st.current_run("r1", "").unwrap().id, "run_b");
        assert_eq!(st.current_run("r1", "run_a").unwrap().id, "run_a");
        assert!(st.current_run("r1", "run_new").is_none());
        assert!(st.current_run("r9", "").is_none());
    }
}
