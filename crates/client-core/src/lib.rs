//! Client core engine (DESIGN.md §2.3).
//!
//! `CanvasEngine` owns workspace state, fetches the hydration bundle over
//! HTTP and applies optimistic mutations locally, pushing a `StateSnapshot`
//! to subscribers on every change.
//!
//! Current scope (M3): state lives in memory, and the last fetched bundle is
//! written to `<storage_path>/last_workspace.json` as an offline fallback.
//! The SQLite store, action outbox and background sync come in M5.

pub mod expr;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use protocol::{ActionIntent, EntityRecord, UiNode, WorkspaceSpec};

const CACHE_FILE: &str = "last_workspace.json";
const DEFAULT_WORKSPACE_ID: &str = "home";

/// State pushed to observers.
///
/// - `workspace_json`: the bundle's `workspace` layout node (`UiNode`) as
///   JSON, or `"null"` if no workspace has ever been loaded.
/// - `entities_json`: the URI-keyed entity map as a JSON object (`"{}"` if
///   empty).
/// - `is_online`: whether the most recent fetch succeeded.
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
            (self.f)(snap);
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
                Ok(()) => Some(PathBuf::from(storage_path).join(CACHE_FILE)),
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

        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            observers: Mutex::new(Vec::new()),
            workspace_id: workspace_id.to_string(),
            workspace_url: format!(
                "{}/v1/workspaces/{}",
                base_url.trim_end_matches('/'),
                workspace_id
            ),
            cache_path,
        });

        let bg = Arc::clone(&inner);
        let spawned = thread::Builder::new()
            .name("canvas-engine-fetch".into())
            .spawn(move || bg.refresh());
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
        // Always deliver the initial snapshot (even at seq 0); a newer one
        // already delivered by a concurrent notify takes precedence.
        if observer.last_seq.fetch_max(seq, Ordering::SeqCst) <= seq {
            (observer.f)(snap);
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
    fn refresh(&self) {
        let result = fetch(&self.workspace_url);
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
                        && let Err(e) = fs::write(path, raw)
                    {
                        eprintln!(
                            "[CanvasEngine] failed to write cache {}: {e}",
                            path.display()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[CanvasEngine] fetch {} failed: {e}", self.workspace_url);
                    st.is_online = false;
                }
            }
            st.seq += 1;
            (st.seq, st.snapshot())
        };
        self.notify(seq_snap);
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

fn fetch(url: &str) -> Result<(WorkspaceSpec, String), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client.get(url).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let raw = resp.text().map_err(|e| e.to_string())?;
    let spec = serde_json::from_str(&raw).map_err(|e| format!("bad bundle: {e}"))?;
    Ok((spec, raw))
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
