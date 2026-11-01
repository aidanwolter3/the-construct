//! Client core engine (DESIGN.md §2.3).
//!
//! `CanvasEngine` owns workspace state, fetches the hydration bundle over
//! HTTP and applies optimistic mutations locally, pushing a `StateSnapshot`
//! to subscribers on every change.
//!
//! Current scope (M3): state lives in memory, and the last fetched bundle is
//! written to `<storage_path>/workspace_<id>.json` (one file per workspace
//! id) as an offline fallback. A background thread retries the initial fetch
//! every few seconds until the server answers; after that it probes
//! `GET {base_url}/healthz` on the same interval so `is_online` tracks the
//! server going away and coming back. Reconnecting does not re-fetch the
//! workspace, so local optimistic changes survive.
//! The SQLite store, action outbox and background sync come in M5.

pub mod expr;

use std::collections::BTreeMap;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Duration;

use protocol::{ActionIntent, EntityRecord, UiNode, WorkspaceSpec};

const DEFAULT_WORKSPACE_ID: &str = "home";
/// Delay between background fetch retries / reachability probes.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Timeout for a single reachability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Offline cache file name for a workspace: `workspace_<id>.json`, with any
/// character outside `[A-Za-z0-9_-]` replaced by `_`.
fn cache_file_name(workspace_id: &str) -> String {
    let safe: String = workspace_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("workspace_{safe}.json")
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    pub workspace_json: String,
    pub entities_json: String,
    pub is_online: bool,
}

struct Observer {
    f: Box<dyn Fn(StateSnapshot) + Send + Sync>,
    /// Highest state `seq` delivered to this observer.
    last_seq: AtomicU64,
}

impl Observer {
    /// Call the observer unless it has already seen this or a newer state.
    fn deliver(&self, seq: u64, snap: StateSnapshot) {
        if self.last_seq.fetch_max(seq, Ordering::SeqCst) < seq {
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
        }
    }

    fn apply_bundle(&mut self, spec: WorkspaceSpec) {
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
    cache_path: Option<PathBuf>,
}

/// The client-side state engine. Cheap to share behind an `Arc`.
pub struct CanvasEngine {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking observer must not brick the engine.
    m.lock().unwrap_or_else(|e| e.into_inner())
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
        let cache_path = if storage_path.is_empty() {
            None
        } else {
            match fs::create_dir_all(storage_path) {
                Ok(()) => Some(PathBuf::from(storage_path).join(cache_file_name(workspace_id))),
                Err(e) => {
                    eprintln!("[CanvasEngine] cannot create storage dir {storage_path}: {e}");
                    None
                }
            }
        };

        let mut state = State::default();
        if let Some(spec) = cache_path.as_ref().and_then(load_cache) {
            println!(
                "[CanvasEngine] loaded cached workspace ({} entities) from offline cache",
                spec.entities.len()
            );
            state.apply_bundle(spec);
            state.seq = 1;
        }

        let base = base_url.trim_end_matches('/');
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            observers: Mutex::new(Vec::new()),
            workspace_id: workspace_id.to_string(),
            workspace_url: format!("{base}/v1/workspaces/{workspace_id}"),
            health_url: format!("{base}/healthz"),
            cache_path,
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
        let (seq, snap) = {
            let st = lock(&self.inner.state);
            (st.seq, st.snapshot())
        };
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

    /// The current snapshot. Before the first fetch (and with no offline
    /// cache) this is `workspace_json: "null"`, `entities_json: "{}"`,
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
        let seq_snap = {
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
            (st.seq, st.snapshot())
        };
        self.notify(seq_snap);
    }

    fn refresh(&self) -> FetchOutcome {
        let result = fetch(&self.workspace_url);
        let outcome;
        let seq_snap = {
            let mut st = lock(&self.state);
            match result {
                Ok((spec, raw)) => {
                    println!(
                        "[CanvasEngine] fetched workspace '{}' ({} entities) from {}",
                        self.workspace_id,
                        spec.entities.len(),
                        self.workspace_url
                    );
                    st.apply_bundle(spec);
                    st.is_online = true;
                    if let Some(path) = &self.cache_path
                        && let Err(e) = write_atomic(path, &raw)
                    {
                        eprintln!(
                            "[CanvasEngine] failed to write cache {}: {e}",
                            path.display()
                        );
                    }
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
            (st.seq, st.snapshot())
        };
        self.notify(seq_snap);
        outcome
    }

    fn dispatch(&self, intent_json: &str) {
        let compact = serde_json::from_str::<serde_json::Value>(intent_json)
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
                ..
            } => {
                let seq_snap = {
                    let mut st = lock(&self.state);
                    let mut entity = st.entities.get(&entity_uri).cloned().unwrap_or_default();
                    match expr::eval(&value_expr, &entity) {
                        Ok(v) => {
                            println!("[CanvasEngine] {entity_uri}#{field} = {v}");
                            entity.set(field, v);
                            st.entities.insert(entity_uri, entity);
                        }
                        Err(e) => {
                            eprintln!("[CanvasEngine] mutate {entity_uri}#{field}: {e}; no change");
                            return;
                        }
                    }
                    st.seq += 1;
                    (st.seq, st.snapshot())
                };
                // Outbox / server sync to `endpoint` is M5.
                self.notify(seq_snap);
            }
            other => {
                eprintln!(
                    "[CanvasEngine] dispatch: intent not handled by client-core yet: {other:?}"
                );
            }
        }
    }

    /// Deliver a snapshot to all observers, with no locks held during the
    /// calls. Each observer skips snapshots older than one it already saw.
    fn notify(&self, (seq, snap): (u64, StateSnapshot)) {
        let observers: Vec<Arc<Observer>> = lock(&self.observers).clone();
        for obs in observers {
            obs.deliver(seq, snap.clone());
        }
    }
}

enum FetchError {
    /// The server responded with this non-success HTTP status code.
    Status(u16),
    /// Transport failure or unusable response body.
    Other(String),
}

fn fetch(url: &str) -> Result<(WorkspaceSpec, String), FetchError> {
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
    let spec =
        serde_json::from_str(&raw).map_err(|e| FetchError::Other(format!("bad bundle: {e}")))?;
    Ok((spec, raw))
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

/// Write via a temp file + rename so a crash mid-write never leaves a
/// truncated cache. Callers serialize writes (state lock held).
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn load_cache(path: &PathBuf) -> Option<WorkspaceSpec> {
    let raw = fs::read_to_string(path).ok()?;
    match serde_json::from_str(&raw) {
        Ok(spec) => Some(spec),
        Err(e) => {
            eprintln!(
                "[CanvasEngine] ignoring corrupt cache {}: {e}",
                path.display()
            );
            None
        }
    }
}
