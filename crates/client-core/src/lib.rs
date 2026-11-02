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
//! changes survive. Flushing the outbox to the server (sync worker) and
//! merging pending rows into re-fetched bundles are not implemented yet.

pub mod expr;
pub mod store;

use std::collections::BTreeMap;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{ActionIntent, EntityRecord, UiNode, WorkspaceSpec};
use serde_json::Value;

use store::{NewOutboxRow, Store, StoreOps};

const DEFAULT_WORKSPACE_ID: &str = "home";
/// SQLite file name inside `storage_path`.
pub const CACHE_DB_FILE: &str = "cache.db";
/// Payload token replaced by the new optimistic field value at dispatch.
pub const VALUE_TOKEN: &str = "$value";
/// Delay between background fetch retries / reachability probes.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Timeout for a single reachability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// Persist a fetched bundle (layout + entities, one transaction) and
    /// apply it in memory. A persistence failure is logged; the fetched
    /// state is still applied (the store is only a fallback for it).
    fn apply_fetched(&mut self, workspace_id: &str, spec: WorkspaceSpec) {
        if let Some(store) = self.store.as_mut() {
            let now = unix_now();
            let uris: Vec<String> = spec.entities.keys().cloned().collect();
            let result = store.transaction(|tx| {
                tx.save_workspace(workspace_id, &spec.workspace, &uris, now)?;
                for (uri, entity) in &spec.entities {
                    tx.upsert_entity(uri, entity, now)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                eprintln!("[CanvasEngine] failed to persist workspace '{workspace_id}': {e}");
            }
        }
        self.workspace = Some(spec.workspace);
        self.entities = spec.entities;
    }
}

struct Inner {
    state: Mutex<State>,
    observers: Mutex<Vec<Arc<Observer>>>,
    workspace_id: String,
    workspace_url: String,
    health_url: String,
}

/// The client-side state engine. Cheap to share behind an `Arc`.
pub struct CanvasEngine {
    inner: Arc<Inner>,
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

/// Open `<storage_path>/cache.db`, falling back to an in-memory DB when
/// `storage_path` is empty or the file cannot be opened.
fn open_store(storage_path: &str) -> Option<Store> {
    if !storage_path.is_empty() {
        let opened = fs::create_dir_all(storage_path)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                Store::open(&PathBuf::from(storage_path).join(CACHE_DB_FILE))
                    .map_err(|e| e.to_string())
            });
        match opened {
            Ok(store) => return Some(store),
            Err(e) => eprintln!(
                "[CanvasEngine] cannot open {storage_path}/{CACHE_DB_FILE}: {e}; using an \
                 in-memory store"
            ),
        }
    }
    match Store::open_in_memory() {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("[CanvasEngine] cannot open an in-memory store: {e}; mutations disabled");
            None
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
        let mut state = State {
            store: open_store(storage_path),
            ..State::default()
        };
        state.load_cached(workspace_id);

        let base = base_url.trim_end_matches('/');
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            observers: Mutex::new(Vec::new()),
            workspace_id: workspace_id.to_string(),
            workspace_url: format!("{base}/v1/workspaces/{workspace_id}"),
            health_url: format!("{base}/healthz"),
        });

        // The background thread holds only a Weak reference between
        // attempts, so it exits once the engine is dropped instead of
        // keeping it alive. Until the server has answered once it retries
        // the full fetch; afterwards it only probes reachability.
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
                    thread::sleep(POLL_INTERVAL);
                }
            });
        if let Err(e) = spawned {
            eprintln!("[CanvasEngine] failed to spawn fetch thread: {e}");
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
        self.notify(snap);
    }

    fn refresh(&self) -> FetchOutcome {
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
        self.notify(snap);
        outcome
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
