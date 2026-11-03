//! The app-adapter interface (docs/DECISIONS.md D40): how an app plugs into
//! the pod.
//!
//! The core knows apps only through [`AppAdapter`]. The `server` binary builds
//! the adapters at compile time and hands them to [`crate::AppState::try_new`],
//! which collects them into a [`Catalog`].

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::{Router, http::HeaderMap, response::Response};
use protocol::{
    DataBundle, NotificationChannel, Orientation, SlotSpec, TextStyle, UiNode, WorkspaceSpec,
};

use crate::AppState;
use crate::layouts::LayoutSource;
use crate::notify::{PollCtx, PollOutcome};
use crate::session::Session;
use crate::store::Migration;

/// The handle adapters get: the shared pod state. It is also the axum state
/// of every route, the adapter's included.
pub type Pod = Arc<AppState>;

/// A boxed, `Send` future: what [`AppAdapter::hydrate`] returns (keeps the
/// trait usable as `dyn AppAdapter`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One app integration plugged into the pod (D40).
///
/// An adapter connects the pod to one upstream service (Pixelfed today) and
/// serves one or more apps. Each app is a workspace whose layout comes from
/// `layouts/<app>/NNNN.json`. The core owns everything generic: layout
/// versions, schema negotiation, SSE, `/v1/apps`, sessions and logout. The
/// adapter supplies only data, its own routes and its own tables.
///
/// Rules for implementors:
/// - Never serve a layout yourself. Return data from `hydrate`, and the core
///   picks the layout version this client can draw and merges it in.
/// - Keep the engine and clients app-agnostic (D34). A new screen or action
///   ships as a layout file plus adapter code, never as a client change.
/// - Upstream credentials stay on the pod. Clients only ever hold a pod
///   session token (D17).
/// - The `server` binary builds each adapter once at startup, and every
///   request shares it. Keep any mutable state behind a lock.
///
/// A layout-only app (layout files plus fixed data, no upstream) needs no
/// adapter of its own: use [`crate::StaticAdapter`].
pub trait AppAdapter: Any + Send + Sync {
    /// A short, unique name for this adapter, such as `pixelfed`.
    ///
    /// It keys the adapter's row in `adapter_migrations`, so never change it
    /// once shipped. Use it as the prefix of the adapter's tables
    /// (`pixelfed_*`) and routes (`/v1/pixelfed/...`).
    fn name(&self) -> &'static str;

    /// The apps this adapter serves, in `GET /v1/apps` order.
    ///
    /// Called once at startup. Each `AppInfo::id` must be unique across all
    /// adapters (the pod refuses to start on a duplicate) and must have a
    /// `layouts/<id>/` directory among `layout_files`.
    fn apps(&self) -> Vec<AppInfo>;

    /// The layout files for this adapter's apps, per app in number order.
    ///
    /// Called once at startup by the layout sync, before the pod serves. The
    /// usual rules apply: numbers have no gaps, every file declares the current
    /// `SCHEMA_VERSION`, and a deployed file is never edited or renumbered
    /// (the pod refuses to start). Every app in `apps` needs at least one file.
    fn layout_files(&self) -> &'static [LayoutSource];

    /// Deployed files that hold node types later removed from the protocol
    /// (`(app_id, number)`). They still parse and are served to the clients
    /// that draw them; no other file may hold an unknown node type (D37).
    /// Only ever add to this list when a node type is removed.
    fn legacy_files(&self) -> &'static [(&'static str, u32)] {
        &[]
    }

    /// This adapter's own HTTP routes: actions, screen data, sign-in, media.
    ///
    /// Called once at startup. The core merges this router into its own,
    /// under the same fault-injection layer. Put routes under
    /// `/v1/<adapter>/...` (and `/v1/auth/<adapter>/...` for sign-in) so they
    /// can't collide with the core or another adapter. Layout files name these
    /// paths, so a path is a public contract: never rename one that a
    /// deployed layout uses. Handlers check the caller with
    /// [`AppState::caller`]; find their adapter with [`AppState::adapter`].
    fn router(&self) -> Router<Pod> {
        Router::new()
    }

    /// Path prefixes of [`Self::router`] routes a client loads without its
    /// bearer, such as image URLs handed to an image loader (e.g.
    /// `/v1/pixelfed/media/`). A pod host (D46) serves only these under the
    /// pod's public prefix ([`crate::public_prefix`]); every other route needs
    /// the pod's token. Keep them to data that is public anyway.
    fn public_paths(&self) -> &'static [&'static str] {
        &[]
    }

    /// The sign-ins this adapter provides, such as `pixelfed`.
    ///
    /// Called once at startup. Each provider has an id that is unique across
    /// adapters. Apps from any adapter can require it with
    /// `AuthPolicy::SignIn(id)`; the core then serves [`Self::signed_out`]
    /// to callers without it. When a user finishes signing in, call
    /// [`crate::store::open_session_on`] in the same transaction that stores
    /// the upstream account. Return an empty list if the adapter needs no
    /// sign-in.
    fn sign_ins(&self) -> Vec<SignInProvider> {
        Vec::new()
    }

    /// The screen for a caller who lacks sign-in `sign_in` (one of
    /// [`Self::sign_ins`]) and opens `app`, which requires it: usually a
    /// sentence and a button whose `AuthLogin` intent names this adapter's
    /// auth routes. Its root must be a `core://Workspace` whose id is
    /// `app.id`. Served as is, with no layout version; with a stale bearer
    /// the core sends it as the body of a 401.
    fn signed_out(&self, sign_in: &str, app: &AppInfo, req: &RequestCtx) -> WorkspaceSpec {
        let _ = req;
        default_signed_out(sign_in, app)
    }

    /// Its own SQL migrations, tracked per adapter.
    ///
    /// Run once at startup, after the core's migrations and before the layout
    /// sync. Step `i` upgrades this adapter's version from `i` to `i + 1` in
    /// its own IMMEDIATE transaction. The list is append-only, like the
    /// core's: never edit or reorder a step that has shipped, because
    /// staging's database has already run it. Name tables `<adapter>_*`.
    fn migrations(&self) -> &'static [Migration] {
        &[]
    }

    /// Upkeep each time a pod opens, after the migrations, for data that
    /// depends on the server's configuration rather than the schema: e.g.
    /// sealing stored credentials under the current key
    /// ([`Store::secrets`](crate::store::Store::secrets)), which a one-time
    /// migration can't do because the key may be set or rotated later. An
    /// error stops the pod from opening, so only fail on database errors.
    fn on_open(&self, store: &crate::store::Store) -> Result<(), String> {
        let _ = store;
        Ok(())
    }

    /// The data for `GET /v1/workspaces/:id`, for one of this adapter's apps.
    ///
    /// Called on every workspace fetch, including each refetch after an SSE
    /// layout event, so keep it to as few upstream calls as you can. `app_id`
    /// is always one of `apps()`. `req` carries the request headers, the
    /// client's schema and the caller's session. For an app with
    /// `AuthPolicy::SignIn`, the core has already checked that the session
    /// has that sign-in. Return `Hydration::Data` in the normal case (see
    /// `Hydration` for the others). Don't panic: an upstream failure becomes
    /// `Hydration::Error`, usually a 502 so the client retries.
    fn hydrate<'a>(
        &'a self,
        pod: &'a Pod,
        app_id: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration>;

    /// The kinds of notification this adapter's apps send (D52), such as
    /// "Likes". Called at startup and when listing channels. Each channel's
    /// `app_id` must be one of [`Self::apps`]; ids are `[a-z0-9_]{1,32}`,
    /// unique per app, and never change once shipped (devices store their
    /// choices by id). Write titles and descriptions for people.
    fn notification_channels(&self) -> Vec<NotificationChannel> {
        Vec::new()
    }

    /// How often the host should call [`Self::poll_notifications`] (at least
    /// 30 s); `None`: this adapter doesn't poll. Polls happen only while the
    /// pod has a registered device.
    fn poll_interval(&self) -> Option<std::time::Duration> {
        None
    }

    /// Fetch new notifications from the upstream (D52): called by the host
    /// scheduler every [`Self::poll_interval`], outside any request, with a
    /// 20 s timeout. Keep your own cursor in your own tables; report each
    /// upstream event with a stable `dedupe_key` (reporting one twice is
    /// harmless). Write the text yourself: clients show `title` and `body` as
    /// they are and open `target_uri` in your app's layout routes. Mark a
    /// first poll's backlog `quiet`. An adapter whose upstream can call the
    /// pod instead reports from its own route with [`AppState::notify`].
    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        let _ = (pod, ctx);
        Box::pin(async { PollOutcome::default() })
    }

    /// Background work to pick up when a pod opens (D50), such as jobs a
    /// restart interrupted. Called once per pod, on the pod's runtime, after
    /// migrations; runs concurrently with requests. Default: nothing.
    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        let _ = pod;
        Box::pin(async {})
    }

    /// The upstream account behind sign-in `sign_in` (one of
    /// [`Self::sign_ins`]) on `session`, which has it: what the client's
    /// Settings list shows (`GET /v1/auth/sign-ins`). Keep it to stored
    /// data, no upstream calls. Default: nothing known (the row shows only
    /// the provider's title).
    fn sign_in_account<'a>(
        &'a self,
        pod: &'a Pod,
        sign_in: &'a str,
        session: &'a Session,
    ) -> BoxFuture<'a, Option<SignInAccount>> {
        let _ = (pod, sign_in, session);
        Box::pin(async { None })
    }
}

/// The account behind a session's sign-in ([`AppAdapter::sign_in_account`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignInAccount {
    /// The upstream server's base URL, e.g. `https://pixelfed.social`.
    pub server: Option<String>,
    /// The upstream username, without a leading `@`.
    pub username: Option<String>,
}

/// What [`AppAdapter::hydrate`] returns.
pub enum Hydration {
    /// The normal case: entities and queries for the app's layout. The core
    /// adds the newest layout version this client can draw and sets
    /// `X-Construct-Layout-Version`. Entity URIs use the adapter's own
    /// scheme, such as `pixelfed://posts/<id>`.
    Data(DataBundle),
    /// A fixed screen that is not a layout version, such as the signed-out
    /// page with its sign-in button. Served as is, with no version header.
    Screen(WorkspaceSpec),
    /// The bearer named a session that no longer works: unknown, expired, or
    /// its upstream token was revoked. The core answers 401 with
    /// `X-Construct-Session: invalid` and this screen as the body. The client
    /// then clears its session and refetches without it.
    StaleSession(WorkspaceSpec),
    /// Any other failure, passed through as is. Use the pod's JSON error
    /// shape: `{"error": "<code>", ...}`.
    Error(Response),
}

/// How an app appears in `GET /v1/apps`, and how the core guards it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppInfo {
    /// The workspace id and its `layouts/` directory name, e.g.
    /// `pixelfed_home`. Never change it once shipped: clients cache by it.
    pub id: &'static str,
    /// Shown in the app list.
    pub title: &'static str,
    /// An icon name the clients know (`photo`).
    pub icon: &'static str,
    /// Listed only with `CONSTRUCT_SHOW_DEV_APPS=1`. Its endpoints work
    /// either way (tests and the simulator use them).
    pub dev: bool,
    /// `AuthPolicy::SignIn("pixelfed")`: the session must carry that sign-in.
    /// Without it the core serves the provider's signed-out screen. A bearer
    /// that is not a valid session gets 401 on the workspace and its SSE
    /// stream, so the client learns it is stale.
    /// `AuthPolicy::Public`: the app needs no sign-in.
    pub auth: AuthPolicy,
}

/// Whether an app needs a sign-in; see [`AppInfo::auth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPolicy {
    /// Anyone may open it; a bearer is ignored unless it is valid.
    Public,
    /// The caller's session must have completed this sign-in (a
    /// [`SignInProvider::id`] some adapter registers).
    SignIn(&'static str),
}

/// A sign-in an adapter offers ([`AppAdapter::sign_ins`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInProvider {
    /// Unique across adapters, e.g. `pixelfed`. Recorded on sessions
    /// (`session_sign_ins`), so never change it once shipped.
    pub id: &'static str,
    /// Shown in the client's list of sign-ins, e.g. `Pixelfed`.
    pub title: &'static str,
}

/// What an adapter learns about a workspace request.
#[derive(Debug, Clone)]
pub struct RequestCtx {
    /// The request headers (`Host` and `X-Forwarded-Proto` for absolute
    /// URLs; [`protocol::SCHEMA_HEADER`]).
    pub headers: HeaderMap,
    /// The client's schema ([`crate::layouts::client_schema`]).
    pub schema: u32,
    /// The caller's valid session, if the request carried one.
    pub session: Option<Session>,
}

/// The default signed-out screen: one line of text.
fn default_signed_out(sign_in: &str, app: &AppInfo) -> WorkspaceSpec {
    WorkspaceSpec {
        version: crate::BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: app.id.into(),
            title: app.title.into(),
            screens: Vec::new(),
            slots: vec![SlotSpec {
                slot_id: "auth".into(),
                provider_uri: format!("core://auth/{sign_in}"),
                content: UiNode::Container {
                    orientation: Orientation::Vertical,
                    padding_dp: 16,
                    children: vec![UiNode::Text {
                        value_expr: format!("Sign in ({sign_in}) to open {}.", app.title),
                        style: TextStyle::Body,
                    }],
                },
            }],
        },
        entities: Default::default(),
        queries: Default::default(),
    }
}

// ---------- catalog ----------

/// Every adapter's apps, layout files and sign-ins, checked once at startup.
pub struct Catalog {
    adapters: Vec<Arc<dyn AppAdapter>>,
    /// `(app, index into adapters)`, in `GET /v1/apps` order.
    apps: Vec<(AppInfo, usize)>,
    files: Vec<LayoutSource>,
    legacy: Vec<(&'static str, u32)>,
    /// `(sign-in id, index into adapters)`.
    sign_ins: Vec<(SignInProvider, usize)>,
}

impl Catalog {
    /// Collect `adapters`. `Err` (the message to stop the start with) for a
    /// duplicate adapter name, app id or sign-in id, or an app that requires
    /// a sign-in no adapter provides.
    pub fn new(adapters: Vec<Arc<dyn AppAdapter>>) -> Result<Self, String> {
        let mut apps: Vec<(AppInfo, usize)> = Vec::new();
        let mut files = Vec::new();
        let mut legacy = Vec::new();
        let mut sign_ins: Vec<(SignInProvider, usize)> = Vec::new();
        let mut names: Vec<&'static str> = Vec::new();
        for (i, a) in adapters.iter().enumerate() {
            let name = a.name();
            if names.contains(&name) {
                return Err(format!("two adapters are named '{name}'"));
            }
            names.push(name);
            for app in a.apps() {
                if apps.iter().any(|(x, _)| x.id == app.id) {
                    return Err(format!("app '{}' is served by two adapters", app.id));
                }
                apps.push((app, i));
            }
            for p in a.sign_ins() {
                if sign_ins.iter().any(|(s, _)| s.id == p.id) {
                    return Err(format!("sign-in '{}' is provided by two adapters", p.id));
                }
                sign_ins.push((p, i));
            }
            files.extend_from_slice(a.layout_files());
            legacy.extend_from_slice(a.legacy_files());
        }
        for (app, _) in &apps {
            if let AuthPolicy::SignIn(id) = app.auth
                && !sign_ins.iter().any(|(s, _)| s.id == id)
            {
                return Err(format!(
                    "app '{}' requires sign-in '{id}', which no adapter provides",
                    app.id
                ));
            }
        }
        for (i, a) in adapters.iter().enumerate() {
            let channels = a.notification_channels();
            for (n, c) in channels.iter().enumerate() {
                if !apps.iter().any(|(app, j)| app.id == c.app_id && *j == i) {
                    return Err(format!(
                        "adapter '{}' declares channel '{}' for '{}', which is not one of its apps",
                        a.name(),
                        c.id,
                        c.app_id
                    ));
                }
                if !crate::notify::valid_channel_id(&c.id) {
                    return Err(format!("channel id '{}' is not [a-z0-9_]{{1,32}}", c.id));
                }
                if channels[..n]
                    .iter()
                    .any(|o| o.app_id == c.app_id && o.id == c.id)
                {
                    return Err(format!(
                        "app '{}' declares channel '{}' twice",
                        c.app_id, c.id
                    ));
                }
            }
            if a.poll_interval().is_some() && channels.is_empty() {
                return Err(format!(
                    "adapter '{}' polls for notifications but declares no channel",
                    a.name()
                ));
            }
        }
        for f in &files {
            if !apps.iter().any(|(a, _)| a.id == f.app_id) {
                return Err(format!(
                    "layouts/{}/{}.json: '{}' is not an app",
                    f.app_id, f.name, f.app_id
                ));
            }
        }
        Ok(Self {
            adapters,
            apps,
            files,
            legacy,
            sign_ins,
        })
    }

    /// No adapters (tests of the core alone).
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("an empty catalog is valid")
    }

    pub fn adapters(&self) -> &[Arc<dyn AppAdapter>] {
        &self.adapters
    }

    /// Every app, in `GET /v1/apps` order.
    pub fn apps(&self) -> impl Iterator<Item = &AppInfo> {
        self.apps.iter().map(|(a, _)| a)
    }

    pub fn app(&self, id: &str) -> Option<&AppInfo> {
        self.apps.iter().find(|(a, _)| a.id == id).map(|(a, _)| a)
    }

    /// Whether the workspace is an app (has a versioned layout).
    pub fn is_app(&self, id: &str) -> bool {
        self.app(id).is_some()
    }

    /// The adapter serving app `id`.
    pub fn adapter_for(&self, id: &str) -> Option<&Arc<dyn AppAdapter>> {
        let (_, i) = self.apps.iter().find(|(a, _)| a.id == id)?;
        self.adapters.get(*i)
    }

    /// The adapter providing sign-in `id`.
    pub fn sign_in_adapter(&self, id: &str) -> Option<&Arc<dyn AppAdapter>> {
        let (_, i) = self.sign_ins.iter().find(|(s, _)| s.id == id)?;
        self.adapters.get(*i)
    }

    /// Sign-in `id` as its adapter declared it.
    pub fn sign_in(&self, id: &str) -> Option<&SignInProvider> {
        self.sign_ins
            .iter()
            .find(|(s, _)| s.id == id)
            .map(|(s, _)| s)
    }

    /// Whether `path` is under an adapter's [`AppAdapter::public_paths`]
    /// (and has no `..` segment).
    pub fn is_public_path(&self, path: &str) -> bool {
        !path.split('/').any(|seg| seg == ".." || seg == ".")
            && self
                .adapters
                .iter()
                .flat_map(|a| a.public_paths())
                .chain(std::iter::once(&crate::uploads::PUBLIC_PATH))
                .any(|p| path.starts_with(p))
    }

    /// Every adapter's layout files.
    pub fn files(&self) -> &[LayoutSource] {
        &self.files
    }

    /// Whether `(app_id, number)` is a legacy file ([`AppAdapter::legacy_files`]).
    pub fn is_legacy(&self, app_id: &str, number: u32) -> bool {
        self.legacy
            .iter()
            .any(|(a, n)| *a == app_id && *n == number)
    }
}
