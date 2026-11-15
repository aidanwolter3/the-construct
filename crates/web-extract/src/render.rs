//! A page rendered in a headless browser (decision `rendered-fetches`): for
//! pages that build their content with JavaScript, or answer a plain fetch
//! with a bot check. The browser loads the page, waits for it to settle
//! (the network goes quiet, or a CSS selector appears) and hands back the
//! rendered HTML, which the extractors read like any fetched page.
//!
//! - **One browser at a time, server-wide** ([`SLOT`]): Chromium takes a
//!   few hundred MB, so a render waits for the one before it. Each render
//!   starts its own browser with a throwaway profile and closes it after.
//! - **A daily cap per pod** ([`Renders`], [`RENDERS_PER_DAY`]), on top of
//!   the host policy ([`crate::hosts`]): a render is one request to the
//!   page's host (spacing, daily cap, shared backoff after a block).
//! - **Public websites only**: every request the browser makes (scripts,
//!   frames, XHR) goes through a proxy of the pod's own ([`proxy`]) that
//!   connects only to public addresses on ports 80 and 443, and caps the
//!   traffic of one render ([`MAX_TRAFFIC`]). The browser resolves no names
//!   itself and sends no UDP.
//! - **A realistic browser profile**: the browser's own Chrome version in a
//!   desktop user agent (not "HeadlessChrome"), a desktop viewport and
//!   `Accept-Language`. A site that still answers with a challenge is a
//!   block ([`FetchError::blocked`]): the pod backs off; it never solves
//!   one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::fetch::{FetchError, Fetched, MAX_BODY, block_reason};

/// How long a render waits for its page by default.
pub const DEFAULT_TIMEOUT_SECS: u32 = 20;
/// Bounds of a render's `timeout_seconds`.
pub const MIN_TIMEOUT_SECS: u32 = 5;
pub const MAX_TIMEOUT_SECS: u32 = 45;
/// Renders per pod per day.
pub const RENDERS_PER_DAY: u32 = 100;
/// Bytes one render may move through the proxy (both ways).
pub const MAX_TRAFFIC: u64 = 40 * 1024 * 1024;
/// The environment variable naming the browser binary (`off`: none).
pub const BROWSER_VAR: &str = "CONSTRUCT_BROWSER";
/// How long the network stays quiet before a page counts as settled.
const IDLE: Duration = Duration::from_millis(500);
/// With requests still open (a long poll, a beacon): quiet this long is
/// settled too.
const QUIET: Duration = Duration::from_secs(2);
/// How long a render waits for the browser before giving up.
const SLOT_WAIT: Duration = Duration::from_secs(120);
/// How long the browser has to start.
const LAUNCH: Duration = Duration::from_secs(30);
/// One DevTools call's timeout.
const CALL: Duration = Duration::from_secs(15);
const VIEWPORT: (u32, u32) = (1366, 900);
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
/// Names tried on `PATH` when [`BROWSER_VAR`] is unset.
const BINARIES: &[&str] = &[
    "chromium",
    "chromium-browser",
    "headless_shell",
    "chrome-headless-shell",
    "google-chrome",
];

/// The server's one browser at a time.
static SLOT: Semaphore = Semaphore::const_new(1);

/// How a render waits for its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// A CSS selector the page must show; empty: wait for the network to
    /// go quiet.
    pub wait_for: String,
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            wait_for: String::new(),
            timeout: Duration::from_secs(u64::from(DEFAULT_TIMEOUT_SECS)),
        }
    }
}

/// The server's headless browser: a Chromium binary.
#[derive(Debug, Clone)]
pub struct Browser {
    binary: PathBuf,
    /// Pages may load from private addresses (tests).
    allow_private: bool,
}

impl Browser {
    pub fn new(binary: impl Into<PathBuf>, allow_private: bool) -> Self {
        Self {
            binary: binary.into(),
            allow_private,
        }
    }

    /// The browser named by [`BROWSER_VAR`], else the first of
    /// [`BINARIES`] on `PATH`; `None` without one (or with `off`).
    pub fn from_env(allow_private: bool) -> Option<Self> {
        match std::env::var(BROWSER_VAR) {
            Ok(v) if v == "off" || v == "0" => None,
            Ok(v) if !v.trim().is_empty() => Some(Self::new(v.trim(), allow_private)),
            _ => Self::find().map(|b| Self::new(b, allow_private)),
        }
    }

    /// The first of [`BINARIES`] on `PATH`.
    pub fn find() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .flat_map(|d| BINARIES.iter().map(move |b| d.join(b)))
            .find(|p| p.is_file())
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

/// A pod's renders today (`Pod::local::<Renders>()`).
#[derive(Debug, Default)]
pub struct Renders(Mutex<Option<(Instant, u32)>>);

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

impl Renders {
    /// Count one render; `Err` (a sentence for a filler's status) once
    /// today's are spent.
    pub async fn take(&self) -> Result<(), String> {
        let mut st = self.0.lock().await;
        let now = Instant::now();
        let (start, count) = st.get_or_insert((now, 0));
        if now.duration_since(*start) >= DAY {
            (*start, *count) = (now, 0);
        }
        if *count >= RENDERS_PER_DAY {
            return Err(format!(
                "the pod already rendered {RENDERS_PER_DAY} pages in its browser today (its daily limit)"
            ));
        }
        *count += 1;
        Ok(())
    }
}

/// Render `url` through the pod's host policy and render cap: admitted
/// first, a block recorded after. The answer is a [`Fetched::Body`] of the
/// rendered HTML (no validators: a rendered page is always read afresh).
pub async fn render(
    browser: &Browser,
    hosts: &crate::hosts::Hosts,
    renders: &Renders,
    url: &str,
    opts: &Options,
) -> Result<Fetched, FetchError> {
    hosts.admit(url).await.map_err(|m| FetchError {
        message: m,
        retry_after: None,
        blocked: true,
    })?;
    renders.take().await.map_err(FetchError::new)?;
    let r = browser.load(url, opts).await;
    match &r {
        Err(e) if e.blocked => hosts.blocked(url, e.retry_after).await,
        Ok(_) => hosts.answered(url).await,
        Err(_) => {}
    }
    r.map(|html| Fetched::Body {
        bytes: html.into_bytes(),
        etag: None,
        last_modified: None,
    })
}

/// A directory removed when dropped (the browser's throwaway profile).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "construct-browser-{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fail(message: impl Into<String>) -> FetchError {
    FetchError::new(message)
}

impl Browser {
    /// Load `url` in a fresh browser and answer its rendered HTML.
    async fn load(&self, url: &str, opts: &Options) -> Result<String, FetchError> {
        let _slot = tokio::time::timeout(SLOT_WAIT, SLOT.acquire())
            .await
            .map_err(|_| {
                fail("the server's browser is busy with other pages; the next fill tries again")
            })?
            .map_err(|_| fail("the server's browser is closed"))?;
        let proxy = proxy::Proxy::start(self.allow_private)
            .await
            .map_err(|e| fail(format!("the browser's proxy could not start: {e}")))?;
        let profile = TempDir::new()
            .map_err(|e| fail(format!("the browser's profile could not be made: {e}")))?;
        let mut child = browser_command(&self.binary, &profile.0)
            .map_err(|e| fail(format!("the browser's profile could not be made: {e}")))?
            .args(flags(proxy.addr, &profile.0))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                fail(format!(
                    "the server's browser ({}) could not start: {e}",
                    self.binary.display()
                ))
            })?;
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(devtools_url(stderr, tx));
        let ws = match tokio::time::timeout(LAUNCH, rx).await {
            Ok(Ok(Ok(ws))) => ws,
            Ok(Ok(Err(last))) => {
                tracing::warn!(target: "browser", "the browser exited on start: {last}");
                return Err(fail(format!("the server's browser did not start ({last})")));
            }
            _ => return Err(fail("the server's browser did not start in time")),
        };
        let r = match Cdp::connect(&ws).await {
            Ok(cdp) => {
                // The page's own timeout, and a little for the calls around it.
                let r = tokio::time::timeout(
                    opts.timeout + Duration::from_secs(10),
                    drive(&cdp, url, opts),
                )
                .await
                .unwrap_or_else(|_| Err(fail("the page took too long to render")));
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    cdp.call(None, "Browser.close", json!({})),
                )
                .await;
                r
            }
            Err(e) => Err(e),
        };
        let _ = child.kill().await;
        drop(proxy);
        r
    }
}

/// The uid and gid the browser runs as when the server is root (Debian's
/// `nobody` and `nogroup`).
const BROWSER_ID: u32 = 65534;

/// The browser's process, before its flags: none of the server's
/// environment (its keys and secrets), and, when the server is root, the
/// unprivileged [`BROWSER_ID`], owning its profile. The browser runs
/// untrusted pages without Chromium's sandbox, so a page that took over
/// the renderer gets a user that can read neither the pods' data (`0700`,
/// `0600`: `pod_core::util`) nor the server's environment.
fn browser_command(binary: &Path, profile: &Path) -> std::io::Result<Command> {
    let mut cmd = Command::new(binary);
    cmd.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", profile);
    // SAFETY: geteuid has no preconditions and can't fail.
    if unsafe { libc::geteuid() } == 0 {
        std::os::unix::fs::chown(profile, Some(BROWSER_ID), Some(BROWSER_ID))?;
        // Dropping to a uid also drops root's supplementary groups.
        cmd.uid(BROWSER_ID).gid(BROWSER_ID);
    }
    Ok(cmd)
}

/// The browser's command line.
fn flags(proxy: std::net::SocketAddr, profile: &Path) -> Vec<String> {
    let (w, h) = VIEWPORT;
    let mut out: Vec<String> = [
        "--headless=new",
        // The server runs as root in its container, where Chromium's
        // sandbox can't start; the browser runs as an unprivileged user
        // with none of the server's environment (`browser_command`), the
        // proxy keeps it on public websites, and each render's profile is
        // thrown away.
        "--no-sandbox",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-sync",
        "--disable-breakpad",
        "--mute-audio",
        "--hide-scrollbars",
        "--metrics-recording-only",
        "--disable-quic",
        "--disable-site-isolation-trials",
        "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication,IsolateOrigins,site-per-process",
        // A browser a person drives, not "navigator.webdriver".
        "--disable-blink-features=AutomationControlled",
        // Images are not read: the HTML names them.
        "--blink-settings=imagesEnabled=false",
        // No UDP around the proxy (WebRTC), and no names resolved by the
        // browser itself: the proxy resolves and checks them.
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
        "--proxy-bypass-list=<-loopback>",
        "--lang=en-US",
        "--remote-debugging-address=127.0.0.1",
        "--remote-debugging-port=0",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    out.push(format!("--proxy-server=http://{proxy}"));
    out.push(format!("--window-size={w},{h}"));
    out.push(format!("--user-data-dir={}", profile.display()));
    out.push("about:blank".into());
    out
}

/// Send the DevTools websocket the browser prints on start (or, if it
/// exits first, its last line of output), then drain the rest of its output
/// so it never blocks on a full pipe.
async fn devtools_url(
    stderr: tokio::process::ChildStderr,
    tx: oneshot::Sender<Result<String, String>>,
) {
    let mut lines = BufReader::new(stderr).lines();
    let mut last = String::new();
    let mut tx = Some(tx);
    while let Ok(Some(line)) = lines.next_line().await {
        match line.trim().strip_prefix("DevTools listening on ") {
            Some(ws) if tx.is_some() => {
                if let Some(tx) = tx.take() {
                    let _ = tx.send(Ok(ws.trim().to_owned()));
                }
            }
            _ => {
                tracing::trace!(target: "browser", "{line}");
                if !line.trim().is_empty() {
                    last = line.trim().chars().take(300).collect();
                }
            }
        }
    }
    if let Some(tx) = tx {
        let _ = tx.send(Err(if last.is_empty() {
            "it exited".into()
        } else {
            last
        }));
    }
}

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A DevTools protocol connection: calls answered by id, events in order.
struct Cdp {
    out: mpsc::UnboundedSender<Message>,
    next: AtomicU64,
    pending: Pending,
    events: Mutex<mpsc::UnboundedReceiver<Value>>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}

impl Drop for Cdp {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

impl Cdp {
    async fn connect(ws: &str) -> Result<Self, FetchError> {
        let (stream, _) = tokio_tungstenite::connect_async(ws)
            .await
            .map_err(|e| fail(format!("the server's browser could not be reached: {e}")))?;
        let (mut sink, mut source) = stream.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let writer = tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::default();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let p = pending.clone();
        let reader = tokio::spawn(async move {
            while let Some(Ok(m)) = source.next().await {
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
                    continue;
                };
                if let Some(id) = v["id"].as_u64() {
                    let tx = p.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    if let Some(tx) = tx {
                        let r = match v.get("error") {
                            Some(e) => Err(e["message"].as_str().unwrap_or("error").to_owned()),
                            None => Ok(v["result"].clone()),
                        };
                        let _ = tx.send(r);
                    }
                } else if v.get("method").is_some() {
                    let _ = ev_tx.send(v);
                }
            }
            // The browser went away: every waiting call fails.
            p.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        Ok(Self {
            out,
            next: AtomicU64::new(1),
            pending,
            events: Mutex::new(ev_rx),
            reader,
            writer,
        })
    }

    /// Call `method` (on `session`, else the browser).
    async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, FetchError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        self.out
            .send(Message::text(msg.to_string()))
            .map_err(|_| fail("the server's browser closed"))?;
        match tokio::time::timeout(CALL, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(fail(format!("the browser refused {method}: {e}"))),
            Ok(Err(_)) => Err(fail("the server's browser closed")),
            Err(_) => Err(fail(format!("the browser did not answer {method}"))),
        }
    }
}

/// A desktop Chrome user agent of the browser's own version.
fn user_agent(product: &str) -> (String, String, String) {
    // "HeadlessChrome/131.0.6778.33" or "Chrome/131.0.6778.33".
    let full = product
        .split_once('/')
        .map(|(_, v)| v)
        .filter(|v| v.split('.').all(|p| p.bytes().all(|b| b.is_ascii_digit())))
        .unwrap_or("131.0.0.0");
    let major = full.split('.').next().unwrap_or("131").to_owned();
    (
        format!(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
        ),
        major,
        full.to_owned(),
    )
}

/// Open `url` in a new tab of the browser behind `cdp`, wait for it to
/// settle and read its HTML.
async fn drive(cdp: &Cdp, url: &str, opts: &Options) -> Result<String, FetchError> {
    let deadline = Instant::now() + opts.timeout;
    let version = cdp.call(None, "Browser.getVersion", json!({})).await?;
    let (ua, major, full) = user_agent(version["product"].as_str().unwrap_or(""));
    let target = cdp
        .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
        .await?;
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target["targetId"], "flatten": true }),
        )
        .await?;
    let session = attached["sessionId"].as_str().unwrap_or("").to_owned();
    let s = Some(session.as_str());
    cdp.call(s, "Network.enable", json!({})).await?;
    cdp.call(s, "Page.enable", json!({})).await?;
    let brands = json!([
        { "brand": "Chromium", "version": major },
        { "brand": "Google Chrome", "version": major },
        { "brand": "Not_A Brand", "version": "24" },
    ]);
    cdp.call(
        s,
        "Network.setUserAgentOverride",
        json!({
            "userAgent": ua,
            "acceptLanguage": ACCEPT_LANGUAGE,
            "platform": "Linux x86_64",
            "userAgentMetadata": {
                "brands": brands, "fullVersion": full, "platform": "Linux",
                "platformVersion": "", "architecture": "x86", "model": "", "mobile": false,
            },
        }),
    )
    .await?;
    cdp.call(
        s,
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": VIEWPORT.0, "height": VIEWPORT.1, "deviceScaleFactor": 1, "mobile": false }),
    )
    .await?;
    let nav = cdp.call(s, "Page.navigate", json!({ "url": url })).await?;
    if let Some(e) = nav["errorText"].as_str().filter(|e| !e.is_empty()) {
        return Err(fail(format!("the browser could not load the page ({e})")));
    }
    let frame = nav["frameId"].as_str().unwrap_or("").to_owned();

    let mut events = cdp.events.lock().await;
    let mut inflight: HashSet<String> = HashSet::new();
    let mut loaded = false;
    let mut status: Option<u16> = None;
    let mut last_activity = Instant::now();
    let mut next_poll = Instant::now();
    let mut found = opts.wait_for.is_empty();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if loaded {
            if opts.wait_for.is_empty() {
                let quiet = now.duration_since(last_activity);
                if (inflight.is_empty() && quiet >= IDLE) || quiet >= QUIET {
                    break;
                }
            } else if now >= next_poll {
                if shows(cdp, s, &opts.wait_for).await? {
                    found = true;
                    break;
                }
                next_poll = now + Duration::from_millis(250);
            }
        }
        let wake = (now + Duration::from_millis(100)).min(deadline);
        let ev = match tokio::time::timeout_at(wake.into(), events.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => return Err(fail("the server's browser closed")),
            Err(_) => continue,
        };
        if ev["sessionId"].as_str() != Some(session.as_str()) {
            continue;
        }
        let p = &ev["params"];
        let main_doc = p["type"] == "Document" && p["frameId"].as_str() == Some(frame.as_str());
        match ev["method"].as_str().unwrap_or("") {
            "Network.requestWillBeSent" => {
                inflight.insert(p["requestId"].as_str().unwrap_or("").to_owned());
                last_activity = Instant::now();
                if main_doc {
                    // The page navigated again (a challenge that passed,
                    // a script redirect): wait for that one.
                    loaded = false;
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                inflight.remove(p["requestId"].as_str().unwrap_or(""));
                last_activity = Instant::now();
            }
            "Network.responseReceived" if main_doc => {
                status = p["response"]["status"]
                    .as_u64()
                    .and_then(|s| u16::try_from(s).ok());
            }
            "Page.loadEventFired" => {
                loaded = true;
                last_activity = Instant::now();
            }
            _ => {}
        }
    }
    drop(events);
    if !found && !shows(cdp, s, &opts.wait_for).await? {
        return Err(fail(format!(
            "the page showed nothing matching \"{}\" within {} seconds",
            opts.wait_for,
            opts.timeout.as_secs()
        )));
    }
    let html = cdp
        .call(
            s,
            "Runtime.evaluate",
            json!({
                "expression": "document.documentElement ? document.documentElement.outerHTML : ''",
                "returnByValue": true,
            }),
        )
        .await?;
    let html = html["result"]["value"].as_str().unwrap_or("").to_owned();
    if html.len() > MAX_BODY {
        return Err(fail(format!(
            "the rendered page is over {} MiB",
            MAX_BODY >> 20
        )));
    }
    match status {
        Some(s @ (403 | 429)) => {
            return Err(FetchError {
                message: format!(
                    "the site answered {s} in the browser: it is refusing automated visits for now"
                ),
                retry_after: None,
                blocked: true,
            });
        }
        Some(s) if s >= 400 => return Err(fail(format!("the site answered {s}"))),
        _ => {}
    }
    if let Some(reason) = block_reason(html.as_bytes()) {
        return Err(FetchError {
            message: format!(
                "the site answered the browser with a check for automated visitors (\"{reason}\") instead of the page"
            ),
            retry_after: None,
            blocked: true,
        });
    }
    Ok(html)
}

/// Whether the page shows an element matching `selector`.
async fn shows(cdp: &Cdp, session: Option<&str>, selector: &str) -> Result<bool, FetchError> {
    let expr = format!(
        "!!document.querySelector({})",
        serde_json::to_string(selector).unwrap_or_default()
    );
    let r = cdp
        .call(
            session,
            "Runtime.evaluate",
            json!({ "expression": expr, "returnByValue": true }),
        )
        .await?;
    if r.get("exceptionDetails").is_some() {
        return Err(fail(format!("\"{selector}\" is not a CSS selector")));
    }
    Ok(r["result"]["value"].as_bool().unwrap_or(false))
}

/// The proxy every request of the browser goes through: public addresses
/// only (unless allowed), ports 80 and 443, and [`MAX_TRAFFIC`] bytes.
mod proxy {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinSet;

    use super::MAX_TRAFFIC;

    /// Longest request head.
    const MAX_HEAD: usize = 32 * 1024;
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    /// A proxy on the loopback interface; stops (with its connections) when
    /// dropped.
    pub struct Proxy {
        pub addr: SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Proxy {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Proxy {
        pub async fn start(allow_private: bool) -> std::io::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            let traffic = Arc::new(AtomicU64::new(0));
            let task = tokio::spawn(async move {
                // Aborting this task drops the set, which aborts every
                // connection.
                let mut conns = JoinSet::new();
                while let Ok((s, _)) = listener.accept().await {
                    conns.spawn(serve(s, allow_private, traffic.clone()));
                    while conns.try_join_next().is_some() {}
                }
            });
            Ok(Self { addr, task })
        }
    }

    fn find_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    /// One connection of the browser: a `CONNECT` tunnel (https, wss) or
    /// one plain `http://` request.
    async fn serve(mut client: TcpStream, allow_private: bool, traffic: Arc<AtomicU64>) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let end = loop {
            match client.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if let Some(e) = find_end(&buf) {
                break e;
            }
            if buf.len() > MAX_HEAD {
                return;
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let rest = buf[end + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let mut request = lines.next().unwrap_or("").split(' ');
        let (method, target, version) = (
            request.next().unwrap_or(""),
            request.next().unwrap_or(""),
            request.next().unwrap_or("HTTP/1.1"),
        );
        let refuse = |mut c: TcpStream| async move {
            let _ = c
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        };
        if method.eq_ignore_ascii_case("CONNECT") {
            let Some((host, port)) = split_host_port(target) else {
                return refuse(client).await;
            };
            let Some(up) = open(&host, port, allow_private).await else {
                return refuse(client).await;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            pipe(client, up, rest, traffic).await;
        } else {
            let Ok(url) = reqwest::Url::parse(target) else {
                return refuse(client).await;
            };
            let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
                return refuse(client).await;
            };
            if url.scheme() != "http" {
                return refuse(client).await;
            }
            let Some(mut up) = open(host, port, allow_private).await else {
                return refuse(client).await;
            };
            // Origin form, one request per connection.
            let mut path = url.path().to_owned();
            if let Some(q) = url.query() {
                path.push('?');
                path.push_str(q);
            }
            let mut out = format!("{method} {path} {version}\r\n");
            for l in lines {
                let name = l.split(':').next().unwrap_or("").trim();
                if [
                    "proxy-connection",
                    "connection",
                    "keep-alive",
                    "proxy-authorization",
                ]
                .iter()
                .any(|h| name.eq_ignore_ascii_case(h))
                {
                    continue;
                }
                out.push_str(l);
                out.push_str("\r\n");
            }
            out.push_str("Connection: close\r\n\r\n");
            if up.write_all(out.as_bytes()).await.is_err() {
                return;
            }
            pipe(client, up, rest, traffic).await;
        }
    }

    /// `host:port` of a `CONNECT` (`[v6]:port` too).
    fn split_host_port(target: &str) -> Option<(String, u16)> {
        let (host, port) = target.rsplit_once(':')?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        Some((host.to_owned(), port.parse().ok()?))
    }

    /// A connection to `host:port` if the browser may reach it.
    async fn open(host: &str, port: u16, allow_private: bool) -> Option<TcpStream> {
        if !allow_private && !matches!(port, 80 | 443) {
            tracing::debug!(target: "browser", %host, port, "refused: not a web port");
            return None;
        }
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .ok()?
            .filter(|a| allow_private || pod_core::net::is_public_ip(a.ip()))
            .collect();
        if addrs.is_empty() {
            tracing::debug!(target: "browser", %host, "refused: no public address");
            return None;
        }
        for a in addrs {
            if let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await {
                return Some(s);
            }
        }
        None
    }

    /// Copy both ways until either side closes or the render's traffic is
    /// spent.
    async fn pipe(client: TcpStream, up: TcpStream, rest: Vec<u8>, traffic: Arc<AtomicU64>) {
        let (mut cr, mut cw) = client.into_split();
        let (mut ur, mut uw) = up.into_split();
        if !rest.is_empty() && uw.write_all(&rest).await.is_err() {
            return;
        }
        tokio::select! {
            _ = copy(&mut cr, &mut uw, &traffic) => {}
            _ = copy(&mut ur, &mut cw, &traffic) => {}
        }
    }

    async fn copy<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        r: &mut R,
        w: &mut W,
        traffic: &AtomicU64,
    ) {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if traffic.fetch_add(n as u64, Ordering::Relaxed) + n as u64 > MAX_TRAFFIC {
                tracing::debug!(target: "browser", "a render's traffic is spent");
                break;
            }
            if w.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn connect_targets() {
            assert_eq!(
                split_host_port("shop.example:443"),
                Some(("shop.example".into(), 443))
            );
            assert_eq!(split_host_port("[::1]:443"), Some(("::1".into(), 443)));
            assert_eq!(split_host_port("nope"), None);
        }

        #[tokio::test]
        async fn private_and_odd_ports_are_refused() {
            assert!(open("127.0.0.1", 443, false).await.is_none());
            assert!(open("localhost", 80, false).await.is_none());
            assert!(open("169.254.169.254", 80, false).await.is_none());
            assert!(open("shop.example", 22, false).await.is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_is_desktop_chrome_of_the_browsers_version() {
        let (ua, major, full) = user_agent("HeadlessChrome/131.0.6778.33");
        assert!(
            ua.contains("Chrome/131.0.0.0") && !ua.contains("Headless"),
            "{ua}"
        );
        assert_eq!((major.as_str(), full.as_str()), ("131", "131.0.6778.33"));
        assert!(user_agent("").0.contains("Chrome/131"));
    }

    #[tokio::test]
    async fn renders_have_a_daily_cap() {
        let r = Renders::default();
        *r.0.lock().await = Some((Instant::now(), RENDERS_PER_DAY));
        let e = r.take().await.unwrap_err();
        assert!(e.contains("daily limit"), "{e}");
    }

    /// What `browser_command` starts, with `sh -c <script>` as the browser.
    async fn run_as_browser(script: &str) -> String {
        let profile = TempDir::new().unwrap();
        let out = browser_command(Path::new("/bin/sh"), &profile.0)
            .unwrap()
            .args(["-c", script])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    #[tokio::test]
    async fn the_browser_gets_none_of_the_servers_environment() {
        // Cargo sets this for the test process; a server's keys are the same.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let env = run_as_browser("env").await;
        assert!(!env.contains("CARGO_MANIFEST_DIR"), "{env}");
        assert!(env.contains("HOME=") && env.contains("PATH="), "{env}");
    }

    #[tokio::test]
    async fn a_root_server_starts_the_browser_unprivileged() {
        let uid = run_as_browser("id -u; touch \"$HOME/written\" && echo ok").await;
        // SAFETY: geteuid has no preconditions and can't fail.
        let me = unsafe { libc::geteuid() };
        let want = if me == 0 { BROWSER_ID } else { me };
        // It runs as `want` and can write its profile.
        assert_eq!(uid, format!("{want}\nok\n"));
    }

    #[test]
    fn the_browser_resolves_nothing_itself() {
        let f = flags("127.0.0.1:9".parse().unwrap(), Path::new("/tmp/p"));
        assert!(f.contains(&"--proxy-server=http://127.0.0.1:9".to_owned()));
        assert!(
            f.iter()
                .any(|x| x.starts_with("--host-resolver-rules=MAP *"))
        );
        assert!(f.iter().any(|x| x.contains("disable_non_proxied_udp")));
    }
}
