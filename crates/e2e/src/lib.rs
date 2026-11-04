//! End-to-end harness (docs/testing.md "End-to-end suite", docs/DECISIONS.md D16/D20).
//!
//! Runs the real pod (`server::app`, in-memory store) and the client-core engine in-process
//! against a Pixelfed upstream chosen by `E2E_UPSTREAM`:
//!
//! - `fake` (default): the fake (`fake_pixelfed`) in-process on a free port, or an already
//!   running one (`E2E_FAKE_URL`, e.g. the binary or its Docker image; its `/_admin` routes
//!   must be on). The "browser" GETs the authorize URL with `auto=1`.
//! - `real`: a real Pixelfed (`PIXELFED_INSTANCE_URL`, default `http://localhost:8090`, the
//!   local dev stack). The browser scripts Pixelfed's web login and consent form as
//!   `E2E_PIXELFED_LOGIN` / `E2E_PIXELFED_PASSWORD` (default `alice` / `alice-dev-pass`); the
//!   upstream state is checked with `E2E_PIXELFED_TOKEN` (default
//!   `dev/pixelfed/.seed/<login>_token`).
//!
//! The engine reaches the pod through a TCP proxy that can be switched off, so a test can take
//! the client offline without touching the pod.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::middleware::{self, Next};
use client_core::{
    ConstructEngine, EngineConfig, EngineOptions, MemoryStore, RedirectMode, SecureStore,
    StateSnapshot, UrlOpener,
};
use serde_json::{Value, json};
use server::{AppState, PixelfedConfig, WithPixelfed};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

pub const WORKSPACE: &str = "pixelfed";
pub const LIKE_ENDPOINT: &str = "/v1/pixelfed/posts/like";
pub const BOOKMARK_ENDPOINT: &str = "/v1/pixelfed/posts/bookmark";
const LIKE_COUNT_EXPR: &str = "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1";
/// Default real instance: the local dev stack (`scripts/pixelfed_dev.sh`).
pub const DEFAULT_REAL_URL: &str = "http://localhost:8090";

/// Engine timings shortened for tests (backoff 50 ms, 100 ms, 200 ms, 400 ms cap).
pub fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(400),
    }
}

/// Which upstream the harness runs against.
pub enum Upstream {
    /// The fake, in-process (`state` set) or external (admin routes over HTTP).
    Fake {
        url: String,
        state: Option<Arc<fake_pixelfed::AppState>>,
    },
    /// A real Pixelfed instance.
    Real {
        url: String,
        login: String,
        password: String,
        /// The signing-in user's API token, to check upstream state.
        token: String,
    },
}

impl Upstream {
    /// From the environment (see the crate docs).
    pub fn from_env() -> UpstreamChoice {
        match std::env::var("E2E_UPSTREAM").as_deref() {
            Ok("real") => {
                let url = std::env::var("PIXELFED_INSTANCE_URL")
                    .ok()
                    .filter(|u| !u.is_empty())
                    .unwrap_or_else(|| DEFAULT_REAL_URL.to_owned());
                let login = env_or("E2E_PIXELFED_LOGIN", "alice");
                let password = env_or("E2E_PIXELFED_PASSWORD", &format!("{login}-dev-pass"));
                let token = std::env::var("E2E_PIXELFED_TOKEN")
                    .ok()
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        let p = seed_dir().join(format!("{login}_token"));
                        std::fs::read_to_string(p).ok().map(|t| t.trim().to_owned())
                    })
                    .expect(
                        "E2E_UPSTREAM=real needs E2E_PIXELFED_TOKEN or dev/pixelfed/.seed/<login>_token \
                         (scripts/pixelfed_dev.sh seed)",
                    );
                UpstreamChoice::Real {
                    url,
                    login,
                    password,
                    token,
                }
            }
            Ok("fake") | Ok("") | Err(_) => match std::env::var("E2E_FAKE_URL") {
                Ok(url) if !url.is_empty() => UpstreamChoice::FakeExternal(url),
                _ => UpstreamChoice::Fake,
            },
            Ok(other) => panic!("E2E_UPSTREAM must be fake or real, not {other:?}"),
        }
    }

    pub fn url(&self) -> &str {
        match self {
            Self::Fake { url, .. } | Self::Real { url, .. } => url,
        }
    }

    pub fn is_real(&self) -> bool {
        matches!(self, Self::Real { .. })
    }

    /// The in-process fake (fault tests).
    pub fn fake(&self) -> &Arc<fake_pixelfed::AppState> {
        match self {
            Self::Fake {
                state: Some(state), ..
            } => state,
            _ => panic!("not an in-process fake"),
        }
    }

    /// `(favourited, bookmarked)` of a status, as the upstream sees it.
    pub fn flags(&self, id: &str) -> (bool, bool) {
        let http = reqwest::blocking::Client::new();
        match self {
            Self::Fake { url, .. } => {
                let report: Value = http
                    .get(format!("{url}/_admin/state?tail=0"))
                    .send()
                    .and_then(|r| r.error_for_status())
                    .expect("GET /_admin/state (is FAKE_PIXELFED_ADMIN on?)")
                    .json()
                    .unwrap();
                let has = |key: &str| {
                    report[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == Some(id))
                };
                (has("favourited"), has("bookmarked"))
            }
            Self::Real { url, token, .. } => {
                let s: Value = http
                    .get(format!("{url}/api/v1/statuses/{id}"))
                    .bearer_auth(token)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .expect("GET /api/v1/statuses/{id}")
                    .json()
                    .unwrap();
                (truthy(&s["favourited"]), truthy(&s["bookmarked"]))
            }
        }
    }

    /// Fake: how many polls new v2 uploads stay "processing" (in-process, or `POST
    /// /_admin/media` on an external fake). Panics on a real upstream.
    pub fn set_media_processing_polls(&self, polls: u32) {
        match self {
            Self::Fake {
                state: Some(state), ..
            } => state.set_media_processing_polls(polls),
            Self::Fake { url, state: None } => {
                reqwest::blocking::Client::new()
                    .post(format!("{url}/_admin/media"))
                    .json(&json!({ "processing_polls": polls }))
                    .send()
                    .and_then(|r| r.error_for_status())
                    .expect("POST /_admin/media (is FAKE_PIXELFED_ADMIN on?)");
            }
            Self::Real { .. } => panic!("media processing polls need the fake"),
        }
    }

    /// Fake: ids of the statuses created since the last reset, oldest first (in-process, or
    /// `created` of `GET /_admin/state` on an external fake). Panics on a real upstream.
    pub fn created_ids(&self) -> Vec<String> {
        match self {
            Self::Fake {
                state: Some(state), ..
            } => state.created_ids(),
            Self::Fake { url, state: None } => {
                let report: Value = reqwest::blocking::Client::new()
                    .get(format!("{url}/_admin/state?tail=0"))
                    .send()
                    .and_then(|r| r.error_for_status())
                    .expect("GET /_admin/state (is FAKE_PIXELFED_ADMIN on?)")
                    .json()
                    .unwrap();
                report["created"]
                    .as_array()
                    .expect("`created` in /_admin/state")
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            }
            Self::Real { .. } => panic!("created ids need the fake"),
        }
    }

    /// Fake: `POST /_admin/reset` (fixture state, no apps or tokens). Real: nothing.
    pub fn reset(&self) {
        if let Self::Fake { url, .. } = self {
            reqwest::blocking::Client::new()
                .post(format!("{url}/_admin/reset"))
                .send()
                .and_then(|r| r.error_for_status())
                .expect("POST /_admin/reset (is FAKE_PIXELFED_ADMIN on?)");
        }
    }

    /// The browser that approves sign-in on this upstream.
    pub fn browser(&self) -> Arc<dyn UrlOpener> {
        match self {
            Self::Fake { .. } => Arc::new(AutoApproveBrowser),
            Self::Real {
                login, password, ..
            } => Arc::new(PixelfedLoginBrowser::new(login, password)),
        }
    }

    /// The username the signed-in snapshot should show.
    pub fn expected_username(&self) -> String {
        match self {
            Self::Fake { .. } => "constructdemo".to_owned(),
            Self::Real { login, .. } => login.split('@').next().unwrap_or(login).to_owned(),
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => matches!(s.as_str(), "1" | "true"),
        _ => false,
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn seed_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/pixelfed/.seed")
}

/// [`Upstream::from_env`]'s answer, before anything is started.
pub enum UpstreamChoice {
    Fake,
    FakeExternal(String),
    Real {
        url: String,
        login: String,
        password: String,
        token: String,
    },
}

/// One request the pod answered.
#[derive(Clone, Debug)]
pub struct Hit {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub at: Instant,
}

/// Upstream + pod + switchable proxy, on one Tokio runtime. Dropping it stops everything.
pub struct Harness {
    pub upstream: Upstream,
    pub pod_state: Arc<AppState>,
    /// The URL engines use (through the proxy).
    pub pod_url: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    proxy: Proxy,
    rt: Option<Runtime>,
}

impl Harness {
    /// The upstream chosen by the environment.
    pub fn from_env() -> Self {
        Self::start(Upstream::from_env())
    }

    /// Always the in-process fake.
    pub fn fake() -> Self {
        Self::start(UpstreamChoice::Fake)
    }

    pub fn start(choice: UpstreamChoice) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let upstream = match choice {
            UpstreamChoice::Fake => {
                let listener = rt
                    .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
                    .unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let state = Arc::new(
                    fake_pixelfed::AppState::new(fake_pixelfed::Config::embedded(&url)).unwrap(),
                );
                let app = fake_pixelfed::app(Arc::clone(&state));
                rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
                Upstream::Fake {
                    url,
                    state: Some(state),
                }
            }
            UpstreamChoice::FakeExternal(url) => Upstream::Fake {
                url: url.trim_end_matches('/').to_owned(),
                state: None,
            },
            UpstreamChoice::Real {
                url,
                login,
                password,
                token,
            } => Upstream::Real {
                url: url.trim_end_matches('/').to_owned(),
                login,
                password,
                token,
            },
        };

        let pod_state = Arc::new(
            server::state_in_memory().with_pixelfed(PixelfedConfig::new(upstream.url(), None)),
        );
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let log = Arc::clone(&hits);
        let app = server::app(Arc::clone(&pod_state)).layer(middleware::from_fn(
            move |req: Request, next: Next| {
                let log = Arc::clone(&log);
                async move {
                    let method = req.method().to_string();
                    let path = req.uri().path().to_owned();
                    let resp = next.run(req).await;
                    log.lock().unwrap().push(Hit {
                        method,
                        path,
                        status: resp.status().as_u16(),
                        at: Instant::now(),
                    });
                    resp
                }
            },
        ));
        let pod_listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let pod_addr = pod_listener.local_addr().unwrap();
        rt.spawn(async move { axum::serve(pod_listener, app).await.unwrap() });

        let proxy = Proxy::new(rt.handle().clone(), pod_addr);
        proxy.set_online(true);
        Self {
            upstream,
            pod_state,
            pod_url: format!("http://{}", proxy.addr),
            hits,
            proxy,
            rt: Some(rt),
        }
    }

    /// An engine on the `pixelfed` workspace with an in-memory cache, this upstream's
    /// browser and a loopback redirect on a random port.
    pub fn engine(&self, store: &Arc<MemoryStore>) -> ConstructEngine {
        self.engine_with(store, Some(self.upstream.browser()))
    }

    pub fn engine_with(
        &self,
        store: &Arc<MemoryStore>,
        opener: Option<Arc<dyn UrlOpener>>,
    ) -> ConstructEngine {
        self.engine_on(store, opener, WORKSPACE)
    }

    /// An engine on app `workspace` (same cache and redirect as [`Self::engine`]).
    pub fn engine_on(
        &self,
        store: &Arc<MemoryStore>,
        opener: Option<Arc<dyn UrlOpener>>,
        workspace: &str,
    ) -> ConstructEngine {
        ConstructEngine::init_with_options(
            "",
            &self.pod_url,
            workspace,
            EngineOptions {
                config: fast_config(),
                secure_store: Some(Arc::clone(store) as Arc<dyn SecureStore>),
                url_opener: opener,
                redirect: RedirectMode::Loopback,
                loopback_port: 0,
            },
        )
    }

    /// The `action.auth.login` intent for this upstream.
    pub fn login_intent(&self) -> String {
        json!({
            "intent": "action.auth.login",
            "instance_url": self.upstream.url(),
            "app_path": "/v1/auth/pixelfed/app",
            "token_path": "/v1/auth/pixelfed/token",
        })
        .to_string()
    }

    /// Every request the pod answered so far.
    pub fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    /// `(path, status)` of the pod's answers to `POST <path>`.
    pub fn posts_to(&self, path: &str) -> Vec<u16> {
        self.hits()
            .into_iter()
            .filter(|h| h.method == "POST" && h.path == path)
            .map(|h| h.status)
            .collect()
    }

    /// Switch the client's path to the pod on or off (off drops open connections and
    /// refuses new ones; the pod itself keeps running).
    pub fn set_pod_reachable(&self, on: bool) {
        self.proxy.set_online(on);
    }

    /// Run `f` on the harness's runtime (e.g. the pod's notification poll,
    /// `pod_state.poll_due()`, without waiting for a scheduler).
    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.as_ref().expect("the runtime runs").block_on(f)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.proxy.set_online(false);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// A TCP forwarder `addr -> target` that can drop everything and come back on the same port.
struct Proxy {
    addr: SocketAddr,
    target: SocketAddr,
    handle: tokio::runtime::Handle,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Proxy {
    fn new(handle: tokio::runtime::Handle, target: SocketAddr) -> Self {
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        Self {
            addr,
            target,
            handle,
            tasks: Arc::default(),
        }
    }

    fn set_online(&self, on: bool) {
        let mut tasks = self.tasks.lock().unwrap();
        for t in tasks.drain(..) {
            t.abort();
        }
        if !on {
            return;
        }
        let (addr, target) = (self.addr, self.target);
        let conns = Arc::clone(&self.tasks);
        // Rebinding right after a close: SO_REUSEADDR, and retry while the old listener's
        // aborted task is being dropped.
        let deadline = Instant::now() + Duration::from_secs(5);
        let listener = loop {
            let attempt = self.handle.block_on(async {
                let socket = tokio::net::TcpSocket::new_v4()?;
                socket.set_reuseaddr(true)?;
                socket.bind(addr)?;
                socket.listen(1024)
            });
            match attempt {
                Ok(l) => break l,
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("proxy cannot rebind {addr}: {e}"),
            }
        };
        tasks.push(self.handle.spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    continue;
                };
                let conn = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                conns.lock().unwrap().push(conn);
            }
        }));
    }
}

// ---------- browsers ----------

/// Approves on the fake: GET `<authorize>&auto=1` (302 to the loopback redirect), followed
/// on a thread (the engine serves its listener once `open` returns).
pub struct AutoApproveBrowser;

impl UrlOpener for AutoApproveBrowser {
    fn open(&self, url: &str) -> bool {
        let url = format!("{url}&auto=1");
        thread::spawn(move || {
            let r = reqwest::blocking::Client::new().get(&url).send();
            if let Err(e) = r {
                eprintln!("[e2e] browser: {e}");
            }
        });
        true
    }
}

/// Signs in on a real Pixelfed like a person would (port of the simulator's
/// `PixelfedLoginBrowser`, docs/DECISIONS.md D21): `GET /login` for the CSRF token, `POST
/// /login` (by e-mail; a bare login gets `@example.test`, the seed domain), `GET` the
/// authorize URL, `POST` the consent page's approve form (not the `_method=DELETE` one),
/// then follow the redirect to the loopback listener on a thread. Own cookie jar, no
/// automatic redirects.
pub struct PixelfedLoginBrowser {
    email: String,
    password: String,
}

impl PixelfedLoginBrowser {
    pub fn new(login: &str, password: &str) -> Self {
        let email = if login.contains('@') {
            login.to_owned()
        } else {
            format!("{login}@example.test")
        };
        Self {
            email,
            password: password.to_owned(),
        }
    }

    fn sign_in(&self, url: &str) -> Result<(), String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        let redirect_uri = parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .ok_or("authorize URL has no redirect_uri")?;
        let origin = parsed.origin().ascii_serialization();
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            // Pixelfed records the login device and fails (500) without a User-Agent.
            .user_agent("construct-e2e/0.1 (scripted browser)")
            .build()
            .unwrap();
        let mut jar = CookieJar::default();

        let page = jar.send(http.get(format!("{origin}/login")))?;
        if page.0 != 200 {
            return Err(format!("GET /login answered {}", page.0));
        }
        let csrf = hidden_inputs(&page.2)
            .into_iter()
            .find(|(k, _)| k == "_token")
            .map(|(_, v)| v)
            .ok_or("no _token on the login page")?;
        let (status, location, _) = jar.send(http.post(format!("{origin}/login")).form(&[
            ("_token", csrf.as_str()),
            ("email", self.email.as_str()),
            ("password", self.password.as_str()),
        ]))?;
        if !(300..400).contains(&status)
            || location.split('?').next().unwrap_or("").ends_with("/login")
        {
            return Err(format!(
                "login as {} rejected ({status} -> {location})",
                self.email
            ));
        }
        eprintln!(
            "[e2e] pixelfed login: signed in to the web UI as {}",
            self.email
        );

        let (status, location, body) = jar.send(http.get(url))?;
        let location = match status {
            300..=399 => location,
            200 => {
                let (action, fields) =
                    approve_form(&body).ok_or("no approve form on the authorize page")?;
                let (status, location, _) = jar.send(http.post(action).form(&fields))?;
                if !(300..400).contains(&status) {
                    return Err(format!("approve POST answered {status}"));
                }
                eprintln!("[e2e] pixelfed login: consent approved");
                location
            }
            s => return Err(format!("authorize page answered {s}")),
        };
        if !location.starts_with(&redirect_uri) {
            return Err(format!(
                "authorize redirected to {location:?}, not {redirect_uri}"
            ));
        }
        thread::spawn(move || {
            if let Err(e) = reqwest::blocking::get(&location) {
                eprintln!("[e2e] pixelfed login: following the redirect failed: {e}");
            }
        });
        Ok(())
    }
}

impl UrlOpener for PixelfedLoginBrowser {
    fn open(&self, url: &str) -> bool {
        match self.sign_in(url) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[e2e] pixelfed login failed: {e}. Open: {url}");
                false
            }
        }
    }
}

/// Minimal cookie jar: name -> value from every `Set-Cookie`, sent back on every request.
#[derive(Default)]
struct CookieJar(HashMap<String, String>);

impl CookieJar {
    /// Sends `req` with the cookies; returns (status, Location, body).
    fn send(
        &mut self,
        req: reqwest::blocking::RequestBuilder,
    ) -> Result<(u16, String, String), String> {
        let cookie = self
            .0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let resp = req
            .header("Accept", "text/html")
            .header("Cookie", cookie)
            .send()
            .map_err(|e| e.to_string())?;
        for v in resp.headers().get_all("set-cookie") {
            if let Some((k, v)) = v
                .to_str()
                .ok()
                .and_then(|s| s.split(';').next())
                .and_then(|kv| kv.split_once('='))
            {
                self.0.insert(k.trim().to_owned(), v.trim().to_owned());
            }
        }
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        Ok((status, location, resp.text().unwrap_or_default()))
    }
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of `name="..."` in a tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if before_ok && let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                let start = tag.len() - rest.len();
                let end = rest.find('"')?;
                return Some(unescape(&tag[start..start + end]));
            }
        }
    }
    None
}

/// Every `<tag ...>` opening tag in `html` (case-insensitive).
fn tags<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&open) {
        let start = from + i;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        out.push(&html[start..start + end + 1]);
        from = start + end + 1;
    }
    out
}

/// name -> value of every hidden input (first occurrence wins), in order.
fn hidden_inputs(html: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for tag in tags(html, "input") {
        if !attr(tag, "type").is_some_and(|t| t.eq_ignore_ascii_case("hidden")) {
            continue;
        }
        if let Some(name) = attr(tag, "name")
            && !out.iter().any(|(k, _)| *k == name)
        {
            out.push((name, attr(tag, "value").unwrap_or_default()));
        }
    }
    out
}

/// (action, hidden fields) of the first form posting to `/oauth/authorize` that is not the
/// deny form (`_method=DELETE`).
fn approve_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<form") {
        let start = from + i;
        let open_end = start + lower[start..].find('>')?;
        let close = open_end + lower[open_end..].find("</form>")?;
        from = close;
        let action = attr(&html[start..=open_end], "action")?;
        let fields = hidden_inputs(&html[open_end..close]);
        if !action
            .split('?')
            .next()
            .unwrap_or("")
            .ends_with("/oauth/authorize")
        {
            continue;
        }
        if fields
            .iter()
            .any(|(k, v)| k == "_method" && v.eq_ignore_ascii_case("DELETE"))
        {
            continue;
        }
        return Some((action, fields));
    }
    None
}

// ---------- snapshot helpers ----------

pub const TIMEOUT: Duration = Duration::from_secs(20);

pub fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn auth(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.auth_json).unwrap()
}

pub fn queries(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.queries_json).unwrap()
}

pub fn entities(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.entities_json).unwrap()
}

/// Post URIs of the home feed, in order.
pub fn feed(snap: &StateSnapshot) -> Vec<String> {
    queries(snap)["timeline:home"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The post entity `pixelfed://posts/<id>`.
pub fn post(snap: &StateSnapshot, id: &str) -> Value {
    entities(snap)[format!("pixelfed://posts/{id}")].clone()
}

pub fn wait_signed_in(engine: &ConstructEngine) {
    wait_until("signed in with the home feed", TIMEOUT, || {
        let s = engine.snapshot();
        auth(&s)["state"] == "signed_in" && !feed(&s).is_empty()
    });
}

/// A feed post that is neither liked nor bookmarked: (id, like_count).
pub fn fresh_post(snap: &StateSnapshot) -> (String, i64) {
    let ents = entities(snap);
    feed(snap)
        .iter()
        .map(|u| &ents[u.as_str()])
        .find(|p| p["is_liked"] == false && p["is_bookmarked"] == false)
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["like_count"].as_i64().unwrap_or(0),
            )
        })
        .expect("a post that is neither liked nor bookmarked")
}

/// Toggle `is_liked` (with the like count), as the feed's heart button does.
pub fn like_intent(id: &str) -> String {
    json!({
        "intent": "action.mutation.optimistic",
        "entity_uri": format!("pixelfed://posts/{id}"),
        "field": "is_liked",
        "value_expr": "!entity.is_liked",
        "rollback_value": false,
        "endpoint": LIKE_ENDPOINT,
        "payload": { "id": id, "liked": "$value" },
        "also": [{ "field": "like_count", "value_expr": LIKE_COUNT_EXPR }],
    })
    .to_string()
}

/// Toggle `is_bookmarked`, as the feed's bookmark button does.
pub fn bookmark_intent(id: &str) -> String {
    json!({
        "intent": "action.mutation.optimistic",
        "entity_uri": format!("pixelfed://posts/{id}"),
        "field": "is_bookmarked",
        "value_expr": "!entity.is_bookmarked",
        "rollback_value": false,
        "endpoint": BOOKMARK_ENDPOINT,
        "payload": { "id": id, "bookmarked": "$value" },
    })
    .to_string()
}

/// Wait until the outbox is empty.
pub fn wait_flushed(engine: &ConstructEngine, timeout: Duration) {
    wait_until("the outbox to flush", timeout, || {
        engine.snapshot().pending_actions == 0
    });
}

/// Wait until the upstream reports `flags` for `id`.
pub fn wait_upstream(upstream: &Upstream, id: &str, flags: (bool, bool)) {
    wait_until(
        &format!("upstream {id} (favourited, bookmarked) = {flags:?}"),
        TIMEOUT,
        || upstream.flags(id) == flags,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pixelfed_consent_forms() {
        let html = r#"<html><form method="post" action="http://localhost:8090/oauth/authorize">
            <input type="hidden" name="_token" value="csrf&amp;1">
            <input type="hidden" name="state" value="">
            <input type="hidden" name="client_id" value="1001">
            <input type="hidden" name="auth_token" value="abc">
            <button type="submit">Authorize</button></form>
            <FORM method="post" action="http://localhost:8090/oauth/authorize">
            <input type="hidden" name="_method" value="DELETE">
            <input type="hidden" name="_token" value="csrf&amp;1"></FORM></html>"#;
        let (action, fields) = approve_form(html).unwrap();
        assert_eq!(action, "http://localhost:8090/oauth/authorize");
        assert_eq!(
            fields,
            vec![
                ("_token".to_owned(), "csrf&1".to_owned()),
                ("state".to_owned(), String::new()),
                ("client_id".to_owned(), "1001".to_owned()),
                ("auth_token".to_owned(), "abc".to_owned()),
            ]
        );
        // Deny form first: the approve form is still found.
        let deny_first = r#"<form action="/oauth/authorize"><input type="hidden" name="_method" value="DELETE"></form>
            <form action="/oauth/authorize"><input type="hidden" name="auth_token" value="t"></form>"#;
        let (_, f) = approve_form(deny_first).unwrap();
        assert_eq!(f, vec![("auth_token".to_owned(), "t".to_owned())]);
        assert!(approve_form("<form action=\"/x\"></form>").is_none());
        assert_eq!(
            attr(r#"<input data-name="x" name="y" value="z">"#, "name").as_deref(),
            Some("y")
        );
    }
}
