//! The source interface (docs/DECISIONS.md D40, decision `source-adapters`):
//! how a data source plugs into the pod.
//!
//! The core knows sources only through [`Source`]. The `server` binary builds
//! the adapters at compile time and hands them to [`crate::AppState::try_new`],
//! which collects them into a [`Catalog`].

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::{Router, http::HeaderMap, response::Response};
use protocol::{DataBundle, NotificationChannel, WorkspaceSpec};

use crate::AppState;
use crate::layouts::LayoutSource;
use crate::notify::{PollCtx, PollOutcome};
use crate::session::Session;
use crate::source::{self, SourceManifest};
use crate::store::{Step, Store};

/// The handle adapters get: the shared pod state. It is also the axum state
/// of every route, the adapter's included.
pub type Pod = Arc<AppState>;

/// A boxed, `Send` future: what [`Source::feed`] returns (keeps the
/// trait usable as `dyn Source`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One source plugged into the pod (D40): an adapter crate `source-*`.
///
/// A source connects the pod to its data (an upstream service such as
/// Pixelfed, or tables of its own), with its routes and jacks, and ships
/// one or more blueprints. Each blueprint's layout comes from
/// `layouts/<blueprint>/NNNN.json` in the source's crate. The core owns everything generic: the
/// construct, feature negotiation, SSE, sessions and logout. The
/// adapter supplies only data, its own routes and its own tables.
///
/// Rules for implementors:
/// - Never serve a layout yourself. Return data from your feeds
///   ([`Self::feed`]), and the core picks the layout version this client
///   can draw and merges it in.
/// - Keep the engine and clients source-agnostic (D34). A new screen or action
///   ships as a layout file plus adapter code, never as a client change.
/// - Upstream credentials stay on the pod. Clients only ever hold a pod
///   session token (D17).
/// - The `server` binary builds each adapter once at startup, and every
///   request shares it. Keep any mutable state behind a lock.
///
/// A layout-only blueprint (layout files plus fixed data, no upstream)
/// needs no source of its own: use [`crate::StaticAdapter`].
pub trait Source: Any + Send + Sync {
    /// A short, unique name for this adapter, such as `pixelfed`.
    ///
    /// It is the scope of the adapter's migrations (`schema_migrations`), so
    /// never change it once shipped. Use it as the prefix of the adapter's tables
    /// (`pixelfed_*`) and routes (`/v1/pixelfed/...`).
    fn name(&self) -> &'static str;

    /// The blueprints this adapter ships (named `@core/<id>` in every pod).
    ///
    /// Called once at startup. Each `BlueprintInfo::id` must be unique across
    /// all adapters (the pod refuses to start on a duplicate) and must have a
    /// `layouts/<id>/` directory among `layout_files`.
    fn blueprints(&self) -> Vec<BlueprintInfo>;

    /// The layout files of this adapter's blueprints, per blueprint in number
    /// order.
    ///
    /// Called once at startup, before the pod serves. The usual rules apply:
    /// numbers have no gaps from the blueprint's oldest file, and a deployed
    /// file is never edited or renumbered; old files may be deleted (decision
    /// `retired-layout-files`). Every blueprint in `blueprints` needs at
    /// least one file.
    fn layout_files(&self) -> &'static [LayoutSource];

    /// This adapter's server components (decision
    /// `plumbing-and-porcelain`), files `components/<name>/NNNN.json` in its
    /// crate, embedded by its build script (`generate_component_files` in
    /// `pod-core/build_support`). They are `core:<name>` like the core's
    /// own, append-only, and their names are unique across crates. Empty by
    /// default.
    fn component_files(&self) -> &'static [crate::components::ComponentSource] {
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
    /// adapters; a source needs one with [`SourceManifest::sign_in`], and its
    /// feeds are skipped for a caller without it. When a user finishes
    /// signing in, call
    /// [`crate::store::open_session_on`] in the same transaction that stores
    /// the upstream account. Return an empty list if the adapter needs no
    /// sign-in.
    fn sign_ins(&self) -> Vec<SignInProvider> {
        Vec::new()
    }

    /// Its own named migrations, tracked per adapter (decision
    /// `named-migrations`).
    ///
    /// Run at startup, after the core's migrations and before the layout
    /// sync: each step whose name has not run yet, in list order, in its own
    /// IMMEDIATE transaction. Add steps at the end with a new name; never
    /// rename, edit or remove a step that has shipped, because staging's
    /// database has already run it. Name tables `<adapter>_*`.
    fn migrations(&self) -> &'static [Step] {
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

    /// What this adapter offers layouts as a data source (decision
    /// `custom-apps-and-data-sources`): its feeds, screen data paths,
    /// actions and entities. The source's id is [`Self::name`]. Called once
    /// at startup; the pod refuses to start when a layout file uses a pod
    /// path its app's sources don't declare. Declare every path a deployed
    /// layout uses, and never remove one.
    fn manifest(&self) -> SourceManifest;

    /// The data of feed `feed` (one of [`Self::manifest`]'s feeds) for a
    /// layout that binds it, `GET /v1/construct/data/<element>`: the core
    /// runs the home feeds the layout binds and merges their data.
    ///
    /// Called on every data fetch, including each refetch after an SSE
    /// data event, so keep it to as few upstream calls as you can.
    /// `req` carries the request headers, the client's schema and the
    /// caller's session. When the source has a sign-in
    /// ([`SourceManifest::sign_in`]), the core calls this only for a session
    /// that has it. Return `Hydration::Data` in the normal case (see
    /// `Hydration` for the others), with entities under your own schemes
    /// and only the query keys your manifest declares. Don't panic: an
    /// upstream failure becomes `Hydration::Error`, usually a 502 so the
    /// client retries.
    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration>;

    /// The kinds of notification this source's blueprints send (D52), such
    /// as "Likes". Called at startup and when listing channels. Each
    /// channel's `layout` must be one of [`Self::apps`]; ids are
    /// `[a-z0-9_]{1,32}`, unique per layout, and never change once shipped (devices store their
    /// choices by id). Write titles and descriptions for people.
    fn notification_channels(&self) -> Vec<NotificationChannel> {
        Vec::new()
    }

    /// Whether this source sends notifications for the pod's own layouts,
    /// whose channels it declares at run time
    /// ([`Self::layout_notification_channels`]; decision
    /// `collection-reminders`). Such an adapter may poll without declaring
    /// a channel at startup.
    fn notifies_layouts(&self) -> bool {
        false
    }

    /// Where this source keeps the pod's notifications, when it keeps
    /// them (decision `notifications-in-a-collection`: Collections keeps
    /// them as a collection). The first source that offers one holds every
    /// source's; without one, the core keeps them in its own tables.
    fn notification_store(&self) -> Option<std::sync::Arc<dyn crate::NotificationStore>> {
        None
    }

    /// The channels this source declares now for the pod's own layouts
    /// (only when [`Self::notifies_layouts`]): each `layout` a live
    /// layout of the pod, ids `[a-z0-9_]{1,32}` that stay the same while the
    /// thing they stand for exists. The core drops any other. Called often
    /// (every notification, every channel list): keep it to a quick read of
    /// the store (blocking is fine).
    fn layout_notification_channels(
        &self,
        store: &Store,
        catalog: &Catalog,
    ) -> Vec<NotificationChannel> {
        let _ = (store, catalog);
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

    /// Whether this pod must stay open while the server runs, for
    /// background work that can't wait for someone to use the pod (e.g.
    /// following an upstream's event stream). Such a pod is opened when the
    /// server starts and never waits for a request: its
    /// [`AppState::poll_due`](crate::AppState::poll_due) reports it due
    /// ([`KEEP_OPEN_WAKE`](crate::notify::KEEP_OPEN_WAKE)), so the host's
    /// scheduler opens it, and [`Self::resume`] starts the work. Called on
    /// every scheduler pass (every 30 s): keep it to a quick read of the
    /// store. Default: `false`.
    fn keeps_open(&self, store: &Store) -> bool {
        let _ = store;
        false
    }

    /// How a client starts sign-in `sign_in` (one of [`Self::sign_ins`])
    /// without a signed-out screen (from its Settings, or a layout's
    /// `action.device` `sign_in`): `GET /v1/auth/sign-ins` lists it under
    /// `providers`.
    /// Default: `None`, the provider is listed without a way to start it.
    fn sign_in_start<'a>(
        &'a self,
        pod: &'a Pod,
        sign_in: &'a str,
    ) -> BoxFuture<'a, Option<SignInStart>> {
        let _ = (pod, sign_in);
        Box::pin(async { None })
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

    /// Checks of a layout against this source's own data, run on every
    /// layout write that binds this source's feeds, after the core's checks
    /// pass (decision `collections`): e.g. that the bindings name fields its
    /// stored definitions have. `feeds`: home's feeds of this source;
    /// `layout`: the layout as clients draw it (materialized); `conn`: the
    /// write's transaction (read the store through it, not the pod's store,
    /// which the write holds). Default: nothing more.
    fn check_custom_layout(
        &self,
        conn: &rusqlite::Connection,
        feeds: &[String],
        layout: &serde_json::Value,
    ) -> Result<(), crate::validate::LayoutError> {
        let _ = (conn, feeds, layout);
        Ok(())
    }

    /// The jacks this adapter declares (decision `jacks`): the abilities it
    /// lets a pod plug in, such as an account it connects. Ids are unique
    /// across the core and every adapter (checked at startup) and stored,
    /// so never renamed. Default: none.
    fn jacks(&self) -> Vec<crate::jacks::JackSpec> {
        Vec::new()
    }

    /// An account jack's state on this pod (one of [`Self::jacks`] whose
    /// kind is `Account`), from stored data only. Default: unplugged.
    fn jack_status<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> BoxFuture<'a, Option<crate::jacks::JackStatus>> {
        let _ = (pod, jack);
        Box::pin(async { None })
    }

    /// Unplug account jack `jack` (forget the account), or `None` when it
    /// can only be unplugged where it was signed in. Default: `None`.
    fn unplug_jack<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> Option<BoxFuture<'a, Result<(), String>>> {
        let _ = (pod, jack);
        None
    }

    /// Extra MCP tools this source offers Claude (decision `collections`),
    /// as MCP tool definitions (`{name, description, inputSchema}`). Names
    /// must not clash with the core's tools or another adapter's (checked
    /// at startup). Default: none.
    fn mcp_tools(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    /// One more paragraph for the MCP `instructions` (how to use
    /// [`Self::mcp_tools`]). Default: none.
    fn mcp_instructions(&self) -> Option<&'static str> {
        None
    }

    /// Run MCP tool `tool` (one of [`Self::mcp_tools`]) with `args`: the
    /// result, or an error body (`{"error": ..., "detail": ...}`) that
    /// becomes an `isError` tool result. `None` for a tool it doesn't have.
    fn mcp_call<'a>(
        &'a self,
        pod: &'a Pod,
        tool: &'a str,
        args: &'a serde_json::Value,
    ) -> Option<BoxFuture<'a, Result<serde_json::Value, serde_json::Value>>> {
        let _ = (pod, tool, args);
        None
    }
}

/// How a client starts a sign-in from its Settings
/// ([`Source::sign_in_start`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SignInStart {
    /// The `action.auth.login` intent for the default server.
    pub intent: protocol::ActionIntent,
    /// When the user may type another server (D45): the text field's
    /// placeholder; the client then sends the intent with the typed server
    /// as `instance_url`.
    pub server_placeholder: Option<String>,
}

/// The account behind a session's sign-in ([`Source::sign_in_account`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignInAccount {
    /// The upstream server's base URL, e.g. `https://pixelfed.social`.
    pub server: Option<String>,
    /// The upstream username, without a leading `@`.
    pub username: Option<String>,
}

/// What [`Source::feed`] returns.
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

/// A blueprint a source ships: the layout files `layouts/<id>/` of its crate,
/// named `@core/<id>` in every pod (decision `content-addressed-construct`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlueprintInfo {
    /// Its id and `layouts/` directory name, e.g. `pixelfed`: its name is
    /// `@core/<id>`, and an edge to it is keyed `<id>`. Renaming it takes an
    /// adapter migration ([`crate::store::rename_app_on`]).
    pub id: &'static str,
    /// Its element's title.
    pub title: &'static str,
    /// An icon name the clients know (`photo`).
    pub icon: &'static str,
    /// Listed to MCP only with `CONSTRUCT_SHOW_DEV_APPS=1`. It works either
    /// way (tests and the simulator use it).
    pub dev: bool,
    /// `AuthPolicy::SignIn("pixelfed")`: what it shows needs that sign-in
    /// (a provider some adapter registers, checked at startup);
    /// `AuthPolicy::Public`: none.
    pub auth: AuthPolicy,
    /// The feeds it draws, as `(source, feed)`: a source is an adapter's
    /// [`Source::name`], a feed one of its manifest's, e.g.
    /// `[("pixelfed", "home")]`. A layout built on it binds these through
    /// home (MCP `list_layouts` lists them).
    pub feeds: &'static [(&'static str, &'static str)],
}

/// Whether a blueprint needs a sign-in; see [`BlueprintInfo::auth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPolicy {
    /// Anyone may open it; a bearer is ignored unless it is valid.
    Public,
    /// The caller's session must have completed this sign-in (a
    /// [`SignInProvider::id`] some adapter registers).
    SignIn(&'static str),
}

/// A sign-in an adapter offers ([`Source::sign_ins`]).
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
    /// URLs; [`protocol::FEATURES_HEADER`]).
    pub headers: HeaderMap,
    /// What the client draws: its features
    /// ([`crate::layouts::client_caps`]).
    pub client: protocol::ClientCaps,
    /// The caller's valid session, if the request carried one.
    pub session: Option<Session>,
}

// ---------- catalog ----------

/// Every adapter's apps, layout files and sign-ins, checked once at startup.
pub struct Catalog {
    adapters: Vec<Arc<dyn Source>>,
    /// Each adapter's manifest, by index into `adapters`.
    manifests: Vec<SourceManifest>,
    /// `(blueprint, index into adapters)`, in adapter order.
    blueprints: Vec<(BlueprintInfo, usize)>,
    files: Vec<LayoutSource>,
    /// `(sign-in id, index into adapters)`.
    sign_ins: Vec<(SignInProvider, usize)>,
    /// Every server component file: the core's, then each adapter's.
    components: Vec<crate::components::ComponentSource>,
}

impl Catalog {
    /// Collect `adapters`. `Err` (the message to stop the start with) for a
    /// duplicate adapter name, app id or sign-in id, an app that requires a
    /// sign-in no adapter provides, or a bad set of data sources: an invalid
    /// manifest, an entity scheme or query key two sources claim, a sign-in
    /// no adapter provides, or an app feed no source declares.
    pub fn new(adapters: Vec<Arc<dyn Source>>) -> Result<Self, String> {
        let mut apps: Vec<(BlueprintInfo, usize)> = Vec::new();
        let mut files = Vec::new();
        let mut sign_ins: Vec<(SignInProvider, usize)> = Vec::new();
        let mut names: Vec<&'static str> = Vec::new();
        for (i, a) in adapters.iter().enumerate() {
            let name = a.name();
            if names.contains(&name) {
                return Err(format!("two adapters are named '{name}'"));
            }
            names.push(name);
            for j in a.jacks() {
                if crate::jacks::core_jacks().iter().any(|c| c.id == j.id)
                    || adapters[..i]
                        .iter()
                        .any(|b| b.jacks().iter().any(|x| x.id == j.id))
                    || a.jacks().iter().filter(|x| x.id == j.id).count() > 1
                {
                    return Err(format!("jack '{}' is declared twice", j.id));
                }
            }
            for app in a.blueprints() {
                if !app
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || app.id.is_empty()
                {
                    return Err(format!(
                        "blueprint id '{}' is not [A-Za-z0-9_]+ ('-' is kept for the pod's layouts)",
                        app.id
                    ));
                }
                if apps.iter().any(|(x, _)| x.id == app.id) {
                    return Err(format!("blueprint '{}' is shipped by two adapters", app.id));
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
        }
        let mut components = crate::components::CORE_COMPONENT_FILES.to_vec();
        for a in &adapters {
            for c in a.component_files() {
                if let Some(o) = components
                    .iter()
                    .find(|o| o.name == c.name && o.dir != c.dir)
                {
                    return Err(format!(
                        "component '{}' is in both {} and {}; component names are unique",
                        c.name, o.dir, c.dir
                    ));
                }
                components.push(*c);
            }
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
                if !apps.iter().any(|(app, j)| app.id == c.layout && *j == i) {
                    return Err(format!(
                        "adapter '{}' declares channel '{}' for '{}', which is not one of its apps",
                        a.name(),
                        c.id,
                        c.layout
                    ));
                }
                if !crate::notify::valid_channel_id(&c.id) {
                    return Err(format!("channel id '{}' is not [a-z0-9_]{{1,32}}", c.id));
                }
                if channels[..n]
                    .iter()
                    .any(|o| o.layout == c.layout && o.id == c.id)
                {
                    return Err(format!(
                        "app '{}' declares channel '{}' twice",
                        c.layout, c.id
                    ));
                }
            }
            if a.poll_interval().is_some() && channels.is_empty() && !a.notifies_layouts() {
                return Err(format!(
                    "adapter '{}' polls for notifications but declares no channel",
                    a.name()
                ));
            }
        }
        let manifests: Vec<SourceManifest> = adapters.iter().map(|a| a.manifest()).collect();
        check_sources(&adapters, &manifests, &apps, &sign_ins)?;
        let mut tool_names: Vec<String> = crate::mcp::core_tool_names();
        for a in &adapters {
            for t in a.mcp_tools() {
                let name = t["name"].as_str().unwrap_or("").to_owned();
                if name.is_empty() || tool_names.contains(&name) {
                    return Err(format!(
                        "adapter '{}' declares MCP tool '{name}', which is empty or taken",
                        a.name()
                    ));
                }
                tool_names.push(name);
            }
        }
        for f in &files {
            if !apps.iter().any(|(a, _)| a.id == f.app_id) {
                return Err(format!(
                    "{}/{}/{}.json: '{}' is not an app",
                    f.dir, f.app_id, f.name, f.app_id
                ));
            }
        }
        Ok(Self {
            adapters,
            manifests,
            blueprints: apps,
            files,
            sign_ins,
            components,
        })
    }

    /// Every server component file of this build: the core's and the
    /// adapters' (checked at start by `components::core_files`).
    pub fn component_files(&self) -> &[crate::components::ComponentSource] {
        &self.components
    }

    /// No adapters (tests of the core alone).
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("an empty catalog is valid")
    }

    pub fn adapters(&self) -> &[Arc<dyn Source>] {
        &self.adapters
    }

    /// Every blueprint, in adapter order.
    pub fn blueprints(&self) -> impl Iterator<Item = &BlueprintInfo> {
        self.blueprints.iter().map(|(a, _)| a)
    }

    pub fn blueprint(&self, id: &str) -> Option<&BlueprintInfo> {
        self.blueprints
            .iter()
            .find(|(a, _)| a.id == id)
            .map(|(a, _)| a)
    }

    /// Whether `id` is a blueprint's.
    pub fn is_blueprint(&self, id: &str) -> bool {
        self.blueprint(id).is_some()
    }

    /// Data source `id` (an adapter name) and its manifest.
    pub fn source(&self, id: &str) -> Option<(&Arc<dyn Source>, &SourceManifest)> {
        let i = self.adapters.iter().position(|a| a.name() == id)?;
        Some((&self.adapters[i], &self.manifests[i]))
    }

    /// Every data source: `(id, manifest)`, in adapter order.
    pub fn sources(&self) -> impl Iterator<Item = (&'static str, &SourceManifest)> {
        self.adapters
            .iter()
            .zip(&self.manifests)
            .map(|(a, m)| (a.name(), m))
    }

    /// The data sources blueprint `id` draws on: those of its feeds, then
    /// the adapter shipping it (whose routes its layouts may always use).
    /// Empty for an unknown blueprint.
    pub fn blueprint_sources(&self, id: &str) -> Vec<(&'static str, &SourceManifest)> {
        let Some((app, i)) = self.blueprints.iter().find(|(a, _)| a.id == id) else {
            return Vec::new();
        };
        let mut out: Vec<(&'static str, &SourceManifest)> = Vec::new();
        let own = self.adapters[*i].name();
        for name in app.feeds.iter().map(|(s, _)| *s).chain([own]) {
            if !out.iter().any(|(n, _)| *n == name)
                && let Some((_, m)) = self.source(name)
            {
                out.push((name, m));
            }
        }
        out
    }

    /// The adapter providing sign-in `id`.
    pub fn sign_in_adapter(&self, id: &str) -> Option<&Arc<dyn Source>> {
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

    /// Whether `path` is under an adapter's [`Source::public_paths`]
    /// (and has no `..` segment).
    pub fn is_public_path(&self, path: &str) -> bool {
        !path.split('/').any(|seg| seg == ".." || seg == ".")
            && self
                .adapters
                .iter()
                .flat_map(|a| a.public_paths())
                .chain(std::iter::once(&crate::uploads::PUBLIC_PATH))
                .chain(std::iter::once(&crate::mcp_servers::CALLBACK_PATH))
                .any(|p| path.starts_with(p))
    }

    /// Every adapter's blueprint layout files.
    pub fn files(&self) -> &[LayoutSource] {
        &self.files
    }
}

/// The startup checks of the data sources (see [`Catalog::new`]).
fn check_sources(
    adapters: &[Arc<dyn Source>],
    manifests: &[SourceManifest],
    apps: &[(BlueprintInfo, usize)],
    sign_ins: &[(SignInProvider, usize)],
) -> Result<(), String> {
    for (i, (a, m)) in adapters.iter().zip(manifests).enumerate() {
        let name = a.name();
        source::check_manifest(name, m)?;
        if let Some(id) = m.sign_in
            && !sign_ins.iter().any(|(s, _)| s.id == id)
        {
            return Err(format!(
                "source '{name}' needs sign-in '{id}', which no adapter provides"
            ));
        }
        for (b, n) in adapters[..i].iter().zip(&manifests[..i]) {
            if let Some(s) = m
                .entity_schemes
                .iter()
                .find(|s| n.entity_schemes.contains(s))
            {
                return Err(format!(
                    "sources '{}' and '{name}' both claim the entity scheme '{s}'",
                    b.name()
                ));
            }
            for k in m.query_keys() {
                if let Some(o) = n.query_keys().find(|o| source::keys_overlap(k, o)) {
                    return Err(format!(
                        "query key '{k}' of source '{name}' overlaps '{o}' of source '{}'",
                        b.name()
                    ));
                }
            }
        }
    }
    for (app, _) in apps {
        for (n, (src, feed)) in app.feeds.iter().enumerate() {
            if feed.starts_with('{') {
                return Err(format!(
                    "app '{}' lists the pattern feed '{src}/{feed}': name one of its feeds",
                    app.id
                ));
            }
            let Some(i) = adapters.iter().position(|a| a.name() == *src) else {
                return Err(format!(
                    "app '{}' lists feed '{src}/{feed}', but there is no source '{src}'",
                    app.id
                ));
            };
            if manifests[i].feed(feed).is_none() {
                return Err(format!(
                    "app '{}' lists feed '{src}/{feed}', which source '{src}' does not declare",
                    app.id
                ));
            }
            if app.feeds[..n].contains(&(*src, *feed)) {
                return Err(format!("app '{}' lists feed '{src}/{feed}' twice", app.id));
            }
        }
    }
    Ok(())
}
