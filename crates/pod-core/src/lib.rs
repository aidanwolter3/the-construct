//! The Construct pod, without any source (D40).
//!
//! The core keeps its state in SQLite (see [`store`]), serves the pod's
//! construct (decision `content-addressed-construct`, see [`construct`]):
//! elements by hash, names that move, home's element as the root, its data
//! and its event stream; and owns sessions and sign-ins (see [`session`]).
//! Core layouts are code (`layouts/<layout>/NNNN.json` in each source
//! crate), named `@core/<layout>` in every pod when it opens.
//!
//! Sources plug in through [`Source`]: the `server` binary builds them and
//! passes them to [`AppState::try_new`]. The core never names a source;
//! sources supply data, routes, sign-ins, tables and core layouts ([`source`]).

pub mod adapter;
pub mod components;
pub mod construct;
pub mod devices;
pub mod disk;
pub mod hints;
pub mod ical;
pub mod image_proxy;
pub mod jacks;
pub mod layouts;
pub mod mcp;
pub mod mcp_servers;
pub mod metrics;
pub mod migrations;
pub mod net;
pub mod notify;
pub mod notify_store;
pub mod porcelain;
pub mod preview;
pub mod public_copy;
pub mod push;
pub mod recent_errors;
pub mod secrets;
pub mod session;
pub mod source;
pub mod static_app;
pub mod store;
pub mod testing;
pub mod tz;
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
use serde_json::{Value, json};
use store::Store;

pub use adapter::{
    AuthPolicy, BoxFuture, Catalog, CoreLayoutInfo, Hydration, Pod, RequestCtx, SignInAccount,
    SignInProvider, SignInStart, Source,
};
pub use notify::{NewNotification, PollCtx, PollOutcome, PollStatus};
pub use notify_store::{ListQuery, NotificationStore, ReadSel, ToStore};
pub use session::{Caller, Session};
pub use source::{EntityDoc, FeedSpec, PathSpec, SourceManifest};
pub use static_app::StaticAdapter;

/// `WorkspaceSpec::version` of the bundles the pod builds.
pub const BUNDLE_VERSION: &str = "1.0.0";

/// Every pod's own home layout (decision `blueprints-and-home`): the root of
/// its construct, which clients open first. It starts as the whole of
/// [`HOME_CORE_LAYOUT`] and is the user's to change.
pub const HOME_APP: &str = "home";
/// The core layout a pod's [`HOME_APP`] starts from.
pub const HOME_CORE_LAYOUT: &str = "shell";

/// A pending injected fault (test hook): the next `remaining` mutation
/// requests get `status` without being applied.
#[derive(Debug, Clone, Copy)]
struct Fault {
    status: StatusCode,
    remaining: u32,
}

/// Request header the pod host (D46) sets to the path prefix under which this
/// pod's public routes ([`Source::public_paths`]) are reachable without a
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
    /// `CONSTRUCT_SHOW_DEV_APPS=1`: MCP also lists the dev core layouts and
    /// sources.
    show_dev_apps: bool,
    /// The construct's generation after each move of a name, fanned out to
    /// its event streams ([`construct::events`]).
    construct_tx: tokio::sync::broadcast::Sender<i64>,
    /// "Your data changed" (D50), fanned out to that session's subscribers.
    data_tx: tokio::sync::broadcast::Sender<layouts::DataEvent>,
    /// Largest upload (`CONSTRUCT_UPLOAD_MAX_BYTES`, D49).
    upload_max_bytes: usize,
    /// Most bytes all of this pod's uploads may take together; `None`: no
    /// quota (the host sets one for every pod it serves).
    upload_quota_bytes: Option<u64>,
    /// Uploads are refused when the data volume is nearly full.
    disk_guard: Option<disk::DiskGuard>,
    /// Set once the adapters' [`Source::resume`] and the upload sweeper
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
    /// The server's public base URL (`CONSTRUCT_PUBLIC_URL`), set by the
    /// host, for links the pod hands out (an MCP server's sign-in redirect).
    public_base: std::sync::OnceLock<String>,
    /// MCP servers may be on private hosts (tests; decision `mcp-jack`).
    mcp_private_hosts: bool,
    /// Layout writes: caps and the write-rate window.
    pub(crate) writes: layouts::WriteState,
    /// What moving this pod from the old layout system found, when this
    /// open did ([`migrations::layout_system`]).
    conversion: Option<migrations::layout_system::Report>,
    /// Layout previews agents are looking at (MCP, `preview`).
    pub(crate) previews: preview::Previews,
    /// Served by a pod host (D46), which sets [`CREDENTIAL_HEADER`] (and
    /// removes any a client sent): only then is that header trusted.
    hosted: std::sync::atomic::AtomicBool,
    /// Per-process secret of internal requests ([`session::INTERNAL_HEADER`]).
    internal_secret: String,
    /// This pod's id on its host and the host's shelf of publications
    /// (decision `sharing`); unset: the pod can't share.
    shelf: std::sync::OnceLock<(String, Arc<dyn construct::share::Shelf>)>,
    /// One change at a time to what the pod shares (calendar feeds,
    /// publications): their caps are counted before the write, so two at
    /// once could both pass, and a feed list saved by one would drop the
    /// other's feed while its copy stays live.
    pub(crate) sharing: tokio::sync::Mutex<()>,
}

/// Request header a pod host sets on a request whose bearer was an MCP
/// credential (`mcp`, decision `mcp-connector`) or whose session came from
/// the web client's cookie ([`CREDENTIAL_WEB`]); the host removes any value
/// a client sent. Trusted only by a hosted pod ([`AppState::set_hosted`]).
pub const CREDENTIAL_HEADER: &str = "x-construct-credential";

/// [`CREDENTIAL_HEADER`] on a request whose session came from the web
/// client's `HttpOnly` cookie (decision `web-client`): an answer leaves that
/// session's token out, so the page's script never holds it.
pub const CREDENTIAL_WEB: &str = "web";

impl AppState {
    /// State over `store` serving `adapters`, after running their migrations
    /// and opening the pod's construct ([`construct::open`]). Panics with
    /// the error; see [`Self::try_new`].
    pub fn new(store: Store, adapters: Vec<Arc<dyn Source>>) -> Self {
        Self::try_new(store, adapters).unwrap_or_else(|e| panic!("{e}"))
    }

    /// [`Self::new`], returning the error (a bad adapter set, a failed
    /// adapter migration, an invalid or changed layout or component file,
    /// or a pod the old layout system held that could not be converted)
    /// instead of panicking.
    pub fn try_new(store: Store, adapters: Vec<Arc<dyn Source>>) -> Result<Self, String> {
        let catalog = Catalog::new(adapters)?;
        // Every source's tables first: one source's open may write another's
        // (Events creates its collection in Collections' tables).
        for a in catalog.adapters() {
            store
                .migrate_adapter(a.name(), a.migrations())
                .map_err(|e| format!("migrating the '{}' adapter's tables: {e}", a.name()))?;
        }
        for a in catalog.adapters() {
            a.on_open(&store)
                .map_err(|e| format!("opening the '{}' adapter's data: {e}", a.name()))?;
        }
        let conversion = construct::open(&store, &catalog)?;
        // A source this build no longer has (decision `remove-recipes-source`):
        // home stops binding its feeds.
        match construct::write::drop_unknown_feeds(&store, &catalog) {
            Ok(gone) if !gone.is_empty() => {
                tracing::info!(feeds = ?gone, "home no longer binds removed sources' feeds")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "home still binds a removed source's feed"),
        }
        Ok(Self {
            locals: Mutex::new(Vec::new()),
            store,
            catalog,
            test_hooks: false,
            fault: Mutex::new(None),
            session_ttl_secs: session::DEFAULT_SESSION_TTL_DAYS * 86_400,
            show_dev_apps: false,
            construct_tx: tokio::sync::broadcast::channel(layouts::CHANNEL_CAPACITY).0,
            data_tx: tokio::sync::broadcast::channel(layouts::CHANNEL_CAPACITY).0,
            upload_max_bytes: uploads::max_bytes_from_env(),
            upload_quota_bytes: None,
            disk_guard: None,
            started: std::sync::atomic::AtomicBool::new(false),
            sse_keepalive: layouts::DEFAULT_KEEPALIVE,
            sse_streams: Mutex::new(HashMap::new()),
            sse_streams_per_token: layouts::DEFAULT_SSE_STREAMS_PER_TOKEN,
            shutdown: tokio::sync::watch::channel(false).0,
            notifier: notify::Notifier::default(),
            push: None,
            public_prefix: std::sync::OnceLock::new(),
            public_base: std::sync::OnceLock::new(),
            mcp_private_hosts: false,
            writes: layouts::WriteState::default(),
            conversion,
            previews: preview::Previews::default(),
            hosted: std::sync::atomic::AtomicBool::new(false),
            internal_secret: {
                let mut buf = [0u8; 32];
                getrandom::fill(&mut buf).expect("OS random number generator");
                util::hex(&buf)
            },
            shelf: std::sync::OnceLock::new(),
            sharing: tokio::sync::Mutex::new(()),
        })
    }

    /// State backed by `<dir>/server.db` (the directory is created, 0700).
    pub fn open_dir(
        dir: &FsPath,
        adapters: Vec<Arc<dyn Source>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        util::create_private_dir(dir)?;
        Ok(Self::try_new(
            Store::open(&dir.join("server.db"))?,
            adapters,
        )?)
    }

    /// Private in-memory database (tests).
    pub fn in_memory(adapters: Vec<Arc<dyn Source>>) -> Self {
        Self::new(Store::open_in_memory().expect("in-memory SQLite"), adapters)
    }

    /// `CONSTRUCT_SHOW_DEV_APPS=1`: MCP also lists the dev core layouts and
    /// sources (they work either way).
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

    /// The server's public base URL (`https://pod.example`), when the host
    /// knows it; set once.
    pub fn set_public_base(&self, base: &str) {
        let _ = self.public_base.set(base.trim_end_matches('/').to_owned());
    }

    /// See [`Self::set_public_base`].
    pub fn public_base(&self) -> Option<&str> {
        self.public_base.get().map(String::as_str)
    }

    /// Let MCP servers be on private hosts (a local test setup; the server
    /// sets it with `CONSTRUCT_FILL_PRIVATE_HOSTS=1`).
    pub fn with_mcp_private_hosts(mut self, allow: bool) -> Self {
        self.mcp_private_hosts = allow;
        self
    }

    /// See [`Self::with_mcp_private_hosts`].
    pub fn mcp_private_hosts(&self) -> bool {
        self.mcp_private_hosts
    }

    /// Largest upload accepted (`CONSTRUCT_UPLOAD_MAX_BYTES`, D49).
    pub fn with_upload_max_bytes(mut self, max: usize) -> Self {
        self.upload_max_bytes = max;
        self
    }

    /// Most bytes this pod's uploads may hold together (decision
    /// `pod-storage-limits`).
    pub fn with_upload_quota_bytes(mut self, quota: Option<u64>) -> Self {
        self.upload_quota_bytes = quota;
        self
    }

    pub fn upload_quota_bytes(&self) -> Option<u64> {
        self.upload_quota_bytes
    }

    /// Refuse uploads when the filesystem of `guard.dir` runs low.
    pub fn with_disk_guard(mut self, guard: Option<disk::DiskGuard>) -> Self {
        self.disk_guard = guard;
        self
    }

    pub fn disk_guard(&self) -> Option<&disk::DiskGuard> {
        self.disk_guard.as_ref()
    }

    /// The pod's uploads (D49), for adapters.
    pub fn uploads<'a>(self: &'a Pod) -> uploads::Uploads<'a> {
        uploads::Uploads { pod: self }
    }

    /// Tell `session`'s clients that data of `source` (an adapter's
    /// [`Source::name`]) changed (`event: data`, D50): every app that
    /// draws on that source ([`Catalog::app_sources`]) reloads. No-op
    /// without subscribers.
    pub fn publish_data(&self, source: &str, session: &Session) {
        let _ = self.data_tx.send(layouts::DataEvent {
            source: source.to_owned(),
            token_hash: Some(session.token_hash.clone()),
            scope: None,
            except_device: None,
        });
    }

    /// Tell every session's clients that the pod's own data of `source`
    /// changed (data that isn't any one account's, such as Collections):
    /// every app that draws on `source` reloads. No-op without subscribers.
    pub fn publish_data_all(&self, source: &str) {
        let _ = self.data_tx.send(layouts::DataEvent {
            source: source.to_owned(),
            token_hash: None,
            scope: None,
            except_device: None,
        });
    }

    /// [`Pod::publish_data_all`] saying what changed (decision
    /// `scoped-data-events`): clients reload only the data `scope`
    /// matches. `except_device` (the `x-construct-device` of the write's
    /// request) isn't told: it already shows the change.
    pub fn publish_data_scoped(
        &self,
        source: &str,
        scope: layouts::DataScope,
        except_device: Option<&str>,
    ) {
        let _ = self.data_tx.send(layouts::DataEvent {
            source: source.to_owned(),
            token_hash: None,
            scope: Some(scope),
            except_device: except_device.map(str::to_owned),
        });
    }

    pub fn subscribe_data(&self) -> tokio::sync::broadcast::Receiver<layouts::DataEvent> {
        self.data_tx.subscribe()
    }

    /// Start this pod's background work once (called by [`app`]): every
    /// adapter's [`Source::resume`] and the hourly upload sweeper. Needs a
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
        // Public links follow the data they copy (decision `public-web-links`).
        public_copy::watch(self);
        // Calendar feeds follow their rows too (decision `ical-feeds`).
        ical::watch(self);
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

    /// This pod is served by a pod host, which owns [`CREDENTIAL_HEADER`].
    /// Plug the pod into its host's shelf of publications as pod `pod_id`
    /// (decision `sharing`); set once, by the host, when it opens the pod.
    pub fn set_shelf(&self, pod_id: &str, shelf: Arc<dyn construct::share::Shelf>) {
        let _ = self.shelf.set((pod_id.to_owned(), shelf));
    }

    /// This pod's id on its host and the host's shelf, if it has one.
    pub fn shelf(&self) -> Option<(&str, &Arc<dyn construct::share::Shelf>)> {
        self.shelf.get().map(|(id, s)| (id.as_str(), s))
    }

    pub fn set_hosted(&self) {
        self.hosted.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The credential kind a pod host vouched for (`mcp`, `web`), if hosted.
    pub fn host_credential(&self, headers: &HeaderMap) -> Option<String> {
        if !self.hosted.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        headers
            .get(CREDENTIAL_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    /// The [`session::INTERNAL_HEADER`] value that makes an internal request
    /// act as session `token_hash`, limited to `scope` ([`scope_layer`]).
    pub(crate) fn internal_as(&self, token_hash: &str, scope: session::Scope) -> String {
        format!("{}:{}:{token_hash}", self.internal_secret, scope.id())
    }

    /// The session hash and scope of an internal request
    /// ([`Self::internal_as`]).
    pub(crate) fn internal_caller(&self, headers: &HeaderMap) -> Option<(String, session::Scope)> {
        let v = headers.get(session::INTERNAL_HEADER)?.to_str().ok()?;
        let (secret, rest) = v.split_once(':')?;
        let (scope, hash) = rest.split_once(':')?;
        let ok = secret.len() == self.internal_secret.len()
            && secret
                .bytes()
                .zip(self.internal_secret.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        ok.then(|| (hash.to_owned(), session::Scope::parse(scope)))
    }

    /// Whether a request of `scope` may make a `method` request of `path`:
    /// [`session::Scope::Mcp`] reads, and runs only the actions sources list
    /// as [`SourceManifest::mcp_actions`].
    pub fn scope_allows(&self, scope: session::Scope, method: &Method, path: &str) -> bool {
        match scope {
            session::Scope::Full => true,
            session::Scope::Mcp => {
                method == Method::GET
                    || method == Method::HEAD
                    || (method == Method::POST
                        && !path.contains('$')
                        && self.catalog.sources().any(|(_, m)| {
                            m.mcp_actions.iter().any(|p| source::path_matches(p, path))
                        }))
            }
        }
    }

    /// Limits on the pod's layouts ([`layouts::Caps`]).
    pub fn with_layout_caps(mut self, caps: layouts::Caps) -> Self {
        self.writes = layouts::WriteState::new(caps);
        self
    }

    /// What moving this pod from the old layout system found (its dry run),
    /// when this open did.
    pub fn conversion(&self) -> Option<&migrations::layout_system::Report> {
        self.conversion.as_ref()
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
    pub fn adapter<T: Source>(&self) -> Option<&T> {
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

    /// Tell the construct's event streams that names moved (`generation`
    /// is the construct's write count after them; no-op without
    /// subscribers).
    pub fn publish_construct(&self, generation: i64) {
        let _ = self.construct_tx.send(generation);
    }

    /// End all open SSE streams (call before graceful shutdown, which
    /// otherwise waits for those never-ending responses).
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub(crate) fn shutdown_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn subscribe_construct(&self) -> tokio::sync::broadcast::Receiver<i64> {
        self.construct_tx.subscribe()
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
        .merge(session::router())
        .merge(notify::router())
        .merge(construct::http::router())
        .merge(mcp::router())
        .merge(image_proxy::router())
        .route("/v1/jacks", get(get_jacks))
        .route("/v1/jacks/screen", get(get_jacks_screen))
        .route("/v1/jacks/plug", post(post_jack_plug))
        .route("/v1/jacks/unplug", post(post_jack_unplug))
        .route(mcp_servers::CALLBACK_PATH, get(mcp_servers::callback))
        .merge(uploads::router(state.upload_max_bytes));
    for a in state.catalog.adapters() {
        router = router.merge(a.router());
    }
    if state.test_hooks {
        router = router.route("/v1/test/fault", post(set_fault));
    }
    router
        .route_layer(middleware::from_fn_with_state(state.clone(), fault_layer))
        .route_layer(middleware::from_fn_with_state(state.clone(), scope_layer))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            devices::note_layer,
        ))
        // Outermost: it sees the answers of the layers above too.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            recent_errors::layer,
        ))
        .with_state(state)
}

/// Refuses an internal request its scope doesn't allow
/// ([`AppState::scope_allows`]): one an MCP credential started can't act
/// upstream or change jacks, sign-ins or agents (issue #435).
async fn scope_layer(State(pod): State<Pod>, req: Request, next: Next) -> Response {
    if let Some((_, scope)) = pod.internal_caller(req.headers())
        && !pod.scope_allows(scope, req.method(), req.uri().path())
    {
        metrics::inc(metrics::AUTH_FAILURES, &[("kind", "mcp_scope")]);
        return json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "mcp_credential_scope",
                    "detail": "started by an MCP credential, which reads and runs only the actions sources list for it, never one that acts for the user upstream" }),
        );
    }
    next.run(req).await
}

/// `None` for a session of the pod (jacks are its owner's: any device
/// that opened the pod with its code), else the answer to send instead.
async fn refuse_unless_signed(pod: &Pod, headers: &HeaderMap) -> Option<Response> {
    match pod.caller(headers).await {
        Ok(Caller::Signed(_)) => None,
        Ok(_) => Some(json_error(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "unauthorized", "detail": "open your pod first" }),
        )),
        Err(e) => Some(e.into_response()),
    }
}

/// A jack call's answer: its JSON, or its refusal as a 400 (404 for an
/// unknown jack).
fn jack_answer(r: Result<Value, Value>) -> Response {
    match r {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) if e["error"] == "unknown_jack" => json_error(StatusCode::NOT_FOUND, e),
        Err(e) if e["error"] == "store_error" => json_error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

/// `GET /v1/jacks/screen[?id=]`: the jacks as Settings' jacks screens bind
/// them ([`jacks::screen`]).
async fn get_jacks_screen(
    State(pod): State<Pod>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    if let Some(r) = refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    jack_answer(jacks::screen(&pod, q.get("id").map(String::as_str)).await)
}

/// `POST /v1/jacks/plug {id, config?}`: plug a switch or config jack from
/// the app's Settings ([`jacks::plug_from_app`]); a secret goes from the
/// device straight to the pod, sealed, and is never shown back.
async fn post_jack_plug(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": "the body is {id, config?}" }),
        );
    };
    jack_answer(jacks::plug_from_app(&pod, &body).await)
}

/// `POST /v1/jacks/unplug {id}`: unplug a switch or config jack from the
/// app's Settings (an account is signed out under Sign-ins).
async fn post_jack_unplug(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": "the body is {id}" }),
        );
    };
    jack_answer(jacks::unplug_tool(&pod, &json!({ "id": body["id"] })).await)
}

/// `GET /v1/jacks`: every jack of the pod and its state (decision `jacks`),
/// for the pod's owner.
async fn get_jacks(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    if let Some(r) = refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    axum::Json(json!({ "jacks": jacks::list(&pod).await })).into_response()
}

/// Run a blocking store operation off the async workers.
pub async fn blocking<T: Send + 'static>(
    state: &Pod,
    f: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let state = state.clone();
    match tokio::task::spawn_blocking(move || f(&state.store)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) if store::is_full(&e) => {
            tracing::warn!("write refused: the pod's storage is full");
            Err(api_error(
                StatusCode::INSUFFICIENT_STORAGE,
                json!({ "error": "pod_storage_full",
                        "detail": "this pod's storage is full: delete something (uploads, layouts, collection rows) first" }),
            ))
        }
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

/// `GET /v1/construct/data/<id>`: the data of the home feeds element `id`
/// needs (decision `content-addressed-construct`), with a use site's
/// bindings (`?bind=`).
pub(crate) async fn get_element_data(
    State(state): State<Pod>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = construct::http::guard(&state, &headers).await {
        return r;
    }
    if !construct_elements::is_object_id(&id) {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_object_id" }),
        );
    }
    let caller = match state.caller(&headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let manifest = construct::seal::wants_manifest(&headers);
    let req = RequestCtx {
        client: layouts::client_caps(&headers),
        headers,
        session: caller.clone().into_session(),
    };
    let (p, lookup) = (state.clone(), id.clone());
    let client = req.client.clone();
    // A use site's bindings (`?bind=`): the data its bound tree reads too.
    let inputs = construct::http::inputs_param(q.get("bind").map(String::as_str));
    let owned = tokio::task::spawn_blocking(move || {
        // The elements a manifest names are made when it is: have them
        // here, should a write have cleared them since this client asked.
        if manifest {
            let _ = construct::seal::manifest_for(&p, &client);
        }
        construct::http::bound_element_feeds(&p, &lookup, &inputs)
    })
    .await
    .unwrap_or_default();
    let feeds: Vec<(&str, &str)> = owned
        .iter()
        .map(|(s, f)| (s.as_str(), f.as_str()))
        .collect();
    match compose(&state, &feeds, &caller, &req).await {
        Ok(b) => Json(b).into_response(),
        Err(r) => *r,
    }
}

/// Run `feeds` (`(source, feed)`, checked by the catalog) concurrently and
/// merge their data in order. A feed whose source needs a sign-in the caller
/// lacks is skipped; a failing one is skipped and recorded; a stale sign-in
/// on a pinned session (a pod token, D46) is detached (on any other session
/// it is a 401 with the source's screen). The data also gets one
/// `core://sources/<id>` entity per source, `{title, signed_in, error?}`,
/// so a layout can tell a signed-out or failing source apart, and the
/// pod's theme (`core://theme`).
pub(crate) async fn compose(
    state: &Pod,
    feeds: &[(&str, &str)],
    caller: &Caller,
    req: &RequestCtx,
) -> Result<protocol::DataBundle, Box<Response>> {
    let runs = feeds.iter().map(|(source, feed)| async move {
        let (adapter, manifest) = state.catalog.source(source)?;
        if let Some(sign_in) = manifest.sign_in
            && !caller.has(sign_in)
        {
            return None;
        }
        Some((
            adapter.name(),
            manifest.sign_in,
            adapter.feed(state, feed, req).await,
        ))
    });
    let mut bundle = protocol::DataBundle::default();
    // Sources whose sign-in went stale or whose feed failed.
    let mut detached: Vec<&str> = Vec::new();
    let mut errors: Vec<(&str, String)> = Vec::new();
    for (source, sign_in, answer) in futures_util::future::join_all(runs)
        .await
        .into_iter()
        .flatten()
    {
        match answer {
            Hydration::Data(data) => {
                bundle.entities.extend(data.entities);
                bundle.queries.extend(data.queries);
            }
            Hydration::Screen(_) => errors.push((source, "unavailable".into())),
            Hydration::StaleSession(screen) => {
                // A pinned session stays; only the unusable sign-in goes.
                if let (Some(session), Some(sign_in)) = (&req.session, sign_in)
                    && session.pinned
                {
                    let (hash, sign_in) = (session.token_hash.clone(), sign_in.to_owned());
                    if let Err(e) =
                        blocking(state, move |s| s.detach_sign_in(&hash, &sign_in)).await
                    {
                        return Err(Box::new(e.into_response()));
                    }
                    detached.push(source);
                    continue;
                }
                return Err(Box::new(session::stale_session(screen)));
            }
            Hydration::Error(r) => {
                let status = r.status();
                tracing::warn!(source, %status, "a feed failed; served without it");
                errors.push((source, format!("error {}", status.as_u16())));
            }
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for (source, _) in feeds {
        let Some((adapter, manifest)) = state.catalog.source(source) else {
            continue;
        };
        let name = adapter.name();
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let signed_in = !detached.contains(&name) && manifest.sign_in.is_none_or(|s| caller.has(s));
        let mut e = protocol::EntityRecord::default();
        e.set("id", name);
        e.set("title", manifest.title);
        e.set("signed_in", signed_in);
        if let Some((_, err)) = errors.iter().find(|(s, _)| *s == name) {
            e.set("error", err.clone());
        }
        bundle.entities.insert(format!("core://sources/{name}"), e);
    }
    // The pod's theme (decision `plumbing-and-porcelain`): the app serves it
    // as `local://theme` for the device's mode.
    match blocking(state, porcelain::theme_entity).await {
        Ok(e) => {
            bundle
                .entities
                .insert(porcelain::THEME_ENTITY.to_owned(), e);
        }
        Err(e) => return Err(Box::new(e.into_response())),
    }
    Ok(bundle)
}

/// What a [`Source::feed`] answers for a feed name it doesn't know (the
/// catalog checks app feeds, so only a direct caller can get it): 404
/// `feed_not_found`.
pub fn unknown_feed(source: &str, feed: &str) -> Hydration {
    Hydration::Error(json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "feed_not_found", "source": source, "feed": feed }),
    ))
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

#[cfg(test)]
mod scope_tests {
    use super::*;
    use tower::ServiceExt;

    async fn status(pod: &Pod, method: Method, path: &str, scope: session::Scope) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(session::INTERNAL_HEADER, pod.internal_as("nobody", scope))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        app(pod.clone()).oneshot(req).await.unwrap().status()
    }

    /// Issue #435: an internal request an MCP credential started reads, but
    /// can't change jacks (or act upstream); the pod token's still can.
    #[tokio::test]
    async fn an_mcp_scoped_internal_request_only_reads() {
        let pod: Pod = Arc::new(AppState::new(Store::open_in_memory().unwrap(), vec![]));
        let mcp = session::Scope::Mcp;
        assert_eq!(
            status(&pod, Method::POST, "/v1/jacks/plug", mcp).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(&pod, Method::POST, "/v1/auth/logout", mcp).await,
            StatusCode::FORBIDDEN
        );
        assert_ne!(
            status(&pod, Method::GET, "/v1/jacks", mcp).await,
            StatusCode::FORBIDDEN
        );
        let full = session::Scope::Full;
        assert_ne!(
            status(&pod, Method::POST, "/v1/jacks/plug", full).await,
            StatusCode::FORBIDDEN
        );
        // A header a client forged (no secret) is no internal request at all.
        assert!(
            pod.internal_caller(&{
                let mut h = HeaderMap::new();
                h.insert(session::INTERNAL_HEADER, "x:full:nobody".parse().unwrap());
                h
            })
            .is_none()
        );
    }
}

/// The server's public base URL from the environment: `CONSTRUCT_PUBLIC_URL`
/// (trailing `/` dropped), else on Fly (`FLY_APP_NAME`) `https://<app>.fly.dev`.
/// Behind Fly the request's `Host` is never trusted for a URL the server
/// hands out (OAuth metadata, redirect URIs, share links). `None` off Fly
/// with no variable: callers fall back to the request, for development.
pub fn public_url_from_env() -> Option<String> {
    public_url_from(
        std::env::var("CONSTRUCT_PUBLIC_URL").ok().as_deref(),
        std::env::var("FLY_APP_NAME").ok().as_deref(),
    )
}

fn public_url_from(public_url: Option<&str>, fly_app: Option<&str>) -> Option<String> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    if let Some(u) = set(public_url) {
        return Some(u.trim_end_matches('/').to_owned());
    }
    set(fly_app).map(|app| format!("https://{app}.fly.dev"))
}

#[cfg(test)]
mod public_url_tests {
    use super::public_url_from;

    #[test]
    fn explicit_url_wins_then_fly_then_none() {
        assert_eq!(
            public_url_from(Some("https://a.example/"), Some("app")).as_deref(),
            Some("https://a.example")
        );
        assert_eq!(
            public_url_from(None, Some("app")).as_deref(),
            Some("https://app.fly.dev")
        );
        assert_eq!(public_url_from(Some(" "), Some("")), None);
        assert_eq!(public_url_from(None, None), None);
    }
}
