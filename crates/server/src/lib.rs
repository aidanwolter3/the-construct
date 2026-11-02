//! The Construct backend: serves fixture hydration bundles, persists
//! counter mutations in SQLite (M5, see [`store`]) and adapts a Pixelfed
//! instance (M6, see [`pixelfed`]; the pod owns the upstream token).
//! Workspace layouts are code (`layouts/<app>/NNNN.json`), synced into one
//! append-only version history per app at startup, served read-only and
//! pushed over SSE (see [`layouts`]). [`agent`] (prompt -> layout patch) is
//! kept in the library but not routed: clients cannot change layouts.
//!
//! Fixtures are embedded at compile time and parsed into
//! [`protocol::WorkspaceSpec`] once, so an invalid fixture fails loudly
//! (panics on first access, which the integration tests exercise).

pub mod agent;
pub mod layouts;
pub mod pixelfed;
pub mod store;

use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pixelfed::upstream::Upstream;
use protocol::WorkspaceSpec;
use serde_json::{Value, json};
use store::Store;

static COUNTER_HOME_JSON: &str = include_str!("../../../fixtures/counter_home_bundle.json");
static PIXELFED_HOME_JSON: &str = include_str!("../../../fixtures/pixelfed_home_bundle.json");

static COUNTER_HOME: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(COUNTER_HOME_JSON).expect("fixtures/counter_home_bundle.json is invalid")
});
static PIXELFED_HOME: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(PIXELFED_HOME_JSON).expect("fixtures/pixelfed_home_bundle.json is invalid")
});

/// Look up a workspace bundle by id.
pub fn workspace(id: &str) -> Option<&'static WorkspaceSpec> {
    match id {
        "home" => Some(&COUNTER_HOME),
        "pixelfed_demo" => Some(&PIXELFED_HOME),
        _ => None,
    }
}

/// Force-parse all embedded fixtures. Call at startup to fail fast.
pub fn validate_fixtures() {
    LazyLock::force(&COUNTER_HOME);
    LazyLock::force(&PIXELFED_HOME);
}

/// The counter entity injected into the `home` bundle.
const HOME_COUNTER_URI: &str = "core://counters/home";

/// A pending injected fault (test hook): the next `remaining` mutation
/// requests get `status` without being applied.
#[derive(Debug, Clone, Copy)]
struct Fault {
    status: StatusCode,
    remaining: u32,
}

/// Default Pixelfed instance: the local fake (`crates/fake-pixelfed`).
pub const DEFAULT_PIXELFED_URL: &str = "http://127.0.0.1:8081";

/// Default pod session lifetime (`CONSTRUCT_SESSION_TTL_DAYS`).
pub const DEFAULT_SESSION_TTL_DAYS: i64 = 90;

/// Which Pixelfed instance the pod talks to (single tenant), and who may own it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedConfig {
    /// Base URL for server->upstream calls (`PIXELFED_INSTANCE_URL`).
    pub instance_url: String,
    /// Base URL for browser-facing links such as the authorize endpoint
    /// (`PIXELFED_PUBLIC_URL`, default = instance URL).
    pub public_url: String,
    /// Explicit owner (`PIXELFED_OWNER_ACCOUNT_ID`): only this upstream
    /// account id may sign in. Unset: the first account to sign in is pinned.
    pub owner_account_id: Option<String>,
    /// `PIXELFED_ALLOW_ACCOUNT_CHANGE=1`: any account may sign in and replace
    /// the stored one (the pre-pinning behaviour).
    pub allow_account_change: bool,
    /// Extra media hosts the proxy may fetch (`PIXELFED_MEDIA_HOSTS`,
    /// comma-separated `host`, `host:port` or `https://host[:port]`), e.g. a CDN.
    pub media_hosts: Vec<String>,
    /// Extra exact redirect URIs accepted by `/v1/auth/pixelfed/app`
    /// (`PIXELFED_EXTRA_REDIRECTS`, comma-separated).
    pub extra_redirects: Vec<String>,
    /// Pod sessions older than this are invalid (`CONSTRUCT_SESSION_TTL_DAYS`).
    pub session_ttl_secs: i64,
}

impl Default for PixelfedConfig {
    fn default() -> Self {
        Self::new(DEFAULT_PIXELFED_URL, None)
    }
}

impl PixelfedConfig {
    pub fn new(instance_url: &str, public_url: Option<&str>) -> Self {
        let instance_url = normalize_base(instance_url);
        let public_url = public_url
            .map(normalize_base)
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| instance_url.clone());
        Self {
            instance_url,
            public_url,
            owner_account_id: None,
            allow_account_change: false,
            media_hosts: Vec::new(),
            extra_redirects: Vec::new(),
            session_ttl_secs: DEFAULT_SESSION_TTL_DAYS * 86_400,
        }
    }

    /// From `PIXELFED_INSTANCE_URL`, `PIXELFED_PUBLIC_URL`,
    /// `PIXELFED_OWNER_ACCOUNT_ID`, `PIXELFED_ALLOW_ACCOUNT_CHANGE`,
    /// `PIXELFED_MEDIA_HOSTS`, `PIXELFED_EXTRA_REDIRECTS`, `CONSTRUCT_SESSION_TTL_DAYS`.
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let list = |k: &str| -> Vec<String> {
            var(k)
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let instance =
            var("PIXELFED_INSTANCE_URL").unwrap_or_else(|| DEFAULT_PIXELFED_URL.to_owned());
        let ttl_days = match var("CONSTRUCT_SESSION_TTL_DAYS").map(|v| v.parse::<i64>()) {
            None => DEFAULT_SESSION_TTL_DAYS,
            Some(Ok(d)) if (1..=36_500).contains(&d) => d,
            Some(_) => {
                tracing::warn!(
                    "CONSTRUCT_SESSION_TTL_DAYS is not a number of days in 1..=36500; using {DEFAULT_SESSION_TTL_DAYS}"
                );
                DEFAULT_SESSION_TTL_DAYS
            }
        };
        Self {
            owner_account_id: var("PIXELFED_OWNER_ACCOUNT_ID"),
            allow_account_change: var("PIXELFED_ALLOW_ACCOUNT_CHANGE")
                .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            media_hosts: list("PIXELFED_MEDIA_HOSTS"),
            extra_redirects: list("PIXELFED_EXTRA_REDIRECTS"),
            session_ttl_secs: ttl_days * 86_400,
            ..Self::new(&instance, var("PIXELFED_PUBLIC_URL").as_deref())
        }
    }

    /// Origins the media proxy may fetch: the instance and public URLs plus
    /// [`Self::media_hosts`].
    pub fn media_allowlist(&self) -> pixelfed::MediaAllowlist {
        pixelfed::MediaAllowlist::new(
            [self.instance_url.as_str(), self.public_url.as_str()],
            self.media_hosts.iter().map(String::as_str),
        )
    }

    /// Only the configured instance (by its instance or public URL) is accepted
    /// in auth calls: the pod never talks to a client-chosen host.
    pub fn accepts(&self, url: &str) -> bool {
        let url = normalize_base(url);
        url.eq_ignore_ascii_case(&self.instance_url) || url.eq_ignore_ascii_case(&self.public_url)
    }
}

fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

/// Shared server state.
pub struct AppState {
    store: Store,
    test_hooks: bool,
    fault: Mutex<Option<Fault>>,
    pixelfed: PixelfedConfig,
    /// [`PixelfedConfig::media_allowlist`], parsed once.
    media_allow: pixelfed::MediaAllowlist,
    upstream: Upstream,
    /// Serializes upstream token refreshes (Passport rotates the refresh
    /// token, so two concurrent refreshes would revoke each other).
    pixelfed_refresh: tokio::sync::Mutex<()>,
    /// `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists the dev apps.
    show_dev_apps: bool,
    /// New app versions, fanned out to SSE subscribers.
    layout_tx: tokio::sync::broadcast::Sender<layouts::LayoutEvent>,
    /// SSE keepalive interval (15 s; shorter in tests).
    sse_keepalive: std::time::Duration,
    /// Set by [`AppState::shutdown`]: open SSE streams end.
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl AppState {
    /// State over `store`, after syncing the embedded layout files into it
    /// ([`layouts::sync`]). Panics with the sync error; see [`Self::try_new`].
    pub fn new(store: Store) -> Self {
        Self::try_new(store).unwrap_or_else(|e| panic!("{e}"))
    }

    /// [`Self::new`], returning the layout sync error (a changed, out-of-order
    /// or invalid layout file) instead of panicking.
    pub fn try_new(store: Store) -> Result<Self, String> {
        let report = layouts::sync(&store)?;
        for warning in &report.missing {
            tracing::warn!("{warning}");
        }
        for path in &report.adopted {
            tracing::info!(file = %path, "layout file hash recorded (deployed before migration v8)");
        }
        for (path, version) in &report.appended {
            tracing::info!(file = %path, version, "layout file deployed as a new version");
        }
        let pixelfed = PixelfedConfig::default();
        let media_allow = pixelfed.media_allowlist();
        Ok(Self {
            store,
            test_hooks: false,
            fault: Mutex::new(None),
            upstream: Upstream::new(&pixelfed.instance_url, media_allow.clone()),
            media_allow,
            pixelfed,
            pixelfed_refresh: tokio::sync::Mutex::new(()),
            show_dev_apps: false,
            layout_tx: tokio::sync::broadcast::channel(layouts::CHANNEL_CAPACITY).0,
            sse_keepalive: layouts::DEFAULT_KEEPALIVE,
            shutdown: tokio::sync::watch::channel(false).0,
        })
    }

    /// `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists `pixelfed_demo`
    /// and `home` (their other endpoints work either way).
    pub fn with_dev_apps(mut self, show: bool) -> Self {
        self.show_dev_apps = show;
        self
    }

    pub fn show_dev_apps(&self) -> bool {
        self.show_dev_apps
    }

    /// Override the SSE keepalive interval (tests).
    pub fn with_sse_keepalive(mut self, interval: std::time::Duration) -> Self {
        self.sse_keepalive = interval;
        self
    }

    pub fn sse_keepalive(&self) -> std::time::Duration {
        self.sse_keepalive
    }

    /// Broadcast a new app version to SSE subscribers (no-op without any).
    /// Nothing at runtime appends versions (they come with a deploy); tests
    /// and tooling that append with [`layouts::append_version`] call this.
    pub fn publish_layout(&self, workspace_id: &str, version: i64) {
        let _ = self.layout_tx.send(layouts::LayoutEvent {
            workspace_id: workspace_id.to_owned(),
            version,
        });
    }

    /// End all open SSE streams (call before graceful shutdown, which
    /// otherwise waits for those never-ending responses).
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub(crate) fn shutdown_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn subscribe_layouts(&self) -> tokio::sync::broadcast::Receiver<layouts::LayoutEvent> {
        self.layout_tx.subscribe()
    }

    /// Point the Pixelfed adapter at another instance.
    pub fn with_pixelfed(mut self, config: PixelfedConfig) -> Self {
        self.media_allow = config.media_allowlist();
        self.upstream = Upstream::new(&config.instance_url, self.media_allow.clone());
        self.pixelfed = config;
        self
    }

    pub fn pixelfed_config(&self) -> &PixelfedConfig {
        &self.pixelfed
    }

    /// State backed by `<dir>/server.db` (the directory is created), with
    /// the layout files synced.
    pub fn open_dir(dir: &FsPath) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(dir)?;
        Ok(Self::try_new(Store::open(&dir.join("server.db"))?)?)
    }

    /// Private in-memory database (tests).
    pub fn in_memory() -> Self {
        Self::new(Store::open_in_memory().expect("in-memory SQLite"))
    }

    /// Enable `POST /v1/test/fault` (`CONSTRUCT_TEST_HOOKS=1`).
    pub fn with_test_hooks(mut self, enabled: bool) -> Self {
        self.test_hooks = enabled;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Consume one injected fault, if any is pending.
    fn take_fault(&self) -> Option<StatusCode> {
        let mut guard = self.fault.lock().unwrap_or_else(|e| e.into_inner());
        let fault = guard.as_mut()?;
        let status = fault.status;
        fault.remaining -= 1;
        if fault.remaining == 0 {
            *guard = None;
        }
        Some(status)
    }
}

/// Build the HTTP router.
pub fn app(state: Arc<AppState>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/workspaces/{id}", get(get_workspace))
        .route("/v1/counters/{id}/{op}", post(counter_mutation))
        .route("/v1/demo/ack", post(demo_ack))
        .merge(pixelfed::routes::router())
        .merge(layouts::router());
    if state.test_hooks {
        router = router.route("/v1/test/fault", post(set_fault));
    }
    router
        .route_layer(middleware::from_fn_with_state(state.clone(), fault_layer))
        .with_state(state)
}

/// Convenience for tests: router over a fresh in-memory store, hooks off.
pub fn app_in_memory() -> Router {
    app(Arc::new(AppState::in_memory()))
}

/// Run a blocking store operation off the async workers.
pub(crate) async fn blocking<T: Send + 'static>(
    state: &Arc<AppState>,
    f: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let state = state.clone();
    match tokio::task::spawn_blocking(move || f(&state.store)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "store error");
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "store_error", "detail": e.to_string() }),
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "store task failed");
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "store_error" }),
            ))
        }
    }
}

async fn get_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = layouts::reject_invalid_workspace_id(&id) {
        return r;
    }
    if id == pixelfed::layout::WORKSPACE_ID {
        return pixelfed::routes::home_workspace(&state, &headers).await;
    }
    let Some(spec) = workspace(&id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "workspace_not_found", "id": id }),
        );
    };
    let served = match layouts::active_for_client(&state, &id, &headers).await {
        Ok(Some(v)) => v,
        Ok(None) => return Json(spec).into_response(),
        Err(e) => return e.into_response(),
    };
    let mut spec = spec.clone();
    spec.workspace = served.layout;
    if id == "home" {
        let count = match blocking(&state, |s| s.counter("home")).await {
            Ok(n) => n,
            Err(e) => return e.into_response(),
        };
        spec.entities
            .entry(HOME_COUNTER_URI.to_owned())
            .or_default()
            .set("count", count);
    }
    (layouts::served_headers(served.version), Json(spec)).into_response()
}

/// A JSON error response, kept small so it can travel in `Result::Err`.
pub(crate) type ApiError = (StatusCode, Json<Value>);

pub(crate) fn api_error(status: StatusCode, body: Value) -> ApiError {
    (status, Json(body))
}

pub(crate) fn json_error(status: StatusCode, body: Value) -> Response {
    api_error(status, body).into_response()
}

/// Test hook: while a fault is pending, every POST except `/v1/test/*`
/// returns the injected status without reaching its handler.
async fn fault_layer(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if req.method() == Method::POST
        && !req.uri().path().starts_with("/v1/test/")
        && let Some(status) = state.take_fault()
    {
        tracing::info!(
            path = %req.uri().path(),
            status = status.as_u16(),
            "injected fault: request not applied"
        );
        return json_error(
            status,
            json!({ "error": "injected_fault", "status": status.as_u16() }),
        );
    }
    next.run(req).await
}

/// `POST /v1/test/fault {"status":503,"count":3}` (only with test hooks on).
/// `count` 0 clears any pending fault.
async fn set_fault(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let status = payload["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .filter(|s| s.as_u16() >= 400);
    let count = payload["count"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok());
    let (Some(status), Some(count)) = (status, count) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_fault", "expected": {"status": "400..=599", "count": "u32"} }),
        );
    };
    let fault = (count > 0).then_some(Fault {
        status,
        remaining: count,
    });
    *state.fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    tracing::info!(
        status = status.as_u16(),
        count,
        "fault injection armed for the next mutations"
    );
    Json(json!({ "ok": true, "status": status.as_u16(), "count": count })).into_response()
}

/// An empty body is treated as `null` even when `Content-Type:
/// application/json` is set (common for HTTP clients that add the header
/// unconditionally). A non-empty body must be JSON-typed and valid; failures
/// are reported as JSON errors like the other endpoints.
pub(crate) fn parse_json_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|mime| {
            let mime = mime.trim();
            mime.eq_ignore_ascii_case("application/json")
                || (mime.starts_with("application/") && mime.ends_with("+json"))
        });
    if !is_json {
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({ "error": "expected_application_json" }),
        ));
    }
    serde_json::from_slice::<Value>(body).map_err(|e| {
        api_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": e.to_string() }),
        )
    })
}

/// Accepts an optional JSON body (ignored beyond validation). The path is
/// validated before the body so an unknown endpoint is always 404.
async fn counter_mutation(
    State(state): State<Arc<AppState>>,
    Path((id, op)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if id != "home" || !matches!(op.as_str(), "increment" | "decrement" | "reset") {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "unknown_counter_endpoint", "id": id, "op": op }),
        );
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let key = id.clone();
    let result = match op.as_str() {
        "increment" => blocking(&state, move |s| s.increment(&key).map(Ok)).await,
        "decrement" => blocking(&state, move |s| s.decrement(&key)).await,
        _ => blocking(&state, move |s| s.reset(&key).map(Ok)).await,
    };
    match result {
        Ok(Ok(count)) => {
            tracing::info!(counter = %id, %op, %payload, count, "counter mutation applied");
            Json(json!({ "ok": true, "count": count })).into_response()
        }
        Ok(Err(store::BelowZero { count })) => {
            tracing::info!(counter = %id, %op, count, "counter mutation rejected: count_below_zero");
            json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({ "error": "count_below_zero", "count": count }),
            )
        }
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/demo/ack`: stateless 200 for the static `pixelfed_demo` fixture's
/// actions, so live-mode likes there are accepted (and not rolled back).
async fn demo_ack(body: Bytes) -> Response {
    tracing::info!(body = %String::from_utf8_lossy(&body), "demo ack");
    Json(json!({ "ok": true })).into_response()
}
