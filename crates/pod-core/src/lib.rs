//! The Construct pod, without any app (D40).
//!
//! The core keeps its state in SQLite (see [`store`]), serves each app's
//! workspace (`GET /v1/workspaces/:id`), the read-only apps and versions API
//! and the layout push channel (see [`layouts`]), and owns sessions and
//! sign-ins (see [`session`]). Workspace layouts are code
//! (`layouts/<app>/NNNN.json`), synced into one append-only version history
//! per app at startup; [`validate`] checks every layout file.
//!
//! Apps plug in through [`AppAdapter`]: the `server` binary builds the
//! adapters and passes them to [`AppState::try_new`]. The core never names an
//! app; adapters supply each app's data, routes, sign-ins and tables.

pub mod adapter;
pub mod layouts;
pub mod net;
pub mod notify;
pub mod push;
pub mod secrets;
pub mod session;
pub mod static_app;
pub mod store;
pub mod uploads;
pub mod util;
pub mod validate;

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use protocol::WorkspaceSpec;
use serde_json::{Value, json};
use store::Store;

pub use adapter::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, Catalog, Hydration, Pod, RequestCtx, SignInAccount,
    SignInProvider,
};
pub use notify::{NewNotification, PollCtx, PollOutcome, PollStatus};
pub use session::{Caller, Session};
pub use static_app::StaticAdapter;

/// `WorkspaceSpec::version` of the bundles the pod builds.
pub const BUNDLE_VERSION: &str = "1.0.0";

/// A pending injected fault (test hook): the next `remaining` mutation
/// requests get `status` without being applied.
#[derive(Debug, Clone, Copy)]
struct Fault {
    status: StatusCode,
    remaining: u32,
}

/// Request header the pod host (D46) sets to the path prefix under which this
/// pod's public routes ([`AppAdapter::public_paths`]) are reachable without a
/// bearer, e.g. `/v1/p/<pod id>`. Absent when the pod is served on its own.
pub const PUBLIC_PREFIX_HEADER: &str = "x-construct-public-prefix";

/// The public path prefix of this pod ([`PUBLIC_PREFIX_HEADER`]), `""` when
/// served on its own. Adapters put it in front of absolute URLs a client
/// loads without its bearer (media).
pub fn public_prefix(headers: &HeaderMap) -> &str {
    headers
        .get(PUBLIC_PREFIX_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|p| {
            p.starts_with('/')
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_'))
        })
        .unwrap_or("")
}

/// Shared server state; adapters reach it as [`Pod`].
pub struct AppState {
    /// Per-pod values adapters keep beside the store ([`AppState::local`]).
    locals: Mutex<Vec<Arc<dyn std::any::Any + Send + Sync>>>,
    store: Store,
    catalog: Catalog,
    test_hooks: bool,
    fault: Mutex<Option<Fault>>,
    /// Pod sessions older than this are invalid (`CONSTRUCT_SESSION_TTL_DAYS`).
    session_ttl_secs: i64,
    /// `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists the dev apps.
    show_dev_apps: bool,
    /// New app versions, fanned out to SSE subscribers.
    layout_tx: tokio::sync::broadcast::Sender<layouts::LayoutEvent>,
    /// "Your data changed" (D50), fanned out to that session's subscribers.
    data_tx: tokio::sync::broadcast::Sender<layouts::DataEvent>,
    /// Largest upload (`CONSTRUCT_UPLOAD_MAX_BYTES`, D49).
    upload_max_bytes: usize,
    /// Set once the adapters' [`AppAdapter::resume`] and the upload sweeper
    /// have been started for this pod.
    started: std::sync::atomic::AtomicBool,
    /// SSE keepalive interval (15 s; shorter in tests). Streams with a
    /// session also re-check it this often.
    sse_keepalive: std::time::Duration,
    /// Open SSE streams per session token hash, capped at
    /// `sse_streams_per_token` ([`layouts::SseSlot`]).
    pub(crate) sse_streams: Mutex<HashMap<String, usize>>,
    sse_streams_per_token: usize,
    /// Set by [`AppState::shutdown`]: open SSE streams end.
    shutdown: tokio::sync::watch::Sender<bool>,
    /// Notifications: the events fan-out, open device streams, polls (D52).
    pub(crate) notifier: notify::Notifier,
    /// Web Push sender shared by the server's pods; `None`: no Web Push.
    push: Option<notify::SharedPush>,
    /// This pod's public path prefix, set by the host (D46), for work done
    /// outside a request (notification polls).
    public_prefix: std::sync::OnceLock<String>,
}

impl AppState {
    /// State over `store` serving `adapters`, after running their migrations
    /// and syncing their layout files into the store ([`layouts::sync`]).
    /// Panics with the error; see [`Self::try_new`].
    pub fn new(store: Store, adapters: Vec<Arc<dyn AppAdapter>>) -> Self {
        Self::try_new(store, adapters).unwrap_or_else(|e| panic!("{e}"))
    }

    /// [`Self::new`], returning the error (a bad adapter set, a failed
    /// adapter migration, or a changed, out-of-order or invalid layout file)
    /// instead of panicking.
    pub fn try_new(store: Store, adapters: Vec<Arc<dyn AppAdapter>>) -> Result<Self, String> {
        let catalog = Catalog::new(adapters)?;
        for a in catalog.adapters() {
            store
                .migrate_adapter(a.name(), a.migrations())
                .map_err(|e| format!("migrating the '{}' adapter's tables: {e}", a.name()))?;
            a.on_open(&store)
                .map_err(|e| format!("opening the '{}' adapter's data: {e}", a.name()))?;
        }
        let report = layouts::sync(&store, &catalog)?;
        for warning in &report.missing {
            tracing::warn!("{warning}");
        }
        for path in &report.adopted {
            tracing::info!(file = %path, "layout file hash recorded (deployed before migration v8)");
        }
        for path in &report.adopted_schema {
            tracing::info!(file = %path, "layout file schema recorded (deployed before migration v9)");
        }
        for (path, version) in &report.appended {
            tracing::info!(file = %path, version, "layout file deployed as a new version");
        }
        Ok(Self {
            locals: Mutex::new(Vec::new()),
            store,
            catalog,
            test_hooks: false,
            fault: Mutex::new(None),
            session_ttl_secs: session::DEFAULT_SESSION_TTL_DAYS * 86_400,
            show_dev_apps: false,
            layout_tx: tokio::sync::broadcast::channel(layouts::CHANNEL_CAPACITY).0,
            data_tx: tokio::sync::broadcast::channel(layouts::CHANNEL_CAPACITY).0,
            upload_max_bytes: uploads::max_bytes_from_env(),
            started: std::sync::atomic::AtomicBool::new(false),
            sse_keepalive: layouts::DEFAULT_KEEPALIVE,
            sse_streams: Mutex::new(HashMap::new()),
            sse_streams_per_token: layouts::DEFAULT_SSE_STREAMS_PER_TOKEN,
            shutdown: tokio::sync::watch::channel(false).0,
            notifier: notify::Notifier::default(),
            push: None,
            public_prefix: std::sync::OnceLock::new(),
        })
    }

    /// State backed by `<dir>/server.db` (the directory is created, 0700).
    pub fn open_dir(
        dir: &FsPath,
        adapters: Vec<Arc<dyn AppAdapter>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        util::create_private_dir(dir)?;
        Ok(Self::try_new(
            Store::open(&dir.join("server.db"))?,
            adapters,
        )?)
    }

    /// Private in-memory database (tests).
    pub fn in_memory(adapters: Vec<Arc<dyn AppAdapter>>) -> Self {
        Self::new(Store::open_in_memory().expect("in-memory SQLite"), adapters)
    }

    /// `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists the dev apps
    /// (their other endpoints work either way).
    pub fn with_dev_apps(mut self, show: bool) -> Self {
        self.show_dev_apps = show;
        self
    }

    pub fn show_dev_apps(&self) -> bool {
        self.show_dev_apps
    }

    /// Pod sessions older than this are invalid.
    pub fn with_session_ttl_secs(mut self, secs: i64) -> Self {
        self.session_ttl_secs = secs;
        self
    }

    pub fn session_ttl_secs(&self) -> i64 {
        self.session_ttl_secs
    }

    /// Override the SSE keepalive interval (tests).
    pub fn with_sse_keepalive(mut self, interval: std::time::Duration) -> Self {
        self.sse_keepalive = interval;
        self
    }

    pub fn sse_keepalive(&self) -> std::time::Duration {
        self.sse_keepalive
    }

    /// Override how many SSE streams one session token may hold open
    /// ([`layouts::DEFAULT_SSE_STREAMS_PER_TOKEN`]).
    pub fn with_sse_streams_per_token(mut self, max: usize) -> Self {
        self.sse_streams_per_token = max;
        self
    }

    pub fn sse_streams_per_token(&self) -> usize {
        self.sse_streams_per_token
    }

    /// Send notifications by Web Push with `sender` (D52).
    pub fn with_push(mut self, sender: Option<notify::SharedPush>) -> Self {
        self.push = sender;
        self
    }

    pub fn push_sender(&self) -> Option<notify::SharedPush> {
        self.push.clone()
    }

    /// The pod's public path prefix ([`PUBLIC_PREFIX_HEADER`]) for work done
    /// outside a request; the host sets it once when it opens the pod.
    pub fn set_public_prefix(&self, prefix: &str) {
        let _ = self.public_prefix.set(prefix.to_owned());
    }

    /// See [`Self::set_public_prefix`]; `""` when unset.
    pub fn public_prefix(&self) -> &str {
        self.public_prefix.get().map_or("", String::as_str)
    }

    /// Largest upload accepted (`CONSTRUCT_UPLOAD_MAX_BYTES`, D49).
    pub fn with_upload_max_bytes(mut self, max: usize) -> Self {
        self.upload_max_bytes = max;
        self
    }

    /// The pod's uploads (D49), for adapters.
    pub fn uploads<'a>(self: &'a Pod) -> uploads::Uploads<'a> {
        uploads::Uploads { pod: self }
    }

    /// Tell `session`'s clients of `workspace_id` that its data changed
    /// (`event: data`, D50): they reload. No-op without subscribers.
    pub fn publish_data(&self, workspace_id: &str, session: &Session) {
        let _ = self.data_tx.send(layouts::DataEvent {
            workspace_id: workspace_id.to_owned(),
            token_hash: session.token_hash.clone(),
        });
    }

    pub fn subscribe_data(&self) -> tokio::sync::broadcast::Receiver<layouts::DataEvent> {
        self.data_tx.subscribe()
    }

    /// Start this pod's background work once (called by [`app`]): every
    /// adapter's [`AppAdapter::resume`] and the hourly upload sweeper. Needs a
    /// Tokio runtime; without one (some sync tests) nothing starts.
    fn start(self: &Pod) {
        use std::sync::atomic::Ordering;
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        for a in self.catalog.adapters() {
            let (a, pod) = (Arc::clone(a), Arc::clone(self));
            rt.spawn(async move { a.resume(&pod).await });
        }
        let weak = Arc::downgrade(self);
        rt.spawn(async move {
            loop {
                let Some(pod) = weak.upgrade() else { return };
                let swept = blocking(&pod, |s| uploads::sweep(s, util::now_secs())).await;
                if let Ok(n) = swept
                    && n > 0
                {
                    tracing::info!(deleted = n, "old unclaimed uploads deleted");
                }
                drop(pod);
                tokio::time::sleep(uploads::SWEEP_EVERY).await;
            }
        });
    }

    /// Enable `POST /v1/test/fault` (`CONSTRUCT_TEST_HOOKS=1`).
    pub fn with_test_hooks(mut self, enabled: bool) -> Self {
        self.test_hooks = enabled;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Every adapter's apps, files and sign-ins.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The adapter of type `T`, if one is registered (how an adapter's route
    /// handlers find their own configuration).
    pub fn adapter<T: AppAdapter>(&self) -> Option<&T> {
        self.catalog.adapters().iter().find_map(|a| {
            let any: &dyn std::any::Any = a.as_ref();
            any.downcast_ref::<T>()
        })
    }

    /// This pod's value of type `T`, created with `T::default()` on first
    /// use. For adapter state that must not be shared between the pods of one
    /// host (D46), e.g. a lock around one pod's token refresh.
    pub fn local<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut locals = self.locals.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = locals
            .iter()
            .find_map(|v| Arc::clone(v).downcast::<T>().ok())
        {
            return v;
        }
        let v = Arc::new(T::default());
        locals.push(v.clone());
        v
    }

    /// Run a blocking store operation off the async workers.
    pub async fn blocking<T: Send + 'static>(
        self: &Pod,
        f: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, ApiError> {
        blocking(self, f).await
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

/// Build the HTTP router: the core's routes plus every adapter's.
pub fn app(state: Pod) -> Router {
    state.start();
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/workspaces/{id}", get(get_workspace))
        .merge(session::router())
        .merge(layouts::router())
        .merge(notify::router())
        .merge(uploads::router(state.upload_max_bytes));
    for a in state.catalog.adapters() {
        router = router.merge(a.router());
    }
    if state.test_hooks {
        router = router.route("/v1/test/fault", post(set_fault));
    }
    router
        .route_layer(middleware::from_fn_with_state(state.clone(), fault_layer))
        .with_state(state)
}

/// Run a blocking store operation off the async workers.
pub async fn blocking<T: Send + 'static>(
    state: &Pod,
    f: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let state = state.clone();
    match tokio::task::spawn_blocking(move || f(&state.store)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            // The detail stays in the log: database errors can name tables,
            // paths and values.
            tracing::error!(error = %e, "store error");
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "store_error" }),
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

/// The signed-out screen for `app`, which requires `sign_in`.
fn signed_out(state: &Pod, sign_in: &str, app: &AppInfo, req: &RequestCtx) -> WorkspaceSpec {
    let adapter = state
        .catalog
        .sign_in_adapter(sign_in)
        .expect("the catalog checked every app's sign-in");
    adapter.signed_out(sign_in, app, req)
}

/// `GET /v1/workspaces/:id`: the app's data from its adapter
/// ([`AppAdapter::hydrate`]) with the newest layout version the client can
/// draw (`X-Construct-Layout-Version`). An app that requires a sign-in
/// ([`AuthPolicy::SignIn`]) gets its signed-out screen instead when the
/// caller lacks it: 200 without a bearer, 401 + `X-Construct-Session: invalid`
/// with one, so the client learns its session is stale.
async fn get_workspace(
    State(state): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = layouts::reject_invalid_workspace_id(&id) {
        return r;
    }
    let (Some(app), Some(adapter)) = (
        state.catalog.app(&id).copied(),
        state.catalog.adapter_for(&id).cloned(),
    ) else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "workspace_not_found", "id": id }),
        );
    };
    let caller = match state.caller(&headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    // Only a session the pod no longer knows is told to go away; a valid one
    // that lacks the app's sign-in gets the signed-out screen (D46: a pod
    // token's session must survive every app's sign-out).
    let stale = caller == Caller::Stale;
    let req = RequestCtx {
        schema: layouts::client_schema(&headers),
        headers,
        session: caller.clone().into_session(),
    };
    if let AuthPolicy::SignIn(sign_in) = app.auth
        && !caller.has(sign_in)
    {
        let screen = signed_out(&state, sign_in, &app, &req);
        return if stale {
            session::stale_session(screen)
        } else {
            Json(screen).into_response()
        };
    }
    let bundle = match adapter.hydrate(&state, &id, &req).await {
        Hydration::Data(bundle) => bundle,
        Hydration::Screen(screen) => return Json(screen).into_response(),
        Hydration::StaleSession(screen) => {
            // A pinned session stays; only the unusable sign-in goes.
            if let (Some(session), AuthPolicy::SignIn(sign_in)) = (&req.session, app.auth)
                && session.pinned
            {
                let (hash, sign_in) = (session.token_hash.clone(), sign_in.to_owned());
                if let Err(e) = blocking(&state, move |s| s.detach_sign_in(&hash, &sign_in)).await {
                    return e.into_response();
                }
                return Json(screen).into_response();
            }
            return session::stale_session(screen);
        }
        Hydration::Error(r) => return r,
    };
    let served = match layouts::active_for_client(&state, &id, &req.headers).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            return json_error(
                StatusCode::CONFLICT,
                json!({ "error": "no_compatible_version", "id": id,
                        "detail": "no layout version of this app fits this client's schema" }),
            );
        }
        Err(e) => return e.into_response(),
    };
    let spec = WorkspaceSpec {
        version: BUNDLE_VERSION.into(),
        workspace: served.layout,
        entities: bundle.entities,
        queries: bundle.queries,
    };
    (layouts::served_headers(served.version), Json(spec)).into_response()
}

/// A JSON error response, kept small so it can travel in `Result::Err`.
pub type ApiError = (StatusCode, Json<Value>);

pub fn api_error(status: StatusCode, body: Value) -> ApiError {
    (status, Json(body))
}

pub fn json_error(status: StatusCode, body: Value) -> Response {
    api_error(status, body).into_response()
}

/// Test hook: while a fault is pending, every POST except `/v1/test/*`
/// returns the injected status without reaching its handler.
async fn fault_layer(State(state): State<Pod>, req: Request, next: Next) -> Response {
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
async fn set_fault(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
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
pub fn parse_json_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, ApiError> {
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
