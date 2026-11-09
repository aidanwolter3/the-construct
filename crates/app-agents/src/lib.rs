//! The `agents` data source (decision `agents-data-source`, issue #165):
//! Claude Code sessions run by an external **agent runner**, shown and
//! controlled from the pod. The pod is the dashboard and the "start work"
//! button; the sessions themselves are watched and steered in the Claude
//! app.
//!
//! The runner speaks CAP v1 ([`cap`]). The pod signs in with the runner's
//! URL and a token (`POST /v1/agents/sign-in`, from the Agents layout's
//! form), stored sealed. A background follower per pod ([`follower`]) takes
//! a snapshot, follows the event stream from its cursor and keeps the
//! result ([`state`], [`db`]), so the feeds answer while the runner is away.
//!
//! - Feeds: `sessions` (`agents:status`, `agents:sessions`), `sandboxes`,
//!   `checks`, `schedules`; a data path for one session's timeline.
//! - Actions: stop a session, wake a sandbox, start a task, run, pause and
//!   resume a schedule. Commands are a fixed set ([`cap::Command`]).
//! - Notifications ([`alerts`]) in the layout that shows the source.
//! - Tasks wiring ([`tasks`]): a linked collection's queued rows start
//!   sessions, and their events come back to the rows.
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
            doc: "The agent runner's Claude Code sessions, active first (the ones that need \
                  you before them), and agents:status: whether the pod is signed in to a \
                  runner and reaches it, with a one-line summary such as 2 working · 1 needs you.",
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
        doc: "One session (agents://sessions/<id>) and its recent events, oldest first.",
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
            doc: "Stop a session: {session_id}. Wrap it in action.confirm.",
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
            doc: "Start a session: {workspace, title, prompt, task_id?}. 429 when the runner is \
                  at its limit.",
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
                    "e.g. 2 working · 1 needs you, or Nothing running (idle sessions don't count)",
                ),
                (
                    "working_count",
                    "sessions starting or working (not idle ones)",
                ),
                ("needs_you_count", "sessions waiting for you"),
                ("color", "#RRGGBB for the summary"),
                (
                    "updated_text",
                    "when the runner last answered, e.g. 2 min ago",
                ),
            ],
        },
        EntityDoc {
            uri: "agents://sessions/{id}",
            doc: "A Claude Code session (agents:sessions).",
            fields: &[
                ("id", "the session id"),
                ("title", "what it works on"),
                (
                    "status",
                    "starting, working, waiting_permission, waiting_input, idle, finished, failed or stale",
                ),
                ("status_text", "e.g. Working, Needs you, Finished"),
                ("status_color", "#RRGGBB"),
                ("active", "true until it finishes or fails"),
                ("needs_you", "true while it waits for a permission or input"),
                ("current_tool", "e.g. Bash: npm test"),
                ("summary", "what it did (once finished)"),
                (
                    "pr_url",
                    "its pull request (https://github.com/.../pull/N), or empty",
                ),
                ("error", "why it failed"),
                ("cost_text", "e.g. $0.42"),
                ("tokens_text", "e.g. 12000 tokens"),
                ("elapsed_text", "how long it has run, e.g. 12 min"),
                ("updated_text", "e.g. 2 min ago"),
                ("task_id", "the Tasks row or schedule run it is for"),
                ("schedule", "the schedule it runs for, or empty"),
                ("sandbox", "the sandbox it runs in"),
                ("workspace", "the runner workspace"),
                ("claude_url", "opens it in the Claude app, or empty"),
            ],
        },
        EntityDoc {
            uri: "agents://sandboxes/{name}",
            doc: "A sandbox sessions run in (agents:sandboxes).",
            fields: &[
                ("name", "its name"),
                ("state", "awake or asleep"),
                ("awake", "true while awake"),
                ("state_text", "Awake or Asleep"),
                ("color", "#RRGGBB"),
                ("last_seen_text", "e.g. 5 min ago"),
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
                ("last_status", "finished, failed, missed, ..."),
                ("last_status_text", "e.g. Finished"),
                ("last_status_color", "#RRGGBB"),
                ("last_summary", "what the last run did"),
                ("last_error", "why the last run failed or was missed"),
                ("last_cost_text", "e.g. $0.42"),
                ("last_session_id", "the last run's session"),
                ("last_run_text", "when the last run was due, e.g. 3 h ago"),
            ],
        },
        EntityDoc {
            uri: "agents://events/{seq}",
            doc: "A line of a session's timeline (agents:timeline:<id>).",
            fields: &[
                ("seq", "the event's number"),
                ("type", "e.g. tool.used"),
                ("text", "e.g. Used Bash: npm test"),
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

const INSTRUCTIONS: &str = "The agents source shows Claude Code sessions an agent runner runs for \
the user (feeds sessions, sandboxes, checks, schedules), with actions to stop a session, wake a \
sandbox, start a task and run, pause or resume a schedule. The blueprint layout agents is a whole \
dashboard: reference it ({type: core://Ref, app: agents, part: layout}) and link it from home, \
where agents:status's summary_text (e.g. 2 working · 1 needs you) makes a one-line summary. The \
pod signs in to the runner from that layout's form (the runner's URL and token): never ask for the \
token in the conversation. agents_link_tasks links a tasks collection (a title and a choice status \
with the option queued): its rows that become queued start sessions, and their results come back \
to the rows.";

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
                    let mut st = db::state(s)?;
                    if let Some((state, _)) = st.as_mut() {
                        tasks::close_done(s, state)?;
                    }
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

    /// The follower sends the notifications itself; polling keeps it
    /// running for pods with a registered device.
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

    fn mcp_tools(&self) -> Vec<Value> {
        vec![json!({
            "name": "agents_link_tasks",
            "description": "Link a tasks collection to the agent runner (or unlink it with collection null). \
        The collection needs a title field and a choice field status with the option queued. Linking adds what \
        the wiring needs: the status options todo, started, review and failed, and optional text fields \
        session_id, workspace and diagnosis. Then a row whose status becomes queued starts a session on the \
        runner (prompt: the title and notes; workspace: the row's workspace, else this default); the row goes \
        to started, then review with the summary and pull request appended to diagnosis, or back to todo with \
        the error if the session fails.",
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
        CapError::Status(429, d) => refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "runner_busy",
            if d.is_empty() {
                "the runner is running as many sessions as it may; try again later".to_owned()
            } else {
                d
            },
        ),
        CapError::Status(s @ (400 | 404), d) => refuse(
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
    if status == 404 {
        "the runner doesn't know that".into()
    } else {
        "the runner refused the request".into()
    }
}

async fn send(pod: &Pod, cmd: Command) -> Response {
    let Some(a) = pod.adapter::<AgentsAdapter>() else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no agents source");
    };
    let runner = match pod.blocking(db::runner).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return refuse(
                StatusCode::CONFLICT,
                "not_signed_in",
                "the pod is not signed in to an agent runner",
            );
        }
        Err(e) => return e.into_response(),
    };
    match cap::command(&a.clients, &runner, &cmd).await {
        Ok(answer) => {
            let mut out = json!({"ok": true});
            for k in ["run_key", "task_id"] {
                if let Some(v) = answer.get(k).and_then(Value::as_str) {
                    out[k] = json!(text::clean(v, 200));
                }
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
    match field(&body, "session_id", 120) {
        Ok(session_id) => send(&pod, Command::StopSession { session_id }).await,
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
    send(&pod, Command::Wake { workspace, minutes }).await
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
        Command::StartTask {
            workspace,
            task_id,
            title,
            prompt,
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

#[derive(serde::Deserialize)]
struct TimelineQuery {
    id: String,
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
    if id.is_empty() || id.len() > 120 {
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "\"id\" names a session",
        );
    }
    let key = id.clone();
    let read = pod
        .blocking(move |s| {
            let signed_in = db::runner(s)?.is_some();
            let st = db::state(s)?.filter(|_| signed_in);
            Ok((st, db::timeline(s, &key)?))
        })
        .await;
    match read {
        Ok((st, events)) => Json(bundle::timeline(
            st.as_ref(),
            &id,
            &events,
            follower::now_ms(),
        ))
        .into_response(),
        Err(e) => e.into_response(),
    }
}

/// The adapter as the server builds it.
pub fn adapter() -> Arc<dyn AppAdapter> {
    Arc::new(AgentsAdapter::new(AgentsConfig::from_env()))
}
