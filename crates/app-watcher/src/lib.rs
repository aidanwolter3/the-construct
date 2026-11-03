//! The watcher (issue #58, decision `watcher`): polls websites for events,
//! matches them against rules and sends a notification for each new match.
//! A website is a row of config (a *target*), not code: its URL, how often
//! to check it and which extractor reads it (`ics`, `jsonld`, `llm`, or
//! `auto`). Rules are rows too (keywords, venue, days, a date window).
//!
//! - Feeds: `matches` (upcoming matched events, `watcher:matches`) and
//!   `targets` (`watcher:targets` with each target's health,
//!   `watcher:rules`).
//! - Screens: `GET /v1/watcher/screens/event?id=…`,
//!   `GET /v1/watcher/screens/target?id=…`.
//! - Actions: `POST /v1/watcher/targets/{create,update,refresh}`,
//!   `/v1/watcher/rules/{create,update}`, `/v1/watcher/events/seen`.
//! - MCP tools for Claude: `watcher_status`, `watcher_set_target`,
//!   `watcher_set_rule`, `watcher_preview`, `watcher_refresh`.
//!
//! Checks run in the background ([`check`]): from the notification poller
//! every minute while a device is registered, and when the app opens with
//! a target overdue (a pod that slept still refreshes). No sign-in: the
//! data is the pod's.

pub mod check;
pub mod db;
pub mod edit;
pub mod extract;
pub mod html;
mod mcp;
pub mod rules;
pub mod when;

use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::store::Migration;
use pod_core::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    PollCtx, PollOutcome, RequestCtx, SourceManifest,
};
use protocol::{Importance, NotificationChannel};
use serde::Deserialize;
use serde_json::{Value, json};

pub use extract::LlmConfig;

/// The adapter name: the prefix of its tables and routes, and the source id.
pub const NAME: &str = "watcher";
/// The app.
pub const APP_ID: &str = "watcher";
/// Its notification channel.
pub const CHANNEL: &str = "matches";

pub const MATCHES_FEED: &str = "matches";
pub const TARGETS_FEED: &str = "targets";

/// The Watcher app.
pub const APP: AppInfo = AppInfo {
    id: APP_ID,
    title: "Watcher",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(NAME, MATCHES_FEED), (NAME, TARGETS_FEED)],
};

/// What layouts may use of the watcher.
pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Watcher",
    sign_in: None,
    entity_schemes: &["watcher"],
    feeds: &[
        FeedSpec {
            name: MATCHES_FEED,
            query_keys: &[db::MATCHES_QUERY],
            doc: "Upcoming events a rule matched, soonest first.",
        },
        FeedSpec {
            name: TARGETS_FEED,
            query_keys: &[db::TARGETS_QUERY, db::RULES_QUERY],
            doc: "The watched websites with their health, and the rules.",
        },
    ],
    data_paths: &[
        PathSpec {
            pattern: "/v1/watcher/screens/event?id={id}",
            query_keys: &["watcher:event:{id}"],
            doc: "One event (watcher://events/{id}), also as a one-item list.",
        },
        PathSpec {
            pattern: "/v1/watcher/screens/target?id={id}",
            query_keys: &["watcher:target:{id}", "watcher:target:{id}:events"],
            doc: "One website (also as a one-item list) and its upcoming events.",
        },
    ],
    actions: &[
        PathSpec {
            pattern: "/v1/watcher/targets/create",
            query_keys: &[],
            doc: "Watch a website: payload {url, title?, extractor? (auto|ics|jsonld|llm), interval_minutes?, hint?, details?, utc_offset?, client_id: \"$nonce\"}.",
        },
        PathSpec {
            pattern: "/v1/watcher/targets/update",
            query_keys: &[],
            doc: "Change a website: payload {id, <field>: value}; archive with archived: true.",
        },
        PathSpec {
            pattern: "/v1/watcher/targets/refresh",
            query_keys: &[],
            doc: "Check a website now: payload {id}.",
        },
        PathSpec {
            pattern: "/v1/watcher/rules/create",
            query_keys: &[],
            doc: "Add a rule: payload {title?, keywords (any of, comma separated), exclude?, venue?, days?, within_days?, client_id: \"$nonce\"}.",
        },
        PathSpec {
            pattern: "/v1/watcher/rules/update",
            query_keys: &[],
            doc: "Change a rule: payload {id, <field>: value}; archive with archived: true.",
        },
        PathSpec {
            pattern: "/v1/watcher/events/seen",
            query_keys: &[],
            doc: "Mark an event's matches seen: payload {id, seen}.",
        },
    ],
    entities: &[
        EntityDoc {
            uri: "watcher://events/{id}",
            doc: "An event found on a watched website.",
            fields: &[
                ("id", "stable id"),
                ("title", "as the site writes it"),
                (
                    "start",
                    "the site's wall clock, YYYY-MM-DDTHH:MM (offset when known)",
                ),
                ("date_text", "e.g. Fri Nov 6"),
                ("time_text", "e.g. 7:30 PM, or empty"),
                ("when_text", "date · time"),
                ("meta", "date · time · venue"),
                ("venue", "where"),
                ("description", "everything the site says (lineups)"),
                ("url", "the event's own page, or empty"),
                ("source", "the website's title"),
                ("target_id", "the website's id"),
                ("matched", "a live rule matched it"),
                ("rules_text", "Matched <rule titles>"),
                ("seen", "its matches were marked seen"),
            ],
        },
        EntityDoc {
            uri: "watcher://targets/{id}",
            doc: "A watched website.",
            fields: &[
                ("id", "target id"),
                ("title", "name"),
                ("url", "the page checked"),
                ("host", "its host"),
                ("extractor", "auto, ics, jsonld or llm"),
                ("interval_text", "e.g. every 6 h"),
                ("health", "ok, pending, failing, empty or archived"),
                (
                    "health_text",
                    "one line, e.g. 4 upcoming events · checked 5 min ago",
                ),
                ("failing", "health is failing or empty"),
                ("upcoming_count", "upcoming events found"),
                ("meta", "host · extractor · interval"),
                ("archived", "archived"),
            ],
        },
        EntityDoc {
            uri: "watcher://rules/{id}",
            doc: "A rule.",
            fields: &[
                ("id", "rule id"),
                ("title", "name"),
                ("summary", "its conditions in one line"),
                ("keywords", "any-of phrases, comma separated"),
                ("exclude", "none-of phrases, comma separated"),
                ("archived", "archived"),
            ],
        },
    ],
    dev: false,
};

// `FILES`: every `layouts/watcher/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// Server configuration (`CONSTRUCT_WATCHER_*`, README).
#[derive(Debug, Clone, Default)]
pub struct WatcherConfig {
    /// The model behind the `llm` extractor; `None`: it is off.
    pub llm: Option<LlmConfig>,
    /// Targets may be on private addresses (local development, tests).
    pub allow_private_hosts: bool,
}

impl WatcherConfig {
    /// From the environment: `CONSTRUCT_WATCHER_ANTHROPIC_API_KEY`,
    /// `CONSTRUCT_WATCHER_LLM_MODEL`,
    /// `CONSTRUCT_WATCHER_LLM_URL`, `CONSTRUCT_WATCHER_PRIVATE_HOSTS=1`.
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let key = var("CONSTRUCT_WATCHER_ANTHROPIC_API_KEY");
        Self {
            llm: key.map(|api_key| LlmConfig {
                api_key,
                model: var("CONSTRUCT_WATCHER_LLM_MODEL")
                    .unwrap_or_else(|| extract::DEFAULT_MODEL.into()),
                base_url: var("CONSTRUCT_WATCHER_LLM_URL")
                    .unwrap_or_else(|| "https://api.anthropic.com".into()),
            }),
            allow_private_hosts: var("CONSTRUCT_WATCHER_PRIVATE_HOSTS").as_deref() == Some("1"),
        }
    }
}

/// The watcher adapter.
pub struct WatcherAdapter {
    config: WatcherConfig,
    fetch_client: reqwest::Client,
    llm_client: reqwest::Client,
}

impl WatcherAdapter {
    pub fn new(config: WatcherConfig) -> Self {
        let fetch = if config.allow_private_hosts {
            pod_core::net::builder(check::FETCH_TIMEOUT)
        } else {
            pod_core::net::public_builder(check::FETCH_TIMEOUT)
        };
        Self {
            fetch_client: fetch.build().expect("HTTP client"),
            llm_client: pod_core::net::builder(check::LLM_TIMEOUT)
                .build()
                .expect("HTTP client"),
            config,
        }
    }

    pub fn config(&self) -> &WatcherConfig {
        &self.config
    }

    /// The client for watched websites (public addresses only unless
    /// [`WatcherConfig::allow_private_hosts`]).
    pub fn fetch_client(&self) -> &reqwest::Client {
        &self.fetch_client
    }

    pub fn llm_client(&self) -> &reqwest::Client {
        &self.llm_client
    }
}

impl AppAdapter for WatcherAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn router(&self) -> Router<Pod> {
        Router::new()
            .route("/v1/watcher/screens/event", get(event_screen))
            .route("/v1/watcher/screens/target", get(target_screen))
            .route("/v1/watcher/targets/create", post(create_target))
            .route("/v1/watcher/targets/update", post(update_target))
            .route("/v1/watcher/targets/refresh", post(refresh_target))
            .route("/v1/watcher/rules/create", post(create_rule))
            .route("/v1/watcher/rules/update", post(update_rule))
            .route("/v1/watcher/events/seen", post(mark_seen))
    }

    fn migrations(&self) -> &'static [Migration] {
        db::MIGRATIONS
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            let now = pod_core::util::now_secs();
            let bundle = match feed {
                MATCHES_FEED => {
                    // Opening the app checks overdue targets (the poller
                    // runs only while a device is registered).
                    let overdue = pod
                        .blocking(|s| s.with_conn(db::next_due))
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|t| t <= now);
                    if overdue {
                        check::spawn_run(pod);
                    }
                    pod.blocking(move |s| s.with_conn(|c| db::matches_bundle(c, now)))
                        .await
                }
                TARGETS_FEED => {
                    pod.blocking(move |s| s.with_conn(|c| db::targets_bundle(c, now)))
                        .await
                }
                other => return pod_core::unknown_feed(NAME, other),
            };
            match bundle {
                Ok(b) => Hydration::Data(b),
                Err(e) => Hydration::Error(e.into_response()),
            }
        })
    }

    fn notification_channels(&self) -> Vec<NotificationChannel> {
        vec![NotificationChannel {
            app_id: APP_ID.into(),
            id: CHANNEL.into(),
            title: "Matching events".into(),
            description: "A watched website lists an event one of your rules matches.".into(),
            default_enabled: true,
            importance: Importance::default(),
        }]
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        _ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        Box::pin(async move {
            // Checks can take minutes (model calls), longer than a poll
            // may: they run in the background and notify themselves.
            check::spawn_run(pod);
            let now = pod_core::util::now_secs();
            let next = pod
                .blocking(|s| s.with_conn(db::next_due))
                .await
                .ok()
                .flatten();
            PollOutcome {
                next_in: Some(Duration::from_secs(
                    next.map_or(3600, |t| (t - now).clamp(60, 3600)) as u64,
                )),
                ..Default::default()
            }
        })
    }

    fn mcp_tools(&self) -> Vec<Value> {
        mcp::tools()
    }

    fn mcp_instructions(&self) -> Option<&'static str> {
        Some(mcp::INSTRUCTIONS)
    }

    fn mcp_call<'a>(
        &'a self,
        pod: &'a Pod,
        tool: &'a str,
        args: &'a Value,
    ) -> Option<BoxFuture<'a, Result<Value, Value>>> {
        mcp::call(self, pod, tool, args)
    }
}

// ---------- routes ----------

fn answer(r: Result<Value, edit::Refusal>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err((status, body)) => pod_core::json_error(status, body),
    }
}

fn body_of(bytes: &Bytes) -> Result<Value, edit::Refusal> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Err(edit::invalid("the body is a JSON object")),
    }
}

#[derive(Deserialize)]
struct IdParam {
    #[serde(default)]
    id: String,
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn event_screen(State(pod): State<Pod>, Query(p): Query<IdParam>) -> Response {
    if !valid_id(&p.id) {
        return answer(Err(edit::invalid("\"id\" is an event id")));
    }
    let id = p.id.clone();
    match pod
        .blocking(move |s| s.with_conn(|c| db::event_screen(c, &id)))
        .await
    {
        Ok(Some(b)) => Json(b).into_response(),
        Ok(None) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "event_not_found", "id": p.id }),
        ),
        Err(e) => e.into_response(),
    }
}

async fn target_screen(State(pod): State<Pod>, Query(p): Query<IdParam>) -> Response {
    if !valid_id(&p.id) {
        return answer(Err(edit::invalid("\"id\" is a target id")));
    }
    let id = p.id.clone();
    let now = pod_core::util::now_secs();
    match pod
        .blocking(move |s| s.with_conn(|c| db::target_screen(c, &id, now)))
        .await
    {
        Ok(Some(b)) => Json(b).into_response(),
        Ok(None) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "target_not_found", "id": p.id }),
        ),
        Err(e) => e.into_response(),
    }
}

async fn create_target(State(pod): State<Pod>, body: Bytes) -> Response {
    let r = match body_of(&body) {
        Ok(mut b) => {
            b.as_object_mut().map(|o| o.remove("id"));
            edit::save_target(&pod, b).await
        }
        Err(e) => Err(e),
    };
    answer(r)
}

async fn update_target(State(pod): State<Pod>, body: Bytes) -> Response {
    let r = match body_of(&body) {
        Ok(b) if b["id"].as_str().is_some_and(|i| !i.is_empty()) => {
            edit::save_target(&pod, b).await
        }
        Ok(_) => Err(edit::invalid("\"id\" is required")),
        Err(e) => Err(e),
    };
    answer(r)
}

async fn refresh_target(State(pod): State<Pod>, body: Bytes) -> Response {
    let b = match body_of(&body) {
        Ok(b) => b,
        Err(e) => return answer(Err(e)),
    };
    let Some(id) = b["id"].as_str().map(str::to_owned) else {
        return answer(Err(edit::invalid("\"id\" is required")));
    };
    let lookup = id.clone();
    match pod
        .blocking(move |s| s.with_conn(|c| db::request_refresh(c, Some(&lookup))))
        .await
    {
        Ok(0) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "target_not_found", "id": id }),
        ),
        Ok(_) => {
            check::spawn_run(&pod);
            Json(json!({ "id": id, "refreshing": true })).into_response()
        }
        Err(e) => e.into_response(),
    }
}

async fn create_rule(State(pod): State<Pod>, body: Bytes) -> Response {
    let r = match body_of(&body) {
        Ok(mut b) => {
            b.as_object_mut().map(|o| o.remove("id"));
            edit::save_rule(&pod, b).await
        }
        Err(e) => Err(e),
    };
    answer(r)
}

async fn update_rule(State(pod): State<Pod>, body: Bytes) -> Response {
    let r = match body_of(&body) {
        Ok(b) if b["id"].as_str().is_some_and(|i| !i.is_empty()) => edit::save_rule(&pod, b).await,
        Ok(_) => Err(edit::invalid("\"id\" is required")),
        Err(e) => Err(e),
    };
    answer(r)
}

async fn mark_seen(State(pod): State<Pod>, body: Bytes) -> Response {
    let b = match body_of(&body) {
        Ok(b) => b,
        Err(e) => return answer(Err(e)),
    };
    let Some(id) = b["id"].as_str().map(str::to_owned).filter(|i| valid_id(i)) else {
        return answer(Err(edit::invalid("\"id\" is required")));
    };
    let seen = match &b["seen"] {
        Value::Bool(v) => *v,
        Value::String(s) => s != "false",
        _ => true,
    };
    let lookup = id.clone();
    match pod
        .blocking(move |s| s.with_conn(|c| db::set_seen(c, &lookup, seen)))
        .await
    {
        Ok(true) => {
            pod.publish_data_all(NAME);
            Json(json!({ "id": id, "seen": seen })).into_response()
        }
        Ok(false) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "event_not_found", "id": id }),
        ),
        Err(e) => e.into_response(),
    }
}
