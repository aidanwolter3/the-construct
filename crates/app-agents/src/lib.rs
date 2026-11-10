//! The `agents` data source (decisions `agents-data-source`, `agents-runs`):
//! Claude Code runs started by an external **agent runner** (the Foreman),
//! shown and controlled from the pod. The pod is the dashboard and the
//! "start work" button; the runs themselves are watched and steered in the
//! Claude app.
//!
//! The runner speaks CAP v2 ([`cap`]) and alone decides each run's state;
//! the pod keeps a mirror of its records ([`state`], [`db`]). The pod signs
//! in with the runner's URL and a token (`POST /v1/agents/sign-in`, from the
//! Agents layout's form), stored sealed. A background follower per signed-in
//! pod ([`follower`]) takes a snapshot and follows the stream from its
//! cursor, so the feeds answer while the runner is away.
//!
//! - Feeds: `sessions` (`agents:status`, `agents:sessions`: the runs),
//!   `sandboxes`, `checks`, `schedules`; a data path for one run's log.
//! - Actions: stop a run, wake a sandbox, start a run, reopen a task, run,
//!   pause and resume a schedule. Commands are a fixed set ([`cap::Command`]).
//! - Notifications ([`alerts`]) in the layout that shows the source.
//! - Tasks wiring ([`tasks`]): a linked collection's queued rows start runs,
//!   and each row's status follows its run through one table.
//! - The `agents` blueprint: `layouts/agents/`.

// Route helpers hand back the refusal as an axum `Response`.
#![allow(clippy::result_large_err)]

pub mod alerts;
pub mod bundle;
pub mod cap;
pub mod db;
pub mod follower;
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
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, Catalog, EntityDoc, FeedSpec, Hydration, PathSpec,
    Pod, PollCtx, PollOutcome, RequestCtx, SourceManifest,
};
use protocol::NotificationChannel;
use serde_json::{Value, json};

use crate::cap::{CapError, Command, Runner};

/// The adapter name and the entity scheme.
pub const NAME: &str = "agents";
/// The blueprint layout.
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
pub const SIGN_OUT_PATH: &str = "/v1/agents/sign-out";
pub const STOP_PATH: &str = "/v1/agents/sessions/stop";
pub const WAKE_PATH: &str = "/v1/agents/sandboxes/wake";
pub const START_PATH: &str = "/v1/agents/tasks/start";
pub const RUN_SCHEDULE_PATH: &str = "/v1/agents/schedules/run";
pub const PAUSE_SCHEDULE_PATH: &str = "/v1/agents/schedules/pause";
pub const RESUME_SCHEDULE_PATH: &str = "/v1/agents/schedules/resume";
pub const REOPEN_PATH: &str = "/v1/agents/tasks/reopen";
pub const TIMELINE_PATH: &str = "/v1/agents/sessions/timeline";

/// Minutes a Wake keeps a sandbox up unless the request says.
pub const WAKE_MINUTES: u32 = 30;

pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Agents",
    sign_in: None,
    entity_schemes: &[NAME],
    feeds: &[
        FeedSpec {
            name: SESSIONS_FEED,
            query_keys: &[STATUS_QUERY, SESSIONS_QUERY],
            doc: "The agent runner's runs (one attempt at one piece of work each): the ones that \
                  need you, then working, queued and ended ones; and agents:status: whether the \
                  pod is signed in to a runner and reaches it, with a one-line summary such as \
                  1 working · 1 needs you · 2 queued.",
        },
        FeedSpec {
            name: SANDBOXES_FEED,
            query_keys: &[SANDBOXES_QUERY],
            doc: "The runner's sandboxes, awake first; each can be woken.",
        },
        FeedSpec {
            name: CHECKS_FEED,
            query_keys: &[CHECKS_QUERY],
            doc: "The runner's security checks, ok or warn, warnings first.",
        },
        FeedSpec {
            name: SCHEDULES_FEED,
            query_keys: &[SCHEDULES_QUERY],
            doc: "The runner's scheduled tasks with their next run and last result; each can \
                  be run now, paused or resumed.",
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
            pattern: WAKE_PATH,
            query_keys: &[],
            doc: "Wake a sandbox: {workspace, minutes?} (workspace: the sandbox's workspace \
                  field; minutes default 30).",
        },
        PathSpec {
            pattern: START_PATH,
            query_keys: &[],
            doc: "Start a run: {workspace, title, prompt, task_id?}. The runner queues it; the \
                  answer has its run_id.",
        },
        PathSpec {
            pattern: REOPEN_PATH,
            query_keys: &[],
            doc: "Reopen a linked task whose run has ended: {id (the row) or run_id (a run of \
                  it), message}. A new run resumes the last one's conversation with the message \
                  as the next prompt; the row goes back to queued.",
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
            doc: "Resume a paused schedule: {name}.",
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
                ("state", "queued, starting, working, needs_you or ended"),
                ("status", "the same as state"),
                (
                    "status_text",
                    "e.g. Queued, Working, Needs you, Done, Needs review, Failed, Stopped, Lost",
                ),
                ("status_color", "#RRGGBB"),
                ("active", "true until it ends"),
                ("needs_you", "true while it waits for you"),
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
                    "once ended: reported, exited, stopped, timed_out, lost, start_failed or expired",
                ),
                (
                    "outcome",
                    "the agent's report: done, review, blocked or failed, or empty",
                ),
                ("outcome_summary", "the report's summary"),
                ("summary", "the last thing Claude said at the end of a turn"),
                ("error", "why it ended badly"),
                (
                    "pr_url",
                    "its pull request (https://github.com/.../pull/N), or empty",
                ),
                (
                    "asleep",
                    "true while its sandbox sleeps (the run is frozen, not ended)",
                ),
                ("current_tool", "e.g. Bash: npm test (while active)"),
                ("cost_text", "e.g. $0.42"),
                ("tokens_text", "e.g. 12000 tokens"),
                ("elapsed_text", "how long it has run, e.g. 12 min"),
                ("updated_text", "e.g. 2 min ago"),
                ("task_id", "the Tasks row or schedule run it is for"),
                ("schedule", "the schedule it runs for, or empty"),
                ("sandbox", "the sandbox it runs in"),
                ("workspace", "the runner workspace"),
                ("origin", "task, schedule or phone"),
                ("session_id", "Claude Code's session id"),
                (
                    "resumed_from",
                    "the run this one continues (Reopen), or empty",
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
                    "unprovisioned, asleep, waking, awake, unreachable or updating",
                ),
                ("awake", "true while awake"),
                (
                    "state_text",
                    "Not set up, Asleep, Waking, Awake, Unreachable or Updating",
                ),
                ("color", "#RRGGBB"),
                ("last_seen_text", "its last heartbeat, e.g. 5 min ago"),
                ("agentd_version", "the version its agent daemon reports"),
                ("workspace", "what Wake sends"),
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
            doc: "A scheduled task on the runner (agents:schedules).",
            fields: &[
                ("name", "its name"),
                ("cron", "when it runs, e.g. 0 3 * * *"),
                ("timezone", "the cron's zone"),
                ("workspace", "the workspace it runs in"),
                ("model", "the model it uses"),
                ("max_runtime_minutes", "its time limit"),
                (
                    "enabled",
                    "false when turned off in the runner's configuration",
                ),
                ("paused", "true while paused"),
                ("can_pause", "enabled and not paused"),
                ("state_text", "Active, Paused or Off"),
                (
                    "next_run_text",
                    "e.g. in 3 h · Sat 03:00 (in the schedule's timezone, else the pod's), or Paused",
                ),
                ("ran", "true once it has a last run"),
                (
                    "last_status",
                    "the last run's state, end reason or outcome, e.g. done or expired",
                ),
                ("last_status_text", "e.g. Done, Expired"),
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

/// The blueprint: an Agents layout over every feed.
pub const APP: AppInfo = AppInfo {
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
a sandbox, start a run, reopen a task and run, pause or resume a schedule. The blueprint layout \
agents is a whole dashboard: reference it ({type: core://Ref, app: agents, part: layout}) and link \
it from home, where agents:status's summary_text (e.g. 1 working · 1 needs you) makes a one-line \
summary. The pod signs in to the runner from that layout's form (the runner's URL and token): never \
ask for the token in the conversation. agents_link_tasks links a tasks collection (a title and a \
choice status with the option queued): its rows that become queued start runs, and each row's \
status follows its run.";

impl AppAdapter for AgentsAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
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
            .route(START_PATH, post(start_route))
            .route(RUN_SCHEDULE_PATH, post(run_schedule_route))
            .route(PAUSE_SCHEDULE_PATH, post(pause_schedule_route))
            .route(RESUME_SCHEDULE_PATH, post(resume_schedule_route))
            .route(REOPEN_PATH, post(reopen_route))
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
                    Ok((st, host, pod_core::tz::time_zone(s)?))
                })
                .await;
            let (st, host, tz) = match read {
                Ok(v) => v,
                Err(e) => return Hydration::Error(e.into_response()),
            };
            let now = follower::now_ms();
            Hydration::Data(match feed {
                SESSIONS_FEED => bundle::sessions(st.as_ref(), &host, now),
                SANDBOXES_FEED => bundle::sandboxes(st.as_ref(), now),
                CHECKS_FEED => bundle::checks(st.as_ref()),
                _ => bundle::schedules(st.as_ref(), now, &tz),
            })
        })
    }

    fn notifies_custom_apps(&self) -> bool {
        true
    }

    fn custom_notification_channels(
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

    fn mcp_tools(&self) -> Vec<Value> {
        vec![json!({
            "name": "agents_link_tasks",
            "description": "Link a tasks collection to the agent runner (or unlink it with collection null). \
        The collection needs a title field and a choice field status with the option queued. Linking adds what \
        the wiring needs: the status options todo, queued, running, needs_you, review, done and failed, and \
        optional text fields run_id, session_id, workspace and diagnosis. Then a row whose status becomes queued \
        starts a run on the runner (prompt: the title and notes; workspace: the row's workspace, else this \
        default), and the row's status follows its run: running, needs_you, then done, review or failed, with \
        the outcome's summary and pull request (or why it failed) appended to diagnosis. A row set to done or \
        todo keeps that status (its run is stopped). The action /v1/agents/tasks/reopen continues an ended \
        run with a follow-up message.",
            "inputSchema": {"type": "object", "properties": {
                "collection": {"type": ["string", "null"], "description": "The collection's name, or null to unlink"},
                "workspace": {"type": "string", "description": "The runner workspace tasks start in by default"}},
                "required": ["collection"]},
        })]
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
                .and_then(|r| r.get("task_id"))
                .and_then(Value::as_str)
            {
                out["task_id"] = json!(text::clean(t, 200));
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
            Some(m @ 1..=720) => m as u32,
            _ => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "\"minutes\" is 1 to 720",
                );
            }
        },
    };
    send(&pod, Command::WakeSprite { workspace, minutes }).await
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
    let task_id = match body.get("task_id").and_then(Value::as_str).map(str::trim) {
        Some(t) if !t.is_empty() && t.len() <= 200 && !t.starts_with("schedule/") => t.to_owned(),
        Some(t) if !t.is_empty() => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"task_id\" is at most 200 characters and not a schedule's",
            );
        }
        _ => format!("pod/{}", pod_core::util::now_secs()),
    };
    send(
        &pod,
        Command::StartRun {
            workspace,
            task_id,
            title,
            prompt,
            resume_from: None,
        },
    )
    .await
}

async fn schedule_route(
    pod: Pod,
    headers: HeaderMap,
    body: Bytes,
    make: fn(String) -> Command,
) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    match field(&body, "name", 100) {
        Ok(name) => send(&pod, make(name)).await,
        Err(r) => r,
    }
}

async fn run_schedule_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    schedule_route(pod, headers, body, |name| Command::RunSchedule { name }).await
}

async fn pause_schedule_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    schedule_route(pod, headers, body, |name| Command::PauseSchedule { name }).await
}

async fn resume_schedule_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    schedule_route(pod, headers, body, |name| Command::ResumeSchedule { name }).await
}

async fn reopen_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let message = match field(&body, "message", 20_000) {
        Ok(m) => m,
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
        match state.runs.get(&id).map(|r| r.task_id.clone()) {
            Some(t) if !t.is_empty() && state::schedule_of_task(&t).is_none() => t,
            _ => {
                return refuse(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "that run is not a task's",
                );
            }
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
        m.insert(id.to_owned(), (std::time::Instant::now(), log.clone()));
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
            Ok((runner, st))
        })
        .await;
    let (runner, st) = match read {
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
        &id,
        log.as_ref(),
        follower::now_ms(),
    ))
    .into_response()
}

/// The adapter as the server builds it.
pub fn adapter() -> Arc<dyn AppAdapter> {
    Arc::new(AgentsAdapter::new(AgentsConfig::from_env()))
}
