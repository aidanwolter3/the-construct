//! One server, many pods (D46).
//!
//! Every pod is a full [`pod_core`] pod (all the same apps) with its own
//! SQLite file, `<data>/pods/<pod id>/server.db`, so pods share no data. A
//! request names its pod by its bearer token: a **pod token** (`cpod_…`), made
//! by an admin ([`Host::create_pod`], the `server pods` CLI) or by anyone on a
//! server that allows **temporary pods** (`POST /v1/pods/temporary`), which
//! the sweeper deletes when they expire. `host.db` keeps only the pods and the
//! SHA-256 of their tokens; a token is shown once, never stored.
//!
//! Inside its pod the token is a *pinned* session (never expires, a logout
//! keeps it), so every pod route, sign-in and SSE stream works unchanged.
//! Without a valid token a caller gets only `/healthz`, the "Open your pod"
//! screen and the two `/v1/pods/*` calls; images (adapters'
//! [`pod_core::AppAdapter::public_paths`]) load without a bearer from
//! `/v1/p/<pod id>/…`. `CONSTRUCT_OPEN_POD` (development only) serves callers
//! without a token from one named pod, like the single-pod server before D46.

pub mod db;
pub mod limits;
pub mod screen;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::session::{SESSION_HEADER, bearer, session_hash};
use pod_core::store::Store;
use pod_core::util::{hex, now_secs};
use pod_core::{AppState, PUBLIC_PREFIX_HEADER, Pod, json_error, parse_json_body};
use serde_json::json;
use tower::ServiceExt;

pub use db::{HostStore, PodKind, PodRow};
use limits::RateLimit;

/// Builds a pod over its opened store: runs the adapters' migrations and the
/// layout sync (the binary's [`AppState::try_new`] with its adapters).
pub type PodFactory = Arc<dyn Fn(Store) -> Result<AppState, String> + Send + Sync>;

/// The prefix of every pod token.
pub const TOKEN_PREFIX: &str = "cpod_";
/// How often the sweeper removes expired pods.
pub const SWEEP_EVERY: Duration = Duration::from_secs(300);
/// How often the scheduler looks for pods whose notification polls are due.
pub const SCHEDULE_EVERY: Duration = Duration::from_secs(30);
/// Pods polled at the same time, at most.
pub const POLL_CONCURRENCY: usize = 8;
/// `host_secrets` key of the generated VAPID private key.
pub const VAPID_SECRET: &str = "vapid_private_key";

/// The server's Web Push VAPID private key (D52): `configured` (the
/// `CONSTRUCT_VAPID_PRIVATE_KEY` value) or one generated once and kept in
/// `<dir>/host.db`. Called before the host opens, so pods can be built with it.
pub fn vapid_private_key(dir: &FsPath, configured: Option<&str>) -> Result<String, String> {
    if let Some(k) = configured {
        return Ok(k.to_owned());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let store = HostStore::open(&dir.join("host.db")).map_err(|e| e.to_string())?;
    store
        .secret_or(
            VAPID_SECRET,
            pod_core::push::PushSender::generate_private_key,
        )
        .map_err(|e| e.to_string())
}

/// Temporary pods (`CONSTRUCT_TEMP_PODS=1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporaryPods {
    /// Lifetime (`CONSTRUCT_TEMP_POD_HOURS`, default 24).
    pub hours: u64,
    /// Live temporary pods at most (`CONSTRUCT_TEMP_POD_MAX`, default 50).
    pub max_live: i64,
}

/// What the host allows besides admin pods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// `CONSTRUCT_OPEN_POD=<name>`: callers without a valid token use this
    /// pod (created if missing). Development and tests only.
    pub open_pod: Option<String>,
    pub temporary: Option<TemporaryPods>,
    /// Settings written into every pod this host creates before its
    /// adapters' migrations run (e.g. turning a data import off), unless the
    /// admin asks otherwise.
    pub new_pod_settings: Vec<(String, String)>,
    /// Extra settings for temporary pods (anyone can make one), e.g. keeping
    /// an adapter from contacting hosts the visitor picks.
    pub temporary_pod_settings: Vec<(String, String)>,
    /// Rate limits key on the `Fly-Client-IP` header (set by Fly's proxy)
    /// instead of the connection's peer. Only behind Fly (`FLY_APP_NAME`):
    /// anywhere else a client could send any value.
    pub trust_fly_client_ip: bool,
}

impl HostConfig {
    /// `CONSTRUCT_OPEN_POD`, `CONSTRUCT_TEMP_PODS`, `CONSTRUCT_TEMP_POD_HOURS`,
    /// `CONSTRUCT_TEMP_POD_MAX`.
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let temporary =
            (var("CONSTRUCT_TEMP_PODS").as_deref() == Some("1")).then(|| TemporaryPods {
                hours: var("CONSTRUCT_TEMP_POD_HOURS")
                    .and_then(|h| h.parse().ok())
                    .filter(|h| (1..=720).contains(h))
                    .unwrap_or(24),
                max_live: var("CONSTRUCT_TEMP_POD_MAX")
                    .and_then(|m| m.parse().ok())
                    .filter(|m| *m >= 1)
                    .unwrap_or(50),
            });
        Self {
            // `off` locks even a loopback server (see the binary's default).
            open_pod: var("CONSTRUCT_OPEN_POD").filter(|v| v != "off"),
            temporary,
            new_pod_settings: Vec::new(),
            temporary_pod_settings: Vec::new(),
            trust_fly_client_ip: var("FLY_APP_NAME").is_some(),
        }
    }
}

/// A pod just made: show `token` once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPod {
    pub id: String,
    pub name: String,
    pub token: String,
    pub expires_at: Option<i64>,
}

/// A new pod token: `cpod_` + 256 random bits in lowercase base32.
pub fn new_pod_token() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    let mut out = String::from(TOKEN_PREFIX);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn new_pod_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    hex(&bytes)
}

/// `[a-z0-9][a-z0-9-]{0,39}`.
pub fn valid_pod_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

fn valid_pod_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One open pod and its router.
struct OpenPod {
    pod: Pod,
    router: Router,
}

/// The pods of this server.
pub struct Host {
    dir: PathBuf,
    store: HostStore,
    factory: PodFactory,
    config: HostConfig,
    open: Mutex<HashMap<String, Arc<OpenPod>>>,
    opening: tokio::sync::Mutex<()>,
    open_pod_id: OnceLock<String>,
    code_tries: RateLimit,
    temp_creates: RateLimit,
    /// Makes the temporary-pod cap check and the creation one step.
    temp_lock: Mutex<()>,
    /// The next wake last written to `host.db` per pod (skips rewrites).
    wakes: Mutex<HashMap<String, Option<i64>>>,
}

impl Host {
    /// The host over `<dir>/host.db` (created) and `<dir>/pods/`.
    pub fn open(dir: &FsPath, factory: PodFactory, config: HostConfig) -> Result<Self, String> {
        std::fs::create_dir_all(dir.join("pods"))
            .map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let store = HostStore::open(&dir.join("host.db"))
            .map_err(|e| format!("opening {}: {e}", dir.join("host.db").display()))?;
        let host = Self {
            dir: dir.to_owned(),
            store,
            factory,
            config,
            open: Mutex::new(HashMap::new()),
            opening: tokio::sync::Mutex::new(()),
            open_pod_id: OnceLock::new(),
            // Per client address (IPv6 per /64). No global limit: a 256-bit
            // code can't be guessed, and a global one lets anyone lock
            // everyone out of entering their code.
            code_tries: RateLimit::new(10, Duration::from_secs(60)),
            temp_creates: RateLimit::new(5, Duration::from_secs(3600)),
            temp_lock: Mutex::new(()),
            wakes: Mutex::new(HashMap::new()),
        };
        if let Some(name) = host.config.open_pod.clone() {
            let id = match host.store.pod_by_name(&name).map_err(|e| e.to_string())? {
                Some(p) => p.id,
                None => {
                    // Today's single-pod behaviour: no settings (all imports run).
                    let made = host.make_pod(&name, PodKind::Admin, None, &[])?;
                    tracing::info!(pod = %name, "open pod created");
                    made.id
                }
            };
            tracing::warn!(pod = %name, "CONSTRUCT_OPEN_POD: callers without a pod token use this pod");
            let _ = host.open_pod_id.set(id);
        }
        Ok(host)
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    pub fn store(&self) -> &HostStore {
        &self.store
    }

    /// The directory of pod `id`. Ids come from `host.db`; anything that is
    /// not a pod id (32 hex digits) is refused before touching the disk.
    fn pod_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_pod_id(id) {
            return Err(format!("refusing the malformed pod id {id:?}"));
        }
        Ok(self.dir.join("pods").join(id))
    }

    /// Create a pod (blocking: opens its database and runs the migrations).
    /// `settings` (with [`HostConfig::new_pod_settings`] under them) are
    /// written before the adapters' migrations.
    pub fn create_pod(
        &self,
        name: &str,
        kind: PodKind,
        lifetime: Option<Duration>,
        settings: &[(String, String)],
    ) -> Result<NewPod, String> {
        let mut all = self.config.new_pod_settings.clone();
        for (k, v) in settings {
            all.retain(|(key, _)| key != k);
            all.push((k.clone(), v.clone()));
        }
        self.make_pod(name, kind, lifetime, &all)
    }

    fn make_pod(
        &self,
        name: &str,
        kind: PodKind,
        lifetime: Option<Duration>,
        settings: &[(String, String)],
    ) -> Result<NewPod, String> {
        if !valid_pod_name(name) {
            return Err(format!(
                "invalid pod name {name:?} (lowercase letters, digits and '-', 1-40)"
            ));
        }
        if self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err(format!("a pod named {name:?} exists"));
        }
        let id = new_pod_id();
        let dir = self.pod_dir(&id)?;
        let result = (|| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let store = Store::open(&dir.join("server.db")).map_err(|e| e.to_string())?;
            for (k, v) in settings {
                store.set_pod_setting(k, v).map_err(|e| e.to_string())?;
            }
            self.finish_pod(&id, name, kind, lifetime, store)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }

    /// Run the adapters over `store`, pin a new token's session and record
    /// the pod.
    fn finish_pod(
        &self,
        id: &str,
        name: &str,
        kind: PodKind,
        lifetime: Option<Duration>,
        store: Store,
    ) -> Result<NewPod, String> {
        let state = (self.factory)(store)?;
        let token = new_pod_token();
        let hash = session_hash(&token);
        state
            .store()
            .create_pinned_session(&hash)
            .map_err(|e| e.to_string())?;
        drop(state);
        let expires_at = lifetime.map(|l| now_secs() + l.as_secs() as i64);
        self.store
            .insert_pod(
                &PodRow {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    kind,
                    created_at: now_secs(),
                    expires_at,
                },
                &hash,
            )
            .map_err(|e| e.to_string())?;
        Ok(NewPod {
            id: id.to_owned(),
            name: name.to_owned(),
            token,
            expires_at,
        })
    }

    /// Make an existing single-pod database (`server.db` from before D46) an
    /// admin pod: a consistent copy (`VACUUM INTO`) is opened and migrated
    /// first; only then is the pod recorded and the original renamed to
    /// `<file>.adopted-<pod id>`. On failure nothing changes.
    pub fn adopt(&self, name: &str, src: &FsPath) -> Result<NewPod, String> {
        if !src.is_file() {
            return Err(format!("{} is not a file", src.display()));
        }
        if !valid_pod_name(name) || self.store.pod_by_name(name).ok().flatten().is_some() {
            return Err(format!("invalid or taken pod name {name:?}"));
        }
        let id = new_pod_id();
        let dir = self.pod_dir(&id)?;
        let dest = dir.join("server.db");
        let result = (|| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            {
                let conn = rusqlite::Connection::open(src).map_err(|e| e.to_string())?;
                conn.busy_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])
                    .map_err(|e| format!("copying {}: {e}", src.display()))?;
            }
            let store = Store::open(&dest).map_err(|e| e.to_string())?;
            self.finish_pod(&id, name, PodKind::Admin, None, store)
        })();
        match result {
            Ok(made) => {
                for suffix in ["", "-wal", "-shm"] {
                    let from = PathBuf::from(format!("{}{suffix}", src.display()));
                    if from.exists() {
                        let to = format!("{}.adopted-{id}{suffix}", src.display());
                        if let Err(e) = std::fs::rename(&from, &to) {
                            tracing::warn!("could not rename {}: {e}", from.display());
                        }
                    }
                }
                Ok(made)
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    /// Delete the originals [`Self::adopt`] left in `dir` once their pod
    /// works (they still hold the old sessions and app tokens): files named
    /// `<anything>.adopted-<pod id>` plus its `-wal` / `-shm`, only for a pod
    /// this host has. Returns the deleted paths. The server image has no
    /// shell (`rm`), so this is the way to remove them on Fly.
    pub fn remove_adopted(&self, dir: &FsPath) -> Result<Vec<PathBuf>, String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut removed = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((_, rest)) = name.rsplit_once(".adopted-") else {
                continue;
            };
            let id = rest
                .strip_suffix("-wal")
                .or_else(|| rest.strip_suffix("-shm"))
                .unwrap_or(rest);
            if !valid_pod_id(id) || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            if self
                .store
                .pods()
                .map_err(|e| e.to_string())?
                .iter()
                .all(|p| p.id != id)
            {
                tracing::warn!(file = %name, "not deleted: no pod with that id");
                continue;
            }
            std::fs::remove_file(entry.path()).map_err(|e| format!("{name}: {e}"))?;
            removed.push(entry.path());
        }
        Ok(removed)
    }

    /// A new token for pod `name`; the old one stops working everywhere and
    /// the pod's sign-ins (made with it) end.
    pub fn rotate(&self, name: &str) -> Result<String, String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        let store =
            Store::open(&self.pod_dir(&pod.id)?.join("server.db")).map_err(|e| e.to_string())?;
        let token = new_pod_token();
        let hash = session_hash(&token);
        store
            .replace_pinned_session(&hash)
            .map_err(|e| e.to_string())?;
        self.store
            .replace_token(&pod.id, &hash)
            .map_err(|e| e.to_string())?;
        Ok(token)
    }

    /// Send pod `name` a test notification (blocking; the admin CLI): the
    /// first channel any adapter declares, stored in the pod's inbox and sent
    /// by Web Push to its devices. A running server's open streams don't
    /// hear it (another process); clients see it on their next refresh.
    /// Returns its title.
    pub fn notify_test(&self, name: &str) -> Result<String, String> {
        let pod = self
            .store
            .live_pod(
                &self
                    .store
                    .pod_by_name(name)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no pod named {name:?}"))?
                    .id,
            )
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("pod {name:?} has expired"))?;
        let store =
            Store::open(&self.pod_dir(&pod.id)?.join("server.db")).map_err(|e| e.to_string())?;
        let state: Pod = Arc::new((self.factory)(store)?);
        state.set_public_prefix(&format!("/v1/p/{}", pod.id));
        let channel = state
            .notification_channels()
            .into_iter()
            .next()
            .ok_or("no app declares a notification channel")?;
        let now = now_secs();
        let title = "Test notification".to_owned();
        let note = pod_core::NewNotification {
            app_id: channel.app_id.clone(),
            channel: channel.id.clone(),
            dedupe_key: format!("test:{now}:{}", &new_pod_id()[..8]),
            title: title.clone(),
            body: format!("Sent with `server pods notify-test` ({})", channel.title),
            image_url: None,
            target_uri: None,
            group_key: None,
            created_at: now,
            quiet: false,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let (_, deliveries) = rt
            .block_on(state.notify_and_deliver(vec![note]))
            .map_err(|(status, body)| format!("{status}: {}", body.0))?;
        tracing::info!(pod = %name, deliveries, "test notification sent");
        Ok(title)
    }

    /// Stop pod `name` now (its token stops working); the sweeper deletes
    /// its files.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        self.store.expire_pod(&pod.id).map_err(|e| e.to_string())
    }

    /// Delete expired pods: close them, remove their files, forget them.
    /// Returns how many went.
    pub fn sweep(&self) -> usize {
        let expired = match self.store.expired_pods() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "sweep: cannot list expired pods");
                return 0;
            }
        };
        let mut swept = 0;
        for pod in expired {
            if let Some(open) = self.cache().remove(&pod.id) {
                open.pod.shutdown();
            }
            let dir = match self.pod_dir(&pod.id) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, "sweep: skipped");
                    continue;
                }
            };
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::error!(pod = %pod.name, error = %e, "sweep: cannot delete the pod's files");
                    continue;
                }
            }
            if let Err(e) = self.store.delete_pod(&pod.id) {
                tracing::error!(pod = %pod.name, error = %e, "sweep: cannot forget the pod");
                continue;
            }
            tracing::info!(pod = %pod.name, kind = ?pod.kind, "pod deleted (expired)");
            swept += 1;
        }
        swept
    }

    /// Run [`Self::sweep`] now and every [`SWEEP_EVERY`].
    pub fn spawn_sweeper(self: &Arc<Self>) {
        let host = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(SWEEP_EVERY);
            loop {
                every.tick().await;
                let Some(h) = host.upgrade() else { return };
                let _ = tokio::task::spawn_blocking(move || h.sweep()).await;
            }
        });
    }

    /// Run due notification polls (D52): every open pod and every pod
    /// `host.db` says is due, at most [`POLL_CONCURRENCY`] at once; each pod's
    /// next wake goes back to `host.db`, so after a restart only pods with
    /// work are opened. Returns how many pods were looked at.
    pub async fn run_due_polls(self: &Arc<Self>) -> usize {
        let now = now_secs();
        let mut ids: Vec<String> = self.cache().keys().cloned().collect();
        match self.blocking(move |h| h.store.due_pods(now)).await {
            Ok(Ok(due)) => {
                for id in due {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            Ok(Err(e)) => tracing::error!(error = %e, "scheduler: cannot list due pods"),
            Err(_) => {}
        }
        let count = ids.len();
        let mut tasks = tokio::task::JoinSet::new();
        for id in ids {
            while tasks.len() >= POLL_CONCURRENCY {
                tasks.join_next().await;
            }
            let host = self.clone();
            tasks.spawn(async move {
                // A pod deleted meanwhile is not reopened.
                let live = host
                    .blocking({
                        let id = id.clone();
                        move |h| h.store.live_pod(&id)
                    })
                    .await;
                if !matches!(live, Ok(Ok(Some(_)))) {
                    return;
                }
                let Ok(open) = host.pod(&id).await else {
                    return;
                };
                let next = open.pod.poll_due().await;
                let unchanged = host
                    .wakes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), next)
                    == Some(next);
                if !unchanged {
                    let _ = host
                        .blocking(move |h| h.store.set_next_wake(&id, next))
                        .await;
                }
            });
        }
        while tasks.join_next().await.is_some() {}
        count
    }

    /// Run [`Self::run_due_polls`] every [`SCHEDULE_EVERY`].
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let host = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(SCHEDULE_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                let Some(h) = host.upgrade() else { return };
                h.run_due_polls().await;
            }
        });
    }

    /// End every open pod's SSE streams (before a graceful shutdown).
    pub fn shutdown(&self) {
        for open in self.cache().values() {
            open.pod.shutdown();
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<OpenPod>>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The open pod `id`, opening it on first use.
    async fn pod(self: &Arc<Self>, id: &str) -> Result<Arc<OpenPod>, Box<Response>> {
        if let Some(p) = self.cache().get(id) {
            return Ok(p.clone());
        }
        let _opening = self.opening.lock().await;
        if let Some(p) = self.cache().get(id) {
            return Ok(p.clone());
        }
        let host = self.clone();
        let pod_id = id.to_owned();
        let built = tokio::task::spawn_blocking(move || {
            let path = host.pod_dir(&pod_id)?.join("server.db");
            if !path.is_file() {
                return Err(format!("{} is missing", path.display()));
            }
            let store = Store::open(&path).map_err(|e| e.to_string())?;
            (host.factory)(store)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        let state = built.map_err(|e| {
            tracing::error!(pod = id, error = %e, "cannot open pod");
            Box::new(json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "pod_unavailable" }),
            ))
        })?;
        state.set_public_prefix(&format!("/v1/p/{id}"));
        let pod: Pod = Arc::new(state);
        let open = Arc::new(OpenPod {
            router: pod_core::app(pod.clone()),
            pod,
        });
        self.cache().insert(id.to_owned(), open.clone());
        Ok(open)
    }

    async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Host) -> T + Send + 'static,
    ) -> Result<T, Box<Response>> {
        let host = self.clone();
        tokio::task::spawn_blocking(move || f(&host))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "host task failed");
                Box::new(json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "host_error" }),
                ))
            })
    }

    /// The live pod `token` opens.
    async fn pod_for_token(self: &Arc<Self>, token: &str) -> Result<Option<String>, Box<Response>> {
        let hash = session_hash(token);
        match self.blocking(move |h| h.store.pod_for_token(&hash)).await? {
            Ok(id) => Ok(id),
            Err(e) => {
                tracing::error!(error = %e, "host store error");
                Err(Box::new(json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "host_error" }),
                )))
            }
        }
    }
}

/// The host's router: its own routes, then every other request forwarded to
/// the caller's pod.
pub fn app(host: Arc<Host>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(screen::OPEN_PATH, post(open_with_token))
        .route(screen::TEMPORARY_PATH, post(temporary_pod))
        .route("/v1/p/{pod_id}/{*rest}", get(public_route))
        .fallback(dispatch)
        .with_state(host)
}

/// The caller's address for rate limits: Fly's `Fly-Client-IP` when the
/// host runs behind Fly (its proxy sets it), else the connection's peer.
/// IPv6 addresses count per /64 (one subscriber usually has a whole /64).
fn client_key(req_headers: &HeaderMap, peer: Option<SocketAddr>, trust_fly: bool) -> String {
    let ip = trust_fly
        .then(|| {
            req_headers
                .get("fly-client-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok())
        })
        .flatten()
        .or_else(|| peer.map(|p| p.ip()));
    match ip {
        Some(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Some(ip) => ip.to_string(),
        None => "unknown".to_owned(),
    }
}

fn peer(req: &Request) -> Option<SocketAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
}

fn rate_limited() -> Response {
    json_error(
        StatusCode::TOO_MANY_REQUESTS,
        json!({ "error": "rate_limited" }),
    )
}

/// Point a request at its pod: the public prefix header is the host's to set.
fn set_prefix(headers: &mut HeaderMap, pod_id: &str) {
    headers.remove(PUBLIC_PREFIX_HEADER);
    if let Ok(v) = HeaderValue::from_str(&format!("/v1/p/{pod_id}")) {
        headers.insert(PUBLIC_PREFIX_HEADER, v);
    }
}

/// Every request that is not the host's: to the bearer's pod (or the open
/// pod), else the answer for a caller without a pod.
async fn dispatch(State(host): State<Arc<Host>>, mut req: Request) -> Response {
    let token = bearer(req.headers()).map(str::to_owned);
    let found = match &token {
        Some(t) => match host.pod_for_token(t).await {
            Ok(found) => found,
            Err(r) => return *r,
        },
        None => None,
    };
    let Some(pod_id) = found.or_else(|| host.open_pod_id.get().cloned()) else {
        return no_pod(&host, &req, token.is_some());
    };
    let open = match host.pod(&pod_id).await {
        Ok(p) => p,
        Err(r) => return *r,
    };
    set_prefix(req.headers_mut(), &pod_id);
    match open.router.clone().oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    }
}

/// A caller without a valid pod token: the "Open your pod" screen for a
/// workspace, an empty app list, else 401 `pod_required`. A token the host
/// does not know also gets `X-Construct-Session: invalid`, so the client
/// forgets it.
fn no_pod(host: &Host, req: &Request, had_token: bool) -> Response {
    let path = req.uri().path();
    let invalid = |mut r: Response| {
        if had_token {
            r.headers_mut()
                .insert(SESSION_HEADER, HeaderValue::from_static("invalid"));
            *r.status_mut() = StatusCode::UNAUTHORIZED;
        }
        r
    };
    if req.method() == Method::GET {
        if let Some(id) = path.strip_prefix("/v1/workspaces/")
            && !id.contains('/')
        {
            if let Some(r) = pod_core::layouts::reject_invalid_workspace_id(id) {
                return r;
            }
            let hours = host.config.temporary.map(|t| t.hours);
            return invalid(Json(screen::open_your_pod(id, hours)).into_response());
        }
        if path == "/v1/apps" {
            return invalid(Json(json!({ "apps": [] })).into_response());
        }
    }
    invalid(json_error(
        StatusCode::UNAUTHORIZED,
        json!({ "error": "pod_required" }),
    ))
}

/// `POST /v1/pods/open {token}`: 200 `{session_token}` when `token` opens a
/// live pod (the client then uses it as its session), 401 `pod_not_found`
/// otherwise. Rate-limited per client and overall.
async fn open_with_token(State(host): State<Arc<Host>>, req: Request) -> Response {
    let key = client_key(req.headers(), peer(&req), host.config.trust_fly_client_ip);
    if !host.code_tries.allow(&key) {
        tracing::warn!("pod code tries rate-limited");
        return rate_limited();
    }
    let headers = req.headers().clone();
    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                json!({ "error": "invalid_request" }),
            );
        }
    };
    let token = match parse_json_body(&headers, &body) {
        Ok(v) => v["token"].as_str().map(|t| t.trim().to_owned()),
        Err(e) => return e.into_response(),
    };
    let Some(token) = token.filter(|t| t.starts_with(TOKEN_PREFIX) && t.len() <= 128) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "pod_not_found" }),
        );
    };
    match host.pod_for_token(&token).await {
        Ok(Some(_)) => Json(json!({ "session_token": token })).into_response(),
        Ok(None) => {
            tracing::info!("pod code rejected");
            json_error(
                StatusCode::UNAUTHORIZED,
                json!({ "error": "pod_not_found" }),
            )
        }
        Err(r) => *r,
    }
}

/// `POST /v1/pods/temporary`: a new temporary pod (when enabled) ->
/// `{session_token, expires_at, pod}`. 404 when disabled, 429 when this
/// client made too many, 503 `temporary_pods_full` at the cap.
async fn temporary_pod(State(host): State<Arc<Host>>, req: Request) -> Response {
    let Some(temp) = host.config.temporary else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "temporary_pods_disabled" }),
        );
    };
    let key = client_key(req.headers(), peer(&req), host.config.trust_fly_client_ip);
    if !host.temp_creates.allow(&key) {
        tracing::warn!("temporary pod creation rate-limited");
        return rate_limited();
    }
    let made = host
        .blocking(move |h| {
            // The cap check and the creation are one step, so a burst can't
            // overshoot the cap.
            let _one_at_a_time = h.temp_lock.lock().unwrap_or_else(|e| e.into_inner());
            if h.store.live_temporary_count().map_err(|e| e.to_string())? >= temp.max_live {
                return Ok(None);
            }
            let mut suffix = [0u8; 4];
            getrandom::fill(&mut suffix).expect("OS random number generator");
            let name = format!("temp-{}", hex(&suffix));
            h.create_pod(
                &name,
                PodKind::Temporary,
                Some(Duration::from_secs(temp.hours * 3600)),
                &h.config.temporary_pod_settings,
            )
            .map(Some)
        })
        .await;
    match made {
        Ok(Ok(Some(pod))) => {
            tracing::info!(pod = %pod.name, "temporary pod created");
            Json(json!({
                "session_token": pod.token,
                "expires_at": pod.expires_at,
                "pod": pod.name,
            }))
            .into_response()
        }
        Ok(Ok(None)) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": "temporary_pods_full" }),
        ),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "cannot create a temporary pod");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "host_error" }),
            )
        }
        Err(r) => *r,
    }
}

/// `GET /v1/p/<pod id>/<path>`: an adapter's public path of that pod
/// ([`pod_core::AppAdapter::public_paths`]), without a bearer.
async fn public_route(
    State(host): State<Arc<Host>>,
    Path((pod_id, rest)): Path<(String, String)>,
    mut req: Request,
) -> Response {
    let not_found = || json_error(StatusCode::NOT_FOUND, json!({ "error": "not_found" }));
    if !valid_pod_id(&pod_id) {
        return not_found();
    }
    let id = pod_id.clone();
    match host.blocking(move |h| h.store.live_pod(&id)).await {
        Ok(Ok(Some(_))) => {}
        Ok(Ok(None)) => return not_found(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "host store error");
            return not_found();
        }
        Err(r) => return *r,
    }
    let path = format!("/{rest}");
    let open = match host.pod(&pod_id).await {
        Ok(p) => p,
        Err(r) => return *r,
    };
    if !open.pod.catalog().is_public_path(&path) {
        return not_found();
    }
    let uri = match req.uri().query() {
        Some(q) => format!("{path}?{q}"),
        None => path,
    };
    let Ok(uri) = uri.parse::<Uri>() else {
        return not_found();
    };
    *req.uri_mut() = uri;
    // The host route's path parameters must not reach the pod's router.
    req.extensions_mut().clear();
    req.headers_mut().remove(header::AUTHORIZATION);
    set_prefix(req.headers_mut(), &pod_id);
    match open.router.clone().oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_names() {
        let t = new_pod_token();
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 52);
        assert!(
            t[5..]
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
        );
        assert_ne!(t, new_pod_token());
        for ok in ["aidan", "temp-1a2b", "a"] {
            assert!(valid_pod_name(ok), "{ok}");
        }
        for bad in ["", "-x", "Aidan", "a b", "a/b", &"x".repeat(41)] {
            assert!(!valid_pod_name(bad), "{bad}");
        }
    }

    #[test]
    fn client_keys() {
        let peer: SocketAddr = "203.0.113.7:4000".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("fly-client-ip", HeaderValue::from_static("198.51.100.1"));
        // Not behind Fly: the header is ignored.
        assert_eq!(client_key(&h, Some(peer), false), "203.0.113.7");
        assert_eq!(client_key(&h, Some(peer), true), "198.51.100.1");
        // Garbage in the header: the peer.
        h.insert(
            "fly-client-ip",
            HeaderValue::from_static("x".repeat(20).leak()),
        );
        assert_eq!(client_key(&h, Some(peer), true), "203.0.113.7");
        // IPv6 per /64.
        h.insert(
            "fly-client-ip",
            HeaderValue::from_static("2001:db8:1:2:aaaa::1"),
        );
        let a = client_key(&h, None, true);
        h.insert(
            "fly-client-ip",
            HeaderValue::from_static("2001:db8:1:2:bbbb::9"),
        );
        assert_eq!(a, client_key(&h, None, true));
        assert_eq!(a, "2001:db8:1:2::/64");
    }
}
