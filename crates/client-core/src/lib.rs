//! Client core engine (DESIGN.md §2.3).
//!
//! `ConstructEngine` owns workspace state, fetches the hydration bundle over
//! HTTP and applies optimistic mutations locally, pushing a `StateSnapshot`
//! to subscribers on every change.
//!
//! Current scope (M5, see docs/DECISIONS.md D13): state is held in memory
//! (the source for snapshots) and written through to a SQLite store
//! ([`store`]) at `<storage_path>/cache.db` (in-memory DB when
//! `storage_path` is empty). On init the engine loads the workspace layout
//! and its entities from that store; every fetched bundle is persisted in
//! one transaction. An optimistic mutation updates the entity and appends
//! an `action_outbox` row in one transaction before observers are notified.
//! The old JSON cache files (`workspace_<id>.json`) are ignored.
//!
//! M6: a mutation's `also` field updates are evaluated against the
//! pre-mutation entity and applied, stored, rolled back and re-applied
//! together with the primary field (docs/DECISIONS.md D15). The bundle's
//! `queries` are persisted and exposed in the snapshot (`queries_json`).
//!
//! A background thread retries the initial fetch every few seconds until
//! the server answers; after that it probes `GET {base_url}/healthz` on the
//! same interval so `is_online` tracks the server going away and coming
//! back. Reconnecting does not re-fetch the workspace, so local optimistic
//! changes survive.
//!
//! A second background thread (the sync worker, docs/DECISIONS.md D14)
//! flushes PENDING outbox rows FIFO to `POST {base_url}{endpoint}` while
//! online: 2xx deletes the row, 4xx deletes it and rolls the field back
//! (re-applying later pending rows), 5xx / 408 / 429 / network errors keep
//! it with exponential backoff. A fetched bundle has every pending row re-applied
//! on top of it, so optimistic values are never clobbered.
//!
//! M6 auth (docs/DECISIONS.md D19): `ActionIntent::AuthLogin` runs the
//! OAuth2 PKCE flow on a background thread ([`auth`]); the resulting pod
//! session token is kept in a [`SecureStore`] and sent as
//! `Authorization: Bearer` on workspace fetches and outbox POSTs. A 401 from
//! the pod clears the session; an outbox row that got a 401 stays PENDING
//! and flushing pauses until the next sign-in.
//!
//! M7 (docs/DECISIONS.md D24, D32): the engine tracks the app's layout
//! version (`X-Construct-Layout-Version`), and a third background thread
//! ([`events`]) listens on the workspace's SSE push channel and refetches
//! when a newer version is announced.

pub mod attachments;
pub mod auth;
pub mod catalog;
pub mod events;
pub mod expr;
pub mod notify;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{
    ActionIntent, DataBundle, EntityRecord, FieldUpdate, Position, UiNode, WorkspaceSpec,
};
use serde_json::{Value, json};

use auth::{LoopbackListener, PendingAuth, Pkce, Redirect, guarded};
pub use auth::{MemoryStore, RedirectMode, SecureStore, UrlOpener};
pub use catalog::{LayoutApiError, SIGN_INS_UNSUPPORTED};
pub use notify::{NotificationListener, PushSubscription};
pub use protocol::{Notification, NotificationList, ReadEvent};
use store::{
    AlsoUpdate, Attachment, AttachmentRef, CreateInfo, DataLoad, NewOutboxRow, OutboxRow, Store,
    StoreOps,
};

const DEFAULT_WORKSPACE_ID: &str = "home";
/// SQLite file name inside `storage_path`.
pub const CACHE_DB_FILE: &str = "cache.db";
/// Payload token replaced by the new optimistic field value at dispatch.
pub const VALUE_TOKEN: &str = "$value";
/// Timeout for a single reachability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// While the initial fetch keeps failing with the same error, log every
/// this many attempts (once a minute at the default 2 s interval).
const FETCH_FAILURE_LOG_EVERY: u32 = 30;
/// Timeout for one data path load (`action.data.load`).
const DATA_LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Load error shown when the pod could not be reached.
pub const LOAD_OFFLINE: &str = "Can't reach your pod. Check your connection and try again.";
/// Load error after the pod rejected the session.
pub const LOAD_SIGNED_OUT: &str = "Sign in to see this.";
/// Load error for a 404.
pub const LOAD_NOT_FOUND: &str = "This isn't available anymore.";
/// Timeout for a single outbox POST.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for one attachment upload (`POST /v1/uploads`, D49).
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Pod path attachments are uploaded to.
pub const UPLOADS_PATH: &str = "/v1/uploads";
/// An accepted create's attachment files are kept this long (the
/// placeholder may still show them until its list is reloaded).
const RELEASED_KEEP_SECS: i64 = 10 * 60;
/// A staged attachment no pending create uses is deleted after this long.
const UNUSED_KEEP_SECS: i64 = 24 * 60 * 60;
/// Most attachments one create may carry.
pub const MAX_ATTACHMENTS: usize = 20;
/// `auth_json` until a sign-in flow sets it.
pub const SIGNED_OUT_AUTH_JSON: &str = r#"{"state":"signed_out"}"#;
/// Timeout for the sign-in calls to the pod (`/v1/auth/...`).
const AUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// `auth_json` error for a sign-in button without the pod's sign-in paths.
pub const SIGN_IN_BUTTON_OUTDATED: &str =
    "This sign-in button is out of date. Restart the app and try again.";
/// `auth_json` error when a custom-scheme redirect matches no sign-in.
pub const SIGN_IN_INTERRUPTED: &str = "Sign-in was interrupted; please try again";
/// Response header carrying the served layout version (the app's latest
/// version number).
pub const LAYOUT_VERSION_HEADER: &str = "X-Construct-Layout-Version";

/// Engine timing knobs. [`EngineConfig::default`] is what `init` uses;
/// tests shorten the intervals so they do not sleep for seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    /// Delay between background fetch retries / reachability probes.
    pub poll_interval: Duration,
    /// Outbox retry delay after the n-th failed attempt is
    /// `min(backoff_base * 2^n, backoff_cap)`.
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(60),
        }
    }
}

impl EngineConfig {
    /// Delay before the next attempt of a row that has now failed
    /// `retry_count` times: `min(backoff_base * 2^retry_count, backoff_cap)`.
    pub fn backoff(&self, retry_count: i64) -> Duration {
        let exp = retry_count.clamp(0, 30) as u32;
        self.backoff_base
            .saturating_mul(1u32 << exp)
            .min(self.backoff_cap)
    }
}

/// Construction options for [`ConstructEngine::init_with_options`].
pub struct EngineOptions {
    pub config: EngineConfig,
    /// Where the pod session is kept. `None`: the platform default
    /// ([`auth::default_secure_store`]: Keychain on macOS, in-memory
    /// elsewhere).
    pub secure_store: Option<Arc<dyn SecureStore>>,
    /// Opens the authorize URL. `None`: sign-in reports "no browser
    /// available" and logs the URL (the flow keeps waiting for a redirect).
    pub url_opener: Option<Arc<dyn UrlOpener>>,
    pub redirect: RedirectMode,
    /// Loopback mode: the port tried first for the redirect listener
    /// ([`DEFAULT_LOOPBACK_PORT`]); a random port if it is busy. 0 = always
    /// random.
    pub loopback_port: u16,
}

/// Default loopback redirect port (`http://127.0.0.1:47823/callback`).
pub const DEFAULT_LOOPBACK_PORT: u16 = 47823;

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            config: EngineConfig::default(),
            secure_store: None,
            url_opener: None,
            redirect: RedirectMode::Loopback,
            loopback_port: DEFAULT_LOOPBACK_PORT,
        }
    }
}

/// The signed-in pod session, as stored (JSON) under
/// [`auth::session_key`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Session {
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
}

impl Session {
    /// Parse a stored value; a bare (non-JSON) string is taken as the token.
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        match serde_json::from_str::<Session>(raw) {
            Ok(s) if !s.token.is_empty() => Some(s),
            Ok(_) => None,
            Err(_) if !raw.starts_with('{') => Some(Session {
                token: raw.to_owned(),
                username: None,
                display_name: None,
                avatar_url: None,
            }),
            Err(_) => None,
        }
    }

    /// `signed_in` with the account, or `signed_out` for a session with no
    /// account (a pod token before or after an app sign-in, D46); either way
    /// `"session": true`, so the UI can offer to leave the pod.
    fn auth_json(&self) -> String {
        if self.username.is_none() && self.display_name.is_none() && self.avatar_url.is_none() {
            return json!({ "state": "signed_out", "session": true }).to_string();
        }
        let mut v = json!({ "state": "signed_in", "session": true });
        for (k, f) in [
            ("username", &self.username),
            ("display_name", &self.display_name),
            ("avatar_url", &self.avatar_url),
        ] {
            if let Some(x) = f {
                v[k] = json!(x);
            }
        }
        v.to_string()
    }
}

fn pending_auth_json() -> String {
    json!({ "state": "pending" }).to_string()
}

/// `auth_json` message while the first feed after a sign-in is fetched.
pub const LOADING_FEED_MESSAGE: &str = "Loading your feed…";

/// Signed in, but the post-sign-in refetch has not landed yet: still
/// `pending`, so the UI does not show the account over the signed-out
/// screen.
fn loading_feed_auth_json() -> String {
    json!({ "state": "pending", "message": LOADING_FEED_MESSAGE }).to_string()
}

fn error_auth_json(message: &str) -> String {
    json!({ "state": "error", "error": message }).to_string()
}

/// A sign-in in progress. A newer `AuthLogin` (or `logout`) cancels it.
struct Flow {
    cancelled: AtomicBool,
    /// The OAuth `state` this flow expects back.
    state: String,
    /// Custom-scheme mode: where `handle_auth_redirect` delivers.
    redirect_tx: Option<Mutex<mpsc::Sender<Redirect>>>,
    /// The redirect arrived: the code is being exchanged, or the session
    /// is in place and the post-sign-in refetch is running. A new
    /// `AuthLogin` is ignored from here on (it would start a second flow).
    exchanging: AtomicBool,
}

impl Flow {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Why a sign-in ended without a session.
enum LoginError {
    /// Superseded, logged out or engine dropped: report nothing.
    Cancelled,
    /// `message` is shown in `auth_json` (short, human); `detail` (URLs,
    /// HTTP status, the pod's error body) is only logged.
    Failed { message: String, detail: String },
    /// A redirect reached an engine with no matching sign-in (in memory or
    /// persisted): [`SIGN_IN_INTERRUPTED`] unless signed in.
    Unmatched,
}

/// Outcome of a fetch attempt.
enum FetchOutcome {
    /// Bundle fetched and applied.
    Fetched,
    /// The server answered, but with a non-success HTTP status.
    HttpError,
    /// The server could not be reached (or the response was unusable).
    Failed,
}

/// State pushed to observers.
///
/// - `workspace_json`: the bundle's `workspace` layout node (`UiNode`) as
///   JSON, or `"null"` if no workspace has ever been loaded.
/// - `entities_json`: the URI-keyed entity map as a JSON object (`"{}"` if
///   empty).
/// - `is_online`: whether the server answered the most recent fetch or
///   reachability probe.
/// - `seq`: the engine's state sequence number; strictly increases with
///   every change, so consumers can drop snapshots that arrive out of order.
/// - `pending_actions`: number of PENDING `action_outbox` rows.
/// - `queries_json`: the bundle's resolved queries as a JSON object
///   (`{query_key: [entity uri, ...]}`, `"{}"` when none). Every URI in it
///   is also in `entities_json` when the bundle (or cache) carried it.
/// - `auth_json`: `{"state":"signed_out"|"pending"|"signed_in"|"error",
///   "username"?, "display_name"?, "avatar_url"?, "error"?, "message"?}` (the
///   account fields when signed in, `error` with a short human message in the
///   error state, `message` = [`LOADING_FEED_MESSAGE`] while `pending` after
///   the code exchange until the first signed-in fetch is done).
/// - `layout_json`: the layout on display, `{"app_id": <workspace id>,
///   "version": n}` (the app version from `X-Construct-Layout-Version` of the
///   applied fetch); `"{}"` while unknown or when the screen is not versioned
///   (e.g. the signed-out `pixelfed_home` screen); `{"app_id", "gone": true}`
///   when the app no longer exists on the pod (a deleted custom app: SSE
///   `deleted`, or a 404): the UI leaves it.
/// - `loads_json`: data path loads (`action.data.load`, docs/DECISIONS.md
///   D34): `{path: {"state": "loading"|"ready"|"error", "error"?}}` (`error`
///   is a short sentence for the UI). The queries of loaded paths are part
///   of `queries_json` (cached, so also shown offline), as are the temporary
///   entities of pending `action.mutation.create` rows.
/// - `notifications_json`: unread notification counts (D52),
///   `{"unread_total": n, "unread_by_app": {app_id: n}}` (apps with none
///   left out; zeros while signed out). Lists come from
///   [`ConstructEngine::list_notifications`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    pub workspace_json: String,
    pub entities_json: String,
    pub is_online: bool,
    pub seq: u64,
    pub pending_actions: u32,
    pub queries_json: String,
    pub auth_json: String,
    pub layout_json: String,
    pub loads_json: String,
    pub notifications_json: String,
}

/// A pending `action.mutation.create` row: its temporary entity is listed in
/// `queries[query_key]`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingCreate {
    row_id: i64,
    query_key: String,
    entity_uri: String,
    at_start: bool,
}

impl PendingCreate {
    fn from_rows(rows: &[OutboxRow]) -> Vec<Self> {
        rows.iter()
            .filter_map(|r| {
                r.create.as_ref().map(|c| PendingCreate {
                    row_id: r.id,
                    query_key: c.query_key.clone(),
                    entity_uri: r.entity_uri.clone(),
                    at_start: c.at_start,
                })
            })
            .collect()
    }
}

/// One data path's load state (see [`StateSnapshot::loads_json`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct LoadStatus {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl LoadStatus {
    fn loading() -> Self {
        Self {
            state: "loading",
            error: None,
        }
    }
    fn ready() -> Self {
        Self {
            state: "ready",
            error: None,
        }
    }
    fn error(message: &str) -> Self {
        Self {
            state: "error",
            error: Some(message.to_owned()),
        }
    }
}

/// Work the sync worker does after an accepted create row (outside the
/// `net` lock): reload the data paths that list its query, or the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Followup {
    Load(String),
    Refresh,
}

struct Observer {
    f: Box<dyn Fn(StateSnapshot) + Send + Sync>,
    /// Highest state `seq` delivered to this observer.
    last_seq: AtomicU64,
}

impl Observer {
    /// Call the observer unless it has already seen this or a newer state.
    fn deliver(&self, snap: StateSnapshot) {
        if self.last_seq.fetch_max(snap.seq, Ordering::SeqCst) < snap.seq {
            self.call(snap);
        }
    }

    /// Invoke the callback, containing any panic so it never unwinds into
    /// `dispatch`/`refresh` callers (which may sit behind an FFI boundary).
    fn call(&self, snap: StateSnapshot) {
        if panic::catch_unwind(AssertUnwindSafe(|| (self.f)(snap))).is_err() {
            eprintln!("[ConstructEngine] observer panicked; ignored");
        }
    }
}

#[derive(Default)]
struct State {
    /// The engine's workspace (= app) id, for `layout_json`.
    workspace_id: String,
    workspace: Option<UiNode>,
    entities: BTreeMap<String, EntityRecord>,
    /// The bundle's resolved queries (`query_key` -> ordered entity URIs).
    queries: BTreeMap<String, Vec<String>>,
    /// Auth status JSON (see [`StateSnapshot::auth_json`]); empty means
    /// signed out.
    auth_json: String,
    /// The pod session (bearer token), if signed in.
    session: Option<Session>,
    /// An outbox POST got a 401: flushing pauses until the next sign-in
    /// (the row stays PENDING instead of being rolled back).
    auth_blocked: bool,
    /// Version of the applied layout (`X-Construct-Layout-Version` of the
    /// last applied fetch); `None` when unknown or the served layout is not
    /// versioned.
    layout_version: Option<u64>,
    is_online: bool,
    /// Bumped on every change (cache load, fetch attempt, mutation). Used to
    /// drop stale out-of-order deliveries.
    seq: u64,
    /// PENDING outbox rows; kept in step with the store on every write.
    pending_actions: u32,
    /// Consecutive transport-level fetch failures and the last error, to
    /// keep the retry loop from logging every attempt.
    fetch_failures: u32,
    last_fetch_error: Option<String>,
    /// Write-through persistence. `None` only if even an in-memory DB could
    /// not be opened; mutations are then rejected.
    store: Option<Store>,
    /// The last answer of each loaded data path (cached in `data_loads`).
    data: BTreeMap<String, DataLoad>,
    /// Load state per data path (this run only).
    loads: BTreeMap<String, LoadStatus>,
    /// Pending create rows (their temporary entities are listed).
    creates: Vec<PendingCreate>,
    /// Accepted creates still listed until a reload of their query
    /// (`(query_key, entity_uri, at_start)`), so the item does not blink out.
    settling: Vec<(String, String, bool)>,
    /// Queued by the flush of an accepted create.
    followups: Vec<Followup>,
    /// Unread notifications (D52; see [`StateSnapshot::notifications_json`]).
    unread_total: u32,
    unread_by_app: BTreeMap<String, u32>,
    /// Staged attachments (D49), by id; each is drawn as the local entity
    /// `construct://attachments/<id>`.
    attachments: BTreeMap<String, Attachment>,
    /// The app is gone from the pod: a custom app was deleted (SSE
    /// `deleted`, or the workspace answered 404). Cleared by a successful
    /// fetch. Reported as `"gone": true` in `layout_json`.
    gone: bool,
}

impl State {
    fn snapshot(&self) -> StateSnapshot {
        let workspace_json = match &self.workspace {
            Some(ws) => serde_json::to_string(ws).unwrap_or_else(|_| "null".into()),
            None => "null".into(),
        };
        let entities_json = if self.attachments.is_empty() {
            serde_json::to_string(&self.entities).unwrap_or_else(|_| "{}".into())
        } else {
            let mut all = serde_json::to_value(&self.entities)
                .ok()
                .and_then(|v| match v {
                    Value::Object(m) => Some(m),
                    _ => None,
                })
                .unwrap_or_default();
            for a in self.attachments.values() {
                all.insert(attachments::entity_uri(&a.id), attachment_entity(a));
            }
            Value::Object(all).to_string()
        };
        let queries_json =
            serde_json::to_string(&self.effective_queries()).unwrap_or_else(|_| "{}".into());
        let loads_json = serde_json::to_string(&self.loads).unwrap_or_else(|_| "{}".into());
        let auth_json = if self.auth_json.is_empty() {
            SIGNED_OUT_AUTH_JSON.to_string()
        } else {
            self.auth_json.clone()
        };
        StateSnapshot {
            workspace_json,
            entities_json,
            is_online: self.is_online,
            seq: self.seq,
            pending_actions: self.pending_actions,
            queries_json,
            auth_json,
            layout_json: self.layout_json(),
            loads_json,
            notifications_json: self.notifications_json(),
        }
    }

    /// The workspace's queries, overlaid with every loaded data path's
    /// queries, plus the temporary entities of settling and pending creates
    /// appended to their query.
    fn effective_queries(&self) -> BTreeMap<String, Vec<String>> {
        let mut out = self.queries.clone();
        for load in self.data.values() {
            for (k, v) in &load.queries {
                out.insert(k.clone(), v.clone());
            }
        }
        let temps = self.settling.iter().map(|(k, u, s)| (k, u, *s)).chain(
            self.creates
                .iter()
                .map(|c| (&c.query_key, &c.entity_uri, c.at_start)),
        );
        for (key, uri, at_start) in temps {
            let list = out.entry(key.clone()).or_default();
            if !list.contains(uri) {
                if at_start {
                    list.insert(0, uri.clone());
                } else {
                    list.push(uri.clone());
                }
            }
        }
        out
    }

    /// URIs the data loads and settling creates reference (kept by a
    /// workspace merge).
    fn data_uris(&self) -> Vec<String> {
        self.data
            .values()
            .flat_map(|d| d.entity_uris.iter().chain(d.queries.values().flatten()))
            .chain(self.settling.iter().map(|(_, u, _)| u))
            .cloned()
            .collect()
    }

    /// See [`StateSnapshot::layout_json`].
    fn layout_json(&self) -> String {
        if self.gone {
            return json!({ "app_id": self.workspace_id, "gone": true }).to_string();
        }
        match self.layout_version {
            Some(n) => json!({ "app_id": self.workspace_id, "version": n }).to_string(),
            None => "{}".to_owned(),
        }
    }

    /// Load the cached workspace, its queries, its entities (including every
    /// URI its queries reference) and every entity with a pending outbox row
    /// from the store.
    fn load_cached(&mut self, workspace_id: &str) {
        let Some(store) = &self.store else { return };
        let loaded = (|| {
            let ws = store.load_workspace(workspace_id)?;
            let pending = store.pending_outbox()?;
            let data = store.load_data_loads()?;
            let mut uris: Vec<String> = ws
                .as_ref()
                .map(|w| w.entity_uris.clone())
                .unwrap_or_default();
            if let Some(w) = &ws {
                uris.extend(w.queries.values().flatten().cloned());
            }
            for d in data.values() {
                uris.extend(d.entity_uris.iter().cloned());
                uris.extend(d.queries.values().flatten().cloned());
            }
            uris.extend(pending.iter().map(|r| r.entity_uri.clone()));
            uris.sort();
            uris.dedup();
            let entities = store.load_entities(&uris)?;
            let creates = PendingCreate::from_rows(&pending);
            let attachments = store.load_attachments()?;
            Ok::<_, store::StoreError>((ws, entities, pending.len(), data, creates, attachments))
        })();
        match loaded {
            Ok((ws, entities, pending, data, creates, attachments)) => {
                self.data = data;
                self.creates = creates;
                self.attachments = attachments.into_iter().map(|a| (a.id.clone(), a)).collect();
                if ws.is_some() || !entities.is_empty() || pending > 0 {
                    println!(
                        "[ConstructEngine] loaded cached workspace '{workspace_id}' (layout: {}, {} \
                         entities, {pending} pending actions) from {CACHE_DB_FILE}",
                        if ws.is_some() { "yes" } else { "no" },
                        entities.len(),
                    );
                    self.seq = 1;
                }
                let (layout, queries) = match ws {
                    Some(w) => (Some(w.layout), w.queries),
                    None => (None, BTreeMap::new()),
                };
                self.workspace = layout;
                self.queries = queries;
                self.entities = entities;
                self.pending_actions = u32::try_from(pending).unwrap_or(u32::MAX);
            }
            Err(e) => eprintln!("[ConstructEngine] failed to load cache: {e}"),
        }
    }

    /// Merge a fetched bundle: replace the bundle's entities, then re-apply
    /// every PENDING outbox row for those entities FIFO (re-evaluating its
    /// primary and `also` expressions and updating their prev values), so
    /// optimistic values survive a refetch. Entities outside the bundle that
    /// have pending rows, or that the bundle's queries reference, are kept as
    /// they are. Layout, queries, entities and the rows' new prev values are
    /// persisted in one transaction; a
    /// persistence failure is logged and the merged state is still applied
    /// (the store is only a fallback for it).
    fn apply_fetched(&mut self, workspace_id: &str, spec: WorkspaceSpec) {
        let WorkspaceSpec {
            workspace,
            entities: fetched,
            queries,
            ..
        } = spec;
        let pending = match self.store.as_ref().map(|s| s.pending_outbox()) {
            Some(Ok(rows)) => rows,
            Some(Err(e)) => {
                eprintln!("[ConstructEngine] failed to read the outbox for the refetch merge: {e}");
                Vec::new()
            }
            None => Vec::new(),
        };
        let uris: Vec<String> = fetched.keys().cloned().collect();
        let mut merged = fetched;
        let mut prev_updates = Vec::new();
        for row in &pending {
            if let Some(entity) = merged.get_mut(&row.entity_uri) {
                if uris.binary_search(&row.entity_uri).is_ok()
                    && let Some(prevs) = reapply_row(entity, row)
                {
                    prev_updates.push((row.id, prevs));
                }
            } else if let Some(local) = self.entities.get(&row.entity_uri) {
                merged.insert(row.entity_uri.clone(), local.clone());
            }
        }
        // Settling creates of a workspace query are replaced by this answer.
        self.settling.retain(|(k, _, _)| !queries.contains_key(k));
        for uri in queries.values().flatten().chain(self.data_uris().iter()) {
            if !merged.contains_key(uri)
                && let Some(local) = self.entities.get(uri)
            {
                merged.insert(uri.clone(), local.clone());
            }
        }
        if !prev_updates.is_empty() {
            println!(
                "[ConstructEngine] re-applied {} pending action(s) onto the fetched workspace \
                 '{workspace_id}'",
                prev_updates.len()
            );
        }
        if let Some(store) = self.store.as_mut() {
            let now = unix_now();
            let result = store.transaction(|tx| {
                tx.save_workspace(workspace_id, &workspace, &uris, &queries, now)?;
                for uri in &uris {
                    tx.upsert_entity(uri, &merged[uri], now)?;
                }
                for (id, (prev, also)) in &prev_updates {
                    tx.set_outbox_prevs(*id, prev, also)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                eprintln!("[ConstructEngine] failed to persist workspace '{workspace_id}': {e}");
            }
        }
        self.workspace = Some(workspace);
        self.entities = merged;
        self.queries = queries;
    }

    /// Merge the answer of data path `path` (docs/DECISIONS.md D34): its
    /// entities replace the local ones (pending outbox rows re-applied on
    /// top, like a workspace fetch), its queries become this path's queries,
    /// and settling creates of those queries are dropped (the answer now
    /// lists the real items). Persisted in one transaction (a failure is
    /// logged; memory is still updated).
    fn apply_data(&mut self, path: &str, bundle: DataBundle) {
        let DataBundle {
            entities: mut fetched,
            queries,
        } = bundle;
        let pending = match self.store.as_ref().map(|s| s.pending_outbox()) {
            Some(Ok(rows)) => rows,
            Some(Err(e)) => {
                eprintln!("[ConstructEngine] failed to read the outbox for the data merge: {e}");
                Vec::new()
            }
            None => Vec::new(),
        };
        let mut prev_updates = Vec::new();
        for row in &pending {
            if let Some(entity) = fetched.get_mut(&row.entity_uri)
                && let Some(prevs) = reapply_row(entity, row)
            {
                prev_updates.push((row.id, prevs));
            }
        }
        let (settled, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.settling)
            .into_iter()
            .partition(|(k, _, _)| queries.contains_key(k));
        self.settling = keep;
        let settled: Vec<String> = settled
            .into_iter()
            .map(|(_, u, _)| u)
            .filter(|u| !fetched.contains_key(u))
            .collect();
        let load = DataLoad {
            entity_uris: fetched.keys().cloned().collect(),
            queries,
        };
        if let Some(store) = self.store.as_mut() {
            let now = unix_now();
            let result = store.transaction(|tx| {
                tx.save_data_load(path, &load, now)?;
                for (uri, e) in &fetched {
                    tx.upsert_entity(uri, e, now)?;
                }
                for (id, (prev, also)) in &prev_updates {
                    tx.set_outbox_prevs(*id, prev, also)?;
                }
                for uri in &settled {
                    tx.delete_entity(uri)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                eprintln!("[ConstructEngine] failed to persist data '{path}': {e}");
            }
        }
        for uri in &settled {
            self.entities.remove(uri);
        }
        println!(
            "[ConstructEngine] loaded {path} ({} entities, {} queries{})",
            fetched.len(),
            load.queries.len(),
            if prev_updates.is_empty() {
                String::new()
            } else {
                format!(", {} pending action(s) re-applied", prev_updates.len())
            }
        );
        self.entities.extend(fetched);
        self.data.insert(path.to_owned(), load);
        self.loads.insert(path.to_owned(), LoadStatus::ready());
    }

    /// Sign-out: forget everything cached that may hold the account's data,
    /// in memory and in the store (one transaction): every entity that no
    /// remaining PENDING row targets, every cached data path answer, and the
    /// cached `active_workspace` row of `workspace_id`. The engine knows no
    /// app, so it cannot tell private from public data; the refetch that
    /// follows a sign-out reloads what is public (docs/DECISIONS.md D36).
    /// `drop_session_rows` (explicit logout) first deletes the PENDING rows
    /// queued while signed in: nobody could send them any more. The in-memory
    /// layout stays until the refetch replaces it.
    fn purge_private(&mut self, workspace_id: &str, drop_session_rows: bool) {
        let mut keep: std::collections::HashSet<String> = Default::default();
        if let Some(store) = self.store.as_mut() {
            let result = store.transaction(|tx| {
                let dropped = if drop_session_rows {
                    tx.delete_pending_session_rows()?
                } else {
                    0
                };
                let entities = tx.delete_unreferenced_entities()?;
                tx.delete_all_data_loads()?;
                tx.delete_workspace(workspace_id)?;
                Ok((dropped, entities))
            });
            match result {
                Ok((dropped, entities)) => println!(
                    "[ConstructEngine] signed out: removed {entities} cached entities, the cached \
                     workspace '{workspace_id}' and data{}",
                    if dropped > 0 {
                        format!(" and {dropped} pending action(s) that need the session")
                    } else {
                        String::new()
                    }
                ),
                Err(e) => eprintln!("[ConstructEngine] ERROR: failed to purge the cache: {e}"),
            }
            // `secure_delete` zeroed the freed pages; flush them out of the
            // WAL too, so the purged rows are not left in old WAL frames.
            match store.checkpoint_truncate() {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "[ConstructEngine] WAL checkpoint after the purge was blocked by a reader; the \
                     WAL is truncated at a later checkpoint"
                ),
                Err(e) => eprintln!("[ConstructEngine] WAL checkpoint after the purge failed: {e}"),
            }
            match store.pending_outbox() {
                Ok(rows) => {
                    keep.extend(rows.iter().map(|r| r.entity_uri.clone()));
                    self.creates = PendingCreate::from_rows(&rows);
                }
                Err(e) => {
                    eprintln!("[ConstructEngine] failed to read the outbox after the purge: {e}")
                }
            }
        }
        self.clear_notifications();
        self.entities.retain(|u, _| keep.contains(u));
        // Staged photos are the account's too; those a remaining (signed-out)
        // create still needs stay.
        let needed: std::collections::HashSet<String> = self
            .store
            .as_ref()
            .and_then(|s| s.pending_outbox().ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.create.as_ref())
            .flat_map(|c| c.attachments.iter().map(|a| a.id.clone()))
            .collect();
        let gone: Vec<String> = self
            .attachments
            .keys()
            .filter(|id| !needed.contains(*id))
            .cloned()
            .collect();
        self.delete_attachments(&gone);
        self.queries.clear();
        self.data.clear();
        self.loads.clear();
        self.settling.clear();
        self.sync_pending_count();
    }

    /// The pod rejected a create row: delete it and its temporary entity.
    fn reject_create(&mut self, row: &OutboxRow) -> store::StoreResult<()> {
        let Some(store) = self.store.as_mut() else {
            return Err(store::StoreError::InvalidQuery);
        };
        store.transaction(|tx| {
            tx.delete_outbox(row.id)?;
            tx.delete_entity(&row.entity_uri)?;
            Ok(())
        })?;
        self.entities.remove(&row.entity_uri);
        self.creates.retain(|c| c.row_id != row.id);
        if let Some(c) = &row.create {
            let ids: Vec<String> = c.attachments.iter().map(|a| a.id.clone()).collect();
            self.delete_attachments(&ids);
        }
        self.sync_pending_count();
        Ok(())
    }

    /// Delete staged attachments: their files, rows and local entities.
    fn delete_attachments(&mut self, ids: &[String]) {
        for id in ids {
            if let Some(a) = self.attachments.remove(id) {
                let _ = fs::remove_file(&a.path);
                if let Some(Err(e)) = self.store.as_ref().map(|s| s.delete_attachment(id)) {
                    eprintln!("[ConstructEngine] attachment {id}: failed to delete its row: {e}");
                }
            }
        }
    }

    /// Delete attachments nobody needs any more: released ones after
    /// [`RELEASED_KEEP_SECS`], unused staged ones after [`UNUSED_KEEP_SECS`].
    /// Attachments a PENDING create carries are always kept.
    fn sweep_attachments(&mut self, now: i64) {
        if self.attachments.is_empty() {
            return;
        }
        let pending = match self.store.as_ref().map(|s| s.pending_outbox()) {
            Some(Ok(rows)) => rows,
            _ => return,
        };
        let used: std::collections::HashSet<&str> = pending
            .iter()
            .filter_map(|r| r.create.as_ref())
            .flat_map(|c| c.attachments.iter().map(|a| a.id.as_str()))
            .collect();
        let gone: Vec<String> = self
            .attachments
            .values()
            .filter(|a| !used.contains(a.id.as_str()))
            .filter(|a| match a.released_at {
                Some(t) => now - t >= RELEASED_KEEP_SECS,
                None => now - a.created_at >= UNUSED_KEEP_SECS,
            })
            .map(|a| a.id.clone())
            .collect();
        if !gone.is_empty() {
            println!("[ConstructEngine] deleted {} old attachment(s)", gone.len());
            self.delete_attachments(&gone);
        }
    }

    /// Re-read the PENDING count from the store (after a delete).
    fn sync_pending_count(&mut self) {
        if let Some(store) = &self.store {
            match store.pending_count() {
                Ok(n) => self.pending_actions = n,
                Err(e) => eprintln!("[ConstructEngine] failed to count pending actions: {e}"),
            }
        }
    }

    /// The server rejected `row` (4xx other than 408/429): delete it and
    /// restore every field it set: the primary field to `prev_value` (or
    /// `rollback_value` if prev is null/absent), each `also` field to its
    /// `prev_value`. Later PENDING rows for the same entity are first
    /// rewound (newest first, to their stored prev values) and afterwards
    /// re-applied FIFO on top of the restored entity (all of their fields
    /// re-evaluated, their prev values updated). One transaction; memory is
    /// only changed if it commits. Returns the restored primary value, the
    /// primary field's final value and the number of re-applied rows.
    fn reject_row(&mut self, row: &OutboxRow) -> store::StoreResult<(Value, Value, usize)> {
        let Some(store) = self.store.as_mut() else {
            return Err(store::StoreError::InvalidQuery);
        };
        let Some(field) = row.field.clone() else {
            // No field recorded: nothing to roll back.
            store.delete_outbox(row.id)?;
            self.sync_pending_count();
            return Ok((Value::Null, Value::Null, 0));
        };
        let later: Vec<OutboxRow> = store
            .pending_outbox()?
            .into_iter()
            .filter(|r| r.id > row.id && r.entity_uri == row.entity_uri)
            .collect();
        let mut entity = self
            .entities
            .get(&row.entity_uri)
            .cloned()
            .unwrap_or_default();
        for r in later.iter().rev() {
            rewind_row(&mut entity, r, None);
        }
        let restored = rewind_row(&mut entity, row, row.rollback_value_json.as_deref());
        let mut prev_updates = Vec::new();
        for r in &later {
            if let Some(prevs) = reapply_row(&mut entity, r) {
                prev_updates.push((r.id, prevs));
            }
        }
        let now = unix_now();
        store.transaction(|tx| {
            tx.delete_outbox(row.id)?;
            tx.upsert_entity(&row.entity_uri, &entity, now)?;
            for (id, (prev, also)) in &prev_updates {
                tx.set_outbox_prevs(*id, prev, also)?;
            }
            Ok(())
        })?;
        let final_value = entity.get(&field).cloned().unwrap_or(Value::Null);
        self.entities.insert(row.entity_uri.clone(), entity);
        self.sync_pending_count();
        Ok((restored, final_value, prev_updates.len()))
    }
}

/// Parse a stored JSON value; `None` if absent, corrupt or null.
fn parse_stored(s: Option<&str>) -> Option<Value> {
    s.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
}

/// Undo `row` on `entity`: set each `also` field to its stored
/// `prev_value` and the primary field to `prev_value_json`, or to
/// `rollback_json` when that is given and prev is null/absent. Returns the
/// primary field's restored value (`Null` if the row has no field).
fn rewind_row(entity: &mut EntityRecord, row: &OutboxRow, rollback_json: Option<&str>) -> Value {
    let Some(field) = &row.field else {
        return Value::Null;
    };
    for a in row.also.iter().rev() {
        entity.set(a.field.clone(), a.prev_value.clone());
    }
    let restored = parse_stored(row.prev_value_json.as_deref())
        .or_else(|| parse_stored(rollback_json))
        .unwrap_or(Value::Null);
    entity.set(field.clone(), restored.clone());
    restored
}

/// Evaluate every `(field, expr)` against `entity` as it is now (the
/// pre-mutation state). Returns each field's current value and the result.
fn eval_updates(
    entity: &EntityRecord,
    updates: &[(&str, &str)],
) -> Vec<(Value, Result<Value, expr::ExprError>)> {
    updates
        .iter()
        .map(|(field, e)| {
            let prev = entity.get(field).cloned().unwrap_or(Value::Null);
            (prev, expr::eval(e, entity))
        })
        .collect()
}

/// Re-apply a pending row to `entity`: re-evaluate its primary and `also`
/// expressions against the entity as it is before the row, then set them
/// all. Returns the row's new prev values (`prev_value_json` and the `also`
/// updates with their `prev_value`), or `None` if the row has no
/// field/expression. A field whose expression fails is left unchanged
/// (logged).
fn reapply_row(entity: &mut EntityRecord, row: &OutboxRow) -> Option<(String, Vec<AlsoUpdate>)> {
    let (Some(field), Some(value_expr)) = (&row.field, &row.value_expr) else {
        return None;
    };
    let updates: Vec<(&str, &str)> = std::iter::once((field.as_str(), value_expr.as_str()))
        .chain(
            row.also
                .iter()
                .map(|a| (a.field.as_str(), a.value_expr.as_str())),
        )
        .collect();
    let results = eval_updates(entity, &updates);
    let mut prevs = Vec::with_capacity(results.len());
    for ((f, _), (prev, result)) in updates.iter().zip(results) {
        match result {
            Ok(v) => {
                entity.set(f.to_string(), v);
            }
            Err(e) => eprintln!(
                "[ConstructEngine] re-apply outbox #{} {}#{f}: {e}; field unchanged",
                row.id, row.entity_uri
            ),
        }
        prevs.push(prev);
    }
    let mut prevs = prevs.into_iter();
    let primary_prev = prevs.next().unwrap_or(Value::Null).to_string();
    let also = row
        .also
        .iter()
        .zip(prevs)
        .map(|(a, prev_value)| AlsoUpdate {
            prev_value,
            ..a.clone()
        })
        .collect();
    Some((primary_prev, also))
}

/// Wakes the sync worker. Lives outside `Inner` so the worker can wait on
/// it while holding only a `Weak<Inner>`.
#[derive(Default)]
struct WorkerSignal {
    /// (wake requested, shut down)
    flags: Mutex<(bool, bool)>,
    cv: Condvar,
}

impl WorkerSignal {
    fn wake(&self) {
        lock(&self.flags).0 = true;
        self.cv.notify_all();
    }

    fn shutdown(&self) {
        lock(&self.flags).1 = true;
        self.cv.notify_all();
    }

    fn is_shut_down(&self) -> bool {
        lock(&self.flags).1
    }

    /// Block until woken, shut down or `timeout` (None = no timeout)
    /// elapses. Returns false once shut down.
    fn wait(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut flags = lock(&self.flags);
        loop {
            if flags.1 {
                return false;
            }
            if flags.0 {
                flags.0 = false;
                return true;
            }
            flags = match deadline {
                None => self.cv.wait(flags).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return true;
                    }
                    self.cv
                        .wait_timeout(flags, d - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
    }
}

/// Result of one outbox POST.
enum FlushResult {
    Accepted(u16),
    /// 401: the pod wants a (new) session; retried after sign-in.
    Unauthorized,
    /// 4xx except 408/429.
    Rejected(u16, String),
    /// 5xx, 408, 429 or any other non-2xx/4xx status.
    ServerError(u16, String),
    Network(String),
}

struct Inner {
    state: Mutex<State>,
    observers: Mutex<Vec<Arc<Observer>>>,
    /// Serializes a workspace fetch+merge with an outbox POST+apply, so a
    /// bundle never races a flush (the row would be counted twice or
    /// dropped). Never held together with HTTP on the state mutex, and
    /// released before observers are notified. Shared by all engines in
    /// this process on the same cache file ([`net_lock_for`]).
    net: Arc<Mutex<()>>,
    signal: Arc<WorkerSignal>,
    config: EngineConfig,
    workspace_id: String,
    base_url: String,
    workspace_url: String,
    health_url: String,
    secure_store: Arc<dyn SecureStore>,
    url_opener: Option<Arc<dyn UrlOpener>>,
    redirect: RedirectMode,
    loopback_port: u16,
    /// `pod_session:<base_url>`.
    session_key: String,
    /// `pending_auth:<base_url>` (custom-scheme mode only).
    pending_key: String,
    /// The sign-in in progress, if any. Never held during secure-store I/O.
    flow: Mutex<Option<Arc<Flow>>>,
    /// Serializes writes of `session_key` (see [`Inner::sync_session_store`]).
    session_io: Mutex<()>,
    /// Serializes reads/writes of `pending_key` (lock order: `pending_io`,
    /// then `flow`).
    pending_io: Mutex<()>,
    /// This installation's id (D52), sent as `x-construct-device`.
    device_id: String,
    /// Device registration state (lock order: `device_io`, then `device`,
    /// then `state`; never held during HTTP except `device_io`).
    device: Mutex<notify::DeviceState>,
    /// Serializes device registrations.
    device_io: Mutex<()>,
    notification_listener: Mutex<Option<NotificationListener>>,
    /// Mark-reads the pod has not confirmed (sent on reconnect).
    pending_reads: Mutex<Vec<notify::ReadReq>>,
    /// Where staged attachments are written (`<storage>/attachments`, or a
    /// temporary directory for an in-memory store).
    attachments_dir: PathBuf,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.signal.shutdown();
    }
}

/// The client-side state engine. Cheap to share behind an `Arc`.
pub struct ConstructEngine {
    inner: Arc<Inner>,
}

impl Drop for ConstructEngine {
    /// Stop the sync worker now, not only once `Inner` is freed: a worker
    /// mid-pass holds a strong reference and would otherwise keep flushing
    /// rows after the engine was closed.
    fn drop(&mut self) {
        self.inner.signal.shutdown();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking observer must not brick the engine.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix time in milliseconds (`action_outbox.next_attempt_at`).
fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The `net` lock for a cache file, shared by every engine in this process
/// that opens the same file. An engine that is being dropped can still be
/// mid-pass (its worker holds a strong reference during a POST) while a new
/// engine on the same storage starts (e.g. an Android activity relaunched):
/// without a shared lock both workers could POST the same outbox row.
/// `None` (in-memory store) gets a private lock.
fn net_lock_for(db_path: Option<&Path>) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
        LazyLock::new(Mutex::default);
    let Some(path) = db_path else {
        return Arc::default();
    };
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut locks = lock(&LOCKS);
    locks.retain(|_, l| l.strong_count() > 0);
    if let Some(l) = locks.get(&key).and_then(Weak::upgrade) {
        return l;
    }
    let l: Arc<Mutex<()>> = Arc::default();
    locks.insert(key, Arc::downgrade(&l));
    l
}

/// Open `<storage_path>/cache.db`, falling back to an in-memory DB when
/// `storage_path` is empty or the file cannot be opened. A file that can be
/// read but not written falls back to an in-memory copy of its workspaces
/// and entities. Also returns the `net` lock for that store (see
/// [`net_lock_for`]).
fn open_store(storage_path: &str) -> (Option<Store>, Arc<Mutex<()>>) {
    if !storage_path.is_empty() {
        let path = PathBuf::from(storage_path).join(CACHE_DB_FILE);
        let opened = match store::make_private_dir(Path::new(storage_path)) {
            Ok(()) => {
                Store::open(&path).map_err(|e| (store::is_read_only_error(&e), e.to_string()))
            }
            Err(e) => Err((false, e.to_string())),
        };
        match opened {
            Ok(store) => return (Some(store), net_lock_for(Some(&path))),
            Err((true, e)) => {
                // Readable but not writable: keep the cached view in
                // memory so the app still shows it, but every change is
                // lost on exit.
                eprintln!(
                    "[ConstructEngine] ERROR: {} is read-only ({e}); using an in-memory copy. \
                     Changes and queued actions will NOT be saved across restarts.",
                    path.display()
                );
                match Store::open_in_memory_copy_of(&path) {
                    Ok((store, ws, ents)) => {
                        eprintln!(
                            "[ConstructEngine] copied {ws} workspace(s) and {ents} entities from the \
                             read-only {CACHE_DB_FILE} (outbox rows are not copied)"
                        );
                        return (Some(store), net_lock_for(None));
                    }
                    Err(e) => eprintln!("[ConstructEngine] in-memory copy failed: {e}"),
                }
            }
            Err((false, e)) => eprintln!(
                "[ConstructEngine] cannot open {storage_path}/{CACHE_DB_FILE}: {e}; using an \
                 in-memory store"
            ),
        }
    }
    match Store::open_in_memory() {
        Ok(store) => (Some(store), net_lock_for(None)),
        Err(e) => {
            eprintln!("[ConstructEngine] cannot open an in-memory store: {e}; mutations disabled");
            (None, net_lock_for(None))
        }
    }
}

/// The schema-9 parts of an `action.mutation.create`.
struct CreateOptions {
    refresh_workspace: bool,
    attachments: BTreeMap<String, Value>,
    at_start: bool,
}

/// A staged attachment, as the engine reports it to the UI
/// ([`ConstructEngine::stage_attachment`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AttachmentInfo {
    pub id: String,
    /// `construct://attachments/<id>`, the local entity a Carousel draws.
    pub uri: String,
    /// A `file://` URL of the staged JPEG.
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// [`AttachmentInfo`] as JSON (what the FFI returns).
pub fn attachment_info_json(info: &AttachmentInfo) -> Result<String, String> {
    serde_json::to_string(info).map_err(|e| e.to_string())
}

/// The local entity of a staged attachment.
fn attachment_entity(a: &Attachment) -> Value {
    let aspect = if a.height > 0 {
        f64::from(a.width) / f64::from(a.height)
    } else {
        1.0
    };
    json!({
        "id": a.id,
        "url": attachments::file_url(Path::new(&a.path)),
        "preview_url": attachments::file_url(Path::new(&a.path)),
        "aspect_ratio": (aspect * 1000.0).round() / 1000.0,
        "width": a.width,
        "height": a.height,
        "local": true,
    })
}

/// A create's `attachments` (form-bound: `{"photos": ["<id>", ...]}`) as
/// refs, checked against the staged attachments: each value a list of
/// staged, not yet sent ids, each id at most once, at most
/// [`MAX_ATTACHMENTS`] in all.
fn attachment_refs(
    attachments: &BTreeMap<String, Value>,
    staged: &BTreeMap<String, Attachment>,
) -> Result<Vec<AttachmentRef>, String> {
    let mut refs: Vec<AttachmentRef> = Vec::new();
    for (name, ids) in attachments {
        // An unbound form list (`"$form.photos"`) or one sent as a JSON string.
        let parsed;
        let ids = match ids {
            Value::String(s) => {
                parsed = serde_json::from_str::<Value>(s)
                    .map_err(|_| format!("attachments '{name}' is not a list of ids"))?;
                &parsed
            }
            v => v,
        };
        let Some(ids) = ids.as_array() else {
            return Err(format!("attachments '{name}' is not a list of ids"));
        };
        for id in ids {
            let Some(id) = id.as_str() else {
                return Err(format!("attachments '{name}' holds a non-string id"));
            };
            match staged.get(id) {
                Some(a) if a.released_at.is_none() => {}
                _ => return Err(format!("attachment {id} is not staged")),
            }
            if refs.iter().any(|r| r.id == id) {
                return Err(format!("attachment {id} is used twice"));
            }
            refs.push(AttachmentRef {
                name: name.clone(),
                id: id.to_owned(),
                upload_id: None,
            });
        }
    }
    if refs.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "{} attachments (at most {MAX_ATTACHMENTS})",
            refs.len()
        ));
    }
    Ok(refs)
}

/// Replace every JSON string exactly equal to [`VALUE_TOKEN`] inside
/// `payload` with `value`.
fn substitute_value_token(payload: Value, value: &Value) -> Value {
    match payload {
        Value::String(s) if s == VALUE_TOKEN => value.clone(),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| substitute_value_token(v, value))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, substitute_value_token(v, value)))
                .collect(),
        ),
        other => other,
    }
}

impl ConstructEngine {
    /// Create an engine for the `"home"` workspace. Returns immediately; the
    /// initial fetch runs on a background thread. Never panics if the server
    /// is unreachable.
    pub fn init(storage_path: &str, base_url: &str) -> Self {
        Self::init_with_workspace(storage_path, base_url, DEFAULT_WORKSPACE_ID)
    }

    /// Like [`ConstructEngine::init`], fetching `GET {base_url}/v1/workspaces/{workspace_id}`.
    /// Default options: platform secure store, no URL opener, loopback redirect.
    pub fn init_with_workspace(storage_path: &str, base_url: &str, workspace_id: &str) -> Self {
        Self::init_with_options(
            storage_path,
            base_url,
            workspace_id,
            EngineOptions::default(),
        )
    }

    /// Like [`ConstructEngine::init_with_workspace`] with custom timing. Uses
    /// an in-memory [`SecureStore`] (tests: never touches the Keychain).
    pub fn init_with_config(
        storage_path: &str,
        base_url: &str,
        workspace_id: &str,
        config: EngineConfig,
    ) -> Self {
        Self::init_with_options(
            storage_path,
            base_url,
            workspace_id,
            EngineOptions {
                config,
                secure_store: Some(Arc::new(MemoryStore::new())),
                ..EngineOptions::default()
            },
        )
    }

    /// The general constructor: timing, secure store, URL opener and
    /// redirect mode. A session already in the store is restored (the
    /// snapshot starts `signed_in`).
    pub fn init_with_options(
        storage_path: &str,
        base_url: &str,
        workspace_id: &str,
        options: EngineOptions,
    ) -> Self {
        let EngineOptions {
            config,
            secure_store,
            url_opener,
            redirect,
            loopback_port,
        } = options;
        let secure_store: Arc<dyn SecureStore> =
            secure_store.unwrap_or_else(|| Arc::from(auth::default_secure_store()));
        let base = base_url.trim_end_matches('/');
        let session_key = auth::session_key(base);
        if is_cleartext_remote(base) {
            eprintln!(
                "[ConstructEngine] WARNING: {base} is plain http:// to a non-local host: the pod \
                 token and all data travel unencrypted. Use https:// for remote servers."
            );
        }

        let (store, net) = open_store(storage_path);
        let attachments_dir = if storage_path.is_empty() {
            std::env::temp_dir().join(format!(
                "construct-attachments-{}-{}",
                std::process::id(),
                auth::random_token(8)
            ))
        } else {
            PathBuf::from(storage_path).join("attachments")
        };
        let mut state = State {
            workspace_id: workspace_id.to_owned(),
            store,
            ..State::default()
        };
        state.load_cached(workspace_id);
        let device_id = notify::load_device_id(state.store.as_ref());
        let push = notify::load_push(state.store.as_ref());
        state.sweep_attachments(unix_now());
        let stored = guarded("SecureStore.get", || secure_store.get(&session_key)).flatten();
        if let Some(session) = stored.as_deref().and_then(Session::parse) {
            println!(
                "[ConstructEngine] restored pod session for {base} (user: {})",
                session.username.as_deref().unwrap_or("?")
            );
            state.auth_json = session.auth_json();
            state.session = Some(session);
            state.load_unread();
        }

        let poll_interval = config.poll_interval;
        let signal = Arc::new(WorkerSignal::default());
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            observers: Mutex::new(Vec::new()),
            net,
            signal: Arc::clone(&signal),
            config,
            workspace_id: workspace_id.to_string(),
            base_url: base.to_string(),
            workspace_url: format!("{base}/v1/workspaces/{workspace_id}"),
            health_url: format!("{base}/healthz"),
            secure_store,
            url_opener,
            redirect,
            loopback_port,
            session_key,
            pending_key: auth::pending_auth_key(base),
            flow: Mutex::new(None),
            session_io: Mutex::new(()),
            pending_io: Mutex::new(()),
            device_id,
            device: Mutex::new(notify::DeviceState::new(push)),
            device_io: Mutex::new(()),
            notification_listener: Mutex::new(None),
            pending_reads: Mutex::new(Vec::new()),
            attachments_dir,
        });

        // Both background threads hold only a Weak reference between
        // passes, so they exit once the engine is dropped instead of
        // keeping it alive. Until the server has answered once the fetch
        // thread retries the full fetch; afterwards it only probes
        // reachability.
        let bg: Weak<Inner> = Arc::downgrade(&inner);
        let spawned = thread::Builder::new()
            .name("construct-engine-fetch".into())
            .spawn(move || {
                let mut answered = false;
                loop {
                    let Some(inner) = bg.upgrade() else { return };
                    if inner.signal.is_shut_down() {
                        return;
                    }
                    if answered {
                        inner.probe();
                    } else {
                        answered = !matches!(inner.refresh(), FetchOutcome::Failed);
                    }
                    drop(inner);
                    thread::sleep(poll_interval);
                }
            });
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn fetch thread: {e}");
        }

        let worker: Weak<Inner> = Arc::downgrade(&inner);
        let spawned = thread::Builder::new()
            .name("construct-engine-sync".into())
            .spawn(move || sync_worker(worker, signal));
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn sync worker: {e}");
        }

        let events: Weak<Inner> = Arc::downgrade(&inner);
        let events_signal = Arc::clone(&inner.signal);
        let events_url = format!("{base}/v1/workspaces/{workspace_id}/events");
        let spawned = thread::Builder::new()
            .name("construct-engine-events".into())
            .spawn(move || events::events_thread(events, events_signal, events_url, poll_interval));
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn the events thread: {e}");
        }

        Self { inner }
    }

    /// Register an observer. It immediately receives the current snapshot
    /// and then every subsequent change. Observers are called
    /// without engine locks held, possibly from a background thread.
    pub fn subscribe<F: Fn(StateSnapshot) + Send + Sync + 'static>(&self, f: F) {
        let observer = Arc::new(Observer {
            f: Box::new(f),
            last_seq: AtomicU64::new(0),
        });
        lock(&self.inner.observers).push(Arc::clone(&observer));
        let snap = lock(&self.inner.state).snapshot();
        let seq = snap.seq;
        // Always deliver the initial snapshot (even at seq 0), unless a
        // concurrent notify already delivered this or a newer state. `notify`
        // never sends seq 0, so `prev == 0` at seq 0 means nothing was sent.
        let prev = observer.last_seq.fetch_max(seq, Ordering::SeqCst);
        if prev < seq || (seq == 0 && prev == 0) {
            observer.call(snap);
        }
    }

    /// Handle an `ActionIntent` JSON string. Never panics; invalid or
    /// unsupported intents are logged and ignored.
    pub fn dispatch(&self, intent_json: &str) {
        self.inner.dispatch(intent_json);
    }

    /// The current snapshot. Before the first fetch (and with no cached
    /// workspace) this is `workspace_json: "null"`, `entities_json: "{}"`,
    /// `is_online: false`.
    pub fn snapshot(&self) -> StateSnapshot {
        lock(&self.inner.state).snapshot()
    }

    /// Re-fetch the workspace on the calling thread (blocking), then notify.
    pub fn refresh(&self) {
        self.inner.refresh();
    }

    /// Deliver an OAuth redirect (custom-scheme mode, e.g.
    /// `construct://oauth?code=..&state=..`) to the pending sign-in. Returns
    /// true if it was taken: delivered to the in-memory flow, or (no flow in
    /// memory, e.g. the app process was restarted while the browser was in
    /// front) handed to a background thread that completes the sign-in
    /// persisted under [`auth::pending_auth_key`] if its `state` matches and
    /// it is not expired, else reports [`SIGN_IN_INTERRUPTED`] (only when
    /// signed out). False if the engine uses a loopback redirect, the URL
    /// carries no code/error, or its `state` does not match the in-memory
    /// flow (which then keeps waiting). Never blocks.
    pub fn handle_auth_redirect(&self, url: &str) -> bool {
        self.inner.handle_auth_redirect(url)
    }

    /// Sign out: cancel any sign-in, forget the session (memory and secure
    /// store) and notify `signed_out` right away; `POST /v1/auth/logout`
    /// (best effort) and the refetch run on a background thread.
    pub fn logout(&self) {
        self.inner.logout();
    }

    /// Forget the session on this device only (Settings' "Leave pod", D46): no
    /// server call, private data purged, the workspace fetched without it.
    pub fn forget_session(&self) {
        self.inner.forget_session();
    }

    /// Stage a picked photo for a create's `attachments` (D49): decoded,
    /// turned upright, scaled to at most 2048 px, re-encoded as a JPEG without
    /// metadata and kept under the engine's storage. Its local entity
    /// `construct://attachments/<id>` appears in the snapshot. Blocking (call
    /// it off the UI thread). `Err` is a short sentence for the user.
    pub fn stage_attachment(&self, path: &str) -> Result<AttachmentInfo, String> {
        self.inner.stage_attachment(path)
    }

    /// Forget a staged attachment the user removed before sending it.
    pub fn discard_attachment(&self, id: &str) {
        self.inner.discard_attachment(id);
    }

    /// The form values this device remembers for this engine's app in form
    /// `scope` (`""` = the app's layout, else a screen id): field -> value.
    /// Which fields are remembered is the layout's choice (`core://ChoiceChips`
    /// `remember`, schema 10); the engine only stores them. Empty on any error.
    pub fn remembered_form_values(&self, scope: &str) -> BTreeMap<String, String> {
        self.inner.remembered_form_values(scope)
    }

    /// Remember a form value of this engine's app (`None` or "" forgets it).
    /// Oversized scopes, fields or values are ignored.
    pub fn remember_form_value(&self, scope: &str, field: &str, value: Option<&str>) {
        self.inner.remember_form_value(scope, field, value);
    }
}

/// Longest form scope or field name the engine remembers.
const MAX_FORM_NAME: usize = 128;
/// Longest form value the engine remembers.
const MAX_FORM_VALUE: usize = 1024;

impl Inner {
    fn remembered_form_values(&self, scope: &str) -> BTreeMap<String, String> {
        let st = lock(&self.state);
        let Some(store) = st.store.as_ref() else {
            return BTreeMap::new();
        };
        store
            .form_values(&self.workspace_id, scope)
            .unwrap_or_else(|e| {
                eprintln!("[ConstructEngine] remembered form values: {e}");
                BTreeMap::new()
            })
    }

    fn remember_form_value(&self, scope: &str, field: &str, value: Option<&str>) {
        let value = value.filter(|v| !v.is_empty());
        if scope.len() > MAX_FORM_NAME
            || field.is_empty()
            || field.len() > MAX_FORM_NAME
            || value.is_some_and(|v| v.len() > MAX_FORM_VALUE)
        {
            return;
        }
        let st = lock(&self.state);
        if let Some(Err(e)) = st
            .store
            .as_ref()
            .map(|s| s.set_form_value(&self.workspace_id, scope, field, value, unix_now()))
        {
            eprintln!("[ConstructEngine] remembering form value '{field}': {e}");
        }
    }

    fn stage_attachment(&self, path: &str) -> Result<AttachmentInfo, String> {
        let id = auth::random_token(16);
        let staged = attachments::stage(Path::new(path), &self.attachments_dir, &id)?;
        let a = Attachment {
            id: id.clone(),
            path: staged.path.to_string_lossy().into_owned(),
            mime: attachments::STAGED_MIME.to_owned(),
            size: i64::try_from(staged.size).unwrap_or(i64::MAX),
            width: staged.width,
            height: staged.height,
            created_at: unix_now(),
            released_at: None,
        };
        let snap = {
            let mut st = lock(&self.state);
            if let Some(Err(e)) = st.store.as_ref().map(|s| s.insert_attachment(&a)) {
                let _ = fs::remove_file(&staged.path);
                eprintln!("[ConstructEngine] attachment {id}: failed to save ({e})");
                return Err("the photo couldn't be saved".into());
            }
            st.attachments.insert(id.clone(), a.clone());
            st.sweep_attachments(a.created_at);
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
        println!(
            "[ConstructEngine] staged attachment {id} ({}x{}, {} KB)",
            a.width,
            a.height,
            a.size / 1024
        );
        Ok(AttachmentInfo {
            uri: attachments::entity_uri(&id),
            url: attachments::file_url(Path::new(&a.path)),
            id,
            width: a.width,
            height: a.height,
        })
    }

    fn discard_attachment(&self, id: &str) {
        let snap = {
            let mut st = lock(&self.state);
            let used = st
                .store
                .as_ref()
                .and_then(|s| s.pending_outbox().ok())
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r.create.as_ref())
                .any(|c| c.attachments.iter().any(|a| a.id == id));
            if used || !st.attachments.contains_key(id) {
                return;
            }
            st.delete_attachments(&[id.to_owned()]);
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
    }

    /// Upload the attachments of a create row that have no upload id yet
    /// (saving each id as it arrives), then return the payload with
    /// `$attachments.<name>` replaced by the upload ids. A failed upload is
    /// the row's flush result.
    fn upload_attachments(
        &self,
        client: &reqwest::blocking::Client,
        row: &OutboxRow,
        token: Option<&str>,
    ) -> Result<String, FlushResult> {
        let Some(info) = row.create.as_ref().filter(|c| !c.attachments.is_empty()) else {
            return Ok(row.payload_json.clone());
        };
        let mut info = info.clone();
        let url = format!("{}{UPLOADS_PATH}", self.base_url);
        for i in 0..info.attachments.len() {
            if info.attachments[i].upload_id.is_some() {
                continue;
            }
            let staged = lock(&self.state)
                .attachments
                .get(&info.attachments[i].id)
                .cloned();
            let Some(staged) = staged else {
                return Err(FlushResult::Rejected(
                    410,
                    format!("attachment {} is gone", info.attachments[i].id),
                ));
            };
            let upload_id = upload_file(client, &url, &staged, token)?;
            println!(
                "[ConstructEngine] outbox #{}: uploaded attachment {} -> {upload_id}",
                row.id, staged.id
            );
            info.attachments[i].upload_id = Some(upload_id);
            let st = lock(&self.state);
            if let Some(Err(e)) = st.store.as_ref().map(|s| s.set_create_info(row.id, &info)) {
                // Uploaded again on the next attempt (same key, same id).
                eprintln!(
                    "[ConstructEngine] outbox #{}: failed to save the upload id: {e}",
                    row.id
                );
            }
        }
        let payload: Value = serde_json::from_str(&row.payload_json).unwrap_or(Value::Null);
        let uploads = |name: &str| {
            let ids: Vec<Value> = info
                .attachments
                .iter()
                .filter(|a| a.name == name)
                .filter_map(|a| a.upload_id.clone().map(Value::String))
                .collect();
            Some(Value::Array(ids))
        };
        Ok(attachments::substitute(payload, &uploads).to_string())
    }

    /// Check whether the server is reachable (any HTTP response counts) and
    /// notify observers if `is_online` changed. Never touches the workspace
    /// or entities.
    fn probe(&self) {
        let reachable = reachable(&self.health_url);
        if self.signal.is_shut_down() {
            return;
        }
        let snap = {
            let mut st = lock(&self.state);
            if st.is_online == reachable {
                return;
            }
            st.is_online = reachable;
            if reachable {
                println!("[ConstructEngine] server reachable again; online");
            } else {
                eprintln!(
                    "[ConstructEngine] server unreachable ({}); offline",
                    self.health_url
                );
            }
            st.seq += 1;
            st.snapshot()
        };
        if reachable {
            self.signal.wake();
        }
        self.notify(snap);
    }

    /// After a logout whose session the pod kept: hold the token again (no
    /// account), unless another session arrived meanwhile.
    fn restore_session(&self, token: String) {
        let snap = {
            let mut st = lock(&self.state);
            if st.session.is_some() {
                return;
            }
            let session = Session {
                token,
                username: None,
                display_name: None,
                avatar_url: None,
            };
            st.auth_json = session.auth_json();
            st.session = Some(session);
            st.seq += 1;
            st.snapshot()
        };
        println!("[ConstructEngine] signed out of the app; the pod session stays");
        self.notify(snap);
        self.sync_session_store();
    }

    fn session_token(&self) -> Option<String> {
        lock(&self.state).session.as_ref().map(|s| s.token.clone())
    }

    /// The pod answered 401 to a request carrying `token`: if that is still
    /// the current session, forget it (memory + secure store) and go
    /// `signed_out`. Returns the snapshot to deliver, if anything changed.
    fn expire_session(&self, token: &str, what: &str) -> Option<StateSnapshot> {
        let snap = {
            let mut st = lock(&self.state);
            if st.session.as_ref().map(|s| s.token.as_str()) != Some(token) {
                return None;
            }
            eprintln!(
                "[ConstructEngine] pod rejected the session on {what} (HTTP 401); signed out"
            );
            st.session = None;
            st.auth_json = SIGNED_OUT_AUTH_JSON.to_string();
            // Pending rows are kept (they resume after the next sign-in).
            st.purge_private(&self.workspace_id, false);
            st.seq += 1;
            st.snapshot()
        };
        // Called on the fetch / sync threads only.
        self.sync_session_store();
        Some(snap)
    }

    /// Make the secure store match the in-memory session (write it, or
    /// delete the key when signed out). Every session change calls this
    /// afterwards, off the UI thread and without the flow lock; `session_io`
    /// serializes the calls and each one writes the state current when it
    /// runs, so the last write always reflects the last change (a logout
    /// racing a sign-in cannot leave a stale session in the store).
    fn sync_session_store(&self) {
        let _io = lock(&self.session_io);
        let raw = lock(&self.state)
            .session
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default());
        match raw {
            Some(raw) => {
                if guarded("SecureStore.set", || {
                    self.secure_store.set(&self.session_key, &raw)
                })
                .is_none()
                {
                    eprintln!(
                        "[ConstructEngine] the session could not be stored; it lasts until exit"
                    );
                }
            }
            None => {
                guarded("SecureStore.delete", || {
                    self.secure_store.delete(&self.session_key)
                });
            }
        }
    }

    fn refresh(&self) -> FetchOutcome {
        let net = lock(&self.net);
        let mut token = self.session_token();
        let mut result = fetch(&self.workspace_url, token.as_deref(), &self.device_id);
        if let (Err(FetchError::Status(401)), Some(t)) = (&result, &token) {
            // The refresh below delivers a newer snapshot than this one.
            let _ = self.expire_session(t, "workspace fetch");
            token = None;
            if !self.signal.is_shut_down() {
                result = fetch(&self.workspace_url, None, &self.device_id);
            }
        }
        if self.signal.is_shut_down() {
            // The engine was dropped during the request (the fetch thread
            // held the last strong reference): apply nothing and wake no
            // one, so no POST can start because of this fetch.
            return FetchOutcome::Failed;
        }
        let outcome;
        let snap = {
            let mut st = lock(&self.state);
            match result {
                Ok(_)
                    if token.is_some()
                        && st.session.as_ref().map(|s| &s.token) != token.as_ref() =>
                {
                    // Fetched with a session that was signed out (or
                    // replaced) meanwhile: never cache that account's data
                    // again. Whoever changed the session refetches.
                    println!(
                        "[ConstructEngine] discarded workspace '{}' fetched with a session that has \
                         since changed",
                        self.workspace_id
                    );
                    st.is_online = true;
                    outcome = FetchOutcome::Fetched;
                }
                Ok((mut spec, version)) => {
                    let failures = std::mem::take(&mut st.fetch_failures);
                    st.last_fetch_error = None;
                    let after = if failures > 0 {
                        format!(" after {failures} failed attempt(s)")
                    } else {
                        String::new()
                    };
                    println!(
                        "[ConstructEngine] fetched workspace '{}' ({} entities) from {}{after}",
                        self.workspace_id,
                        spec.entities.len(),
                        self.workspace_url
                    );
                    match (version, st.layout_version, &st.workspace) {
                        (Some(v), Some(have), Some(current)) if v < have => {
                            // A fetch that started before a newer layout was
                            // applied (by a later fetch): keep the newer
                            // layout, take the entities.
                            //
                            // Trade-off: a pod whose database was reset (its
                            // versions restart at 1) also lands here, and this
                            // engine keeps showing its old layout until it is
                            // recreated (app restart, switching apps/servers).
                            // Version numbers are monotonic per pod by design
                            // (D32), so we favour ignoring stale responses over
                            // detecting resets.
                            println!(
                                "[ConstructEngine] fetched layout v{v} is older than the applied \
                                 v{have}; layout kept"
                            );
                            spec.workspace = current.clone();
                        }
                        _ => st.layout_version = version,
                    }
                    st.apply_fetched(&self.workspace_id, spec);
                    st.is_online = true;
                    st.gone = false;
                    outcome = FetchOutcome::Fetched;
                }
                Err(FetchError::Status(code)) => {
                    // The server responded, so the network is reachable;
                    // keep whatever workspace we already have.
                    if code == 404 {
                        eprintln!(
                            "[ConstructEngine] workspace '{}' not found (HTTP 404)",
                            self.workspace_id
                        );
                        st.gone = true;
                    } else {
                        eprintln!(
                            "[ConstructEngine] fetch {} failed: HTTP {code}",
                            self.workspace_url
                        );
                    }
                    st.is_online = true;
                    outcome = FetchOutcome::HttpError;
                }
                Err(FetchError::Other(e)) => {
                    // The fetch thread retries every poll interval while the
                    // server is down: log the first failure, then only when
                    // the error changes or every FETCH_FAILURE_LOG_EVERY-th.
                    st.fetch_failures = st.fetch_failures.saturating_add(1);
                    let n = st.fetch_failures;
                    if n == 1
                        || st.last_fetch_error.as_deref() != Some(e.as_str())
                        || n.is_multiple_of(FETCH_FAILURE_LOG_EVERY)
                    {
                        eprintln!(
                            "[ConstructEngine] fetch {} failed: {e} (attempt {n}; retrying every \
                             {} ms, repeats are not logged)",
                            self.workspace_url,
                            self.config.poll_interval.as_millis()
                        );
                    }
                    st.last_fetch_error = Some(e);
                    st.is_online = false;
                    outcome = FetchOutcome::Failed;
                }
            }
            st.seq += 1;
            st.snapshot()
        };
        drop(net);
        if snap.is_online {
            self.signal.wake();
        }
        self.notify(snap);
        outcome
    }

    /// One sync-worker pass: flush due PENDING rows FIFO, one at a time,
    /// while online. Returns how long to wait before the next pass (`None`:
    /// until woken by a dispatch or by going online).
    fn flush_pass(&self, client: &reqwest::blocking::Client) -> Option<Duration> {
        loop {
            let net = lock(&self.net);
            if self.signal.is_shut_down() {
                // The engine was dropped mid-pass: stop before the next row.
                return None;
            }
            let (row, token) = {
                let mut st = lock(&self.state);
                if !st.is_online || st.auth_blocked {
                    return None;
                }
                let token = st.session.as_ref().map(|s| s.token.clone());
                let store = st.store.as_ref()?;
                match store.first_pending() {
                    Ok(Some(row)) => (row, token),
                    Ok(None) => {
                        // Another engine on the same cache file may have
                        // flushed rows this one counted: resync to 0.
                        if st.pending_actions != 0 {
                            st.pending_actions = 0;
                            st.seq += 1;
                            let snap = st.snapshot();
                            drop(st);
                            drop(net);
                            self.notify(snap);
                        }
                        return None;
                    }
                    Err(e) => {
                        eprintln!("[ConstructEngine] sync worker: cannot read the outbox: {e}");
                        return Some(self.config.poll_interval);
                    }
                }
            };
            let now = unix_now_ms();
            let cap_ms = i64::try_from(self.config.backoff_cap.as_millis()).unwrap_or(i64::MAX);
            if row.next_attempt_at > now && row.next_attempt_at - now <= cap_ms {
                // FIFO: never skip past a row that is not due yet. A row due
                // further out than the backoff cap can only come from the
                // wall clock moving backwards; it is treated as due.
                return Some(Duration::from_millis((row.next_attempt_at - now) as u64));
            }

            if self.signal.is_shut_down() {
                // Checked again right before the POST: once `ConstructEngine`
                // has been dropped no new POST starts (one already in flight
                // may complete).
                return None;
            }
            let url = format!("{}{}", self.base_url, row.endpoint);
            // The pod session is only ever sent to the pod itself: an
            // endpoint such as `@evil.example/x` or `.evil.example/x` would
            // otherwise move the request (and the bearer) to another host.
            let sent = token
                .as_deref()
                .filter(|_| same_origin(&self.base_url, &url));
            let device = same_origin(&self.base_url, &url).then_some(self.device_id.as_str());
            let result = match self.upload_attachments(client, &row, sent) {
                Ok(body) => post_json(client, &url, &body, sent, device),
                Err(r) => r,
            };
            if let FlushResult::Unauthorized = result {
                let (snap, blocked) = self.outbox_unauthorized(&row, &url, sent, token.as_deref());
                drop(net);
                let signed_out = snap.is_some();
                if let Some(snap) = snap {
                    self.notify(snap);
                }
                if blocked {
                    if signed_out && !self.signal.is_shut_down() {
                        // The private feed was purged: show the signed-out
                        // bundle instead of an empty one.
                        self.refresh();
                    }
                    // Paused until a sign-in wakes the worker.
                    return None;
                }
                // The session changed during the POST: retry with the new one.
                continue;
            }
            let (snap, failed) = self.apply_flush_result(&row, &url, result);
            drop(net);
            if let Some(snap) = snap {
                self.notify(snap);
            }
            self.run_followups();
            if failed {
                // Next pass re-reads the row and waits for its
                // next_attempt_at (or for going online again).
                return Some(self.config.backoff(row.retry_count + 1));
            }
        }
    }

    /// An outbox POST got a 401. The row stays PENDING with its optimistic
    /// value (not rolled back, retry count unchanged): flushing pauses
    /// (`auth_blocked`) until the next sign-in, and the session that was
    /// sent (`sent`), if any, is forgotten. If the session changed while the
    /// POST was in flight (`session` is the one current when it started; a
    /// sign-in or logout completed meanwhile), nothing is paused or
    /// forgotten. Returns the snapshot to deliver, if any, and whether
    /// flushing is now paused.
    fn outbox_unauthorized(
        &self,
        row: &OutboxRow,
        url: &str,
        sent: Option<&str>,
        session: Option<&str>,
    ) -> (Option<StateSnapshot>, bool) {
        let expired = sent.and_then(|t| self.expire_session(t, "outbox POST"));
        let mut st = lock(&self.state);
        if expired.is_none() && st.session.as_ref().map(|s| s.token.as_str()) != session {
            eprintln!(
                "[ConstructEngine] outbox #{} POST {url} -> 401 with a session that has since \
                 changed; retrying",
                row.id
            );
            return (None, false);
        }
        eprintln!(
            "[ConstructEngine] outbox #{} POST {url} -> 401; kept PENDING, flushing paused until \
             sign-in ({} pending)",
            row.id, st.pending_actions
        );
        st.auth_blocked = true;
        let snap = expired.map(|_| {
            st.seq += 1;
            st.snapshot()
        });
        (snap, true)
    }

    /// Record the outcome of POSTing `row` (state lock held only here).
    /// Returns a snapshot to deliver if anything visible changed, and
    /// whether the row is still pending (failure: stop the pass).
    fn apply_flush_result(
        &self,
        row: &OutboxRow,
        url: &str,
        result: FlushResult,
    ) -> (Option<StateSnapshot>, bool) {
        let id = row.id;
        let mut st = lock(&self.state);
        let (error, network) = match result {
            FlushResult::Accepted(code) => {
                let deleted = st.store.as_ref().map(|s| s.delete_outbox(id));
                if let Some(Err(e)) = deleted {
                    // The row stays and will be sent again.
                    eprintln!(
                        "[ConstructEngine] outbox #{id} POST {url} -> {code}, but deleting the row \
                         failed: {e}"
                    );
                    return (None, true);
                }
                st.sync_pending_count();
                println!(
                    "[ConstructEngine] outbox #{id} POST {url} -> {code}, deleted ({} pending)",
                    st.pending_actions
                );
                if let Some(c) = &row.create {
                    // Keep listing it until its query is reloaded.
                    st.creates.retain(|x| x.row_id != id);
                    st.settling
                        .push((c.query_key.clone(), row.entity_uri.clone(), c.at_start));
                    let now = unix_now();
                    for a in &c.attachments {
                        if let Some(staged) = st.attachments.get_mut(&a.id) {
                            staged.released_at = Some(now);
                        }
                        if let Some(Err(e)) =
                            st.store.as_ref().map(|s| s.release_attachment(&a.id, now))
                        {
                            eprintln!("[ConstructEngine] attachment {}: {e}", a.id);
                        }
                    }
                    let paths: Vec<String> = st
                        .data
                        .iter()
                        .filter(|(_, d)| d.queries.contains_key(&c.query_key))
                        .map(|(p, _)| p.clone())
                        .collect();
                    let refresh = c.refresh_workspace
                        || (paths.is_empty() && st.queries.contains_key(&c.query_key));
                    st.followups.extend(paths.into_iter().map(Followup::Load));
                    if refresh {
                        st.followups.push(Followup::Refresh);
                    }
                }
                st.seq += 1;
                return (Some(st.snapshot()), false);
            }
            FlushResult::Rejected(code, body) if row.create.is_some() => {
                match st.reject_create(row) {
                    Ok(()) => {
                        println!(
                            "[ConstructEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
                             removed {} ({} pending)",
                            row.entity_uri, st.pending_actions
                        );
                        st.seq += 1;
                        return (Some(st.snapshot()), false);
                    }
                    Err(e) => {
                        eprintln!(
                            "[ConstructEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
                             removing it failed ({e}); will retry"
                        );
                        (format!("HTTP {code}: {body}; removal failed: {e}"), false)
                    }
                }
            }
            FlushResult::Rejected(code, body) => {
                let field = row.field.as_deref().unwrap_or("?");
                match st.reject_row(row) {
                    Ok((restored, now_value, reapplied)) => {
                        println!(
                            "[ConstructEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
                             rolled back {}#{field} to {restored}, re-applied {reapplied} later \
                             row(s) -> {now_value} ({} pending)",
                            row.entity_uri, st.pending_actions
                        );
                        st.seq += 1;
                        return (Some(st.snapshot()), false);
                    }
                    Err(e) => {
                        // Keep the row; retry the rollback on a later pass.
                        eprintln!(
                            "[ConstructEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
                             rollback failed to persist ({e}); will retry"
                        );
                        (format!("HTTP {code}: {body}; rollback failed: {e}"), false)
                    }
                }
            }
            FlushResult::ServerError(code, body) => (format!("HTTP {code}: {body}"), false),
            FlushResult::Network(e) => (e, true),
            // Handled by `outbox_unauthorized` before this is called.
            FlushResult::Unauthorized => ("HTTP 401".to_string(), false),
        };

        let retry_count = row.retry_count + 1;
        let delay = self.config.backoff(retry_count);
        let next = unix_now_ms() + delay.as_millis() as i64;
        if let Some(Err(e)) = st.store.as_ref().map(|s| s.mark_retry(id, next, &error)) {
            eprintln!("[ConstructEngine] outbox #{id}: failed to record the retry: {e}");
        }
        eprintln!(
            "[ConstructEngine] outbox #{id} POST {url} -> {error}; retry #{retry_count} in {} ms \
             ({} pending)",
            delay.as_millis(),
            st.pending_actions
        );
        if network && st.is_online {
            eprintln!("[ConstructEngine] server unreachable ({url}); offline");
            st.is_online = false;
            st.seq += 1;
            return (Some(st.snapshot()), true);
        }
        (None, true)
    }

    fn dispatch(self: &Arc<Self>, intent_json: &str) {
        let started = Instant::now();
        let compact = serde_json::from_str::<Value>(intent_json)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| intent_json.to_string());
        println!("[ConstructEngine] dispatch: {compact}");
        let intent: ActionIntent = match serde_json::from_str(intent_json) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("[ConstructEngine] dispatch: invalid intent ({e}); ignored");
                return;
            }
        };

        match intent {
            ActionIntent::OptimisticMutate {
                entity_uri,
                field,
                value_expr,
                rollback_value,
                endpoint,
                payload,
                also,
            } => self.optimistic_mutate(
                started,
                entity_uri,
                field,
                value_expr,
                rollback_value,
                endpoint,
                payload,
                also,
            ),
            ActionIntent::AuthLogin {
                instance_url,
                app_path,
                token_path,
            } => self.start_login(instance_url, app_path, token_path),
            ActionIntent::OpenSession { path, payload } => self.open_session(path, payload),
            ActionIntent::LoadData { path } => {
                if self.begin_load(&path) {
                    let inner = Arc::clone(self);
                    let spawned = thread::Builder::new()
                        .name("construct-load".into())
                        .spawn(move || inner.load_data(&path));
                    if let Err(e) = spawned {
                        eprintln!("[ConstructEngine] cannot start a load thread: {e}");
                    }
                }
            }
            ActionIntent::Create {
                query_key,
                entity_uri,
                entity,
                endpoint,
                payload,
                refresh_workspace,
                attachments,
                position,
                ..
            } => self.create(
                started,
                query_key,
                entity_uri,
                entity,
                endpoint,
                payload,
                CreateOptions {
                    refresh_workspace,
                    attachments,
                    at_start: position == Position::Start,
                },
            ),
            other => {
                eprintln!(
                    "[ConstructEngine] dispatch: intent not handled by client-core yet: {other:?}"
                );
            }
        }
    }

    /// Apply an optimistic mutation: entity update + outbox row in one
    /// SQLite transaction, then the in-memory state, then notify. The
    /// primary and every `also` expression are evaluated against the
    /// pre-mutation entity and applied together; if any expression fails or
    /// the transaction fails nothing changes and observers are not notified.
    #[allow(clippy::too_many_arguments)]
    fn optimistic_mutate(
        &self,
        started: Instant,
        entity_uri: String,
        field: String,
        value_expr: String,
        rollback_value: Value,
        endpoint: String,
        payload: Value,
        also: Vec<FieldUpdate>,
    ) {
        let (snap, value, outbox_id) = {
            let mut guard = lock(&self.state);
            let st = &mut *guard;
            let mut entity = st.entities.get(&entity_uri).cloned().unwrap_or_default();
            let updates: Vec<(&str, &str)> = std::iter::once((field.as_str(), value_expr.as_str()))
                .chain(
                    also.iter()
                        .map(|a| (a.field.as_str(), a.value_expr.as_str())),
                )
                .collect();
            let mut evaluated = Vec::with_capacity(updates.len());
            for ((f, _), (prev, result)) in updates.iter().zip(eval_updates(&entity, &updates)) {
                match result {
                    Ok(v) => evaluated.push((prev, v)),
                    Err(e) => {
                        eprintln!("[ConstructEngine] mutate {entity_uri}#{f}: {e}; no change");
                        return;
                    }
                }
            }
            for ((f, _), (_, v)) in updates.iter().zip(&evaluated) {
                entity.set(f.to_string(), v.clone());
            }
            let mut evaluated = evaluated.into_iter();
            let (prev, value) = evaluated.next().unwrap_or((Value::Null, Value::Null));
            let also_rows: Vec<AlsoUpdate> = also
                .into_iter()
                .zip(evaluated)
                .map(|(a, (prev_value, _))| AlsoUpdate {
                    field: a.field,
                    value_expr: a.value_expr,
                    prev_value,
                })
                .collect();
            let Some(store) = st.store.as_mut() else {
                eprintln!("[ConstructEngine] mutate {entity_uri}#{field}: no store; no change");
                return;
            };
            let now = unix_now();
            let row = NewOutboxRow {
                entity_uri: entity_uri.clone(),
                endpoint,
                payload_json: substitute_value_token(payload, &value).to_string(),
                created_at: now,
                field: field.clone(),
                value_expr,
                prev_value_json: prev.to_string(),
                rollback_value_json: rollback_value.to_string(),
                also: also_rows,
                create: None,
                session_bound: st.session.is_some(),
            };
            let persisted = store.transaction(|tx| {
                tx.upsert_entity(&entity_uri, &entity, now)?;
                tx.insert_outbox(&row)
            });
            let outbox_id = match persisted {
                Ok(id) => id,
                Err(e) => {
                    // Atomic: memory is untouched, the click is a no-op.
                    eprintln!(
                        "[ConstructEngine] ERROR: mutate {entity_uri}#{field}: failed to persist \
                         ({e}); no change"
                    );
                    return;
                }
            };
            let also_desc: String = row
                .also
                .iter()
                .map(|a| {
                    format!(
                        ", {}={}",
                        a.field,
                        entity.get(&a.field).unwrap_or(&Value::Null)
                    )
                })
                .collect();
            st.entities.insert(entity_uri.clone(), entity);
            st.pending_actions = st.pending_actions.saturating_add(1);
            st.seq += 1;
            (st.snapshot(), format!("{value}{also_desc}"), outbox_id)
        };
        let pending = snap.pending_actions;
        self.notify(snap);
        self.signal.wake();
        println!(
            "[ConstructEngine] mutate {entity_uri}#{field} = {value} (outbox #{outbox_id}, \
             {pending} pending); dispatch->notify {} us",
            started.elapsed().as_micros()
        );
    }

    /// `action.mutation.create`: store the temporary entity and an outbox row
    /// in one transaction, list the entity in its query, notify. Invalid
    /// input (an entity that is not an object, empty URIs, an endpoint that
    /// is not a pod path) or a URI already in use is a logged no-op.
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        started: Instant,
        query_key: String,
        entity_uri: String,
        entity: Value,
        endpoint: String,
        payload: Value,
        options: CreateOptions,
    ) {
        let CreateOptions {
            refresh_workspace,
            attachments,
            at_start,
        } = options;
        let entity = match EntityRecord::try_from(entity) {
            Ok(e) => e,
            Err(_) => {
                eprintln!(
                    "[ConstructEngine] create {entity_uri}: entity is not an object; ignored"
                );
                return;
            }
        };
        if query_key.is_empty() || entity_uri.is_empty() || !endpoint.starts_with('/') {
            eprintln!(
                "[ConstructEngine] create: query_key, entity_uri and a pod endpoint are required; \
                 ignored"
            );
            return;
        }
        let (snap, id) = {
            let mut guard = lock(&self.state);
            let st = &mut *guard;
            if st.entities.contains_key(&entity_uri) {
                eprintln!("[ConstructEngine] create {entity_uri}: already exists; ignored");
                return;
            }
            let refs = match attachment_refs(&attachments, &st.attachments) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[ConstructEngine] create {entity_uri}: {e}; ignored");
                    return;
                }
            };
            // `$attachments.<name>` in the placeholder: the local entities.
            let entity = if refs.is_empty() {
                entity
            } else {
                let local = |name: &str| {
                    attachments.contains_key(name).then(|| {
                        Value::Array(
                            refs.iter()
                                .filter(|r| r.name == name)
                                .map(|r| Value::String(attachments::entity_uri(&r.id)))
                                .collect(),
                        )
                    })
                };
                match EntityRecord::try_from(attachments::substitute(
                    serde_json::to_value(&entity).unwrap_or_default(),
                    &local,
                )) {
                    Ok(e) => e,
                    Err(_) => entity,
                }
            };
            let Some(store) = st.store.as_mut() else {
                eprintln!("[ConstructEngine] create {entity_uri}: no store; ignored");
                return;
            };
            let now = unix_now();
            let row = NewOutboxRow {
                entity_uri: entity_uri.clone(),
                endpoint,
                payload_json: payload.to_string(),
                created_at: now,
                field: String::new(),
                value_expr: String::new(),
                prev_value_json: "null".into(),
                rollback_value_json: "null".into(),
                also: Vec::new(),
                create: Some(CreateInfo {
                    query_key: query_key.clone(),
                    refresh_workspace,
                    at_start,
                    attachments: refs,
                }),
                session_bound: st.session.is_some(),
            };
            let persisted = store.transaction(|tx| {
                tx.upsert_entity(&entity_uri, &entity, now)?;
                tx.insert_outbox(&row)
            });
            let id = match persisted {
                Ok(id) => id,
                Err(e) => {
                    eprintln!(
                        "[ConstructEngine] ERROR: create {entity_uri}: failed to persist ({e}); no \
                         change"
                    );
                    return;
                }
            };
            st.entities.insert(entity_uri.clone(), entity);
            st.creates.push(PendingCreate {
                row_id: id,
                query_key: query_key.clone(),
                entity_uri: entity_uri.clone(),
                at_start,
            });
            st.pending_actions = st.pending_actions.saturating_add(1);
            st.seq += 1;
            (st.snapshot(), id)
        };
        let pending = snap.pending_actions;
        self.notify(snap);
        self.signal.wake();
        println!(
            "[ConstructEngine] create {entity_uri} in '{query_key}' (outbox #{id}, {pending} \
             pending); dispatch->notify {} us",
            started.elapsed().as_micros()
        );
    }

    /// Mark `path` loading (and notify). False if it is already loading or
    /// is not a pod path.
    fn begin_load(&self, path: &str) -> bool {
        let valid = path.starts_with('/')
            && !path.starts_with("//")
            && path.len() <= 2048
            && !path.chars().any(char::is_whitespace)
            && same_origin(&self.base_url, &format!("{}{path}", self.base_url));
        let snap = {
            let mut st = lock(&self.state);
            if !valid {
                eprintln!("[ConstructEngine] load {path:?}: not a pod path; ignored");
                st.loads.insert(
                    path.to_owned(),
                    LoadStatus::error("That isn't a valid address."),
                );
            } else if st.loads.get(path).is_some_and(|l| l.state == "loading") {
                return false;
            } else {
                st.loads.insert(path.to_owned(), LoadStatus::loading());
            }
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
        valid
    }

    /// Load a data path (after [`Inner::begin_load`]): GET it with the
    /// session and the schema header, merge the [`DataBundle`]
    /// ([`State::apply_data`]) or record the error, notify. Serialized with
    /// fetches and flushes (`net`). A 401 for the current session signs out
    /// like a fetch 401 (and refetches).
    fn load_data(&self, path: &str) {
        let net = lock(&self.net);
        let token = self.session_token();
        let url = format!("{}{path}", self.base_url);
        let result = fetch_data(&url, token.as_deref(), &self.device_id);
        if self.signal.is_shut_down() {
            return;
        }
        let mut expired = None;
        let snap = {
            let mut st = lock(&self.state);
            match result {
                Ok(_)
                    if token.is_some()
                        && st.session.as_ref().map(|s| &s.token) != token.as_ref() =>
                {
                    println!(
                        "[ConstructEngine] discarded {path} loaded with a session that has since changed"
                    );
                    st.loads.remove(path);
                }
                Ok(bundle) => {
                    st.apply_data(path, bundle);
                    st.is_online = true;
                }
                Err(FetchError::Status(401))
                    if st.session.as_ref().map(|s| &s.token) != token.as_ref() =>
                {
                    // Something else already signed out (or in again): that sign-out forgot load states.
                    st.loads.remove(path);
                }
                Err(FetchError::Status(401)) => {
                    st.loads
                        .insert(path.to_owned(), LoadStatus::error(LOAD_SIGNED_OUT));
                    drop(st);
                    if let Some(t) = &token {
                        expired = self.expire_session(t, "data load");
                    }
                    st = lock(&self.state);
                }
                Err(FetchError::Status(code)) => {
                    eprintln!("[ConstructEngine] load {url} failed: HTTP {code}");
                    let message = match code {
                        404 => LOAD_NOT_FOUND.to_owned(),
                        502 | 504 => "The service behind your pod didn't answer. Please try again."
                            .to_owned(),
                        c if c >= 500 => {
                            format!("Your pod had a problem (HTTP {c}). Please try again.")
                        }
                        c => format!("Your pod answered HTTP {c}."),
                    };
                    st.loads
                        .insert(path.to_owned(), LoadStatus::error(&message));
                }
                Err(FetchError::Other(e)) => {
                    eprintln!("[ConstructEngine] load {url} failed: {e}");
                    st.loads
                        .insert(path.to_owned(), LoadStatus::error(LOAD_OFFLINE));
                }
            }
            st.seq += 1;
            st.snapshot()
        };
        drop(net);
        let signed_out = expired.is_some();
        self.notify(snap);
        if signed_out && !self.signal.is_shut_down() {
            self.refresh();
        }
    }

    /// Run the work queued by accepted create rows (sync worker, no locks
    /// held).
    fn run_followups(&self) {
        let followups = std::mem::take(&mut lock(&self.state).followups);
        for f in followups {
            if self.signal.is_shut_down() {
                return;
            }
            match f {
                Followup::Load(path) => {
                    if self.begin_load(&path) {
                        self.load_data(&path);
                    }
                }
                Followup::Refresh => {
                    self.refresh();
                }
            }
        }
    }

    /// The pod said our data changed (`event: data`, D50): refetch the
    /// workspace, then every data path loaded so far.
    fn reload_all(&self) {
        self.refresh();
        let paths: Vec<String> = lock(&self.state).data.keys().cloned().collect();
        for path in paths {
            if self.signal.is_shut_down() {
                return;
            }
            if self.begin_load(&path) {
                self.load_data(&path);
            }
        }
    }

    /// Set `auth_json` (bump seq) and notify.
    fn set_auth_json(&self, auth_json: String) {
        let snap = {
            let mut st = lock(&self.state);
            st.auth_json = auth_json;
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
    }

    /// Replace the current sign-in flow (cancelling the old one) with `new`.
    fn replace_flow(&self, new: Option<Arc<Flow>>) {
        let old = std::mem::replace(&mut *lock(&self.flow), new);
        if let Some(old) = old {
            old.cancelled.store(true, Ordering::SeqCst);
        }
    }

    /// Whether `flow` is still the current, uncancelled sign-in.
    fn is_current(&self, flow: &Arc<Flow>) -> bool {
        !flow.is_cancelled()
            && !self.signal.is_shut_down()
            && lock(&self.flow)
                .as_ref()
                .is_some_and(|f| Arc::ptr_eq(f, flow))
    }

    /// `ActionIntent::OpenSession` (D46): POST `payload` to the pod path
    /// `path` on an auth thread; the answer's `session_token` becomes the
    /// session (no account yet) and the workspace is fetched again.
    fn open_session(self: &Arc<Self>, path: String, payload: Value) {
        let valid = path.starts_with('/')
            && !path.starts_with("//")
            && same_origin(&self.base_url, &format!("{}{path}", self.base_url));
        if !valid {
            eprintln!("[ConstructEngine] open session {path:?}: not a pod path; ignored");
            return;
        }
        if lock(&self.flow)
            .as_ref()
            .is_some_and(|f| f.exchanging.load(Ordering::SeqCst))
        {
            println!("[ConstructEngine] open session ignored: a sign-in is being completed");
            return;
        }
        let flow = Arc::new(Flow {
            cancelled: AtomicBool::new(false),
            state: auth::random_state(),
            redirect_tx: None,
            exchanging: AtomicBool::new(true),
        });
        self.replace_flow(Some(Arc::clone(&flow)));
        self.set_auth_json(pending_auth_json());
        let url = format!("{}{path}", self.base_url);
        let device = self.device_id.clone();
        self.spawn_auth_thread(flow, false, move |weak, flow| {
            let client = auth_client(&device).map_err(|e| failed(auth::SIGN_IN_FAILED, e))?;
            let answer = post_auth(&client, &url, None, &payload)?;
            let Some(token) = json_str(&answer, "session_token") else {
                return Err(failed(
                    auth::SIGN_IN_FAILED,
                    format!("POST {url}: no session_token"),
                ));
            };
            println!("[ConstructEngine] session opened via {path}");
            let inner = weak.upgrade().ok_or(LoginError::Cancelled)?;
            inner.complete_sign_in(
                flow,
                Session {
                    token,
                    username: None,
                    display_name: None,
                    avatar_url: None,
                },
            )
        });
    }

    /// See [`ConstructEngine::forget_session`].
    fn forget_session(self: &Arc<Self>) {
        self.replace_flow(None);
        lock(&self.device).registered_for = None;
        let (snap, token) = {
            let mut st = lock(&self.state);
            let token = st.session.take().map(|s| s.token);
            st.auth_json = SIGNED_OUT_AUTH_JSON.to_string();
            st.auth_blocked = false;
            st.purge_private(&self.workspace_id, true);
            st.seq += 1;
            (st.snapshot(), token)
        };
        println!("[ConstructEngine] session forgotten on this device");
        self.notify(snap);
        let inner = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("construct-engine-forget".into())
            .spawn(move || {
                inner.sync_session_store();
                if let Some(token) = token {
                    // The session stays valid on the pod: take this device
                    // off it so no notifications are pushed here.
                    notify::delete_device(&inner.base_url, &inner.device_id, &token);
                }
                inner.drop_stale_pending();
                if !inner.signal.is_shut_down() {
                    inner.refresh();
                }
            });
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn the forget thread: {e}");
            self.sync_session_store();
        }
    }

    /// `ActionIntent::AuthLogin`: go `pending` and run the flow on a
    /// background thread (the dispatching thread never blocks). The pod's
    /// sign-in endpoints come from the intent (`app_path`, `token_path`), so
    /// the engine knows no app (docs/DECISIONS.md D36).
    fn start_login(self: &Arc<Self>, instance_url: String, app_path: String, token_path: String) {
        let valid = |p: &str| {
            p.starts_with('/')
                && !p.starts_with("//")
                && same_origin(&self.base_url, &format!("{}{p}", self.base_url))
        };
        if !valid(&app_path) || !valid(&token_path) {
            eprintln!(
                "[ConstructEngine] sign-in ignored: the button has no pod sign-in paths \
                 (app_path {app_path:?}, token_path {token_path:?})"
            );
            self.set_auth_json(error_auth_json(SIGN_IN_BUTTON_OUTDATED));
            return;
        }
        if lock(&self.flow)
            .as_ref()
            .is_some_and(|f| f.exchanging.load(Ordering::SeqCst))
        {
            println!(
                "[ConstructEngine] sign-in ignored: a sign-in is already being completed (code \
                 exchange or first feed fetch)"
            );
            return;
        }
        let (redirect_tx, redirect_rx) = match &self.redirect {
            RedirectMode::Custom(_) => {
                let (tx, rx) = mpsc::channel();
                (Some(Mutex::new(tx)), Some(rx))
            }
            RedirectMode::Loopback => (None, None),
        };
        let flow = Arc::new(Flow {
            cancelled: AtomicBool::new(false),
            state: auth::random_state(),
            redirect_tx,
            exchanging: AtomicBool::new(false),
        });
        self.replace_flow(Some(Arc::clone(&flow)));
        self.set_auth_json(pending_auth_json());
        println!("[ConstructEngine] sign-in started for {instance_url}");
        let target = SignInTarget {
            instance_url,
            app_path,
            token_path,
        };
        self.spawn_auth_thread(flow, true, move |weak, flow| {
            login_flow(weak, flow, &target, redirect_rx)
        });
    }

    /// Run `body` (a sign-in) on a `construct-engine-auth` thread and report
    /// its failure in `auth_json` if `flow` is still current. `cleanup`:
    /// afterwards delete the persisted pending sign-in unless it belongs to
    /// the (then) current flow; skipped when the engine was dropped, so a
    /// flow cut short by the app going away can be completed by the next
    /// engine.
    fn spawn_auth_thread(
        self: &Arc<Self>,
        flow: Arc<Flow>,
        cleanup: bool,
        body: impl FnOnce(&Weak<Inner>, &Arc<Flow>) -> Result<(), LoginError> + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        let thread_flow = Arc::clone(&flow);
        let spawned = thread::Builder::new()
            .name("construct-engine-auth".into())
            .spawn(move || {
                let flow = thread_flow;
                let result = body(&weak, &flow);
                let Some(inner) = weak.upgrade() else { return };
                match result {
                    Ok(()) => {}
                    Err(LoginError::Cancelled) => {
                        println!("[ConstructEngine] sign-in cancelled");
                    }
                    Err(LoginError::Failed { message, detail }) => {
                        if inner.is_current(&flow) {
                            eprintln!(
                                "[ConstructEngine] sign-in failed: {detail} (shown: {message})"
                            );
                            inner.replace_flow(None);
                            inner.set_auth_json(error_auth_json(&message));
                        }
                    }
                    Err(LoginError::Unmatched) => {
                        if inner.is_current(&flow) {
                            inner.replace_flow(None);
                            if lock(&inner.state).session.is_some() {
                                // E.g. a replayed redirect: nothing to report.
                                eprintln!(
                                    "[ConstructEngine] auth redirect matches no sign-in; ignored \
                                     (signed in)"
                                );
                            } else {
                                eprintln!(
                                    "[ConstructEngine] auth redirect matches no pending sign-in \
                                     (interrupted or expired)"
                                );
                                inner.set_auth_json(error_auth_json(SIGN_IN_INTERRUPTED));
                            }
                        }
                    }
                }
                if cleanup && !inner.signal.is_shut_down() {
                    inner.drop_stale_pending();
                }
            });
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn the sign-in thread: {e}");
            self.replace_flow(None);
            self.set_auth_json(error_auth_json(auth::SIGN_IN_FAILED));
        }
    }

    /// Persist `pending` for `flow` (custom-scheme mode) if it is still the
    /// current sign-in. Auth thread only.
    fn persist_pending(&self, flow: &Arc<Flow>, pending: &PendingAuth) {
        let _io = lock(&self.pending_io);
        if self.is_current(flow) {
            let raw = pending.to_json();
            guarded("SecureStore.set", || {
                self.secure_store.set(&self.pending_key, &raw)
            });
        }
    }

    /// Delete the persisted pending sign-in unless it belongs to the current
    /// in-memory flow (custom-scheme mode only). Background threads only.
    fn drop_stale_pending(&self) {
        if !matches!(self.redirect, RedirectMode::Custom(_)) {
            return;
        }
        let _io = lock(&self.pending_io);
        let Some(raw) = guarded("SecureStore.get", || {
            self.secure_store.get(&self.pending_key)
        })
        .flatten() else {
            return;
        };
        let current = lock(&self.flow).as_ref().map(|f| f.state.clone());
        let ours = PendingAuth::parse(&raw).is_some_and(|p| Some(&p.state) == current.as_ref());
        if !ours {
            guarded("SecureStore.delete", || {
                self.secure_store.delete(&self.pending_key)
            });
        }
    }

    /// Delete the persisted pending sign-in if it is the one for `state`.
    fn drop_stale_pending_of(&self, state: &str) {
        let _io = lock(&self.pending_io);
        let raw = guarded("SecureStore.get", || {
            self.secure_store.get(&self.pending_key)
        })
        .flatten();
        if raw
            .as_deref()
            .and_then(PendingAuth::parse)
            .is_some_and(|p| p.state == state)
        {
            guarded("SecureStore.delete", || {
                self.secure_store.delete(&self.pending_key)
            });
        }
    }

    /// Load and delete the persisted pending sign-in if its state is
    /// `state` and it has not expired (an expired one is deleted too).
    fn take_pending(&self, state: &str) -> Option<PendingAuth> {
        let _io = lock(&self.pending_io);
        let raw = guarded("SecureStore.get", || {
            self.secure_store.get(&self.pending_key)
        })
        .flatten()?;
        let delete = || {
            guarded("SecureStore.delete", || {
                self.secure_store.delete(&self.pending_key)
            })
        };
        let Some(pending) = PendingAuth::parse(&raw) else {
            delete();
            return None;
        };
        if pending.is_expired(unix_now()) {
            println!("[ConstructEngine] the persisted sign-in has expired; deleted");
            delete();
            return None;
        }
        if pending.state != state {
            return None;
        }
        delete();
        Some(pending)
    }

    /// See [`ConstructEngine::handle_auth_redirect`].
    fn handle_auth_redirect(self: &Arc<Self>, url: &str) -> bool {
        let Some(redirect) = auth::parse_redirect(url) else {
            eprintln!("[ConstructEngine] auth redirect without code/error ignored");
            return false;
        };
        if !matches!(self.redirect, RedirectMode::Custom(_)) {
            eprintln!(
                "[ConstructEngine] auth redirect ignored: this engine uses a loopback redirect"
            );
            return false;
        }
        let recovery = {
            let mut current = lock(&self.flow);
            match current.as_ref() {
                Some(flow) => {
                    let Some(tx) = &flow.redirect_tx else {
                        eprintln!(
                            "[ConstructEngine] auth redirect ignored: a sign-in is being completed"
                        );
                        return false;
                    };
                    if redirect.state() != Some(flow.state.as_str()) {
                        eprintln!(
                            "[ConstructEngine] auth redirect with a mismatched state ignored"
                        );
                        return false;
                    }
                    return lock(tx).send(redirect).is_ok();
                }
                None => {
                    // No flow in memory (e.g. a new process): complete the
                    // persisted one in the background. Installed as the
                    // current flow so AuthLogin / logout cancel it.
                    let flow = Arc::new(Flow {
                        cancelled: AtomicBool::new(false),
                        state: redirect.state().unwrap_or_default().to_owned(),
                        redirect_tx: None,
                        // The redirect is here: a tap must not restart.
                        exchanging: AtomicBool::new(true),
                    });
                    *current = Some(Arc::clone(&flow));
                    flow
                }
            }
        };
        println!(
            "[ConstructEngine] auth redirect with no sign-in in memory; checking the persisted one"
        );
        self.spawn_auth_thread(recovery, false, move |weak, flow| {
            recover_sign_in(weak, flow, redirect)
        });
        true
    }

    /// Install `session` (auth still `pending`, "Loading your feed…"),
    /// store it, wake the outbox and refetch (on the calling auth thread);
    /// once that fetch is done (fetched or failed) go `signed_in`. The flow
    /// stays current until then, so a new `AuthLogin` is ignored and a
    /// `logout` cancels it.
    fn complete_sign_in(&self, flow: &Arc<Flow>, session: Session) -> Result<(), LoginError> {
        let username = session.username.clone();
        let token = session.token.clone();
        let snap = {
            // Held across the check and the update (lock order: flow, then
            // state) so a concurrent `logout` either cancels this flow
            // first or runs after the session is in place and removes it
            // again. The secure store is written afterwards, without this
            // lock (`sync_session_store` orders it against logout's delete).
            let current = lock(&self.flow);
            if flow.is_cancelled()
                || self.signal.is_shut_down()
                || !current.as_ref().is_some_and(|f| Arc::ptr_eq(f, flow))
            {
                return Err(LoginError::Cancelled);
            }
            let mut st = lock(&self.state);
            st.auth_json = loading_feed_auth_json();
            st.session = Some(session);
            st.auth_blocked = false;
            st.seq += 1;
            st.snapshot()
        };
        println!(
            "[ConstructEngine] signed in as {}; fetching the feed",
            username.as_deref().unwrap_or("?")
        );
        self.notify(snap);
        self.sync_session_store();
        self.signal.wake();
        self.refresh();
        // Signed in now unless a logout / 401 / newer flow got there first.
        let snap = {
            let mut current = lock(&self.flow);
            if !current.as_ref().is_some_and(|f| Arc::ptr_eq(f, flow)) {
                return Ok(());
            }
            *current = None;
            if flow.is_cancelled() || self.signal.is_shut_down() {
                return Ok(());
            }
            let mut st = lock(&self.state);
            match &st.session {
                Some(s) if s.token == token => {
                    st.auth_json = s.auth_json();
                    st.seq += 1;
                    st.snapshot()
                }
                _ => return Ok(()),
            }
        };
        self.notify(snap);
        // Failures are logged; the events stream's next connect retries.
        let _ = self.ensure_device_registered(false);
        Ok(())
    }

    /// See [`ConstructEngine::logout`].
    fn logout(self: &Arc<Self>) {
        self.replace_flow(None);
        lock(&self.device).registered_for = None;
        let (snap, token) = {
            let mut st = lock(&self.state);
            let token = st.session.take().map(|s| s.token);
            st.auth_json = SIGNED_OUT_AUTH_JSON.to_string();
            st.auth_blocked = false;
            st.purge_private(&self.workspace_id, true);
            st.seq += 1;
            (st.snapshot(), token)
        };
        println!("[ConstructEngine] signed out");
        self.notify(snap);
        let weak = Arc::downgrade(self);
        // Strong until the store is synced, so that dropping the engine
        // right after `logout()` cannot leave the session in the store.
        let strong = Arc::clone(self);
        let url = format!("{}/v1/auth/logout", self.base_url);
        let (base_url, device) = (self.base_url.clone(), self.device_id.clone());
        let spawned = thread::Builder::new()
            .name("construct-engine-logout".into())
            .spawn(move || {
                // Secure-store I/O off the calling (UI) thread.
                strong.sync_session_store();
                strong.drop_stale_pending();
                drop(strong);
                let mut kept = None;
                if let Some(token) = token {
                    // While the session is still valid on the pod: no more
                    // notifications for this device.
                    notify::delete_device(&base_url, &device, &token);
                    match auth_client(&device).and_then(|c| {
                        c.post(&url)
                            .bearer_auth(&token)
                            .send()
                            .map_err(|e| e.to_string())
                    }) {
                        Ok(r) => {
                            println!("[ConstructEngine] POST {url} -> {}", r.status().as_u16());
                            // A pod token's session stays (D46): keep it.
                            let body: Value = r.json().unwrap_or(Value::Null);
                            if body["session_kept"] == true {
                                kept = Some(token);
                            }
                        }
                        Err(e) => eprintln!("[ConstructEngine] POST {url} failed (ignored): {e}"),
                    }
                }
                if let Some(inner) = weak.upgrade()
                    && !inner.signal.is_shut_down()
                {
                    if let Some(token) = kept {
                        inner.restore_session(token);
                    }
                    inner.refresh();
                }
            });
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] failed to spawn the logout thread: {e}");
            // Keep the store consistent even then (blocking this once).
            self.sync_session_store();
            self.drop_stale_pending();
        }
    }

    /// Deliver a snapshot to all observers, with no locks held during the
    /// calls. Each observer skips snapshots older than one it already saw.
    fn notify(&self, snap: StateSnapshot) {
        let observers: Vec<Arc<Observer>> = lock(&self.observers).clone();
        for obs in observers {
            obs.deliver(snap.clone());
        }
    }
}

enum FetchError {
    /// The server responded with this non-success HTTP status code.
    Status(u16),
    /// Transport failure or unusable response body.
    Other(String),
}

/// `GET url`, with `Authorization: Bearer <token>` when signed in. Returns
/// the bundle and its [`LAYOUT_VERSION_HEADER`] (`None` when absent, e.g.
/// the unversioned signed-out `pixelfed_home` screen).
fn fetch(
    url: &str,
    token: Option<&str>,
    device: &str,
) -> Result<(WorkspaceSpec, Option<u64>), FetchError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| FetchError::Other(format!("client build: {e}")))?;
    let mut req = client
        .get(url)
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header(protocol::DEVICE_HEADER, device);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().map_err(|e| FetchError::Other(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    let version = resp
        .headers()
        .get(LAYOUT_VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let raw = resp.text().map_err(|e| FetchError::Other(e.to_string()))?;
    let spec =
        serde_json::from_str(&raw).map_err(|e| FetchError::Other(format!("bad bundle: {e}")))?;
    Ok((spec, version))
}

/// `GET url` for a data path, with the session and the schema header.
fn fetch_data(url: &str, token: Option<&str>, device: &str) -> Result<DataBundle, FetchError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(DATA_LOAD_TIMEOUT)
        .build()
        .map_err(|e| FetchError::Other(format!("client build: {e}")))?;
    let mut req = client
        .get(url)
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header(protocol::DEVICE_HEADER, device);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().map_err(|e| FetchError::Other(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    let raw = resp.text().map_err(|e| FetchError::Other(e.to_string()))?;
    serde_json::from_str(&raw).map_err(|e| FetchError::Other(format!("bad data bundle: {e}")))
}

/// HTTP client for the sign-in calls to the pod (they carry the device
/// header like every other pod call).
fn auth_client(device: &str) -> Result<reqwest::blocking::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(device) {
        headers.insert(protocol::DEVICE_HEADER, v);
    }
    reqwest::blocking::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(5))
        .timeout(AUTH_HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))
}

/// A sign-in failure: `message` for `auth_json`, `detail` for the log.
fn failed(message: &str, detail: impl Into<String>) -> LoginError {
    LoginError::Failed {
        message: message.to_owned(),
        detail: detail.into(),
    }
}

/// `POST url` with a JSON body; the parsed JSON response on 2xx, else a
/// failure whose message is mapped from the pod's `error` code
/// ([`auth::sign_in_error_message`]) and whose detail (logged only) has the
/// URL, status and error body.
fn post_auth(
    client: &reqwest::blocking::Client,
    url: &str,
    bearer: Option<&str>,
    body: &Value,
) -> Result<Value, LoginError> {
    let mut req = client.post(url).json(body);
    // The current session (a pod token, D46) goes along: a pod host routes
    // the sign-in to its pod, and the pod adds the sign-in to that session.
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().map_err(|e| {
        failed(
            auth::sign_in_error_message(None, None),
            format!("POST {url}: cannot reach the pod: {e}"),
        )
    })?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        let code = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned));
        let body: String = text.chars().take(300).collect();
        return Err(failed(
            auth::sign_in_error_message(Some(status.as_u16()), code.as_deref()),
            format!("POST {url} -> HTTP {}: {body}", status.as_u16()),
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        failed(
            auth::SIGN_IN_FAILED,
            format!("POST {url}: bad response: {e}"),
        )
    })
}

fn json_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The sign-in flow (runs on the auth thread, holding only a `Weak`
/// between steps): register the app with the pod, build the PKCE authorize
/// URL, open it, wait for the redirect, verify `state`, exchange the code
/// through the pod and store the session.
/// Where a sign-in goes: the instance and the pod's endpoints for it.
struct SignInTarget {
    instance_url: String,
    app_path: String,
    token_path: String,
}

fn login_flow(
    weak: &Weak<Inner>,
    flow: &Arc<Flow>,
    target: &SignInTarget,
    redirect_rx: Option<mpsc::Receiver<Redirect>>,
) -> Result<(), LoginError> {
    let instance_url = target.instance_url.as_str();
    let (base_url, opener, redirect_mode, loopback_port, signal, device) = {
        let inner = weak.upgrade().ok_or(LoginError::Cancelled)?;
        (
            inner.base_url.clone(),
            inner.url_opener.clone(),
            inner.redirect.clone(),
            inner.loopback_port,
            Arc::clone(&inner.signal),
            inner.device_id.clone(),
        )
    };
    let cancelled = || flow.is_cancelled() || signal.is_shut_down();

    let listener = match redirect_mode {
        RedirectMode::Loopback => Some(LoopbackListener::bind_preferred(loopback_port).map_err(
            |e| {
                failed(
                    auth::SIGN_IN_FAILED,
                    format!("cannot open the loopback redirect listener: {e}"),
                )
            },
        )?),
        RedirectMode::Custom(_) => None,
    };
    let redirect_uri = match (&listener, &redirect_mode) {
        (Some(l), _) => l.redirect_uri(),
        (None, RedirectMode::Custom(uri)) => uri.clone(),
        (None, RedirectMode::Loopback) => unreachable!("loopback always has a listener"),
    };

    let client = auth_client(&device).map_err(|e| failed(auth::SIGN_IN_FAILED, e))?;
    let bearer = weak.upgrade().and_then(|i| i.session_token());
    let app = post_auth(
        &client,
        &format!("{base_url}{}", target.app_path),
        bearer.as_deref(),
        &json!({ "instance_url": instance_url, "redirect_uri": redirect_uri }),
    )?;
    let (Some(client_id), Some(endpoint)) = (
        json_str(&app, "client_id"),
        json_str(&app, "authorize_endpoint"),
    ) else {
        return Err(failed(
            auth::SIGN_IN_FAILED,
            format!("unexpected {} response: {app}", target.app_path),
        ));
    };
    let scope = json_str(&app, "scopes").unwrap_or_else(|| "read write".to_owned());
    let pkce = Pkce::generate();
    let url = auth::authorize_url(
        &endpoint,
        &client_id,
        &redirect_uri,
        &scope,
        &flow.state,
        &pkce.challenge,
    );
    if cancelled() {
        return Err(LoginError::Cancelled);
    }
    if listener.is_none() {
        // Custom scheme: the redirect may reach a new process (the app was
        // killed while the browser was in front), so keep what it needs.
        let inner = weak.upgrade().ok_or(LoginError::Cancelled)?;
        inner.persist_pending(
            flow,
            &PendingAuth {
                state: flow.state.clone(),
                code_verifier: pkce.verifier.clone(),
                redirect_uri: redirect_uri.clone(),
                instance_url: instance_url.to_owned(),
                token_path: target.token_path.clone(),
                created_at: unix_now(),
            },
        );
    }

    let opened = opener
        .as_ref()
        .and_then(|o| guarded("UrlOpener.open", || o.open(&url)))
        .unwrap_or(false);
    if opened {
        println!(
            "[ConstructEngine] opened the authorize page; waiting for the redirect to {redirect_uri}"
        );
    } else {
        // Keep waiting: a human can open the logged URL by hand.
        eprintln!(
            "[ConstructEngine] no browser available; to sign in, open this URL (waiting {} s for \
             the redirect to {redirect_uri}):\n{url}",
            auth::REDIRECT_TIMEOUT.as_secs()
        );
        if let Some(inner) = weak.upgrade()
            && inner.is_current(flow)
        {
            inner.set_auth_json(error_auth_json("no browser available"));
        }
    }

    let redirect = match (&listener, redirect_rx) {
        (Some(l), _) => l
            .wait(auth::REDIRECT_TIMEOUT, &cancelled, &|r| {
                let ok = r.state() == Some(flow.state.as_str());
                if !ok {
                    eprintln!(
                        "[ConstructEngine] loopback redirect with a mismatched state ignored"
                    );
                }
                ok
            })
            .map_err(|e| match e {
                auth::WaitError::Cancelled => LoginError::Cancelled,
                other @ auth::WaitError::TimedOut => {
                    failed(auth::SIGN_IN_TIMED_OUT, other.to_string())
                }
                other => failed(auth::SIGN_IN_FAILED, other.to_string()),
            })?,
        (None, Some(rx)) => {
            let deadline = Instant::now() + auth::REDIRECT_TIMEOUT;
            loop {
                if cancelled() {
                    return Err(LoginError::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(failed(
                        auth::SIGN_IN_TIMED_OUT,
                        auth::WaitError::TimedOut.to_string(),
                    ));
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(r) => break r,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(LoginError::Cancelled);
                    }
                }
            }
        }
        (None, None) => return Err(failed(auth::SIGN_IN_FAILED, "no redirect channel")),
    };
    if cancelled() {
        return Err(LoginError::Cancelled);
    }
    if listener.is_none()
        && let Some(inner) = weak.upgrade()
    {
        // The redirect arrived: the persisted copy is no longer needed.
        inner.drop_stale_pending_of(&flow.state);
    }
    finish_sign_in(
        weak,
        flow,
        &client,
        &base_url,
        redirect,
        instance_url,
        &target.token_path,
        &pkce.verifier,
        &redirect_uri,
    )
}

/// The common tail of a sign-in once its redirect is in: an `error=`
/// redirect fails, a code is exchanged through the pod for a session, which
/// is then stored.
#[allow(clippy::too_many_arguments)]
fn finish_sign_in(
    weak: &Weak<Inner>,
    flow: &Arc<Flow>,
    client: &reqwest::blocking::Client,
    base_url: &str,
    redirect: Redirect,
    instance_url: &str,
    token_path: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<(), LoginError> {
    let code = match redirect {
        Redirect::Code { code, .. } => code,
        Redirect::Error {
            error, description, ..
        } => {
            let more = description.map(|d| format!(" ({d})")).unwrap_or_default();
            let message = if error == "access_denied" {
                auth::SIGN_IN_DENIED
            } else {
                auth::SIGN_IN_REJECTED
            };
            return Err(failed(
                message,
                format!("authorization failed: {error}{more}"),
            ));
        }
    };
    flow.exchanging.store(true, Ordering::SeqCst);
    if let Some(inner) = weak.upgrade()
        && inner.is_current(flow)
    {
        inner.set_auth_json(pending_auth_json());
    }

    let bearer = weak.upgrade().and_then(|i| i.session_token());
    let token = post_auth(
        client,
        &format!("{base_url}{token_path}"),
        bearer.as_deref(),
        &json!({
            "instance_url": instance_url,
            "code": code,
            "code_verifier": code_verifier,
            "redirect_uri": redirect_uri,
        }),
    )?;
    let Some(session_token) = json_str(&token, "session_token") else {
        return Err(failed(
            auth::SIGN_IN_FAILED,
            "the pod returned no session_token",
        ));
    };
    let account = token.get("account").cloned().unwrap_or(Value::Null);
    let session = Session {
        token: session_token,
        username: json_str(&account, "username"),
        display_name: json_str(&account, "display_name"),
        avatar_url: json_str(&account, "avatar_url"),
    };
    let inner = weak.upgrade().ok_or(LoginError::Cancelled)?;
    inner.complete_sign_in(flow, session)
}

/// Complete a custom-scheme sign-in whose redirect reached an engine with
/// no flow in memory, from the copy persisted by `login_flow` (state must
/// match, not expired; it is deleted either way).
fn recover_sign_in(
    weak: &Weak<Inner>,
    flow: &Arc<Flow>,
    redirect: Redirect,
) -> Result<(), LoginError> {
    let (pending, base_url, device) = {
        let inner = weak.upgrade().ok_or(LoginError::Cancelled)?;
        let pending = redirect
            .state()
            .and_then(|state| inner.take_pending(state))
            .ok_or(LoginError::Unmatched)?;
        (pending, inner.base_url.clone(), inner.device_id.clone())
    };
    if flow.is_cancelled() {
        return Err(LoginError::Cancelled);
    }
    println!(
        "[ConstructEngine] completing the sign-in for {} started before a restart",
        pending.instance_url
    );
    if !pending.token_path.starts_with('/') {
        return Err(failed(
            SIGN_IN_BUTTON_OUTDATED,
            "the persisted sign-in has no token path (started by an older build)",
        ));
    }
    let client = auth_client(&device).map_err(|e| failed(auth::SIGN_IN_FAILED, e))?;
    finish_sign_in(
        weak,
        flow,
        &client,
        &base_url,
        redirect,
        &pending.instance_url,
        &pending.token_path,
        &pending.code_verifier,
        &pending.redirect_uri,
    )
}

/// The sync worker thread: runs flush passes until the engine is dropped.
/// Holds only a `Weak<Inner>` while waiting.
fn sync_worker(engine: Weak<Inner>, signal: Arc<WorkerSignal>) {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(FLUSH_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ConstructEngine] sync worker: cannot build HTTP client: {e}; outbox disabled"
            );
            return;
        }
    };
    loop {
        let wait = {
            let Some(inner) = engine.upgrade() else {
                return;
            };
            inner.flush_pass(&client)
        };
        if !signal.wait(wait) {
            return;
        }
    }
}

/// Whether `url` has the same scheme, host and port as `base`.
/// Whether `base_url` is `http://` to a host other than this machine (or
/// the Android emulator's alias for it, 10.0.2.2): the bearer token would
/// cross a network in cleartext. Only warned about, never refused.
pub fn is_cleartext_remote(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.octets() == [10, 0, 2, 2]),
        Ok(std::net::IpAddr::V6(ip)) => !ip.is_loopback(),
        Err(_) => !host.eq_ignore_ascii_case("localhost"),
    }
}

fn same_origin(base: &str, url: &str) -> bool {
    match (reqwest::Url::parse(base), reqwest::Url::parse(url)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin() && a.origin().is_tuple(),
        _ => false,
    }
}

/// `POST url` with a JSON body (and the bearer when signed in),
/// classified for the outbox.
fn post_json(
    client: &reqwest::blocking::Client,
    url: &str,
    body: &str,
    token: Option<&str>,
    device: Option<&str>,
) -> FlushResult {
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string());
    if let Some(d) = device {
        req = req.header(protocol::DEVICE_HEADER, d);
    }
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    match req.send() {
        Ok(r) => classify(r).0,
        Err(e) => FlushResult::Network(e.to_string()),
    }
}

/// Classify an outbox response; a 2xx also returns its body.
fn classify(resp: reqwest::blocking::Response) -> (FlushResult, String) {
    let code = resp.status().as_u16();
    if resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return (FlushResult::Accepted(code), body);
    }
    (classify_error(code, resp), String::new())
}

fn classify_error(code: u16, resp: reqwest::blocking::Response) -> FlushResult {
    if code == 401 {
        return FlushResult::Unauthorized;
    }
    let body = resp.text().unwrap_or_default();
    // 408 (request timeout) and 429 (too many requests) are transient:
    // retried with backoff like 5xx, never rolled back.
    if (400..500).contains(&code) && code != 408 && code != 429 {
        FlushResult::Rejected(code, body)
    } else {
        FlushResult::ServerError(code, body)
    }
}

/// `POST {base}/v1/uploads` with one staged file (D49). The attachment id is
/// the idempotency key, so a retried upload gets the same upload id.
fn upload_file(
    client: &reqwest::blocking::Client,
    url: &str,
    a: &Attachment,
    token: Option<&str>,
) -> Result<String, FlushResult> {
    let bytes = match fs::read(&a.path) {
        Ok(b) => b,
        // The file is gone (storage cleared): this create can never be sent.
        Err(e) => return Err(FlushResult::Rejected(410, format!("attachment file: {e}"))),
    };
    let mut req = client
        .post(url)
        .timeout(UPLOAD_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, a.mime.as_str())
        .header("Idempotency-Key", a.id.as_str())
        .body(bytes);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .map_err(|e| FlushResult::Network(e.to_string()))?;
    match classify(resp) {
        (FlushResult::Accepted(code), body) => serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("upload_id")?.as_str().map(str::to_owned))
            .ok_or(FlushResult::ServerError(
                code,
                format!("upload answer without an id: {body}"),
            )),
        (other, _) => Err(other),
    }
}

/// True if the server sent any HTTP response to `GET url`.
fn reachable(url: &str) -> bool {
    reqwest::blocking::Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
        .and_then(|c| c.get(url).send())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cleartext_remote_base_urls_are_flagged() {
        for url in [
            "http://example.com",
            "http://192.168.1.5:8080",
            "http://[2001:db8::1]:8080",
            "http://pod.local",
        ] {
            assert!(is_cleartext_remote(url), "{url}");
        }
        for url in [
            "https://example.com",
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:8080",
            "http://10.0.2.2:8080",
            "not a url",
        ] {
            assert!(!is_cleartext_remote(url), "{url}");
        }
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let c = EngineConfig::default();
        let secs: Vec<u64> = (1..=8).map(|n| c.backoff(n).as_secs()).collect();
        assert_eq!(secs, vec![2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(c.backoff(10_000), Duration::from_secs(60));
        let fast = EngineConfig {
            backoff_base: Duration::from_millis(10),
            backoff_cap: Duration::from_millis(80),
            ..EngineConfig::default()
        };
        assert_eq!(fast.backoff(2), Duration::from_millis(40));
        assert_eq!(fast.backoff(4), Duration::from_millis(80));
    }

    #[test]
    fn value_token_substituted_everywhere() {
        let payload = json!({
            "liked": "$value",
            "nested": {"a": ["$value", "x", {"b": "$value"}]},
            "not_exact": "$value ",
            "n": 3
        });
        let out = substitute_value_token(payload, &json!(true));
        assert_eq!(
            out,
            json!({
                "liked": true,
                "nested": {"a": [true, "x", {"b": true}]},
                "not_exact": "$value ",
                "n": 3
            })
        );
        assert_eq!(substitute_value_token(json!("$value"), &json!(5)), json!(5));
    }

    #[test]
    fn bearer_only_goes_to_the_pod_origin() {
        let base = "http://pod.example:8080";
        assert!(same_origin(base, &format!("{base}/v1/pixelfed/posts/like")));
        for endpoint in ["@evil.example/x", ".evil.example/x", "0/x", ":9/x"] {
            assert!(
                !same_origin(base, &format!("{base}{endpoint}")),
                "{endpoint}"
            );
        }
        assert!(!same_origin(
            "http://pod.example",
            "http://pod.example.evil.example/x"
        ));
        assert!(!same_origin("not a url", "not a url/x"));
    }

    fn session(token: &str) -> Session {
        Session {
            token: token.into(),
            username: None,
            display_name: None,
            avatar_url: None,
        }
    }

    fn outbox_row() -> OutboxRow {
        OutboxRow {
            id: 1,
            entity_uri: "pixelfed://posts/1".into(),
            endpoint: "/x".into(),
            payload_json: "{}".into(),
            created_at: 0,
            retry_count: 0,
            status: store::STATUS_PENDING.into(),
            field: None,
            value_expr: None,
            prev_value_json: None,
            rollback_value_json: None,
            next_attempt_at: 0,
            last_error: None,
            also: Vec::new(),
            create: None,
        }
    }

    #[test]
    fn outbox_401_after_a_concurrent_sign_in_does_not_pause_flushing() {
        let engine = ConstructEngine::init_with_config(
            "",
            "http://127.0.0.1:1",
            "home",
            EngineConfig::default(),
        );
        let inner = &engine.inner;
        let row = outbox_row();
        // A sign-in completed while a POST with the old (or no) token was in flight.
        lock(&inner.state).session = Some(session("new"));
        for sent in [Some("old"), None] {
            let (snap, blocked) = inner.outbox_unauthorized(&row, "u", sent, sent);
            assert!(snap.is_none() && !blocked, "{sent:?}");
            let st = lock(&inner.state);
            assert!(!st.auth_blocked);
            assert_eq!(st.session.as_ref().map(|s| s.token.as_str()), Some("new"));
        }
        // The current session itself was rejected: forget it and pause.
        let (snap, blocked) = inner.outbox_unauthorized(&row, "u", Some("new"), Some("new"));
        assert!(blocked);
        assert!(snap.unwrap().auth_json.contains("signed_out"));
        let st = lock(&inner.state);
        assert!(st.auth_blocked && st.session.is_none());
        drop(st);
        // Signed in, but the bearer was withheld (cross-origin endpoint): pause.
        lock(&inner.state).auth_blocked = false;
        lock(&inner.state).session = Some(session("s"));
        let (_, blocked) = inner.outbox_unauthorized(&row, "u", None, Some("s"));
        assert!(blocked && lock(&inner.state).auth_blocked);
    }
}
