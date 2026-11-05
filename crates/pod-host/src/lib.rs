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
//! screen, the two `/v1/pods/*` calls and the Android sign-in redirect's
//! [`app_links`] routes; images (adapters'
//! [`pod_core::AppAdapter::public_paths`]) load without a bearer from
//! `/v1/p/<pod id>/…`. `CONSTRUCT_OPEN_POD` (development only) serves callers
//! without a token from one named pod, like the single-pod server before D46.

pub mod app_links;
pub mod db;
pub mod limits;
pub mod oauth;
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
use pod_core::metrics;
use pod_core::session::{SESSION_HEADER, bearer, session_hash};
use pod_core::store::Store;
use pod_core::util::{hex, now_secs};
use pod_core::{AppState, PUBLIC_PREFIX_HEADER, Pod, json_error, parse_json_body};
use serde_json::json;
use tower::ServiceExt;

pub use db::{CredentialKind, HostStore, PodKind, PodRow};
use limits::{ClientKeys, RateLimit, TwoLevel};

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
    pod_core::util::create_private_dir(dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))?;
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

/// How much disk the pods may use (decision `pod-storage-limits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageLimits {
    /// Most bytes an admin pod's uploads hold together
    /// (`CONSTRUCT_POD_UPLOAD_QUOTA_BYTES`, default 500 MiB).
    pub pod_upload_quota_bytes: u64,
    /// The same for a temporary pod (`CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES`,
    /// default 25 MiB): anyone can make one.
    pub temporary_upload_quota_bytes: u64,
    /// Uploads and new temporary pods are refused when the data volume has
    /// less free (`CONSTRUCT_MIN_FREE_DISK_BYTES`, default 100 MiB).
    pub min_free_disk_bytes: u64,
}

impl Default for StorageLimits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            pod_upload_quota_bytes: 500 * MIB,
            temporary_upload_quota_bytes: 25 * MIB,
            min_free_disk_bytes: pod_core::disk::DEFAULT_MIN_FREE_BYTES,
        }
    }
}

impl StorageLimits {
    /// The defaults, overridden by the `CONSTRUCT_*` variables above.
    pub fn from_env() -> Self {
        let bytes = |k: &str, default: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        let d = Self::default();
        Self {
            pod_upload_quota_bytes: bytes(
                "CONSTRUCT_POD_UPLOAD_QUOTA_BYTES",
                d.pod_upload_quota_bytes,
            ),
            temporary_upload_quota_bytes: bytes(
                "CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES",
                d.temporary_upload_quota_bytes,
            ),
            min_free_disk_bytes: bytes("CONSTRUCT_MIN_FREE_DISK_BYTES", d.min_free_disk_bytes),
        }
    }
}

/// What the host allows besides admin pods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// `CONSTRUCT_OPEN_POD=<name>`: callers without a valid token use this
    /// pod (created if missing). Development and tests only.
    pub open_pod: Option<String>,
    pub temporary: Option<TemporaryPods>,
    /// Extra settings for temporary pods (anyone can make one), e.g. keeping
    /// an adapter from contacting hosts the visitor picks.
    pub temporary_pod_settings: Vec<(String, String)>,
    /// Rate limits key on the `Fly-Client-IP` header (set by Fly's proxy)
    /// instead of the connection's peer. Only behind Fly (`FLY_APP_NAME`):
    /// anywhere else a client could send any value.
    pub trust_fly_client_ip: bool,
    /// Upload quotas and the low-disk guard.
    pub storage: StorageLimits,
    /// Send `Strict-Transport-Security` on every response. Only behind an
    /// HTTPS proxy (Fly, `FLY_APP_NAME`): on plain HTTP it would be ignored,
    /// and on `localhost` it would pin the name to HTTPS for other servers.
    pub hsts: bool,
    /// `CONSTRUCT_PUBLIC_URL`: the base URL clients reach this server at
    /// (OAuth metadata, decision `mcp-connector`); else from each request's
    /// `Host`.
    pub public_url: Option<String>,
    /// The Android app allowed to open this server's https sign-in redirect
    /// (`CONSTRUCT_ANDROID_PACKAGE`, `CONSTRUCT_ANDROID_CERT_SHA256`; decision
    /// `android-app-link-sign-in-redirect`).
    pub android: app_links::AndroidAppLinks,
    /// A pre-registered OAuth client for MCP apps that can't register
    /// themselves (`CONSTRUCT_OAUTH_CLIENT_ID`, `_SECRET`, `_REDIRECT_URIS`;
    /// decision `oauth-preregistered-client`).
    pub oauth_client: Option<oauth::StaticClient>,
}

impl HostConfig {
    /// `CONSTRUCT_OPEN_POD`, `CONSTRUCT_TEMP_PODS`, `CONSTRUCT_TEMP_POD_HOURS`,
    /// `CONSTRUCT_TEMP_POD_MAX`, [`StorageLimits::from_env`] and
    /// [`app_links::AndroidAppLinks::from_env`].
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
            temporary_pod_settings: Vec::new(),
            trust_fly_client_ip: var("FLY_APP_NAME").is_some(),
            storage: StorageLimits::from_env(),
            hsts: var("FLY_APP_NAME").is_some(),
            public_url: var("CONSTRUCT_PUBLIC_URL"),
            android: app_links::AndroidAppLinks::from_env(),
            oauth_client: oauth::StaticClient::from_env(),
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
    /// OAuth client registrations per client address.
    registrations: RateLimit,
    temp_creates: TwoLevel,
    /// Makes the temporary-pod cap check and the creation one step.
    temp_lock: Mutex<()>,
    /// The next wake last written to `host.db` per pod (skips rewrites).
    wakes: Mutex<HashMap<String, Option<i64>>>,
}

impl Host {
    /// The host over `<dir>/host.db` (created) and `<dir>/pods/`. Both
    /// directories are private to the server's user (0700), the databases
    /// 0600.
    pub fn open(dir: &FsPath, factory: PodFactory, config: HostConfig) -> Result<Self, String> {
        for d in [dir.to_owned(), dir.join("pods")] {
            pod_core::util::create_private_dir(&d)
                .map_err(|e| format!("creating {}: {e}", d.display()))?;
        }
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
            registrations: RateLimit::new(20, Duration::from_secs(3600)),
            // Per IPv4 address or IPv6 /64, and per /48 (security audit #31,
            // M2): a /48 holds 65536 /64s, and a few are enough to take
            // every temporary pod.
            temp_creates: TwoLevel {
                narrow: RateLimit::new(5, Duration::from_secs(3600)),
                wide: RateLimit::new(10, Duration::from_secs(3600)),
            },
            temp_lock: Mutex::new(()),
            wakes: Mutex::new(HashMap::new()),
        };
        if let Some(name) = host.config.open_pod.clone() {
            let id = match host.store.pod_by_name(&name).map_err(|e| e.to_string())? {
                Some(p) => p.id,
                None => {
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
    /// `settings` are written before the adapters' migrations.
    pub fn create_pod(
        &self,
        name: &str,
        kind: PodKind,
        lifetime: Option<Duration>,
        settings: &[(String, String)],
    ) -> Result<NewPod, String> {
        self.make_pod(name, kind, lifetime, settings)
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
            pod_core::util::create_private_dir(&dir).map_err(|e| e.to_string())?;
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
            pod_core::util::create_private_dir(&dir).map_err(|e| e.to_string())?;
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

    /// A new MCP credential for pod `name` (`cmcp_…`, shown once), e.g. for
    /// Claude Code (`claude mcp add --transport http ... --header
    /// "Authorization: Bearer <it>"`); `label` says whose it is.
    pub fn issue_mcp(&self, name: &str, label: &str) -> Result<String, String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        let token = {
            let mut buf = [0u8; 32];
            getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
            format!("{}{}", oauth::MCP_TOKEN_PREFIX, hex(&buf))
        };
        self.store
            .insert_mcp_credential(&pod.id, &session_hash(&token), label)
            .map_err(|e| e.to_string())?;
        Ok(token)
    }

    /// Revoke every MCP credential of pod `name` (Claude is disconnected);
    /// how many went.
    pub fn revoke_mcp(&self, name: &str) -> Result<usize, String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        self.store.revoke_mcp(&pod.id).map_err(|e| e.to_string())
    }

    /// `(label, created_at)` of pod `name`'s MCP credentials.
    pub fn mcp_credentials(&self, name: &str) -> Result<Vec<(String, i64)>, String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        self.store
            .mcp_credentials(&pod.id)
            .map_err(|e| e.to_string())
    }

    /// A new token for pod `name`; the old one stops working everywhere and
    /// the pod's sign-ins (made with it) end. Claude's MCP credentials stay.
    pub fn rotate(&self, name: &str) -> Result<String, String> {
        let token = new_pod_token();
        self.set_token(name, &token)?;
        Ok(token)
    }

    /// For local testing only (`server serve --pod-code … --mcp-key …`): the
    /// admin pod `name`, made if missing, opens with `code`, and `mcp_key`
    /// (if given) is one of its MCP credentials. The same codes then work
    /// across restarts. Changing the code ends the pod's sign-ins, as
    /// [`Self::rotate`] does; an unchanged code changes nothing.
    pub fn ensure_local_pod(
        &self,
        name: &str,
        code: &str,
        mcp_key: Option<&str>,
    ) -> Result<(), String> {
        if !code.starts_with(TOKEN_PREFIX) || code.len() < TOKEN_PREFIX.len() + 8 {
            return Err(format!(
                "a pod code is {TOKEN_PREFIX} and at least 8 more characters"
            ));
        }
        if let Some(k) = mcp_key
            && (!k.starts_with(oauth::MCP_TOKEN_PREFIX)
                || k.len() < oauth::MCP_TOKEN_PREFIX.len() + 8)
        {
            return Err(format!(
                "an MCP key is {} and at least 8 more characters",
                oauth::MCP_TOKEN_PREFIX
            ));
        }
        let pod = match self.store.pod_by_name(name).map_err(|e| e.to_string())? {
            Some(p) => p,
            None => {
                self.make_pod(name, PodKind::Admin, None, &[])?;
                self.store
                    .pod_by_name(name)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("pod {name:?} was not made"))?
            }
        };
        let hash = session_hash(code);
        let current = self.store.pod_for_token(&hash).map_err(|e| e.to_string())?;
        if current.as_deref() != Some(pod.id.as_str()) {
            if current.is_some() {
                return Err("that pod code belongs to another pod".to_owned());
            }
            self.set_token(name, code)?;
        }
        if let Some(k) = mcp_key {
            self.store
                .insert_mcp_credential_if_missing(&pod.id, &session_hash(k), "local testing")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Pod `name` opens with `token` from now on (the old one stops working).
    fn set_token(&self, name: &str, token: &str) -> Result<(), String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        let store =
            Store::open(&self.pod_dir(&pod.id)?.join("server.db")).map_err(|e| e.to_string())?;
        let hash = session_hash(token);
        store
            .replace_pinned_session(&hash)
            .map_err(|e| e.to_string())?;
        self.store
            .replace_token(&pod.id, &hash)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Send pod `name` a test notification (blocking; the admin CLI): the
    /// first channel any adapter declares, stored in the pod's inbox and sent
    /// by Web Push to its devices. A running server's open streams don't
    /// hear it (another process); clients see it on their next refresh.
    /// Returns its title.
    /// Set pod setting `key` of admin pod `name` (e.g. turning on the
    /// monitoring source, decision `server-monitoring`). A running server reads it
    /// on the next request. Temporary pods are refused: anyone can make one.
    pub fn set_admin_pod_setting(&self, name: &str, key: &str, value: &str) -> Result<(), String> {
        let pod = self
            .store
            .pod_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no pod named {name:?}"))?;
        if pod.kind != PodKind::Admin {
            return Err(format!(
                "pod {name:?} is temporary; only admin pods can change this"
            ));
        }
        let store =
            Store::open(&self.pod_dir(&pod.id)?.join("server.db")).map_err(|e| e.to_string())?;
        store.set_pod_setting(key, value).map_err(|e| e.to_string())
    }

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

    /// The gauges for monitoring (decision `server-monitoring`): pods, MCP
    /// credentials and free disk, measured now. Blocking (reads `host.db`).
    pub fn monitoring_gauges(&self) -> Vec<metrics::Gauge> {
        let mut gauges = Vec::new();
        match self.store.monitoring_counts() {
            Ok((admin, temporary, mcp)) => {
                gauges.push(metrics::Gauge {
                    name: "construct_pods",
                    help: "Live pods, by kind.",
                    series: vec![
                        (vec![("kind".into(), "admin".into())], admin as f64),
                        (vec![("kind".into(), "temporary".into())], temporary as f64),
                    ],
                });
                gauges.push(metrics::Gauge::one(
                    "construct_mcp_credentials",
                    "MCP credentials (Claude connections) of live pods.",
                    mcp as f64,
                ));
            }
            Err(e) => tracing::error!(error = %e, "metrics: host store error"),
        }
        if let Some(t) = self.config.temporary {
            gauges.push(metrics::Gauge::one(
                "construct_temporary_pods_max",
                "Most live temporary pods allowed.",
                t.max_live as f64,
            ));
        }
        if let Some(free) = pod_core::disk::free_bytes(&self.dir) {
            gauges.push(metrics::Gauge::one(
                "construct_disk_free_bytes",
                "Free space on the data volume.",
                free as f64,
            ));
        }
        gauges.push(metrics::Gauge::one(
            "construct_disk_min_free_bytes",
            "Free space below which uploads and new temporary pods stop.",
            self.config.storage.min_free_disk_bytes as f64,
        ));
        gauges
    }

    /// True when the data volume is nearly full.
    fn low_on_disk(&self) -> bool {
        pod_core::disk::DiskGuard::new(&self.dir, self.config.storage.min_free_disk_bytes)
            .too_low_for(0)
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
            // Unknown (expired meanwhile): the smaller quota.
            let kind = host
                .store
                .live_pod(&pod_id)
                .map_err(|e| e.to_string())?
                .map_or(PodKind::Temporary, |p| p.kind);
            let storage = host.config.storage;
            let quota = match kind {
                PodKind::Admin => storage.pod_upload_quota_bytes,
                PodKind::Temporary => storage.temporary_upload_quota_bytes,
            };
            let guard = pod_core::disk::DiskGuard::new(&host.dir, storage.min_free_disk_bytes);
            let store = Store::open(&path).map_err(|e| e.to_string())?;
            Ok((host.factory)(store)?
                .with_upload_quota_bytes(Some(quota))
                .with_disk_guard(Some(guard)))
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
        state.set_hosted();
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

    /// The live pod a credential belongs to, and its kind.
    async fn credential(
        self: &Arc<Self>,
        token: &str,
    ) -> Result<Option<(String, CredentialKind)>, Box<Response>> {
        let hash = session_hash(token);
        match self.blocking(move |h| h.store.credential(&hash)).await? {
            Ok(found) => Ok(found),
            Err(e) => {
                tracing::error!(error = %e, "host store error");
                Err(Box::new(json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "host_error" }),
                )))
            }
        }
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
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/v1/mcp",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::authorization_server),
        )
        .route("/oauth/register", post(oauth::register))
        .route(
            "/oauth/authorize",
            get(oauth::authorize_page).post(oauth::authorize_submit),
        )
        .route("/oauth/token", post(oauth::token))
        .route(app_links::ASSET_LINKS_PATH, get(app_links::asset_links))
        .route(app_links::REDIRECT_PATH, get(app_links::redirect_page))
        .fallback(dispatch)
        .with_state(host.clone())
        .layer(axum::middleware::map_response_with_state(
            host.config.hsts,
            security_headers,
        ))
        .layer(axum::middleware::from_fn(count_responses))
}

/// Count every response by status code (decision `server-monitoring`), except the
/// health check Fly polls every few seconds.
async fn count_responses(req: Request, next: axum::middleware::Next) -> Response {
    let health = req.uri().path() == "/healthz";
    let resp = next.run(req).await;
    if !health {
        metrics::inc(
            metrics::HTTP_RESPONSES,
            &[("status", resp.status().as_str())],
        );
    }
    resp
}

/// Headers on every response, unless the route set its own (an upload's
/// `sandbox` CSP stays). Clients only fetch JSON, SSE and images, so a page
/// that loads one of these in a browser gets nothing it could run or frame.
async fn security_headers(State(hsts): State<bool>, mut resp: Response) -> Response {
    let h = resp.headers_mut();
    let defaults = [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    for (name, value) in defaults {
        h.entry(name)
            .or_insert_with(|| HeaderValue::from_static(value));
    }
    if hsts {
        h.entry(header::STRICT_TRANSPORT_SECURITY)
            .or_insert_with(|| HeaderValue::from_static("max-age=31536000"));
    }
    resp
}

/// Whether a request may use the open pod: its `Host` (or HTTP/2
/// authority) must be `localhost`, a `*.localhost` name or an IP literal
/// (`127.0.0.1`, `[::1]`, the emulator's `10.0.2.2`, a LAN address), any
/// port. A DNS-rebinding page always sends its own domain name, so it never
/// passes; a request without a Host (HTTP/1.0, in-process tests) does not
/// come from a browser and passes.
pub fn open_pod_host_allowed(headers: &HeaderMap, uri: &Uri) -> bool {
    let host = match headers.get(header::HOST) {
        Some(v) => match v.to_str() {
            Ok(v) => v.trim().to_owned(),
            Err(_) => return false,
        },
        None => match uri.authority() {
            Some(a) => a.as_str().rsplit('@').next().unwrap_or_default().to_owned(),
            None => return true,
        },
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        // `[v6]` or `[v6]:port`.
        let Some((v6, port)) = rest.split_once(']') else {
            return false;
        };
        if !(port.is_empty() || port.strip_prefix(':').is_some_and(valid_port)) {
            return false;
        }
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if valid_port(port) => name,
            Some(_) => return false,
            None => host.as_str(),
        }
    };
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.parse::<std::net::Ipv4Addr>().is_ok()
}

fn valid_port(p: &str) -> bool {
    !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
}

/// The caller's address for rate limits: Fly's `Fly-Client-IP` when the
/// host runs behind Fly (its proxy sets it), else the connection's peer.
/// IPv6 addresses count per /64 (one subscriber usually has a whole /64)
/// and per /48 ([`ClientKeys`]).
pub(crate) fn client_keys(
    req_headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trust_fly: bool,
) -> ClientKeys {
    let ip = trust_fly
        .then(|| {
            req_headers
                .get("fly-client-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok())
        })
        .flatten()
        .or_else(|| peer.map(|p| p.ip()));
    ClientKeys::of(ip)
}

pub(crate) fn peer(req: &Request) -> Option<SocketAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
}

pub(crate) fn rate_limited() -> Response {
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
    // Headers only the host (or the pod itself) may set.
    req.headers_mut().remove(pod_core::CREDENTIAL_HEADER);
    req.headers_mut().remove(pod_core::session::INTERNAL_HEADER);
    let token = bearer(req.headers()).map(str::to_owned);
    let credential = match &token {
        Some(t) => match host.credential(t).await {
            Ok(found) => found,
            Err(r) => return *r,
        },
        None => None,
    };
    // The MCP endpoint (decision `mcp-connector`): an MCP credential or the
    // pod token, never the open pod; an MCP credential reaches nothing else.
    let is_mcp = req.uri().path() == oauth::MCP_PATH;
    let found = match credential {
        Some((id, CredentialKind::Mcp)) if is_mcp => {
            req.headers_mut().remove(header::AUTHORIZATION);
            req.headers_mut()
                .insert(pod_core::CREDENTIAL_HEADER, HeaderValue::from_static("mcp"));
            Some(id)
        }
        Some((_, CredentialKind::Mcp)) => {
            metrics::inc(metrics::AUTH_FAILURES, &[("kind", "mcp_scope")]);
            return json_error(
                StatusCode::FORBIDDEN,
                json!({ "error": "mcp_credential_scope",
                        "detail": "this credential only reaches the pod's MCP endpoint" }),
            );
        }
        Some((id, CredentialKind::PodToken)) => Some(id),
        None if is_mcp => return oauth::mcp_unauthorized(&host, req.headers()),
        None => None,
    };
    let pod_id = match (found, host.open_pod_id.get()) {
        (Some(id), _) => id,
        (None, Some(open)) if open_pod_host_allowed(req.headers(), req.uri()) => open.clone(),
        (None, Some(_)) => {
            // A DNS-rebinding page names its own domain: no open pod for it.
            tracing::warn!(
                host = ?req.headers().get(header::HOST),
                "open pod refused for a Host that is not localhost or an IP address"
            );
            metrics::inc(metrics::AUTH_FAILURES, &[("kind", "open_pod_host")]);
            return json_error(
                StatusCode::FORBIDDEN,
                json!({ "error": "host_not_allowed" }),
            );
        }
        (None, None) => return no_pod(&host, &req, token.is_some()),
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
    if had_token {
        metrics::inc(metrics::AUTH_FAILURES, &[("kind", "unknown_token")]);
    }
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
/// otherwise. Rate-limited per client.
///
/// The limit keeps the code screen from being used to hammer `host.db`; it is
/// not what stops guessing. Any route takes `Bearer cpod_…` without it
/// (security audit #31, Info), and that is fine: a code is 256 random bits,
/// so no rate of guesses finds one. Limiting failed bearers on every route
/// would add nothing against guessing, but would let one client behind a
/// shared address (CGNAT) lock its neighbours out of their pods.
async fn open_with_token(State(host): State<Arc<Host>>, req: Request) -> Response {
    let key = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip).narrow;
    if !host.code_tries.allow(&key) {
        tracing::warn!("pod code tries rate-limited");
        metrics::inc(metrics::RATE_LIMITED, &[("limit", "pod_code")]);
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
            metrics::inc(metrics::AUTH_FAILURES, &[("kind", "pod_code")]);
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
/// client (or its IPv6 /48) made too many, 503 `temporary_pods_full` at the
/// cap or `low_disk_space` when the data volume is nearly full.
async fn temporary_pod(State(host): State<Arc<Host>>, req: Request) -> Response {
    let Some(temp) = host.config.temporary else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "temporary_pods_disabled" }),
        );
    };
    let keys = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip);
    if !host.temp_creates.allow(&keys) {
        tracing::warn!("temporary pod creation rate-limited");
        metrics::inc(metrics::RATE_LIMITED, &[("limit", "temporary_pod")]);
        return rate_limited();
    }
    if host.low_on_disk() {
        tracing::warn!("temporary pod refused: low disk space");
        metrics::inc(metrics::TEMPORARY_PODS, &[("outcome", "low_disk")]);
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": "low_disk_space" }),
        );
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
            metrics::inc(metrics::TEMPORARY_PODS, &[("outcome", "created")]);
            Json(json!({
                "session_token": pod.token,
                "expires_at": pod.expires_at,
                "pod": pod.name,
            }))
            .into_response()
        }
        Ok(Ok(None)) => {
            metrics::inc(metrics::TEMPORARY_PODS, &[("outcome", "full")]);
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "temporary_pods_full" }),
            )
        }
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

    /// A route's own headers (an upload's `sandbox` CSP) are kept.
    #[tokio::test]
    async fn security_headers_keep_a_routes_own() {
        let mut resp = Response::new(axum::body::Body::empty());
        resp.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox"),
        );
        let resp = security_headers(State(true), resp).await;
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], "sandbox");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
        assert_eq!(h.get_all(header::CONTENT_SECURITY_POLICY).iter().count(), 1);
    }

    /// Security audit #31 (L1): only names a DNS-rebinding page can't send.
    #[test]
    fn open_pod_hosts() {
        let check = |host: Option<&str>, uri: &str| {
            let mut h = HeaderMap::new();
            if let Some(v) = host {
                h.insert(header::HOST, HeaderValue::from_str(v).unwrap());
            }
            open_pod_host_allowed(&h, &uri.parse().unwrap())
        };
        for ok in [
            "localhost",
            "localhost:8080",
            "LocalHost.:8080",
            "app.localhost:1",
            "127.0.0.1",
            "127.0.0.1:8080",
            "10.0.2.2:8080",
            "192.168.1.20:8080",
            "[::1]",
            "[::1]:8080",
        ] {
            assert!(check(Some(ok), "/v1/apps"), "{ok}");
        }
        for bad in [
            "evil.example",
            "evil.example:8080",
            "localhost.evil.example",
            "127.0.0.1.nip.io:8080",
            "localhost:80x",
            "[::1]x",
            "[nope]:8080",
            "",
        ] {
            assert!(!check(Some(bad), "/v1/apps"), "{bad}");
        }
        assert!(check(None, "/v1/apps"), "no Host: not a browser");
        assert!(
            check(None, "http://localhost:8080/v1/apps"),
            "HTTP/2 authority"
        );
        assert!(!check(None, "http://evil.example:8080/v1/apps"));
    }

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
        for ok in ["dana", "temp-1a2b", "a"] {
            assert!(valid_pod_name(ok), "{ok}");
        }
        for bad in ["", "-x", "Dana", "a b", "a/b", &"x".repeat(41)] {
            assert!(!valid_pod_name(bad), "{bad}");
        }
    }

    #[test]
    fn client_keys() {
        let peer: SocketAddr = "203.0.113.7:4000".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("fly-client-ip", HeaderValue::from_static("198.51.100.1"));
        // Not behind Fly: the header is ignored.
        let client_key = |h: &HeaderMap, p, trust| super::client_keys(h, p, trust).narrow;
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
        assert_eq!(
            super::client_keys(&h, None, true).wide.as_deref(),
            Some("2001:db8:1::/48")
        );
    }
}
