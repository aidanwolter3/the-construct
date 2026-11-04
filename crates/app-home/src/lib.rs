//! The home app: The Construct as a phone's launcher. A dark sketchpad home
//! screen with pinned apps, every app (the pod's and the device's) a swipe
//! up away, and + to have Claude build a new app.
//!
//! - `GET /v1/workspaces/launcher`: the `home` feed: the pod's apps
//!   (`home:apps`), the pins (`home:pins`) and the open app requests
//!   (`home:requests`). The device's own apps (`device:apps`) are filled in
//!   by the client and never reach the pod.
//! - `POST /v1/home/pins`, `POST /v1/home/pins/remove`: pin and unpin.
//! - `POST /v1/home/requests {id, prompt}`: ask for an app; the builder
//!   ([`builder`]) runs Claude with the pod's MCP tools and the request
//!   ends `ready` with the new app's id, or `failed` with a reason.
//! - `GET /v1/home/screens/request?id=<id>`: one request (its screen).

pub mod builder;
pub mod db;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::store::Migration;
use pod_core::util::now_secs;
use pod_core::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, SourceManifest, json_error,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub use builder::BuilderConfig;

/// The adapter name: the prefix of its tables and routes.
pub const NAME: &str = "home";

/// The home feed (`AppInfo::feeds` entry `("home", "home")`).
pub const HOME_FEED: &str = "home";

/// The home app. Its id is `launcher`: `home` was the removed counter app
/// (D39), whose layout history staging's pods still hold.
pub const APP: AppInfo = AppInfo {
    id: "launcher",
    title: "Home",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(NAME, HOME_FEED)],
};

/// Most pins.
pub const MAX_PINS: i64 = 40;
/// Most app requests per pod per day.
pub const MAX_REQUESTS_PER_DAY: i64 = 20;
/// Longest request, in characters.
pub const MAX_PROMPT_CHARS: usize = 2000;
/// Longest pin title and target.
pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_TARGET_CHARS: usize = 200;

pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Home",
    sign_in: None,
    entity_schemes: &["home"],
    feeds: &[FeedSpec {
        name: HOME_FEED,
        query_keys: &[db::APPS_QUERY, db::PINS_QUERY, db::REQUESTS_QUERY],
        doc: "The pod's apps by title, the apps pinned to the home screen, and the app requests still waiting, building or failed.",
    }],
    data_paths: &[PathSpec {
        pattern: "/v1/home/screens/request?id={id}",
        query_keys: &["home:request:{id}"],
        doc: "One app request (an empty list until it reaches the pod).",
    }],
    actions: &[
        PathSpec {
            pattern: "/v1/home/pins",
            query_keys: &[],
            doc: "Pin an app: {id, kind: construct|device, target (app id or package), title}.",
        },
        PathSpec {
            pattern: "/v1/home/pins/remove",
            query_keys: &[],
            doc: "Unpin: {id}.",
        },
        PathSpec {
            pattern: "/v1/home/requests",
            query_keys: &[],
            doc: "Ask Claude for a new app: {id, prompt}; it is built in the background.",
        },
    ],
    entities: &[
        EntityDoc {
            uri: "home://apps/{id}",
            doc: "An app of the pod, as the app list shows it (not the home app).",
            fields: &[
                ("id", "the app id (for action.app.open)"),
                ("title", "its title"),
                ("letter", "first letter of the title, upper case"),
                ("kind", "\"construct\""),
            ],
        },
        EntityDoc {
            uri: "home://pins/{id}",
            doc: "An app pinned to the home screen.",
            fields: &[
                ("id", "the pin id"),
                (
                    "kind",
                    "construct (an app of the pod) or device (an installed app)",
                ),
                ("target", "the app id, or the device app's package"),
                ("title", "what to show"),
                (
                    "unpinned",
                    "false (set on the device while an unpin is sent)",
                ),
            ],
        },
        EntityDoc {
            uri: "home://requests/{id}",
            doc: "An app asked for with +.",
            fields: &[
                ("id", "the request id"),
                ("prompt", "what the user asked for"),
                (
                    "title",
                    "the app's title once built, else the prompt's first line",
                ),
                ("status", "queued, building, ready or failed"),
                ("status_text", "a line of progress"),
                ("app_id", "the new app (ready)"),
                ("app_title", "its title (ready)"),
                ("error", "why it failed (failed)"),
            ],
        },
    ],
    dev: false,
};

// `FILES`: every `layouts/launcher/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The home adapter.
pub struct HomeAdapter {
    builder: Option<BuilderConfig>,
    client: reqwest::Client,
}

impl HomeAdapter {
    /// `builder`: the model the builder calls; `None`: requests fail with
    /// [`builder::NO_MODEL`].
    pub fn new(builder: Option<BuilderConfig>) -> Self {
        Self {
            builder,
            client: pod_core::net::builder(builder::CALL_TIMEOUT)
                .build()
                .expect("HTTP client"),
        }
    }

    pub fn builder(&self) -> Option<&BuilderConfig> {
        self.builder.as_ref()
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

impl AppAdapter for HomeAdapter {
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
            .route("/v1/home/pins", post(add_pin))
            .route("/v1/home/pins/remove", post(remove_pin))
            .route("/v1/home/requests", post(add_request))
            .route("/v1/home/screens/request", get(request_screen))
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
            if feed != HOME_FEED {
                return pod_core::unknown_feed(NAME, feed);
            }
            match home_bundle(pod).await {
                Ok(b) => Hydration::Data(b),
                Err(e) => Hydration::Error(e.into_response()),
            }
        })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let now = now_secs();
            if let Ok(n) = pod
                .blocking(move |s| s.with_conn(|c| db::fail_interrupted(c, now)))
                .await
                && n > 0
            {
                tracing::info!(requests = n, "app requests interrupted by a restart");
                pod.publish_data_all(NAME);
            }
        })
    }
}

/// The home feed's data: the app list as `GET /v1/apps` shows it (hidden
/// apps and dev apps left out unless shown, and without the home app).
async fn home_bundle(pod: &Pod) -> Result<protocol::DataBundle, pod_core::ApiError> {
    let builtin: Vec<AppInfo> = pod
        .catalog()
        .apps()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .copied()
        .collect();
    let all_builtin: Vec<String> = pod.catalog().apps().map(|a| a.id.to_owned()).collect();
    let now = now_secs();
    pod.blocking(move |s| {
        let hidden = pod_core::custom::hidden_apps(s)?;
        let custom = s.with_conn(pod_core::custom::custom_apps_on)?;
        let mut existing = all_builtin;
        existing.extend(custom.iter().map(|c| c.id.clone()));
        let listed: Vec<db::ListedApp> = builtin
            .iter()
            .map(|a| (a.id.to_owned(), a.title.to_owned()))
            .chain(custom.into_iter().map(|c| (c.id, c.title)))
            .filter(|(id, _)| id != APP.id && !hidden.contains(id))
            .map(|(id, title)| db::ListedApp { id, title })
            .collect();
        s.with_conn(|c| db::home_bundle(c, &listed, &existing, now))
    })
    .await
}

fn invalid(expected: Value) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request", "expected": expected }),
    )
}

/// `[A-Za-z0-9_-]{1,64}`: a client-made id (a `$nonce`).
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// An app id (`[A-Za-z0-9_-]`) or an Android package (`[A-Za-z0-9._]`).
fn valid_target(t: &str) -> bool {
    (1..=MAX_TARGET_CHARS).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn text<'a>(body: &'a Value, k: &str) -> &'a str {
    body[k].as_str().map(str::trim).unwrap_or("")
}

fn parse(headers: &HeaderMap, body: &Bytes) -> Result<Value, Box<Response>> {
    pod_core::parse_json_body(headers, body).map_err(|e| Box::new(e.into_response()))
}

/// `POST /v1/home/pins {id, kind, target, title}`: pin an app (last).
/// Pinning an app that is already pinned changes nothing.
async fn add_pin(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse(&headers, &body) {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let pin = db::Pin {
        id: text(&body, "id").to_owned(),
        kind: text(&body, "kind").to_owned(),
        target: text(&body, "target").to_owned(),
        title: text(&body, "title").to_owned(),
    };
    let title_len = pin.title.chars().count();
    if !valid_id(&pin.id)
        || !matches!(pin.kind.as_str(), "construct" | "device")
        || !valid_target(&pin.target)
        || !(1..=MAX_TITLE_CHARS).contains(&title_len)
    {
        return invalid(
            json!({ "id": "[A-Za-z0-9_-]{1,64}", "kind": "construct | device",
            "target": "an app id or package", "title": format!("1..={MAX_TITLE_CHARS} characters") }),
        );
    }
    let now = now_secs();
    let out = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                if db::pin_count(c)? >= MAX_PINS {
                    return Ok(None);
                }
                db::add_pin(c, &pin, now).map(Some)
            })
        })
        .await;
    match out {
        Ok(Some(id)) => {
            pod.publish_data_all(NAME);
            Json(json!({ "ok": true, "id": id })).into_response()
        }
        Ok(None) => json_error(
            StatusCode::CONFLICT,
            json!({ "error": "too_many_pins", "max": MAX_PINS }),
        ),
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/home/pins/remove {id}`: unpin (an unknown pin is fine).
async fn remove_pin(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse(&headers, &body) {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let id = text(&body, "id").to_owned();
    if !valid_id(&id) {
        return invalid(json!({ "id": "[A-Za-z0-9_-]{1,64}" }));
    }
    match pod
        .blocking(move |s| s.with_conn(|c| db::remove_pin(c, &id)))
        .await
    {
        Ok(_) => {
            pod.publish_data_all(NAME);
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/home/requests {id, prompt}`: ask for an app. 202: it is being
/// built (the request's status follows); the same id again changes nothing.
async fn add_request(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse(&headers, &body) {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let id = text(&body, "id").to_owned();
    let prompt = text(&body, "prompt").to_owned();
    if !valid_id(&id) || !(1..=MAX_PROMPT_CHARS).contains(&prompt.chars().count()) {
        return invalid(json!({ "id": "[A-Za-z0-9_-]{1,64}",
            "prompt": format!("1..={MAX_PROMPT_CHARS} characters") }));
    }
    let now = now_secs();
    let key = id.clone();
    let added = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                if db::request(c, &key)?.is_some() {
                    return Ok(Some(false));
                }
                if db::requests_since(c, now - 86_400)? >= MAX_REQUESTS_PER_DAY {
                    return Ok(None);
                }
                db::add_request(c, &key, &prompt, now).map(Some)
            })
        })
        .await;
    match added {
        Ok(Some(new)) => {
            if new {
                pod.publish_data_all(NAME);
                builder::spawn(&pod, id.clone());
            }
            (StatusCode::ACCEPTED, Json(json!({ "ok": true, "id": id }))).into_response()
        }
        Ok(None) => json_error(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "too_many_requests", "max_per_day": MAX_REQUESTS_PER_DAY }),
        ),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct RequestParams {
    #[serde(default)]
    id: String,
}

/// `GET /v1/home/screens/request?id=<id>`: the request screen's data.
async fn request_screen(State(pod): State<Pod>, Query(p): Query<RequestParams>) -> Response {
    let id = p.id.trim().to_owned();
    if !valid_id(&id) {
        return invalid(json!({ "id": "request id" }));
    }
    match pod
        .blocking(move |s| s.with_conn(|c| db::request_bundle(c, &id)))
        .await
    {
        Ok(b) => Json(b).into_response(),
        Err(e) => e.into_response(),
    }
}
