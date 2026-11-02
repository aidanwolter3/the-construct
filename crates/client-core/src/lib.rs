//! Client core engine (DESIGN.md §2.3).
//!
//! `CanvasEngine` owns workspace state, fetches the hydration bundle over
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
//! A background thread retries the initial fetch every few seconds until
//! the server answers; after that it probes `GET {base_url}/healthz` on the
//! same interval so `is_online` tracks the server going away and coming
//! back. Reconnecting does not re-fetch the workspace, so local optimistic
//! changes survive.
//!
//! A second background thread (the sync worker, docs/DECISIONS.md D14)
//! flushes PENDING outbox rows FIFO to `POST {base_url}{endpoint}` while
//! online: 2xx deletes the row, 4xx deletes it and rolls the field back
//! (re-applying later pending rows), 5xx / network errors keep it with
//! exponential backoff. A fetched bundle has every pending row re-applied
//! on top of it, so optimistic values are never clobbered.

pub mod expr;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{ActionIntent, EntityRecord, UiNode, WorkspaceSpec};
use serde_json::Value;

use store::{NewOutboxRow, OutboxRow, Store, StoreOps};

const DEFAULT_WORKSPACE_ID: &str = "home";
/// SQLite file name inside `storage_path`.
pub const CACHE_DB_FILE: &str = "cache.db";
/// Payload token replaced by the new optimistic field value at dispatch.
pub const VALUE_TOKEN: &str = "$value";
/// Timeout for a single reachability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Timeout for a single outbox POST.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    pub workspace_json: String,
    pub entities_json: String,
    pub is_online: bool,
    pub seq: u64,
    pub pending_actions: u32,
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
            eprintln!("[CanvasEngine] observer panicked; ignored");
        }
    }
}

#[derive(Default)]
struct State {
    workspace: Option<UiNode>,
    entities: BTreeMap<String, EntityRecord>,
    is_online: bool,
    /// Bumped on every change (cache load, fetch attempt, mutation). Used to
    /// drop stale out-of-order deliveries.
    seq: u64,
    /// PENDING outbox rows; kept in step with the store on every write.
    pending_actions: u32,
    /// Write-through persistence. `None` only if even an in-memory DB could
    /// not be opened; mutations are then rejected.
    store: Option<Store>,
}

impl State {
    fn snapshot(&self) -> StateSnapshot {
        let workspace_json = match &self.workspace {
            Some(ws) => serde_json::to_string(ws).unwrap_or_else(|_| "null".into()),
            None => "null".into(),
        };
        let entities_json = serde_json::to_string(&self.entities).unwrap_or_else(|_| "{}".into());
        StateSnapshot {
            workspace_json,
            entities_json,
            is_online: self.is_online,
            seq: self.seq,
            pending_actions: self.pending_actions,
        }
    }

    /// Load the cached workspace, its entities and every entity with a
    /// pending outbox row from the store.
    fn load_cached(&mut self, workspace_id: &str) {
        let Some(store) = &self.store else { return };
        let loaded = (|| {
            let ws = store.load_workspace(workspace_id)?;
            let pending = store.pending_outbox()?;
            let mut uris: Vec<String> = ws
                .as_ref()
                .map(|w| w.entity_uris.clone())
                .unwrap_or_default();
            uris.extend(pending.iter().map(|r| r.entity_uri.clone()));
            uris.sort();
            uris.dedup();
            let entities = store.load_entities(&uris)?;
            Ok::<_, store::StoreError>((ws, entities, pending.len()))
        })();
        match loaded {
            Ok((ws, entities, pending)) => {
                if ws.is_some() || !entities.is_empty() || pending > 0 {
                    println!(
                        "[CanvasEngine] loaded cached workspace '{workspace_id}' (layout: {}, {} \
                         entities, {pending} pending actions) from {CACHE_DB_FILE}",
                        if ws.is_some() { "yes" } else { "no" },
                        entities.len(),
                    );
                    self.seq = 1;
                }
                self.workspace = ws.map(|w| w.layout);
                self.entities = entities;
                self.pending_actions = u32::try_from(pending).unwrap_or(u32::MAX);
            }
            Err(e) => eprintln!("[CanvasEngine] failed to load cache: {e}"),
        }
    }

    /// Merge a fetched bundle: replace the bundle's entities, then re-apply
    /// every PENDING outbox row for those entities FIFO (re-evaluating its
    /// `value_expr` and updating its `prev_value_json`), so optimistic
    /// values survive a refetch. Entities outside the bundle that have
    /// pending rows are kept as they are. Layout, entities and the rows'
    /// new `prev_value_json` are persisted in one transaction; a
    /// persistence failure is logged and the merged state is still applied
    /// (the store is only a fallback for it).
    fn apply_fetched(&mut self, workspace_id: &str, spec: WorkspaceSpec) {
        let WorkspaceSpec {
            workspace,
            entities: fetched,
            ..
        } = spec;
        let pending = match self.store.as_ref().map(|s| s.pending_outbox()) {
            Some(Ok(rows)) => rows,
            Some(Err(e)) => {
                eprintln!("[CanvasEngine] failed to read the outbox for the refetch merge: {e}");
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
                    && let Some(prev) = reapply_row(entity, row)
                {
                    prev_updates.push((row.id, prev));
                }
            } else if let Some(local) = self.entities.get(&row.entity_uri) {
                merged.insert(row.entity_uri.clone(), local.clone());
            }
        }
        if !prev_updates.is_empty() {
            println!(
                "[CanvasEngine] re-applied {} pending action(s) onto the fetched workspace \
                 '{workspace_id}'",
                prev_updates.len()
            );
        }
        if let Some(store) = self.store.as_mut() {
            let now = unix_now();
            let result = store.transaction(|tx| {
                tx.save_workspace(workspace_id, &workspace, &uris, now)?;
                for uri in &uris {
                    tx.upsert_entity(uri, &merged[uri], now)?;
                }
                for (id, prev) in &prev_updates {
                    tx.set_outbox_prev_value(*id, prev)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                eprintln!("[CanvasEngine] failed to persist workspace '{workspace_id}': {e}");
            }
        }
        self.workspace = Some(workspace);
        self.entities = merged;
    }

    /// Re-read the PENDING count from the store (after a delete).
    fn sync_pending_count(&mut self) {
        if let Some(store) = &self.store {
            match store.pending_count() {
                Ok(n) => self.pending_actions = n,
                Err(e) => eprintln!("[CanvasEngine] failed to count pending actions: {e}"),
            }
        }
    }

    /// The server rejected `row` (4xx): delete it, set the entity field
    /// back to `prev_value` (or `rollback_value` if prev is null/absent),
    /// then re-apply every later PENDING row for the same entity+field FIFO.
    /// One transaction; memory is only changed if it commits. Returns the
    /// restored value, the field's final value and the number of re-applied
    /// rows.
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
        let parse = |s: &Option<String>| {
            s.as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .filter(|v| !v.is_null())
        };
        let restored = parse(&row.prev_value_json)
            .or_else(|| parse(&row.rollback_value_json))
            .unwrap_or(Value::Null);
        let mut entity = self
            .entities
            .get(&row.entity_uri)
            .cloned()
            .unwrap_or_default();
        entity.set(field.clone(), restored.clone());
        let mut prev_updates = Vec::new();
        for later in store.pending_outbox()? {
            if later.id > row.id
                && later.entity_uri == row.entity_uri
                && later.field.as_deref() == Some(field.as_str())
                && let Some(prev) = reapply_row(&mut entity, &later)
            {
                prev_updates.push((later.id, prev));
            }
        }
        let now = unix_now();
        store.transaction(|tx| {
            tx.delete_outbox(row.id)?;
            tx.upsert_entity(&row.entity_uri, &entity, now)?;
            for (id, prev) in &prev_updates {
                tx.set_outbox_prev_value(*id, prev)?;
            }
            Ok(())
        })?;
        let final_value = entity.get(&field).cloned().unwrap_or(Value::Null);
        self.entities.insert(row.entity_uri.clone(), entity);
        self.sync_pending_count();
        Ok((restored, final_value, prev_updates.len()))
    }
}

/// Re-apply a pending row to `entity`: re-evaluate its `value_expr` and set
/// its field. Returns the field's previous value as JSON (the row's new
/// `prev_value_json`), or `None` if the row has no field/expression. If the
/// expression fails the field is left unchanged (logged).
fn reapply_row(entity: &mut EntityRecord, row: &OutboxRow) -> Option<String> {
    let (Some(field), Some(expr)) = (&row.field, &row.value_expr) else {
        return None;
    };
    let prev = entity.get(field).cloned().unwrap_or(Value::Null);
    match expr::eval(expr, entity) {
        Ok(v) => {
            entity.set(field.clone(), v);
        }
        Err(e) => eprintln!(
            "[CanvasEngine] re-apply outbox #{} {}#{field}: {e}; field unchanged",
            row.id, row.entity_uri
        ),
    }
    Some(prev.to_string())
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
    Rejected(u16, String),
    /// 5xx or any other non-2xx/4xx status.
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
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.signal.shutdown();
    }
}

/// The client-side state engine. Cheap to share behind an `Arc`.
pub struct CanvasEngine {
    inner: Arc<Inner>,
}

impl Drop for CanvasEngine {
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
/// `storage_path` is empty or the file cannot be opened. Also returns the
/// `net` lock for that store (see [`net_lock_for`]).
fn open_store(storage_path: &str) -> (Option<Store>, Arc<Mutex<()>>) {
    if !storage_path.is_empty() {
        let path = PathBuf::from(storage_path).join(CACHE_DB_FILE);
        let opened = fs::create_dir_all(storage_path)
            .map_err(|e| e.to_string())
            .and_then(|()| Store::open(&path).map_err(|e| e.to_string()));
        match opened {
            Ok(store) => return (Some(store), net_lock_for(Some(&path))),
            Err(e) => eprintln!(
                "[CanvasEngine] cannot open {storage_path}/{CACHE_DB_FILE}: {e}; using an \
                 in-memory store"
            ),
        }
    }
    match Store::open_in_memory() {
        Ok(store) => (Some(store), net_lock_for(None)),
        Err(e) => {
            eprintln!("[CanvasEngine] cannot open an in-memory store: {e}; mutations disabled");
            (None, net_lock_for(None))
        }
    }
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

impl CanvasEngine {
    /// Create an engine for the `"home"` workspace. Returns immediately; the
    /// initial fetch runs on a background thread. Never panics if the server
    /// is unreachable.
    pub fn init(storage_path: &str, base_url: &str) -> Self {
        Self::init_with_workspace(storage_path, base_url, DEFAULT_WORKSPACE_ID)
    }

    /// Like [`CanvasEngine::init`], fetching `GET {base_url}/v1/workspaces/{workspace_id}`.
    pub fn init_with_workspace(storage_path: &str, base_url: &str, workspace_id: &str) -> Self {
        Self::init_with_config(storage_path, base_url, workspace_id, EngineConfig::default())
    }

    /// Like [`CanvasEngine::init_with_workspace`] with custom timing.
    pub fn init_with_config(
        storage_path: &str,
        base_url: &str,
        workspace_id: &str,
        config: EngineConfig,
    ) -> Self {
        let (store, net) = open_store(storage_path);
        let mut state = State {
            store,
            ..State::default()
        };
        state.load_cached(workspace_id);

        let base = base_url.trim_end_matches('/');
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
        });

        // Both background threads hold only a Weak reference between
        // passes, so they exit once the engine is dropped instead of
        // keeping it alive. Until the server has answered once the fetch
        // thread retries the full fetch; afterwards it only probes
        // reachability.
        let bg: Weak<Inner> = Arc::downgrade(&inner);
        let spawned = thread::Builder::new()
            .name("canvas-engine-fetch".into())
            .spawn(move || {
                let mut answered = false;
                loop {
                    let Some(inner) = bg.upgrade() else { return };
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
            eprintln!("[CanvasEngine] failed to spawn fetch thread: {e}");
        }

        let worker: Weak<Inner> = Arc::downgrade(&inner);
        let spawned = thread::Builder::new()
            .name("canvas-engine-sync".into())
            .spawn(move || sync_worker(worker, signal));
        if let Err(e) = spawned {
            eprintln!("[CanvasEngine] failed to spawn sync worker: {e}");
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
}

impl Inner {
    /// Check whether the server is reachable (any HTTP response counts) and
    /// notify observers if `is_online` changed. Never touches the workspace
    /// or entities.
    fn probe(&self) {
        let reachable = reachable(&self.health_url);
        let snap = {
            let mut st = lock(&self.state);
            if st.is_online == reachable {
                return;
            }
            st.is_online = reachable;
            if reachable {
                println!("[CanvasEngine] server reachable again; online");
            } else {
                eprintln!("[CanvasEngine] server unreachable ({}); offline", self.health_url);
            }
            st.seq += 1;
            st.snapshot()
        };
        if reachable {
            self.signal.wake();
        }
        self.notify(snap);
    }

    fn refresh(&self) -> FetchOutcome {
        let net = lock(&self.net);
        let result = fetch(&self.workspace_url);
        let outcome;
        let snap = {
            let mut st = lock(&self.state);
            match result {
                Ok(spec) => {
                    println!(
                        "[CanvasEngine] fetched workspace '{}' ({} entities) from {}",
                        self.workspace_id,
                        spec.entities.len(),
                        self.workspace_url
                    );
                    st.apply_fetched(&self.workspace_id, spec);
                    st.is_online = true;
                    outcome = FetchOutcome::Fetched;
                }
                Err(FetchError::Status(code)) => {
                    // The server responded, so the network is reachable;
                    // keep whatever workspace we already have.
                    if code == 404 {
                        eprintln!(
                            "[CanvasEngine] workspace '{}' not found (HTTP 404)",
                            self.workspace_id
                        );
                    } else {
                        eprintln!(
                            "[CanvasEngine] fetch {} failed: HTTP {code}",
                            self.workspace_url
                        );
                    }
                    st.is_online = true;
                    outcome = FetchOutcome::HttpError;
                }
                Err(FetchError::Other(e)) => {
                    eprintln!("[CanvasEngine] fetch {} failed: {e}", self.workspace_url);
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
            let row = {
                let mut st = lock(&self.state);
                if !st.is_online {
                    return None;
                }
                let store = st.store.as_ref()?;
                match store.first_pending() {
                    Ok(Some(row)) => row,
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
                        eprintln!("[CanvasEngine] sync worker: cannot read the outbox: {e}");
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

            let url = format!("{}{}", self.base_url, row.endpoint);
            let result = post_json(client, &url, &row.payload_json);
            let (snap, failed) = self.apply_flush_result(&row, &url, result);
            drop(net);
            if let Some(snap) = snap {
                self.notify(snap);
            }
            if failed {
                // Next pass re-reads the row and waits for its
                // next_attempt_at (or for going online again).
                return Some(self.config.backoff(row.retry_count + 1));
            }
        }
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
                        "[CanvasEngine] outbox #{id} POST {url} -> {code}, but deleting the row \
                         failed: {e}"
                    );
                    return (None, true);
                }
                st.sync_pending_count();
                println!(
                    "[CanvasEngine] outbox #{id} POST {url} -> {code}, deleted ({} pending)",
                    st.pending_actions
                );
                st.seq += 1;
                return (Some(st.snapshot()), false);
            }
            FlushResult::Rejected(code, body) => {
                let field = row.field.as_deref().unwrap_or("?");
                match st.reject_row(row) {
                    Ok((restored, now_value, reapplied)) => {
                        println!(
                            "[CanvasEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
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
                            "[CanvasEngine] outbox #{id} POST {url} -> {code}, rejected: {body}; \
                             rollback failed to persist ({e}); will retry"
                        );
                        (format!("HTTP {code}: {body}; rollback failed: {e}"), false)
                    }
                }
            }
            FlushResult::ServerError(code, body) => (format!("HTTP {code}: {body}"), false),
            FlushResult::Network(e) => (e, true),
        };

        let retry_count = row.retry_count + 1;
        let delay = self.config.backoff(retry_count);
        let next = unix_now_ms() + delay.as_millis() as i64;
        if let Some(Err(e)) = st.store.as_ref().map(|s| s.mark_retry(id, next, &error)) {
            eprintln!("[CanvasEngine] outbox #{id}: failed to record the retry: {e}");
        }
        eprintln!(
            "[CanvasEngine] outbox #{id} POST {url} -> {error}; retry #{retry_count} in {} ms \
             ({} pending)",
            delay.as_millis(),
            st.pending_actions
        );
        if network && st.is_online {
            eprintln!("[CanvasEngine] server unreachable ({url}); offline");
            st.is_online = false;
            st.seq += 1;
            return (Some(st.snapshot()), true);
        }
        (None, true)
    }

    fn dispatch(&self, intent_json: &str) {
        let started = Instant::now();
        let compact = serde_json::from_str::<Value>(intent_json)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| intent_json.to_string());
        println!("[CanvasEngine] dispatch: {compact}");
        let intent: ActionIntent = match serde_json::from_str(intent_json) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("[CanvasEngine] dispatch: invalid intent ({e}); ignored");
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
            } => self.optimistic_mutate(
                started,
                entity_uri,
                field,
                value_expr,
                rollback_value,
                endpoint,
                payload,
            ),
            other => {
                eprintln!(
                    "[CanvasEngine] dispatch: intent not handled by client-core yet: {other:?}"
                );
            }
        }
    }

    /// Apply an optimistic mutation: entity update + outbox row in one
    /// SQLite transaction, then the in-memory state, then notify. If the
    /// transaction fails nothing changes and observers are not notified.
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
    ) {
        let (snap, value, outbox_id) = {
            let mut guard = lock(&self.state);
            let st = &mut *guard;
            let mut entity = st.entities.get(&entity_uri).cloned().unwrap_or_default();
            let value = match expr::eval(&value_expr, &entity) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[CanvasEngine] mutate {entity_uri}#{field}: {e}; no change");
                    return;
                }
            };
            let prev = entity
                .set(field.clone(), value.clone())
                .unwrap_or(Value::Null);
            let Some(store) = st.store.as_mut() else {
                eprintln!("[CanvasEngine] mutate {entity_uri}#{field}: no store; no change");
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
            };
            let persisted = store.transaction(|tx| {
                tx.upsert_entity(&entity_uri, &entity, now)?;
                tx.insert_outbox(&row)
            });
            let outbox_id = match persisted {
                Ok(id) => id,
                Err(e) => {
                    eprintln!(
                        "[CanvasEngine] mutate {entity_uri}#{field}: failed to persist ({e}); \
                         no change"
                    );
                    return;
                }
            };
            st.entities.insert(entity_uri.clone(), entity);
            st.pending_actions = st.pending_actions.saturating_add(1);
            st.seq += 1;
            (st.snapshot(), value, outbox_id)
        };
        let pending = snap.pending_actions;
        self.notify(snap);
        self.signal.wake();
        println!(
            "[CanvasEngine] mutate {entity_uri}#{field} = {value} (outbox #{outbox_id}, \
             {pending} pending); dispatch->notify {} us",
            started.elapsed().as_micros()
        );
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

fn fetch(url: &str) -> Result<WorkspaceSpec, FetchError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| FetchError::Other(format!("client build: {e}")))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| FetchError::Other(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    let raw = resp.text().map_err(|e| FetchError::Other(e.to_string()))?;
    serde_json::from_str(&raw).map_err(|e| FetchError::Other(format!("bad bundle: {e}")))
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
            eprintln!("[CanvasEngine] sync worker: cannot build HTTP client: {e}; outbox disabled");
            return;
        }
    };
    loop {
        let wait = {
            let Some(inner) = engine.upgrade() else { return };
            inner.flush_pass(&client)
        };
        if !signal.wait(wait) {
            return;
        }
    }
}

/// `POST url` with a JSON body, classified for the outbox.
fn post_json(client: &reqwest::blocking::Client, url: &str, body: &str) -> FlushResult {
    let resp = match client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
    {
        Ok(r) => r,
        Err(e) => return FlushResult::Network(e.to_string()),
    };
    let code = resp.status().as_u16();
    if resp.status().is_success() {
        return FlushResult::Accepted(code);
    }
    let body = resp.text().unwrap_or_default();
    if (400..500).contains(&code) {
        FlushResult::Rejected(code, body)
    } else {
        FlushResult::ServerError(code, body)
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
}
