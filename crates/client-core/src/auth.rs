//! OAuth2 PKCE building blocks for "Sign in with Pixelfed" (docs/DECISIONS.md D19).
//!
//! The pod owns the upstream token (D17): the client only runs the
//! browser part of the authorization-code flow (PKCE verifier, state,
//! authorize URL, redirect capture) and hands `code` + `code_verifier` to the
//! pod, which returns a pod session token. That token lives in a
//! [`SecureStore`] under [`session_key`]. A custom-scheme sign-in waiting
//! for its redirect is also persisted there ([`PendingAuth`] under
//! [`pending_auth_key`]) so a redirect that cold-starts the app can still be
//! completed.
//!
//! Everything here is synchronous and free of engine state; the flow itself
//! is driven by the engine (`lib.rs`) on a background thread.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// How long a sign-in waits for the browser redirect.
pub const REDIRECT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Path of the loopback redirect URI.
pub const LOOPBACK_PATH: &str = "/callback";
/// Cap on one loopback request (request line + headers).
const MAX_REQUEST_BYTES: u64 = 16 * 1024;
/// Time one loopback connection may take to send its request.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Length of a generated code verifier (RFC 7636 allows 43..=128).
pub const VERIFIER_LEN: usize = 64;

/// RFC 3986 unreserved characters (the PKCE verifier alphabet).
const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// `auth_json` error messages (docs/DECISIONS.md D19). Short and human: the
/// status line shows them on a phone; the technical detail (URL, HTTP
/// status, the pod's error body) goes to the log only.
pub const SIGN_IN_NOT_OWNER: &str = "This pod belongs to a different Pixelfed account.";
pub const SIGN_IN_REJECTED: &str = "Pixelfed rejected the sign-in. Please try again.";
pub const SIGN_IN_INSTANCE_NOT_ALLOWED: &str =
    "This pod isn't configured for that Pixelfed instance.";
pub const SIGN_IN_REDIRECT_NOT_ALLOWED: &str = "This pod doesn't accept sign-ins from this app.";
pub const SIGN_IN_UPSTREAM_DOWN: &str = "Can't reach Pixelfed right now.";
pub const SIGN_IN_POD_UNREACHABLE: &str = "Can't reach your pod.";
pub const SIGN_IN_DENIED: &str = "Sign-in was cancelled.";
pub const SIGN_IN_TIMED_OUT: &str = "Sign-in timed out. Please try again.";
pub const SIGN_IN_FAILED: &str = "Something went wrong. Please try again.";

/// The `auth_json` message for a failed pod auth call: `status` is `None`
/// when the pod could not be reached, `code` the `error` field of its JSON
/// body.
pub fn sign_in_error_message(status: Option<u16>, code: Option<&str>) -> &'static str {
    let Some(status) = status else {
        return SIGN_IN_POD_UNREACHABLE;
    };
    match code {
        Some("account_not_owner") => SIGN_IN_NOT_OWNER,
        Some("oauth_exchange_failed") => SIGN_IN_REJECTED,
        Some("instance_not_allowed") => SIGN_IN_INSTANCE_NOT_ALLOWED,
        Some("redirect_uri_not_allowed") => SIGN_IN_REDIRECT_NOT_ALLOWED,
        Some("upstream_unreachable" | "upstream_error") => SIGN_IN_UPSTREAM_DOWN,
        _ if status == 502 || status == 504 => SIGN_IN_UPSTREAM_DOWN,
        _ => SIGN_IN_FAILED,
    }
}

/// Platform secure storage for the pod session (Keychain, Android Keystore,
/// ...). Implementations may be called from any thread; the engine contains
/// panics from them.
pub trait SecureStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str);
    fn delete(&self, key: &str);
}

/// Opens a URL in the user's browser. Returns false if it could not (no
/// browser, headless, ...). Called on the engine's auth thread, never on the
/// thread that dispatched the intent.
pub trait UrlOpener: Send + Sync {
    fn open(&self, url: &str) -> bool;
}

/// How the authorization redirect reaches the engine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectMode {
    /// Desktop: a one-shot HTTP listener on `127.0.0.1:<random port>`,
    /// redirect URI `http://127.0.0.1:<port>/callback`.
    #[default]
    Loopback,
    /// A registered custom-scheme URI (Android: `construct://oauth`); the app
    /// passes the redirect to `handle_auth_redirect`.
    Custom(String),
}

/// The `SecureStore` key of the pod session for a pod base URL.
pub fn session_key(base_url: &str) -> String {
    format!("pod_session:{}", base_url.trim_end_matches('/'))
}

/// How long a persisted custom-scheme sign-in ([`PendingAuth`]) can still be
/// completed by a later engine (e.g. after the app process was killed while
/// the browser was in front).
pub const PENDING_AUTH_TTL: Duration = Duration::from_secs(10 * 60);

/// The `SecureStore` key of the pending custom-scheme sign-in for a pod base URL.
pub fn pending_auth_key(base_url: &str) -> String {
    format!("pending_auth:{}", base_url.trim_end_matches('/'))
}

/// A custom-scheme sign-in waiting for its redirect, persisted under
/// [`pending_auth_key`] so that the redirect can be exchanged even if it
/// reaches a new process (docs/DECISIONS.md D19).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingAuth {
    pub state: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub instance_url: String,
    /// Unix seconds.
    pub created_at: i64,
}

impl PendingAuth {
    pub fn parse(raw: &str) -> Option<Self> {
        serde_json::from_str::<PendingAuth>(raw)
            .ok()
            .filter(|p| !p.state.is_empty() && !p.code_verifier.is_empty())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Older than [`PENDING_AUTH_TTL`] at unix time `now` (or created more
    /// than a minute in the future: the clock moved).
    pub fn is_expired(&self, now: i64) -> bool {
        let age = now - self.created_at;
        age > PENDING_AUTH_TTL.as_secs() as i64 || age < -60
    }
}

/// Process-local store (tests, platforms without a secure store).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecureStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map().get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) {
        self.map().insert(key.to_owned(), value.to_owned());
    }
    fn delete(&self, key: &str) {
        self.map().remove(key);
    }
}

/// macOS login Keychain (generic password, service [`KeychainStore::SERVICE`],
/// account = key) via the `keyring` crate.
#[cfg(target_os = "macos")]
pub struct KeychainStore;

#[cfg(target_os = "macos")]
impl KeychainStore {
    pub const SERVICE: &'static str = "org.theconstruct.client";

    fn entry(key: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(Self::SERVICE, key)
            .map_err(|e| eprintln!("[ConstructEngine] keychain: bad entry {key}: {e}"))
            .ok()
    }
}

#[cfg(target_os = "macos")]
impl SecureStore for KeychainStore {
    fn get(&self, key: &str) -> Option<String> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Some(v),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                eprintln!("[ConstructEngine] keychain: read {key} failed: {e}");
                None
            }
        }
    }
    fn set(&self, key: &str, value: &str) {
        if let Some(entry) = Self::entry(key)
            && let Err(e) = entry.set_password(value)
        {
            eprintln!("[ConstructEngine] keychain: write {key} failed: {e}");
        }
    }
    fn delete(&self, key: &str) {
        if let Some(entry) = Self::entry(key) {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => eprintln!("[ConstructEngine] keychain: delete {key} failed: {e}"),
            }
        }
    }
}

/// The store used when the platform passes none: the Keychain on macOS,
/// in-memory elsewhere (the session then lasts only for the process).
pub fn default_secure_store() -> Box<dyn SecureStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(KeychainStore)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(MemoryStore::new())
    }
}

/// Run a platform callback, turning a panic (e.g. a foreign exception that
/// UniFFI surfaces as a panic) into `None`.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            eprintln!("[ConstructEngine] {what} panicked; ignored");
            None
        }
    }
}

/// `n` characters drawn uniformly from the unreserved set with the OS CSPRNG.
fn random_unreserved(n: usize) -> String {
    // 66 symbols: reject bytes >= 198 (= 3 * 66) so `b % 66` is uniform.
    let limit = 256 - (256 % UNRESERVED.len());
    let mut out = String::with_capacity(n);
    let mut buf = [0u8; 64];
    while out.len() < n {
        getrandom::fill(&mut buf).expect("OS random number generator");
        for &b in &buf {
            if out.len() == n {
                break;
            }
            if (b as usize) < limit {
                out.push(UNRESERVED[b as usize % UNRESERVED.len()] as char);
            }
        }
    }
    out
}

/// `BASE64URL-NOPAD(SHA256(verifier))` (RFC 7636 S256).
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh [`VERIFIER_LEN`]-character verifier from the CSPRNG.
    pub fn generate() -> Self {
        Self::from_verifier(random_unreserved(VERIFIER_LEN))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = s256_challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }
}

/// A random `state` value (32 unreserved characters, CSPRNG).
pub fn random_state() -> String {
    random_unreserved(32)
}

/// Percent-encode everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if UNRESERVED.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The authorize URL: `endpoint?response_type=code&client_id=..&redirect_uri=..
/// &scope=..&state=..&code_challenge=..&code_challenge_method=S256`.
pub fn authorize_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("state", state),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect();
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{sep}{}", query.join("&"))
}

/// What the authorization server sent back to the redirect URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redirect {
    Code {
        code: String,
        state: Option<String>,
    },
    Error {
        error: String,
        description: Option<String>,
        state: Option<String>,
    },
}

impl Redirect {
    pub fn state(&self) -> Option<&str> {
        match self {
            Redirect::Code { state, .. } | Redirect::Error { state, .. } => state.as_deref(),
        }
    }
}

/// Parse a redirect: an absolute URL (`construct://oauth?code=..&state=..`,
/// `http://127.0.0.1:1234/callback?..`) or a request target
/// (`/callback?..`). `None` if it carries neither `code` nor `error`.
pub fn parse_redirect(url: &str) -> Option<Redirect> {
    let parsed = reqwest::Url::parse(url)
        .or_else(|_| reqwest::Url::parse(&format!("http://localhost{url}")))
        .ok()?;
    let q: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    let state = q.get("state").filter(|s| !s.is_empty()).cloned();
    if let Some(error) = q.get("error").filter(|s| !s.is_empty()) {
        return Some(Redirect::Error {
            error: error.clone(),
            description: q.get("error_description").cloned(),
            state,
        });
    }
    let code = q.get("code").filter(|s| !s.is_empty())?.clone();
    Some(Redirect::Code { code, state })
}

/// Why waiting for a redirect ended without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitError {
    TimedOut,
    Cancelled,
    Io(String),
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitError::TimedOut => write!(f, "timed out waiting for the browser redirect"),
            WaitError::Cancelled => write!(f, "cancelled"),
            WaitError::Io(e) => write!(f, "redirect listener failed: {e}"),
        }
    }
}

/// One-shot HTTP listener on `127.0.0.1:<port>` for the loopback
/// redirect (RFC 8252 §7.3).
pub struct LoopbackListener {
    listener: TcpListener,
    port: u16,
}

const PAGE_OK: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>The Construct</title>\
</head><body style=\"font-family: sans-serif; margin: 3em\"><h1>Signed in</h1><p>You can close \
this window and return to The Construct.</p></body></html>";

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>The Construct</title></head><body \
         style=\"font-family: sans-serif; margin: 3em\"><h1>{title}</h1><p>{text}</p></body></html>"
    )
}

impl LoopbackListener {
    /// Bind `127.0.0.1:<random port>`.
    pub fn bind() -> std::io::Result<Self> {
        Self::bind_preferred(0)
    }

    /// Bind `127.0.0.1:<preferred>` so the redirect URI (and with it the
    /// upstream OAuth app the pod registers per redirect URI) stays the same
    /// across sign-ins; if that port is busy (e.g. a cancelled flow's
    /// listener is still closing, or another app owns it), retry briefly,
    /// then fall back to a random port. `preferred == 0`: random only.
    pub fn bind_preferred(preferred: u16) -> std::io::Result<Self> {
        let listener = if preferred == 0 {
            TcpListener::bind("127.0.0.1:0")?
        } else {
            let mut attempt = 0;
            loop {
                match TcpListener::bind(("127.0.0.1", preferred)) {
                    Ok(l) => break l,
                    Err(_) if attempt < 3 => {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        eprintln!(
                            "[ConstructEngine] loopback port {preferred} unavailable ({e}); using a \
                             random port"
                        );
                        break TcpListener::bind("127.0.0.1:0")?;
                    }
                }
            }
        };
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{LOOPBACK_PATH}", self.port)
    }

    /// Serve requests until a `GET /callback` arrives whose redirect is
    /// accepted by `accept` (e.g. its `state` matches), up to `timeout`.
    /// Other paths (favicon, ...) get a 404 and rejected callbacks a 400;
    /// both keep the listener waiting. Polls `cancelled` every 50 ms.
    pub fn wait(
        &self,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
        accept: &dyn Fn(&Redirect) -> bool,
    ) -> Result<Redirect, WaitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled() {
                return Err(WaitError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(WaitError::TimedOut);
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(r) = handle_conn(stream, accept) {
                        return Ok(r);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(WaitError::Io(e.to_string())),
            }
        }
    }
}

/// Answer one connection; returns the redirect if it was an accepted callback.
fn handle_conn(stream: TcpStream, accept: &dyn Fn(&Redirect) -> bool) -> Option<Redirect> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    // Bounded in size and time: another local process must not be able to
    // grow memory without limit or hold the (single-threaded) listener.
    let deadline = Instant::now() + REQUEST_READ_TIMEOUT;
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut read_line = |buf: &mut String| -> bool {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let _ = reader.get_ref().get_ref().set_read_timeout(Some(left));
        matches!(reader.read_line(buf), Ok(n) if n > 0)
    };
    let mut request_line = String::new();
    if !read_line(&mut request_line) {
        return None;
    }
    // Drain the headers (up to the blank line) so the client sees a clean reply.
    let mut line = String::new();
    for _ in 0..100 {
        line.clear();
        if !read_line(&mut line) || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut stream = reader.into_inner().into_inner();
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = target.split('?').next().unwrap_or("");
    let (status, body, result) = if method != "GET" || path != LOOPBACK_PATH {
        ("404 Not Found", page("Not found", ""), None)
    } else {
        match parse_redirect(target) {
            Some(r) if accept(&r) => match &r {
                Redirect::Code { .. } => ("200 OK", PAGE_OK.to_owned(), Some(r)),
                Redirect::Error { error, .. } => (
                    "200 OK",
                    page(
                        "Sign-in failed",
                        &format!("The server said: {}", html_escape(error)),
                    ),
                    Some(r),
                ),
            },
            Some(_) => (
                "400 Bad Request",
                page(
                    "Sign-in link expired",
                    "This sign-in response does not match the pending request. Start sign-in \
                     again from the app.",
                ),
                None,
            ),
            None => (
                "400 Bad Request",
                page("Bad request", "Missing code."),
                None,
            ),
        }
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
    result
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_verifier_is_valid_and_random() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_eq!(a.verifier.len(), VERIFIER_LEN);
        assert!(a.verifier.bytes().all(|c| UNRESERVED.contains(&c)));
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(a.challenge, s256_challenge(&a.verifier));
        assert_eq!(a.challenge.len(), 43);
        let s = random_state();
        assert_eq!(s.len(), 32);
        assert_ne!(s, random_state());
    }

    #[test]
    fn random_unreserved_uses_the_whole_alphabet() {
        let s = random_unreserved(20_000);
        for c in UNRESERVED {
            assert!(s.as_bytes().contains(c), "missing {}", *c as char);
        }
    }

    #[test]
    fn authorize_url_is_percent_encoded() {
        let url = authorize_url(
            "https://pix.example/oauth/authorize",
            "12",
            "http://127.0.0.1:5000/callback",
            "read write",
            "s~t.a-t_e",
            "abc",
        );
        assert_eq!(
            url,
            "https://pix.example/oauth/authorize?response_type=code&client_id=12&redirect_uri=\
             http%3A%2F%2F127.0.0.1%3A5000%2Fcallback&scope=read%20write&state=s~t.a-t_e&\
             code_challenge=abc&code_challenge_method=S256"
        );
        assert!(
            authorize_url("https://x/a?x=1", "c", "r", "s", "t", "u")
                .starts_with("https://x/a?x=1&")
        );
    }

    #[test]
    fn redirects_parse() {
        assert_eq!(
            parse_redirect("construct://oauth?code=a%2Fb&state=xyz"),
            Some(Redirect::Code {
                code: "a/b".into(),
                state: Some("xyz".into())
            })
        );
        assert_eq!(
            parse_redirect("/callback?code=c"),
            Some(Redirect::Code {
                code: "c".into(),
                state: None
            })
        );
        assert_eq!(
            parse_redirect("http://127.0.0.1:1/callback?error=access_denied&state=s"),
            Some(Redirect::Error {
                error: "access_denied".into(),
                description: None,
                state: Some("s".into())
            })
        );
        assert_eq!(parse_redirect("construct://oauth?foo=1"), None);
        assert_eq!(parse_redirect("not a url"), None);
        assert_eq!(session_key("http://h:1/"), "pod_session:http://h:1");
    }

    #[test]
    fn loopback_ignores_other_paths_and_wrong_state() {
        let l = LoopbackListener::bind().unwrap();
        let base = format!("http://127.0.0.1:{}", l.port());
        let t = std::thread::spawn(move || {
            let c = reqwest::blocking::Client::new();
            let fav = c.get(format!("{base}/favicon.ico")).send().unwrap();
            assert_eq!(fav.status().as_u16(), 404);
            let bad = c
                .get(format!("{base}/callback?code=x&state=wrong"))
                .send()
                .unwrap();
            assert_eq!(bad.status().as_u16(), 400);
            let ok = c
                .get(format!("{base}/callback?code=c1&state=good"))
                .send()
                .unwrap();
            assert_eq!(ok.status().as_u16(), 200);
            assert!(ok.text().unwrap().contains("Signed in"));
        });
        let r = l
            .wait(Duration::from_secs(10), &|| false, &|r| {
                r.state() == Some("good")
            })
            .unwrap();
        assert_eq!(
            r,
            Redirect::Code {
                code: "c1".into(),
                state: Some("good".into())
            }
        );
        t.join().unwrap();
    }

    #[test]
    fn loopback_survives_an_oversized_request() {
        let l = LoopbackListener::bind().unwrap();
        let port = l.port();
        let t = std::thread::spawn(move || {
            // 1 MiB without a newline: read up to the cap, answered 404, closed.
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let _ = s.write_all(&vec![b'A'; 1 << 20]);
            drop(s);
            let ok = reqwest::blocking::Client::new()
                .get(format!("http://127.0.0.1:{port}/callback?code=c&state=s"))
                .send()
                .unwrap();
            assert_eq!(ok.status().as_u16(), 200);
        });
        let r = l
            .wait(Duration::from_secs(10), &|| false, &|r| {
                r.state() == Some("s")
            })
            .unwrap();
        assert_eq!(r.state(), Some("s"));
        t.join().unwrap();
    }

    #[test]
    fn loopback_times_out_and_cancels() {
        let l = LoopbackListener::bind().unwrap();
        assert_eq!(
            l.wait(Duration::from_millis(100), &|| false, &|_| true),
            Err(WaitError::TimedOut)
        );
        assert_eq!(
            l.wait(Duration::from_secs(10), &|| true, &|_| true),
            Err(WaitError::Cancelled)
        );
    }

    #[test]
    fn loopback_prefers_the_given_port_and_falls_back_when_busy() {
        // A free port (released right away; the tiny reuse race is acceptable in a test).
        let free = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let l = LoopbackListener::bind_preferred(free).unwrap();
        assert_eq!(l.port(), free);
        assert_eq!(
            l.redirect_uri(),
            format!("http://127.0.0.1:{free}/callback")
        );
        // Busy (held by `l`): a different, random port.
        let other = LoopbackListener::bind_preferred(free).unwrap();
        assert_ne!(other.port(), free);
        assert_ne!(other.port(), 0);
    }

    #[test]
    fn pending_auth_roundtrip_and_expiry() {
        let p = PendingAuth {
            state: "s".into(),
            code_verifier: "v".into(),
            redirect_uri: "construct://oauth".into(),
            instance_url: "https://pix.example".into(),
            created_at: 1_000,
        };
        assert_eq!(PendingAuth::parse(&p.to_json()), Some(p.clone()));
        assert_eq!(PendingAuth::parse("garbage"), None);
        assert_eq!(
            PendingAuth::parse(
                &PendingAuth {
                    state: String::new(),
                    ..p.clone()
                }
                .to_json()
            ),
            None
        );
        assert!(!p.is_expired(1_000));
        assert!(!p.is_expired(1_600));
        assert!(p.is_expired(1_601));
        assert!(p.is_expired(900), "created in the future");
        assert_eq!(pending_auth_key("http://h:1/"), "pending_auth:http://h:1");
    }

    #[test]
    fn pod_error_codes_map_to_short_messages() {
        let m = sign_in_error_message;
        assert_eq!(m(Some(403), Some("account_not_owner")), SIGN_IN_NOT_OWNER);
        assert_eq!(
            m(Some(400), Some("oauth_exchange_failed")),
            SIGN_IN_REJECTED
        );
        assert_eq!(
            m(Some(400), Some("instance_not_allowed")),
            SIGN_IN_INSTANCE_NOT_ALLOWED
        );
        assert_eq!(
            m(Some(400), Some("redirect_uri_not_allowed")),
            SIGN_IN_REDIRECT_NOT_ALLOWED
        );
        assert_eq!(
            m(Some(502), Some("upstream_unreachable")),
            SIGN_IN_UPSTREAM_DOWN
        );
        assert_eq!(m(Some(502), Some("upstream_error")), SIGN_IN_UPSTREAM_DOWN);
        assert_eq!(m(Some(502), None), SIGN_IN_UPSTREAM_DOWN);
        assert_eq!(m(Some(504), Some("whatever")), SIGN_IN_UPSTREAM_DOWN);
        assert_eq!(m(None, None), SIGN_IN_POD_UNREACHABLE);
        assert_eq!(m(Some(500), None), SIGN_IN_FAILED);
        assert_eq!(m(Some(400), Some("app_not_registered")), SIGN_IN_FAILED);
        for msg in [
            SIGN_IN_NOT_OWNER,
            SIGN_IN_REJECTED,
            SIGN_IN_INSTANCE_NOT_ALLOWED,
            SIGN_IN_REDIRECT_NOT_ALLOWED,
            SIGN_IN_UPSTREAM_DOWN,
            SIGN_IN_POD_UNREACHABLE,
            SIGN_IN_DENIED,
            SIGN_IN_TIMED_OUT,
            SIGN_IN_FAILED,
        ] {
            assert!(
                msg.len() <= 60 && !msg.contains("http") && !msg.contains("HTTP"),
                "{msg}"
            );
        }
    }

    #[test]
    fn memory_store_roundtrip_and_guarded_panics() {
        let s = MemoryStore::new();
        assert_eq!(s.get("k"), None);
        s.set("k", "v");
        assert_eq!(s.get("k").as_deref(), Some("v"));
        s.delete("k");
        assert_eq!(s.get("k"), None);
        assert_eq!(guarded("x", || -> u8 { panic!("boom") }), None);
        assert_eq!(guarded("x", || 3), Some(3));
    }
}
