//! The `agents` data source (decisions `agents-data-source`, `agents-runs`,
//! `agents-cap-v3`): Claude Code runs started by an external **agent
//! runner** (the Foreman), shown and controlled from the pod. The pod owns
//! the work items (tasks and schedules) and the notifications; the runner
//! owns the runs, the Sprites and each run's verified outcome. The runs
//! themselves are watched and steered in the Claude app.
//!
//! The runner speaks CAP v3 ([`cap`]); the pod keeps a mirror of its
//! records ([`state`], [`db`]). The pod signs in with the runner's URL and a
//! token (`POST /v1/agents/sign-in`, from the Agents layout's form), stored
//! sealed. A background follower per signed-in pod ([`follower`]) catches
//! up from its stored cursor with the event log and follows the stream, so
//! the feeds answer while the runner is away and nothing is missed.
//!
//! - Feeds: `sessions` (`agents:status`, `agents:sessions`: the runs),
//!   `sandboxes`, `checks`, `schedules` (the pod's own, [`schedules`]); a
//!   data path for one run's log.
//! - Actions: stop a run, wake or repair a sandbox, start a run, reopen a
//!   task, mark a run reviewed; create, edit, delete, run now, pause and
//!   resume a schedule. Runner commands are a fixed set ([`cap::Command`]).
//! - Notifications ([`alerts`]) in the layout that shows the source.
//! - Tasks wiring ([`tasks`]): a linked collection's queued rows start runs,
//!   and each row's status follows its run through one table.
//! - The `agents` core layout: `layouts/agents/`.

// Route helpers hand back the refusal as an axum `Response`.
#![allow(clippy::result_large_err)]

pub mod alerts;
pub mod bundle;
pub mod cap;
pub mod cron;
pub mod db;
pub mod follower;
pub mod schedules;
pub mod state;
pub mod tasks;
pub mod text;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::session::Caller;
use pod_core::store::{Step, Store};
use pod_core::{
    AuthPolicy, BoxFuture, Catalog, CoreLayoutInfo, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    PollCtx, PollOutcome, RequestCtx, Source, SourceManifest,
};
use protocol::NotificationChannel;
use serde_json::{Value, json};

use crate::cap::{CapError, Command, Runner};

/// The adapter name and the entity scheme.
pub const NAME: &str = "agents";
/// The core layout layout.
pub const APP_ID: &str = "agents";

pub const SESSIONS_FEED: &str = "sessions";
pub const SANDBOXES_FEED: &str = "sandboxes";
pub const CHECKS_FEED: &str = "checks";
pub const SCHEDULES_FEED: &str = "schedules";

pub const STATUS_QUERY: &str = "agents:status";
pub const SESSIONS_QUERY: &str = "agents:sessions";
pub const SANDBOXES_QUERY: &str = "agents:sandboxes";
pub const CHECKS_QUERY: &str = "agents:checks";
pub const SCHEDULES_QUERY: &str = "agents:schedules";

pub const SIGN_IN_PATH: &str = "/v1/agents/sign-in";
/// The agent runner jack's id.
pub const JACK: &str = "agent_runner";
pub const SIGN_OUT_PATH: &str = "/v1/agents/sign-out";
pub const STOP_PATH: &str = "/v1/agents/sessions/stop";
pub const WAKE_PATH: &str = "/v1/agents/sandboxes/wake";
pub const REPAIR_PATH: &str = "/v1/agents/sandboxes/repair";
pub const START_PATH: &str = "/v1/agents/tasks/start";
pub const RUN_SCHEDULE_PATH: &str = "/v1/agents/schedules/run";
pub const PAUSE_SCHEDULE_PATH: &str = "/v1/agents/schedules/pause";
pub const RESUME_SCHEDULE_PATH: &str = "/v1/agents/schedules/resume";
pub const CREATE_SCHEDULE_PATH: &str = "/v1/agents/schedules/create";
pub const UPDATE_SCHEDULE_PATH: &str = "/v1/agents/schedules/update";
pub const DELETE_SCHEDULE_PATH: &str = "/v1/agents/schedules/delete";
pub const REOPEN_PATH: &str = "/v1/agents/tasks/reopen";
pub const CONTINUE_PATH: &str = "/v1/agents/sessions/continue";
pub const REVIEW_PATH: &str = "/v1/agents/sessions/review";
pub const TIMELINE_PATH: &str = "/v1/agents/sessions/timeline";

/// Minutes a Wake keeps a sandbox up unless the request says (the runner
/// takes at most [`cap::MAX_WAKE_MIN`]).
pub const WAKE_MINUTES: u32 = 30;

pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Agents",
    sign_in: None,
    entity_schemes: &[NAME],
    feeds: &[
        FeedSpec {
            name: SESSIONS_FEED,
            query_keys: &[STATUS_QUERY, SESSIONS_QUERY, "agents:schedule_runs:{name}"],
            doc: "The agent runner's runs (one attempt at one piece of work each): the ones that \
                  need you (or are parked), then running, queued, ended ones waiting on CI, review \
                  or deploy, and the rest; agents:schedule_runs:<name>, a schedule's runs with \
                  the latest started first; and agents:status: whether the pod is signed in to a \
                  runner and reaches it, with a one-line summary such as \
                  1 working · 1 needs you · 2 queued · 1 waiting.",
        },
        FeedSpec {
            name: SANDBOXES_FEED,
            query_keys: &[SANDBOXES_QUERY],
            doc: "The runner's sandboxes: down first, then degraded or suspended (with the \
                  reason), then awake; an asleep one can be woken, a down one repaired.",
        },
        FeedSpec {
            name: CHECKS_FEED,
            query_keys: &[CHECKS_QUERY],
            doc: "The runner's security checks, ok or warn, warnings first.",
        },
        FeedSpec {
            name: SCHEDULES_FEED,
            query_keys: &[SCHEDULES_QUERY],
            doc: "The pod's schedules (it sends their runs), with their next run and their last \
                  run's state and outcome; each can be run now, paused, resumed, edited or \
                  deleted, and new ones created (also with the agents_schedules tool).",
        },
    ],
    data_paths: &[PathSpec {
        pattern: "/v1/agents/sessions/timeline?id={id}",
        query_keys: &["agents:timeline:{id}"],
        doc: "One run (agents://sessions/<id>) and its log from the runner, oldest first.",
    }],
    actions: &[
        PathSpec {
            pattern: SIGN_IN_PATH,
            query_keys: &[],
            doc: "Sign the pod in to an agent runner: {url, token} (url https://, the token \
                  the runner issued). Checked with the runner first; stored sealed.",
        },
        PathSpec {
            pattern: SIGN_OUT_PATH,
            query_keys: &[],
            doc: "Forget the runner and everything it sent: {}. Wrap it in action.confirm.",
        },
        PathSpec {
            pattern: STOP_PATH,
            query_keys: &[],
            doc: "Stop a run: {run_id} (the session entity's id; session_id is taken too). \
                  Wrap it in action.confirm.",
        },
        PathSpec {
            pattern: REVIEW_PATH,
            query_keys: &[],
            doc: "Mark an ended run that needs review reviewed: {run_id}. The runner, which \
                  owns the outcome, sets it done (409 unless it needs review); a linked task \
                  row follows it to done.",
        },
        PathSpec {
            pattern: WAKE_PATH,
            query_keys: &[],
            doc: "Wake a sandbox: {workspace, minutes?} (workspace: the sandbox's workspace \
                  field; minutes 1 to 60, default 30).",
        },
        PathSpec {
            pattern: REPAIR_PATH,
            query_keys: &[],
            doc: "Repair a sandbox that is down: {workspace}. The runner sets it up again (a \
                  new agent daemon and key). Wrap it in action.confirm.",
        },
        PathSpec {
            pattern: START_PATH,
            query_keys: &[],
            doc: "Start a run: {workspace, title, prompt, task_id?, model?, max_runtime_min?}. \
                  The runner queues it; the answer has its run_id.",
        },
        PathSpec {
            pattern: REOPEN_PATH,
            query_keys: &[],
            doc: "Reopen an ended run: {id (a linked task's row) or run_id (any ended run, a \
                  task's or a schedule's), message}. A new run resumes the last one's \
                  conversation with the message as the next prompt; a task's row goes back to \
                  queued.",
        },
        PathSpec {
            pattern: CONTINUE_PATH,
            query_keys: &[],
            doc: "Continue a run that timed out: {run_id}. A new run resumes its conversation, \
                  told it ran out of time, with its schedule's (or its own) time limit; once per \
                  run (a second press answers the same run). 409 not_timed_out otherwise; show \
                  it where can_continue is true.",
        },
        PathSpec {
            pattern: RUN_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Run a schedule now: {name}.",
        },
        PathSpec {
            pattern: PAUSE_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Pause a schedule: {name}. Wrap it in action.confirm.",
        },
        PathSpec {
            pattern: RESUME_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Resume a paused schedule: {name}. It runs from its next time on.",
        },
        PathSpec {
            pattern: CREATE_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Create a schedule: {name, cron (5 fields, e.g. 0 3 * * *), timezone? (IANA; \
                  default the pod's), workspace, prompt, model?, max_runtime_min? (1-480, \
                  default 240), enabled?}.",
        },
        PathSpec {
            pattern: UPDATE_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Edit a schedule: {name, new_name?, cron?, timezone?, workspace?, prompt?, \
                  model? (default clears it), max_runtime_min?}; blank fields keep their value.",
        },
        PathSpec {
            pattern: DELETE_SCHEDULE_PATH,
            query_keys: &[],
            doc: "Delete a schedule: {name}. Its runs go on. Wrap it in action.confirm.",
        },
    ],
    entities: &[
        EntityDoc {
            uri: "agents://runner/status",
            doc: "The connection to the runner (agents:status).",
            fields: &[
                ("signed_in", "true once the pod is signed in to a runner"),
                ("connected", "true while the runner answers"),
                (
                    "stale",
                    "signed in but the runner doesn't answer: the data is its last word",
                ),
                ("host", "the runner's host name"),
                ("status_text", "e.g. Connected to runner.example"),
                (
                    "summary_text",
                    "e.g. 1 working · 1 needs you · 2 queued, or Nothing running",
                ),
                ("working_count", "runs starting or working"),
                ("needs_you_count", "runs waiting for you"),
                ("queued_count", "runs waiting for a slot"),
                ("color", "#RRGGBB for the summary"),
                (
                    "updated_text",
                    "when the runner last answered, e.g. 2 min ago",
                ),
            ],
        },
        EntityDoc {
            uri: "agents://sessions/{id}",
            doc: "A run: one attempt at one piece of work (agents:sessions); id is the run's.",
            fields: &[
                ("id", "the run's id"),
                ("title", "what it works on"),
                (
                    "state",
                    "queued, launching, working, needs_you, parked, stalled, interrupted, stopping, closing or ended",
                ),
                ("status", "the same as state"),
                (
                    "status_text",
                    "e.g. Queued, Working, Needs you, Parked, Stalled, Waiting on CI, Done, Needs review, Reviewed, CI failed, Lost",
                ),
                ("status_color", "#RRGGBB"),
                (
                    "state_detail",
                    "e.g. waiting on 1 background task (while active)",
                ),
                (
                    "explain_text",
                    "what Parked, Stalled or Restarting means, or empty",
                ),
                ("active", "true until it ends"),
                (
                    "needs_you",
                    "true while it waits for you (needs_you or parked)",
                ),
                ("parked", "true while parked (its sandbox may sleep)"),
                ("stalled", "true while its sandbox is suspended or degraded"),
                ("asleep", "the same as parked (older layouts)"),
                (
                    "queue_reason",
                    "when queued: waiting_for_slot, sprite_waking, sprite_degraded or sprite_down",
                ),
                ("queue_text", "e.g. Waiting for a free slot"),
                ("needs_review", "true once ended needing your review"),
                ("reviewed", "true once marked reviewed"),
                (
                    "waiting",
                    "true once ended while its outcome waits on GitHub",
                ),
                ("stage", "what it waits on: ci, review or deploy"),
                ("stage_text", "e.g. Waiting on deploy"),
                (
                    "needs_you_reason",
                    "permission, question or stopped (while waiting)",
                ),
                ("needs_you_label", "e.g. Wants permission, Has a question"),
                (
                    "needs_you_text",
                    "the question, the permission, or Claude's last message",
                ),
                (
                    "end_reason",
                    "once ended: reported, stopped, timed_out, lost, start_failed or expired",
                ),
                (
                    "outcome",
                    "verified against GitHub: none, pending, done, failed or needs_review",
                ),
                (
                    "outcome_reason",
                    "when failed: ci, closed_unmerged, deploy, blocked or failed",
                ),
                (
                    "outcome_text",
                    "the outcome in words once ended, e.g. Deploy failed",
                ),
                ("outcome_summary", "the agent's report's summary"),
                (
                    "claim",
                    "what the agent reported: done, review, blocked or failed",
                ),
                (
                    "claim_mismatch",
                    "true when the report disagrees with GitHub",
                ),
                ("claim_mismatch_text", "says so, or empty"),
                ("follow_up", "a follow-up the agent suggested"),
                ("summary", "the last thing Claude said at the end of a turn"),
                ("error", "why it ended badly"),
                (
                    "pr_url",
                    "its pull request (https://github.com/.../pull/N), or empty",
                ),
                (
                    "deploy_url",
                    "its deploy's workflow run (https://github.com/.../actions/runs/N), or empty",
                ),
                ("attempt_text", "e.g. Attempt 2 of 3, after a restart"),
                ("current_tool", "e.g. Bash: npm test (while active)"),
                ("cost_text", "e.g. $0.42"),
                ("tokens_text", "e.g. 12000 tokens"),
                ("elapsed_text", "how long it has run, e.g. 12 min"),
                ("updated_text", "e.g. 2 min ago"),
                ("work_item_id", "the Tasks row or schedule it is for"),
                ("task_id", "the same as work_item_id"),
                (
                    "occurrence",
                    "a schedule's occurrence: the time it is for, RFC 3339 with its offset",
                ),
                ("schedule", "the schedule it runs for, or empty"),
                ("sandbox", "the sandbox it runs in"),
                ("workspace", "the runner workspace"),
                ("model", "the model asked for, or empty for the default"),
                ("branch", "its git branch"),
                ("origin", "task, schedule or phone"),
                ("session_id", "Claude Code's session id"),
                (
                    "resumed_from",
                    "the run this one continues (Reopen or Continue), or empty",
                ),
                (
                    "continued_by",
                    "the newest run that continues this one, or empty",
                ),
                (
                    "can_continue",
                    "true when it timed out and hasn't been continued: Continue (/v1/agents/sessions/continue {run_id}) carries it on",
                ),
                ("claude_url", "opens it in the Claude app, or empty"),
            ],
        },
        EntityDoc {
            uri: "agents://sandboxes/{name}",
            doc: "A sandbox runs run in (agents:sandboxes).",
            fields: &[
                ("name", "its name"),
                (
                    "state",
                    "unprovisioned, provisioning, asleep, waking, awake, degraded, suspended, down or updating",
                ),
                ("awake", "true while awake"),
                (
                    "state_text",
                    "Not set up, Setting up, Asleep, Waking, Awake, Degraded, Suspended, Down or Updating",
                ),
                ("color", "#RRGGBB"),
                ("reason", "why degraded, suspended or down"),
                ("troubled", "true while degraded, suspended or down"),
                ("down", "true while down (Repair applies)"),
                ("can_wake", "asleep or suspended (Wake applies)"),
                ("slots_text", "e.g. 1 of 2 slots in use"),
                ("preflight_text", "e.g. Preflight failed: disk, or empty"),
                ("disk_warning", "true after a second disk failure in 7 days"),
                ("disk_text", "says it likely needs replacing, or empty"),
                ("last_seen_text", "its last heartbeat, e.g. 5 min ago"),
                ("agentd_version", "the version its agent daemon reports"),
                ("workspace", "what Wake and Repair send"),
            ],
        },
        EntityDoc {
            uri: "agents://checks/{id}",
            doc: "A security check on the runner (agents:checks); the same shape as monitor's.",
            fields: &[
                ("id", "the check's id"),
                ("title", "what it checks"),
                ("status", "ok or warn"),
                ("ok", "true when ok"),
                ("status_text", "OK or Warning"),
                ("detail", "one line"),
                ("color", "#RRGGBB"),
            ],
        },
        EntityDoc {
            uri: "agents://schedules/{name}",
            doc: "One of the pod's schedules (agents:schedules): the pod sends a run for each \
                  of its times.",
            fields: &[
                ("id", "its id (the work item of its runs)"),
                ("name", "its name"),
                ("cron", "when it runs, e.g. 0 3 * * *"),
                ("timezone", "the cron's zone"),
                ("workspace", "the workspace it runs in"),
                ("model", "the model it asks for, or empty for the default"),
                ("model_text", "the model, or Default model"),
                ("max_runtime_min", "its time limit in minutes"),
                ("max_runtime_minutes", "the same (older layouts)"),
                ("prompt", "what it asks Claude to do"),
                ("enabled", "false while paused"),
                ("paused", "true while paused"),
                ("can_pause", "true while active"),
                ("state_text", "Active or Paused"),
                (
                    "next_run_text",
                    "e.g. in 3 h · Sat 03:00 (in the schedule's timezone), or Paused",
                ),
                ("ran", "true once it has a last run"),
                (
                    "last_status",
                    "the last run's state or outcome, e.g. done or pending, or missed, refused or sending",
                ),
                ("last_status_text", "e.g. Done, Waiting on CI, Missed"),
                ("last_status_color", "#RRGGBB"),
                ("last_summary", "what the last run did"),
                ("last_error", "why the last run ended badly"),
                ("last_cost_text", "e.g. $0.42"),
                ("last_run_id", "the last run's id"),
                ("last_session_id", "the same as last_run_id"),
                ("last_run_text", "when the last run was due, e.g. 3 h ago"),
            ],
        },
        EntityDoc {
            uri: "agents://events/{run}-{n}",
            doc: "A line of a run's log (agents:timeline:<id>).",
            fields: &[
                ("seq", "its place in the log"),
                ("type", "the runner's kind of line, e.g. tool"),
                ("text", "e.g. Bash: npm test"),
                ("time_text", "e.g. 4 min ago"),
            ],
        },
    ],
    dev: false,
};

/// The core layout: an Agents layout over every feed.
pub const APP: CoreLayoutInfo = CoreLayoutInfo {
    id: APP_ID,
    title: "Agents",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[
        (NAME, SESSIONS_FEED),
        (NAME, SANDBOXES_FEED),
        (NAME, CHECKS_FEED),
        (NAME, SCHEDULES_FEED),
    ],
};

// `FILES`: every `layouts/*.json` of this crate, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// How the adapter runs.
#[derive(Debug, Clone)]
pub struct AgentsConfig {
    /// Runners on any address over http or https (tests, local
    /// development); otherwise https on public hosts only.
    pub allow_private_hosts: bool,
    /// How often queued Tasks rows are looked at (besides each Collections
    /// change).
    pub tasks_every: Duration,
    /// The longest wait between reconnects.
    pub backoff_max: Duration,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            allow_private_hosts: false,
            tasks_every: Duration::from_secs(30),
            backoff_max: follower::BACKOFF_MAX,
        }
    }
}

/// The env var that lets runners live on private addresses (local
/// development only).
pub const PRIVATE_HOSTS_VAR: &str = "CONSTRUCT_AGENTS_PRIVATE_HOSTS";

impl AgentsConfig {
    pub fn from_env() -> Self {
        Self {
            allow_private_hosts: std::env::var(PRIVATE_HOSTS_VAR).as_deref() == Ok("1"),
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct AgentsAdapter {
    pub config: AgentsConfig,
    pub clients: cap::Clients,
}

impl Default for AgentsAdapter {
    fn default() -> Self {
        Self::new(AgentsConfig::default())
    }
}

impl AgentsAdapter {
    pub fn new(config: AgentsConfig) -> Self {
        Self {
            clients: cap::Clients::new(config.allow_private_hosts),
            config,
        }
    }
}

const INSTRUCTIONS: &str = "The agents source shows the runs (Claude Code work) an agent runner \
runs for the user (feeds sessions, sandboxes, checks, schedules), with actions to stop a run, wake \
or repair a sandbox, start a run, reopen a task, mark a run reviewed, and manage schedules. The \
core layout agents is a whole dashboard: use it whole ({type: core://Use, ref: @core/agents}) \
as a layout and link it from home, where agents:status's summary_text (e.g. 1 working · 1 needs you) \
makes a one-line summary. The pod signs in to the runner from that layout's form (the runner's URL \
and token): never ask for the token in the conversation. agents_link_tasks links a tasks \
collection (a title and a choice status with the option queued): its rows that you or Claude set to \
queued start runs (rows a filler wrote can't), and each row's status follows its run. Schedules \
live in the pod: agents_schedules lists, creates, edits, deletes, pauses, resumes and runs them.";

impl Source for AgentsAdapter {
    fn element_catalog(&self) -> &'static str {
        include_str!("../catalog.json")
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn migrations(&self) -> &'static [Step] {
        db::MIGRATIONS
    }

    fn on_open(&self, store: &Store) -> Result<(), String> {
        db::reseal(store).map_err(|e| e.to_string())
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn router(&self) -> Router<Pod> {
        Router::new()
            .route(SIGN_IN_PATH, post(sign_in_route))
            .route(SIGN_OUT_PATH, post(sign_out_route))
            .route(STOP_PATH, post(stop_route))
            .route(WAKE_PATH, post(wake_route))
            .route(REPAIR_PATH, post(repair_route))
            .route(CREATE_SCHEDULE_PATH, post(create_schedule_route))
            .route(UPDATE_SCHEDULE_PATH, post(update_schedule_route))
            .route(DELETE_SCHEDULE_PATH, post(delete_schedule_route))
            .route(START_PATH, post(start_route))
            .route(RUN_SCHEDULE_PATH, post(run_schedule_route))
            .route(PAUSE_SCHEDULE_PATH, post(pause_schedule_route))
            .route(RESUME_SCHEDULE_PATH, post(resume_schedule_route))
            .route(REOPEN_PATH, post(reopen_route))
            .route(CONTINUE_PATH, post(continue_route))
            .route(REVIEW_PATH, post(review_route))
            .route(TIMELINE_PATH, get(timeline_route))
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if MANIFEST.feed(feed).is_none() {
                return pod_core::unknown_feed(NAME, feed);
            }
            follower::spawn(pod);
            let read = pod
                .blocking(|s| {
                    let st = db::state(s)?;
                    let host = db::runner(s)?
                        .map(|r| cap::host_of(&r.url))
                        .unwrap_or_default();
                    let st = st.filter(|_| !host.is_empty());
                    let schedules = db::schedules(s)?;
                    let mut last = Vec::new();
                    for sc in &schedules {
                        last.push(db::last_occurrence(s, &sc.id)?);
                    }
                    Ok((st, host, pod_core::tz::time_zone(s)?, schedules, last))
                })
                .await;
            let (st, host, tz, list, last) = match read {
                Ok(v) => v,
                Err(e) => return Hydration::Error(e.into_response()),
            };
            let now = follower::now_ms();
            Hydration::Data(match feed {
                SESSIONS_FEED => bundle::sessions(st.as_ref(), &list, &host, now),
                SANDBOXES_FEED => bundle::sandboxes(st.as_ref(), now),
                CHECKS_FEED => bundle::checks(st.as_ref()),
                _ => {
                    let views: Vec<bundle::ScheduleView<'_>> = list
                        .iter()
                        .zip(&last)
                        .map(|(schedule, last)| bundle::ScheduleView {
                            schedule,
                            last: last.as_ref(),
                        })
                        .collect();
                    bundle::schedules(st.as_ref(), &views, now, &tz)
                }
            })
        })
    }

    fn notifies_layouts(&self) -> bool {
        true
    }

    fn layout_notification_channels(
        &self,
        store: &Store,
        catalog: &Catalog,
    ) -> Vec<NotificationChannel> {
        match alerts::owner(store, catalog) {
            Ok(Some(app)) => alerts::channels(&app),
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::error!(error = %e, "agent notifications: layouts could not be read");
                Vec::new()
            }
        }
    }

    /// The follower sends the notifications itself; polling makes sure it
    /// runs.
    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        _ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        Box::pin(async move {
            follower::spawn(pod);
            // The timer also runs from the poll, so schedules fire however
            // the follower is doing (it holds a lock: one pass at a time).
            if let Ok(Some(runner)) = pod.blocking(db::runner).await {
                let pass =
                    schedules::tick(pod, &self.clients, Some(&runner), follower::now_ms()).await;
                follower::settle(pod, Vec::new(), pass.notices).await;
            }
            PollOutcome::default()
        })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        Box::pin(async move { follower::spawn(pod) })
    }

    /// A pod signed in to a runner stays open, so its follower runs from
    /// server start whether or not anyone uses the pod.
    fn keeps_open(&self, store: &Store) -> bool {
        db::signed_in(store).unwrap_or(false)
    }

    /// The agent runner jack (decision `jacks`): the Foreman, which runs
    /// Claude Code work on cloud sandboxes. Plugged by the Agents layout's
    /// sign-in form (its URL and token); unplugging signs out.
    fn jacks(&self) -> Vec<pod_core::jacks::JackSpec> {
        vec![pod_core::jacks::JackSpec {
            id: JACK,
            title: "Agent runner",
            summary: "An agent runner (the Foreman): runs, sandboxes and checks of Claude Code \
                      work, the pod's schedules and tasks collections that start runs \
                      (agents_schedules, agents_link_tasks), and their notifications (the agents \
                      core layout and source).",
            kind: pod_core::jacks::JackKind::Account {
                how: "sign in from the Agents layout's form (the runner's https:// URL and its token; \
                      never ask for the token in the conversation)",
            },
        }]
    }

    fn jack_status<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> BoxFuture<'a, Option<pod_core::jacks::JackStatus>> {
        Box::pin(async move {
            if jack != JACK {
                return None;
            }
            let runner = pod.blocking(db::runner).await.ok().flatten();
            Some(match runner {
                Some(r) => pod_core::jacks::JackStatus {
                    plugged: true,
                    detail: Some(r.url),
                },
                None => pod_core::jacks::JackStatus::default(),
            })
        })
    }

    fn unplug_jack<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> Option<BoxFuture<'a, Result<(), String>>> {
        (jack == JACK).then(|| {
            Box::pin(async move {
                pod.blocking(db::sign_out)
                    .await
                    .map_err(|_| "signing out of the runner failed".to_owned())?;
                pod.local::<follower::Ctl>().changed();
                pod.publish_data_all(NAME);
                Ok(())
            }) as BoxFuture<'a, Result<(), String>>
        })
    }

    fn mcp_tools(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "agents_link_tasks",
                "description": "Link a tasks collection to the agent runner (or unlink it with collection null). \
            The collection needs a title field and a choice field status with the option queued. Linking adds what \
            the wiring needs: the status options todo, queued, running, needs_you, waiting, review, done and failed, \
            and optional text fields run_id, session_id, workspace, stage and diagnosis. Then a row that the user or \
            Claude sets to queued starts a run on the runner (prompt: the title and notes; workspace: the row's \
            workspace, else this default; model and max_runtime_min: the row's fields of those names, else these \
            defaults). A row a filler made, or whose status, title, notes or workspace a filler or a model wrote, \
            does not start: it fails with the reason. The row's status follows its run: running, needs_you, then \
            waiting (stage ci, review or deploy) while GitHub settles its outcome, and done, review or failed, with \
            what it did and its pull request (or why it failed) appended to diagnosis. A row set to done or todo \
            keeps that status (its run is stopped). The action /v1/agents/tasks/reopen continues an ended run with a \
            follow-up message; /v1/agents/sessions/review marks a run reviewed (its row follows it to done).",
                "inputSchema": {"type": "object", "properties": {
                    "collection": {"type": ["string", "null"], "description": "The collection's name, or null to unlink"},
                    "workspace": {"type": "string", "description": "The runner workspace tasks start in by default"},
                    "model": {"type": "string", "description": "The model tasks ask for by default (omit for the sandbox's default)"},
                    "max_runtime_min": {"type": "integer", "description": "Tasks' time limit by default, 1 to 480 minutes (omit for the runner's 240)"}},
                    "required": ["collection"]},
            }),
            json!({
                "name": "agents_schedules",
                "description": "Manage the pod's schedules for the agent runner: the pod sends one run for each time \
            a schedule's cron names, in its time zone (daylight saving handled: a skipped time runs just after the \
            gap, a repeated one once). op list answers every schedule with its next run. op create {name, cron (5 \
            fields: minute hour day-of-month month day-of-week, e.g. 52 3 * * *), timezone (IANA, default the \
            pod's), workspace, prompt, model?, max_runtime_min? (1-480, default 240), enabled?}. op update {name, \
            new_name?, and any of those}: what you leave out keeps its value. op delete, pause, resume and run (now) \
            take {name}. A time not sent within 2 h (the pod or runner was down) is skipped and the user is told. \
            Ask before creating or changing a schedule's prompt: it runs unattended with the workspace's access.",
                "inputSchema": {"type": "object", "properties": {
                    "op": {"type": "string", "enum": ["list", "create", "update", "delete", "pause", "resume", "run"]},
                    "name": {"type": "string"},
                    "new_name": {"type": "string"},
                    "cron": {"type": "string"},
                    "timezone": {"type": "string"},
                    "workspace": {"type": "string"},
                    "prompt": {"type": "string"},
                    "model": {"type": "string"},
                    "max_runtime_min": {"type": "integer"},
                    "enabled": {"type": "boolean"}},
                    "required": ["op"]},
            }),
        ]
    }

    fn mcp_summary(&self) -> Option<&'static str> {
        Some("agent runs and schedules")
    }

    fn mcp_instructions(&self) -> Option<&'static str> {
        Some(INSTRUCTIONS)
    }

    fn mcp_call<'a>(
        &'a self,
        pod: &'a Pod,
        tool: &'a str,
        args: &'a Value,
    ) -> Option<BoxFuture<'a, Result<Value, Value>>> {
        match tool {
            "agents_link_tasks" => Some(Box::pin(tasks::set_link(pod, args))),
            "agents_schedules" => Some(Box::pin(schedules::tool(pod, args))),
            _ => None,
        }
    }
}

// ---------- routes ----------

fn refuse(status: StatusCode, error: &str, detail: impl Into<String>) -> Response {
    pod_core::json_error(status, json!({"error": error, "detail": detail.into()}))
}

/// The caller's session, or the refusal: actions reach a service outside
/// the pod, so they need a valid session.
async fn check_caller(pod: &Pod, headers: &HeaderMap) -> Result<(), Response> {
    match pod.caller(headers).await {
        Ok(Caller::Signed(_)) => Ok(()),
        Ok(Caller::Stale) => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "the session is not valid",
        )),
        Ok(Caller::Anonymous) => Err(refuse(
            StatusCode::FORBIDDEN,
            "session_required",
            "open the pod first",
        )),
        Err(e) => Err(e.into_response()),
    }
}

/// The JSON object body, or the refusal.
async fn body_of(pod: &Pod, headers: &HeaderMap, body: &Bytes) -> Result<Value, Response> {
    check_caller(pod, headers).await?;
    let v = pod_core::parse_json_body(headers, body).map_err(IntoResponse::into_response)?;
    match v {
        Value::Object(_) => Ok(v),
        Value::Null => Ok(json!({})),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body is a JSON object",
        )),
    }
}

/// A text field of `body`: trimmed, non-empty, at most `max` characters.
fn field(body: &Value, k: &str, max: usize) -> Result<String, Response> {
    body.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= max)
        .map(str::to_owned)
        .ok_or_else(|| {
            refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("\"{k}\" is text of 1 to {max} characters"),
            )
        })
}

/// A runner failure as the pod's answer. A refused token is 502, never
/// 401, which would tell the client its own session is stale.
fn runner_error(e: CapError) -> Response {
    match e {
        CapError::Status(s @ (400 | 404 | 409), d) => refuse(
            StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_REQUEST),
            "runner_refused",
            if d.is_empty() { e_text(s) } else { d },
        ),
        CapError::Unauthorized => refuse(
            StatusCode::BAD_GATEWAY,
            "runner_refused_token",
            "the runner refused the pod's token; sign in to the runner again",
        ),
        other => refuse(
            StatusCode::BAD_GATEWAY,
            "runner_unreachable",
            other.to_string(),
        ),
    }
}

fn e_text(status: u16) -> String {
    match status {
        404 => "the runner doesn't know that".into(),
        409 => "the runner can't do that now".into(),
        _ => "the runner refused the request".into(),
    }
}

/// The adapter and the runner the pod is signed in to, or the refusal.
async fn signed_in(pod: &Pod) -> Result<(&AgentsAdapter, Runner), Response> {
    let Some(a) = pod.adapter::<AgentsAdapter>() else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "no agents source",
        ));
    };
    match pod.blocking(db::runner).await {
        Ok(Some(r)) => Ok((a, r)),
        Ok(None) => Err(refuse(
            StatusCode::CONFLICT,
            "not_signed_in",
            "the pod is not signed in to an agent runner",
        )),
        Err(e) => Err(e.into_response()),
    }
}

async fn send(pod: &Pod, cmd: Command) -> Response {
    let (a, runner) = match signed_in(pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match cap::command(&a.clients, &runner, &cmd).await {
        Ok(answer) => {
            let mut out = json!({"ok": true});
            if let Some(id) = cap::run_id_of(&answer) {
                out["run_id"] = json!(id);
            }
            if let Some(t) = answer
                .get("run")
                .and_then(|r| r.get("work_item_id"))
                .and_then(Value::as_str)
            {
                out["work_item_id"] = json!(text::clean(t, 200));
            }
            Json(out).into_response()
        }
        Err(e) => runner_error(e),
    }
}

async fn sign_in_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(a) = pod.adapter::<AgentsAdapter>() else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no agents source");
    };
    // A temporary pod (anyone can make one) has the jack locked: it would
    // make the server follow a host of the visitor's choosing.
    let locked = pod
        .blocking(|s| s.with_conn(|c| pod_core::jacks::locked_on(c, JACK)))
        .await;
    if !matches!(locked, Ok(false)) {
        return refuse(
            StatusCode::FORBIDDEN,
            "jack_locked",
            "this server keeps the agent_runner jack unplugged on this pod",
        );
    }
    let url = match field(&body, "url", 500) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let url = match cap::check_url(&url, a.config.allow_private_hosts) {
        Ok(u) => u,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "invalid_url", e),
    };
    let token = match field(&body, "token", 4096) {
        Ok(t) if t.chars().all(|c| c.is_ascii_graphic()) => t,
        Ok(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the token is one word of printable characters",
            );
        }
        Err(r) => return r,
    };
    let runner = Runner { url, token };
    let snap = match cap::snapshot(&a.clients, &runner).await {
        Ok(s) => s,
        Err(CapError::Unauthorized) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "runner_refused_token",
                "the runner refused that token",
            );
        }
        Err(e) => return runner_error(e),
    };
    let mut st = state::State::default();
    st.apply_snapshot(&snap);
    let host = cap::host_of(&runner.url);
    let now = follower::now_ms();
    if let Err(e) = pod
        .blocking(move |s| db::sign_in(s, &runner, &st, now))
        .await
    {
        return e.into_response();
    }
    pod.local::<follower::Ctl>().changed();
    follower::spawn(&pod);
    pod.publish_data_all(NAME);
    tracing::info!(%host, "signed in to an agent runner");
    Json(json!({"ok": true, "host": host})).into_response()
}

async fn sign_out_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = body_of(&pod, &headers, &body).await {
        return r;
    }
    if let Err(e) = pod.blocking(db::sign_out).await {
        return e.into_response();
    }
    pod.local::<follower::Ctl>().changed();
    pod.publish_data_all(NAME);
    Json(json!({"ok": true})).into_response()
}

async fn stop_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let key = if body.get("run_id").is_some() {
        "run_id"
    } else {
        "session_id"
    };
    match field(&body, key, 120) {
        Ok(run_id) if cap::valid_id(&run_id) => send(&pod, Command::StopRun { run_id }).await,
        Ok(_) => refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "\"run_id\" names a run",
        ),
        Err(r) => r,
    }
}

async fn wake_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let workspace = match field(&body, "workspace", 100) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let minutes = match body.get("minutes") {
        None | Some(Value::Null) => WAKE_MINUTES,
        Some(v) => match v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        {
            Some(m @ 1..=60) => m as u32,
            _ => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "\"minutes\" is 1 to 60",
                );
            }
        },
    };
    send(&pod, Command::WakeSprite { workspace, minutes }).await
}

async fn repair_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    match field(&body, "workspace", 100) {
        Ok(workspace) => send(&pod, Command::RepairSprite { workspace }).await,
        Err(r) => r,
    }
}

async fn start_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let (workspace, title, prompt) = match (
        field(&body, "workspace", 100),
        field(&body, "title", 200),
        field(&body, "prompt", 20_000),
    ) {
        (Ok(w), Ok(t), Ok(p)) => (w, t, p),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => return r,
    };
    let work_item_id = match body.get("task_id").and_then(Value::as_str).map(str::trim) {
        Some(t) if !t.is_empty() && t.len() <= 200 && !t.starts_with("sch_") => t.to_owned(),
        Some(t) if !t.is_empty() => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"task_id\" is at most 200 characters and not a schedule's",
            );
        }
        _ => format!("pod/{}", follower::now_ms()),
    };
    let model = match body.get("model").and_then(Value::as_str).map(str::trim) {
        Some(m) if m.len() > 100 || !m.chars().all(|c| c.is_ascii_graphic()) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"model\" is one word of at most 100 characters",
            );
        }
        Some(m) if !m.is_empty() => Some(m.to_owned()),
        _ => None,
    };
    let max_runtime_min = match body.get("max_runtime_min") {
        None | Some(Value::Null) => None,
        Some(v) => match v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        {
            Some(m @ 1..=480) => Some(m as u32),
            _ => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "\"max_runtime_min\" is 1 to 480",
                );
            }
        },
    };
    // One request per press: a resend of the same press (the outbox) may
    // carry its own client id.
    let request_id = match body.get("client_id").and_then(Value::as_str) {
        Some(c) if cap::valid_id(c) => format!("start_{c}"),
        _ => format!("start_{}_{}", follower::now_ms(), work_item_id.len()),
    };
    send(
        &pod,
        Command::StartRun(cap::StartRun {
            request_id,
            workspace,
            work_item_id,
            occurrence: None,
            origin: cap::ORIGIN_TASK.into(),
            title,
            prompt,
            model,
            max_runtime_min,
            resume_from: None,
        }),
    )
    .await
}

/// A schedule operation's answer as the route's response.
fn schedule_answer(r: Result<Value, schedules::Refusal>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err((status, body)) => pod_core::json_error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
            body,
        ),
    }
}

async fn create_schedule_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match body_of(&pod, &headers, &body).await {
        Ok(b) => schedule_answer(schedules::create(&pod, &b).await),
        Err(r) => r,
    }
}

async fn update_schedule_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match body_of(&pod, &headers, &body).await {
        Ok(b) => schedule_answer(schedules::update(&pod, &b).await),
        Err(r) => r,
    }
}

async fn delete_schedule_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match body_of(&pod, &headers, &body).await {
        Ok(b) => schedule_answer(schedules::delete(&pod, &b).await),
        Err(r) => r,
    }
}

async fn run_schedule_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let sc = match schedules::find(&pod, &body).await {
        Ok(s) => s,
        Err(e) => return schedule_answer(Err(e)),
    };
    let (a, runner) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match schedules::run_now(&pod, &a.clients, &runner, &sc, follower::now_ms()).await {
        Ok(answer) => {
            Json(json!({"ok": true, "run_id": cap::run_id_of(&answer), "work_item_id": sc.id}))
                .into_response()
        }
        Err(e) => runner_error(e),
    }
}

async fn pause_schedule_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    match body_of(&pod, &headers, &body).await {
        Ok(b) => schedule_answer(schedules::set_enabled(&pod, &b, false).await),
        Err(r) => r,
    }
}

async fn resume_schedule_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match body_of(&pod, &headers, &body).await {
        Ok(b) => schedule_answer(schedules::set_enabled(&pod, &b, true).await),
        Err(r) => r,
    }
}

async fn reopen_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let message = match field(&body, "message", 20_000) {
        Ok(m) => follow_up_prompt(&m),
        Err(r) => return r,
    };
    // The row, or a run of it (a run's screen knows only the run).
    let by_run = body.get("id").is_none() && body.get("run_id").is_some();
    let id = match field(&body, if by_run { "run_id" } else { "id" }, 200) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let (a, runner) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let state = match pod.blocking(db::state).await {
        Ok(st) => st.map(|(s, _)| s).unwrap_or_default(),
        Err(e) => return e.into_response(),
    };
    let id = if by_run {
        match state.runs.get(&id) {
            Some(r) if r.origin == cap::ORIGIN_TASK && !r.work_item_id.is_empty() => {
                r.work_item_id.clone()
            }
            // A schedule's run (or one the phone started): no row to follow,
            // so the new run is sent as it is.
            Some(r) => {
                let request_id = format!("reopen_{}_{}", r.id, follower::now_ms());
                return resume(&pod, a, &runner, r, &message, request_id).await;
            }
            None => return refuse(StatusCode::NOT_FOUND, "not_found", "no such run"),
        }
    } else {
        id
    };
    match tasks::reopen(&pod, &a.clients, &runner, &state, &id, &message).await {
        Ok(run_id) => Json(json!({"ok": true, "run_id": run_id})).into_response(),
        Err((status, error, detail)) => refuse(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            error,
            detail,
        ),
    }
}

/// What closes every Reopen message. A resumed conversation gets only the
/// developer's words, and an agent that answers them and stops without
/// reporting leaves its run needing you (then parked) though its work is done.
pub const FOLLOW_UP_REMINDER: &str = "When you have finished, report again as the task asks \
(`foreman report`), so this run ends. Don't stop with only a reply unless you need the developer.";

/// A Reopen message as the new run gets it: the developer's words, then
/// [`FOLLOW_UP_REMINDER`].
pub fn follow_up_prompt(message: &str) -> String {
    format!("{}\n\n{FOLLOW_UP_REMINDER}", message.trim_end())
}

/// What Continue tells the run that carries on a timed-out one.
pub const CONTINUE_MESSAGE: &str = "You reached your time limit and were stopped before you finished. \
Continue the task from where you left off: check what is already done (branches, pull requests, CI) \
before repeating anything, then finish the rest and report as the task asks.";

/// Continue a run that timed out: a new run resumes its conversation with
/// [`CONTINUE_MESSAGE`]. One per run (the request id is the run's), and
/// only for a run that timed out and hasn't been continued.
async fn continue_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let run_id = match field(&body, "run_id", 120) {
        Ok(id) if cap::valid_id(&id) => id,
        Ok(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"run_id\" names a run",
            );
        }
        Err(r) => return r,
    };
    let (a, runner) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let state = match pod.blocking(db::state).await {
        Ok(st) => st.map(|(s, _)| s).unwrap_or_default(),
        Err(e) => return e.into_response(),
    };
    let Some(r) = state.runs.get(&run_id) else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no such run");
    };
    if let Some(next) = state.continuation_of(&r.id) {
        return Json(json!({"ok": true, "run_id": next.id})).into_response();
    }
    if !r.timed_out() {
        return refuse(
            StatusCode::CONFLICT,
            "not_timed_out",
            "only a run that timed out can be continued; use Reopen to follow up another",
        );
    }
    if r.origin == cap::ORIGIN_TASK && !r.work_item_id.is_empty() {
        // A task's run: Reopen keeps its row in step with the new run.
        return match tasks::reopen(
            &pod,
            &a.clients,
            &runner,
            &state,
            &r.work_item_id,
            CONTINUE_MESSAGE,
        )
        .await
        {
            Ok(run_id) => Json(json!({"ok": true, "run_id": run_id})).into_response(),
            Err((status, error, detail)) => refuse(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                error,
                detail,
            ),
        };
    }
    let request_id = format!("continue_{}", r.id);
    resume(&pod, a, &runner, r, CONTINUE_MESSAGE, request_id).await
}

/// Start a run that resumes ended run `r`'s conversation with `message`,
/// for the same work item. A schedule's run takes the schedule's model
/// and time limit as they are now (so a raised limit applies), else the
/// run's own. The answer's run goes into the mirror at once.
async fn resume(
    pod: &Pod,
    a: &AgentsAdapter,
    runner: &Runner,
    r: &state::Run,
    message: &str,
    request_id: String,
) -> Response {
    if r.is_active() {
        return refuse(
            StatusCode::CONFLICT,
            "run_not_ended",
            "the run hasn't ended; answer it in the Claude app or stop it first",
        );
    }
    let schedule = if r.origin == cap::ORIGIN_SCHEDULE {
        let item = r.work_item_id.clone();
        pod.blocking(move |s| Ok(db::schedules(s)?.into_iter().find(|sc| sc.id == item)))
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let (model, max_runtime_min) = match &schedule {
        Some(sc) => (sc.model.clone(), sc.max_runtime_min),
        None => (r.model.clone(), r.max_runtime_min),
    };
    let cmd = Command::StartRun(cap::StartRun {
        request_id,
        workspace: r.workspace.clone(),
        work_item_id: r.work_item_id.clone(),
        occurrence: None,
        origin: if r.origin == cap::ORIGIN_SCHEDULE {
            cap::ORIGIN_SCHEDULE
        } else {
            cap::ORIGIN_TASK
        }
        .into(),
        title: if r.title.is_empty() {
            "Untitled run".to_owned()
        } else {
            r.title.chars().take(200).collect()
        },
        prompt: message.to_owned(),
        model: (!model.is_empty()).then_some(model),
        max_runtime_min: (max_runtime_min > 0)
            .then(|| max_runtime_min.min(cap::MAX_RUNTIME_MIN as i64) as u32),
        resume_from: Some(r.id.clone()),
    });
    let answer = match cap::command(&a.clients, runner, &cmd).await {
        Ok(v) => v,
        Err(e) => return runner_error(e),
    };
    if let Some(record) = answer.get("run").filter(|r| r.is_object()).cloned() {
        let saved = pod
            .blocking(move |s| {
                if let Some((mut st, _)) = db::state(s)? {
                    st.apply_run(&record);
                    db::save(s, &st, follower::now_ms())?;
                }
                Ok(())
            })
            .await;
        if saved.is_err() {
            tracing::warn!("a continued run could not be stored");
        }
        pod.publish_data_all(NAME);
    }
    Json(json!({"ok": true, "run_id": cap::run_id_of(&answer)})).into_response()
}

async fn review_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let run_id = match field(&body, "run_id", 120) {
        Ok(id) if cap::valid_id(&id) => id,
        Ok(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"run_id\" names a run",
            );
        }
        Err(r) => return r,
    };
    let (a, runner) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    // The runner owns the outcome: it sets it done (409 unless the run
    // needs review).
    let answer = match cap::command(
        &a.clients,
        &runner,
        &Command::MarkReviewed {
            run_id: run_id.clone(),
        },
    )
    .await
    {
        Ok(v) => v,
        Err(CapError::Status(409, d)) => {
            return refuse(
                StatusCode::CONFLICT,
                "not_needing_review",
                if d.is_empty() {
                    "only an ended run that needs review can be marked reviewed".to_owned()
                } else {
                    d
                },
            );
        }
        Err(e) => return runner_error(e),
    };
    // Shown at once: the answer's run goes into the mirror (the event that
    // follows changes nothing more).
    if let Some(record) = answer.get("run").filter(|r| r.is_object()).cloned() {
        let saved = pod
            .blocking(move |s| {
                if let Some((mut st, _)) = db::state(s)? {
                    st.apply_run(&record);
                    db::save(s, &st, follower::now_ms())?;
                }
                Ok(())
            })
            .await;
        if saved.is_err() {
            tracing::warn!("a reviewed run could not be stored");
        }
    }
    pod.publish_data_all(NAME);
    Json(json!({"ok": true, "run_id": run_id})).into_response()
}

#[derive(serde::Deserialize)]
struct TimelineQuery {
    id: String,
}

/// Run logs read lately, per pod: `(run id) -> (when, log)`.
#[derive(Debug, Default)]
pub struct LogCache(
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Value)>>,
);

/// How long a run's log is served from [`LogCache`].
pub const LOG_FRESH: Duration = Duration::from_secs(10);
/// The most run logs [`LogCache`] holds (the oldest goes first).
pub const LOG_CACHED: usize = 16;

impl LogCache {
    fn get(&self, id: &str) -> Option<Value> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(id)
            .filter(|(at, _)| at.elapsed() < LOG_FRESH)
            .map(|(_, v)| v.clone())
    }

    fn put(&self, id: &str, log: &Value) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (at, _)| at.elapsed() < LOG_FRESH);
        while m.len() >= LOG_CACHED && !m.contains_key(id) {
            let Some(oldest) = m
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert(id.to_owned(), (std::time::Instant::now(), log.clone()));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

async fn timeline_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<TimelineQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let id = q.id.trim().to_owned();
    if !cap::valid_id(&id) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "\"id\" names a run",
        );
    }
    let read = pod
        .blocking(move |s| {
            let runner = db::runner(s)?;
            let st = db::state(s)?.filter(|_| runner.is_some());
            Ok((runner, st, db::schedules(s)?))
        })
        .await;
    let (runner, st, list) = match read {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let cache = pod.local::<LogCache>();
    let log = match (cache.get(&id), runner, pod.adapter::<AgentsAdapter>()) {
        (Some(l), _, _) => Some(l),
        (None, Some(runner), Some(a)) => match cap::run_log(&a.clients, &runner, &id).await {
            Ok(l) => {
                cache.put(&id, &l);
                Some(l)
            }
            Err(e) => {
                tracing::info!(error = %e, "a run's log could not be read");
                None
            }
        },
        _ => None,
    };
    Json(bundle::timeline(
        st.as_ref(),
        &list,
        &id,
        log.as_ref(),
        follower::now_ms(),
    ))
    .into_response()
}

/// The adapter as the server builds it.
pub fn adapter() -> Arc<dyn Source> {
    Arc::new(AgentsAdapter::new(AgentsConfig::from_env()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_cache_holds_a_few_logs() {
        let c = LogCache::default();
        for i in 0..LOG_CACHED * 4 {
            c.put(&format!("run_{i}"), &json!({"entries": []}));
        }
        assert_eq!(c.len(), LOG_CACHED);
        // The newest stay.
        assert!(c.get(&format!("run_{}", LOG_CACHED * 4 - 1)).is_some());
        assert!(c.get("run_0").is_none());
    }
}
