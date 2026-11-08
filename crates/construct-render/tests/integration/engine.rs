//! A live session run by the engine (`construct_render::engine`, feature
//! `remote`) against a small stub of a pod: home and the layouts list come
//! from the engine's cache offline, a tap's write waits in the outbox until
//! the pod is back, a `layout` event redraws the open session, a layout opens
//! again after leaving the pod and opening it again, and signing
//! in to an upstream runs the engine's flow (a loopback redirect on the
//! desktop, a link that opens the app on a phone).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use client_core::{EngineConfig, MemoryStore, SecureStore};
use construct_render::engine::PodEngine;
use construct_render::interact::{Effect, Session, Target};
use construct_render::signin::SignIn;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(15);
/// The upstream sign-in a layout starts (as the pod's Pixelfed blueprint).
static LOGIN: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({ "intent": "action.auth.login", "instance_url": "https://pixelfed.example",
        "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" })
});

/// Home at `version`: its title says which, and a button adds one to the
/// counter.
fn home(version: u64, count: i64) -> Value {
    json!({
        "version": "1.0.0",
        "workspace": { "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": format!("Home v{version}"), "style": "Title" },
            { "type": "core://Text", "value_expr": "Count ${core://counters/home#count}", "style": "Body" },
            { "type": "core://Button", "label": "Add one", "on_click": {
                "intent": "action.mutation.optimistic", "entity_uri": "core://counters/home",
                "field": "count", "value_expr": "entity.count + 1", "rollback_value": 0,
                "endpoint": "/v1/counters/home/increment", "payload": {} } }
        ] },
        "entities": { "core://counters/home": { "count": count } },
        "queries": {}
    })
}

/// Settings' sign-in buttons: a layout's own, each provider's, and
/// Sign out.
fn sign_in_buttons() -> Vec<Value> {
    vec![
        json!({ "type": "core://Button", "label": "Sign in with Pixelfed", "on_click": *LOGIN }),
        json!({ "type": "core://List", "query_key": "local:providers", "item_template": {
            "type": "core://Button", "label": "Sign in to $item.title", "on_click": {
                "intent": "action.device", "command": "sign_in", "id": "$item.id",
                "value": "$form?.server" } } }),
        json!({ "type": "core://Button", "label": "Sign out", "on_click": {
            "intent": "action.device", "command": "sign_out" } }),
    ]
}

/// A New post screen: a media picker and a Share button that creates a
/// post with the picked photos (Pixelfed's compose, smaller).
fn compose_home() -> Value {
    json!({
        "version": "1.0.0",
        "workspace": { "type": "core://Column", "children": [
            { "type": "core://MediaPicker", "field": "photos", "max_items": 4, "label": "Add photos" },
            { "type": "core://Button", "label": "Share", "on_click": {
                "intent": "action.mutation.create", "query_key": "timeline", "position": "start",
                "entity_uri": "x://posts/pending-$nonce",
                "entity": { "media_uris": "$attachments.photos" },
                "attachments": { "photos": "$form.photos" },
                "endpoint": "/v1/posts/create",
                "payload": { "media": "$attachments.photos", "client_id": "$nonce" } } }
        ] },
        "entities": {},
        "queries": {}
    })
}

#[derive(Default)]
struct Pod {
    version: AtomicU64,
    count: Mutex<i64>,
    /// Closes every connection unanswered: the pod can't be reached.
    down: AtomicBool,
    posts: AtomicUsize,
    /// `PUT /v1/apps/<id>/hidden` bodies, by path.
    hidden: Mutex<Vec<(String, String)>>,
    streams: Mutex<Vec<mpsc::Sender<String>>>,
    base: Mutex<String>,
    /// Home has the sign-in buttons ([`sign_in_buttons`]).
    sign_in: AtomicBool,
    /// The sign-ins' registrations (`app_path`) and code exchanges
    /// (`token_path`), as posted.
    registered: Mutex<Vec<Value>>,
    exchanged: Mutex<Vec<Value>>,
    logouts: AtomicUsize,
    /// Home is a New post screen (a media picker and Share).
    compose: AtomicBool,
    /// Bytes of each upload, and the bodies of the posts created.
    uploads: Mutex<Vec<usize>>,
    created: Mutex<Vec<Value>>,
    /// As a pod host: a request without a session gets the "Open your pod"
    /// screen for any layout; with one, home and the layout `garden`.
    pod_code: AtomicBool,
}

struct Stub {
    base: String,
    pod: Arc<Pod>,
}

impl Stub {
    fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let pod = Arc::new(Pod::default());
        pod.version.store(1, Ordering::SeqCst);
        *pod.base.lock().unwrap() = base.clone();
        let shared = pod.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let pod = shared.clone();
                thread::spawn(move || handle(stream, &pod));
            }
        });
        Stub { base, pod }
    }

    /// A new home version, announced on the event streams.
    fn publish(&self, version: u64) {
        self.pod.version.store(version, Ordering::SeqCst);
        let event = format!("event: layout\ndata: {{\"version\":{version}}}\n\n");
        for tx in self.pod.streams.lock().unwrap().iter() {
            let _ = tx.send(event.clone());
        }
    }
}

fn handle(stream: TcpStream, pod: &Pod) {
    if pod.down.load(Ordering::SeqCst) {
        return;
    }
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or("").to_owned(),
        parts.next().unwrap_or("").to_owned(),
    );
    let mut length = 0usize;
    let mut bearer = false;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        bearer |= lower.starts_with("authorization: bearer ");
    }
    let mut body = vec![0u8; length];
    let _ = std::io::Read::read_exact(&mut reader, &mut body);
    let mut out = stream;
    let send = |out: &mut TcpStream, status: &str, extra: &str, body: &str| {
        let _ = write!(
            out,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        );
    };
    let version = pod.version.load(Ordering::SeqCst);
    if pod.pod_code.load(Ordering::SeqCst) {
        let screen = |title: &str, children: Vec<Value>| {
            let mut all =
                vec![json!({ "type": "core://Text", "value_expr": title, "style": "Title" })];
            all.extend(children);
            json!({ "version": "1.0.0", "workspace": { "type": "core://Column", "children": all },
                "entities": {}, "queries": {} })
            .to_string()
        };
        match (method.as_str(), path.as_str()) {
            ("POST", "/v1/pods/open") => {
                send(&mut out, "200 OK", "", r#"{"session_token":"pod-session"}"#)
            }
            ("GET", p)
                if p.starts_with("/v1/workspaces/") && !p.ends_with("/events") && !bearer =>
            {
                let open = screen(
                    "Open your pod",
                    vec![
                        json!({ "type": "core://TextField", "field": "pod_token", "placeholder": "Pod code" }),
                        json!({ "type": "core://Button", "label": "Open pod", "on_click": {
                            "intent": "action.auth.session", "path": "/v1/pods/open",
                            "payload": { "token": "$form.pod_token" } } }),
                    ],
                );
                send(&mut out, "200 OK", "", &open)
            }
            ("GET", "/v1/workspaces/home") => send(&mut out, "200 OK", "", &screen("Home", vec![])),
            ("GET", "/v1/workspaces/garden") => {
                send(&mut out, "200 OK", "", &screen("Garden", vec![]))
            }
            _ => send(&mut out, "404 Not Found", "", "{}"),
        }
        return;
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => send(&mut out, "200 OK", "", "{}"),
        ("GET", "/v1/workspaces/home") => {
            let count = *pod.count.lock().unwrap();
            let header = format!("X-Construct-Layout-Version: {version}\r\n");
            let mut home = if pod.compose.load(Ordering::SeqCst) {
                compose_home()
            } else {
                home(version, count)
            };
            if pod.sign_in.load(Ordering::SeqCst) {
                home["workspace"]["children"]
                    .as_array_mut()
                    .unwrap()
                    .extend(sign_in_buttons());
            }
            send(&mut out, "200 OK", &header, &home.to_string());
        }
        ("POST", "/v1/uploads") => {
            let mut uploads = pod.uploads.lock().unwrap();
            uploads.push(body.len());
            let id = json!({ "upload_id": format!("up-{}", uploads.len()) });
            send(&mut out, "201 Created", "", &id.to_string());
        }
        ("POST", "/v1/posts/create") => {
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            pod.created.lock().unwrap().push(v);
            send(&mut out, "200 OK", "", "{}");
        }
        ("GET", "/v1/workspaces/home/events") => {
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n: open\n\n"
            );
            let _ = out.flush();
            let (tx, rx) = mpsc::channel::<String>();
            pod.streams.lock().unwrap().push(tx);
            while let Ok(event) = rx.recv() {
                if pod.down.load(Ordering::SeqCst)
                    || out.write_all(event.as_bytes()).is_err()
                    || out.flush().is_err()
                {
                    return;
                }
            }
        }
        ("GET", "/v1/apps/home/versions/1") => {
            let v = json!({ "app_id": "home", "version": 1, "layout": home(1, 0)["workspace"] });
            send(&mut out, "200 OK", "", &v.to_string());
        }
        ("PUT", p) if p.ends_with("/hidden") => {
            let b = String::from_utf8_lossy(&body).into_owned();
            pod.hidden.lock().unwrap().push((p.to_owned(), b));
            send(&mut out, "200 OK", "", "{}");
        }
        ("GET", p) if p.starts_with("/v1/apps") => {
            let apps = json!({ "apps": [
                { "id": "home", "title": "Home", "kind": "custom", "latest_version": version },
                { "id": "garden", "title": "Garden", "kind": "custom", "latest_version": 3 }
            ] });
            send(&mut out, "200 OK", "", &apps.to_string());
        }
        ("POST", "/v1/auth/pixelfed/app") => {
            pod.registered
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or_default());
            let base = pod.base.lock().unwrap().clone();
            let app = json!({ "client_id": "client-1", "scopes": "read write",
                "authorize_endpoint": format!("{base}/oauth/authorize") });
            send(&mut out, "200 OK", "", &app.to_string());
        }
        ("POST", "/v1/auth/pixelfed/token") => {
            pod.exchanged
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or_default());
            let token = json!({ "session_token": "signed-in-session",
                "account": { "username": "ada", "display_name": "Ada" } });
            send(&mut out, "200 OK", "", &token.to_string());
        }
        ("GET", "/v1/auth/sign-ins") => {
            let list = json!({ "sign_ins": [], "providers": [{
                "id": "pixelfed", "title": "Pixelfed", "signed_in": false,
                "server_placeholder": "pixelfed.social", "intent": *LOGIN }] });
            send(&mut out, "200 OK", "", &list.to_string());
        }
        ("POST", "/v1/auth/logout") => {
            pod.logouts.fetch_add(1, Ordering::SeqCst);
            send(&mut out, "200 OK", "", r#"{"session_kept":true}"#);
        }
        ("POST", "/v1/counters/home/increment") => {
            pod.posts.fetch_add(1, Ordering::SeqCst);
            *pod.count.lock().unwrap() += 1;
            send(&mut out, "200 OK", "", "{}");
        }
        _ => send(&mut out, "404 Not Found", "", "{}"),
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
    }
}

/// A store holding a session for `base`.
fn store(base: &str) -> Arc<dyn SecureStore> {
    let s = MemoryStore::new();
    s.set(
        &client_core::auth::session_key(base),
        r#"{"token":"session-for-tests"}"#,
    );
    Arc::new(s)
}

fn storage(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "construct-render-engine-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The texts the session draws now.
fn texts(s: &mut Session) -> Vec<String> {
    s.scene()
        .unwrap()
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect()
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + TIMEOUT;
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn shows(s: &mut Session, text: &str) -> bool {
    texts(s).iter().any(|t| t == text)
}

#[test]
fn opens_from_the_cache_offline_and_sends_the_write_once_back() {
    let stub = Stub::start();
    let dir = storage("offline");
    let store = store(&stub.base);
    {
        let pod = PodEngine::open_with(&stub.base, Some(&dir), store.clone(), config());
        let mut s = pod.session(construct_render::Viewport::PHONE);
        wait_until("home", || shows(&mut s, "Home v1"));
        wait_until("the layouts list", || {
            s.poll();
            s.data
                .queries
                .get("local:layouts")
                .is_some_and(|q| !q.is_empty())
        });
    }
    // The pod can't be reached: a new start shows home and its layouts as
    // last seen, at once.
    stub.pod.down.store(true, Ordering::SeqCst);
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store, config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    assert!(shows(&mut s, "Home v1"), "drew {:?}", texts(&mut s));
    assert!(shows(&mut s, "Count 0"));
    assert_eq!(
        s.data.queries["local:layouts"],
        vec!["local://layouts/garden".to_owned()]
    );

    // A tap offline changes the count here and waits in the outbox.
    let out = s.tap(&Target::Text("Add one".into())).unwrap();
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::Sent { path } if path == "/v1/counters/home/increment")),
        "{:?}",
        out.effects
    );
    assert!(shows(&mut s, "Count 1"));
    wait_until("the pending write", || {
        s.poll();
        s.data.entities["local://connection"].get("pending") == Some(&json!(1))
    });
    assert_eq!(stub.pod.posts.load(Ordering::SeqCst), 0);

    // Back online, it reaches the pod, and the count stays.
    stub.pod.down.store(false, Ordering::SeqCst);
    wait_until("the write to reach the pod", || {
        stub.pod.posts.load(Ordering::SeqCst) == 1
    });
    wait_until("the outbox to empty", || {
        s.poll();
        s.data.entities["local://connection"].get("pending") == Some(&json!(0))
    });
    assert!(shows(&mut s, "Count 1"), "drew {:?}", texts(&mut s));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_layout_event_redraws_the_open_session() {
    let stub = Stub::start();
    let dir = storage("events");
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v1"));
    // Another device or an agent publishes a new version: the open session
    // shows it without opening it again.
    wait_until("the event stream", || {
        !stub.pod.streams.lock().unwrap().is_empty()
    });
    stub.publish(2);
    wait_until("home v2", || shows(&mut s, "Home v2"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A query parameter of an address, percent-decoded.
fn param(url: &str, name: &str) -> String {
    let query = url.split_once('?').map_or("", |(_, q)| q);
    let raw = query
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} in {url}"));
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push(u8::from_str_radix(&raw[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// A browser that the test drives: each page it opens goes to `opened`.
fn browser(redirect: client_core::RedirectMode) -> (SignIn, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sign_in = SignIn::new(
        move |url| tx.lock().unwrap().send(url.to_owned()).is_ok(),
        move |_| redirect.clone(),
    );
    (sign_in, rx)
}

fn session_token(store: &Arc<dyn SecureStore>, base: &str) -> Option<String> {
    let raw = store.get(&client_core::auth::session_key(base))?;
    serde_json::from_str::<Value>(&raw).ok()?["token"]
        .as_str()
        .map(str::to_owned)
}

#[test]
fn signs_in_on_the_desktop_through_a_loopback_redirect_and_signs_out() {
    let stub = Stub::start();
    stub.pod.sign_in.store(true, Ordering::SeqCst);
    let store = store(&stub.base);
    let (sign_in, opened) = browser(client_core::RedirectMode::Loopback);
    let pod = PodEngine::open_signing_in(&stub.base, None, store.clone(), config(), Some(&sign_in));
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v1"));

    let out = s
        .tap(&Target::Text("Sign in with Pixelfed".into()))
        .unwrap();
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::Device { command } if command == "sign_in")),
        "{:?}",
        out.effects
    );
    // The engine registered with the pod and opened the authorize page.
    let page = opened.recv_timeout(TIMEOUT).expect("the browser opened");
    assert!(
        page.starts_with(&format!("{}/oauth/authorize?", stub.base)),
        "{page}"
    );
    let redirect = param(&page, "redirect_uri");
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    assert_eq!(
        stub.pod.registered.lock().unwrap()[0]["redirect_uri"],
        json!(redirect)
    );
    wait_until("signing in to show", || {
        s.poll();
        s.data.entities["local://session"].get("status") != Some(&json!(""))
    });

    // The browser comes back to the loopback listener.
    let back = format!("{redirect}?code=c0de&state={}", param(&page, "state"));
    let status = reqwest::blocking::get(&back).unwrap().status();
    assert!(status.is_success(), "{status}");
    wait_until("the code exchange", || {
        !stub.pod.exchanged.lock().unwrap().is_empty()
    });
    assert_eq!(stub.pod.exchanged.lock().unwrap()[0]["code"], json!("c0de"));
    wait_until("the new session", || {
        session_token(&store, &stub.base).as_deref() == Some("signed-in-session")
    });

    // Sign out goes through the pod (which keeps a pod code's session).
    let out = s.tap(&Target::Text("Sign out".into())).unwrap();
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::Device { command } if command == "sign_out")),
        "{:?}",
        out.effects
    );
    wait_until("the sign-out", || {
        stub.pod.logouts.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn signs_in_on_a_phone_with_a_link_back_into_the_app() {
    let stub = Stub::start();
    stub.pod.sign_in.store(true, Ordering::SeqCst);
    let store = store(&stub.base);
    let (sign_in, opened) = browser(client_core::RedirectMode::Custom(
        construct_render::signin::CUSTOM_SCHEME.to_owned(),
    ));
    let pod = PodEngine::open_signing_in(&stub.base, None, store.clone(), config(), Some(&sign_in));
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v1"));
    // Settings' provider, on the server typed for it.
    wait_until("the providers", || {
        s.poll();
        shows(&mut s, "Sign in to Pixelfed")
    });
    s.form
        .insert("server".into(), "https://pixels.example".into());
    s.tap(&Target::Text("Sign in to Pixelfed".into())).unwrap();
    let page = opened.recv_timeout(TIMEOUT).expect("the browser opened");
    assert_eq!(param(&page, "redirect_uri"), "construct://oauth");
    let registered = stub.pod.registered.lock().unwrap()[0].clone();
    assert_eq!(registered["instance_url"], json!("https://pixels.example"));

    // A link that isn't this sign-in's is not taken; this one's is.
    assert!(!s.auth_redirect("construct://oauth?code=c0de&state=someone-else"));
    let link = format!(
        "construct://oauth?code=c0de&state={}",
        param(&page, "state")
    );
    assert!(construct_render::signin::is_redirect(
        &link,
        construct_render::signin::APP_LINK_HOST
    ));
    assert!(s.auth_redirect(&link));
    wait_until("the new session", || {
        session_token(&store, &stub.base).as_deref() == Some("signed-in-session")
    });
    let exchanged = stub.pod.exchanged.lock().unwrap()[0].clone();
    assert_eq!(exchanged["code"], json!("c0de"));
    assert_eq!(exchanged["redirect_uri"], json!("construct://oauth"));
}

#[test]
fn without_a_way_to_sign_in_the_tap_is_not_sent() {
    let stub = Stub::start();
    stub.pod.sign_in.store(true, Ordering::SeqCst);
    let pod = PodEngine::open_with(&stub.base, None, store(&stub.base), config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v1"));
    let out = s
        .tap(&Target::Text("Sign in with Pixelfed".into()))
        .unwrap();
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::NotSent { .. })),
        "{:?}",
        out.effects
    );
    assert!(stub.pod.registered.lock().unwrap().is_empty());
}

#[test]
fn a_pinned_home_stays_on_its_version_until_following_the_latest() {
    let stub = Stub::start();
    stub.pod.version.store(2, Ordering::SeqCst);
    let dir = storage("pins");
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v2"));
    let device = |command: &str| json!({ "intent": "action.device", "command": command, "app": "home", "id": "1" });
    s.dispatch(&device("pin_version")).unwrap();
    assert!(shows(&mut s, "Home v1"), "drew {:?}", texts(&mut s));
    // A new version published meanwhile doesn't replace the pinned one.
    wait_until("the event stream", || {
        !stub.pod.streams.lock().unwrap().is_empty()
    });
    stub.publish(3);
    thread::sleep(Duration::from_millis(500));
    assert!(shows(&mut s, "Home v1"), "drew {:?}", texts(&mut s));
    // The engine keeps the pin: a new start draws it too.
    let again = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    assert!(shows(
        &mut again.session(construct_render::Viewport::PHONE),
        "Home v1"
    ));
    s.dispatch(&device("follow_latest")).unwrap();
    wait_until("the latest home", || shows(&mut s, "Home v3"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn leaving_the_pod_and_opening_it_again_shows_its_layouts() {
    let stub = Stub::start();
    stub.pod.pod_code.store(true, Ordering::SeqCst);
    let dir = storage("leave");
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    let open =
        json!({ "intent": "action.navigation.open", "layout": "garden", "presentation": "Push" });
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home"));
    s.dispatch(&open).unwrap();
    wait_until("the layout", || {
        s.poll();
        shows(&mut s, "Garden")
    });
    // Leave the pod (as `PodHome::reopen`) and open it again.
    pod.forget_session();
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("the sign-in screen", || {
        s.poll();
        shows(&mut s, "Open pod")
    });
    s.form.insert("pod_token".into(), "cpod_test".into());
    let out = s.tap(&Target::Text("Open pod".into())).unwrap();
    assert!(
        out.effects
            .iter()
            .any(|e| matches!(e, Effect::SignedIn { .. })),
        "{:?}",
        out.effects
    );
    // The layout opens again: the same JSON as before, loaded afresh.
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home again", || shows(&mut s, "Home"));
    s.dispatch(&open).unwrap();
    wait_until("the layout again", || {
        s.poll();
        shows(&mut s, "Garden")
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hiding_a_layout_goes_to_the_pod() {
    let stub = Stub::start();
    let dir = storage("hide");
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Home v1"));
    let out = s
        .dispatch(&json!({ "intent": "action.device", "command": "hide_layout", "app": "garden" }))
        .unwrap();
    assert!(
        out.effects.contains(&Effect::Device {
            command: "hide_layout".into()
        }),
        "{:?}",
        out.effects
    );
    assert_eq!(
        stub.pod.hidden.lock().unwrap()[0],
        (
            "/v1/apps/garden/hidden".to_owned(),
            r#"{"hidden":true}"#.to_owned()
        )
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_picked_photo_is_staged_uploaded_and_posted() {
    let stub = Stub::start();
    stub.pod.compose.store(true, Ordering::SeqCst);
    let dir = storage("photos");
    let pod = PodEngine::open_with(&stub.base, Some(&dir), store(&stub.base), config());
    let mut s = pod.session(construct_render::Viewport::PHONE);
    wait_until("home", || shows(&mut s, "Share"));

    // A photo as the picker leaves it: a file the engine stages.
    std::fs::create_dir_all(&dir).unwrap();
    let photo = dir.join("picked.png");
    image::RgbaImage::from_pixel(64, 48, image::Rgba([200, 80, 40, 255]))
        .save(&photo)
        .unwrap();
    s.tap(&Target::Id("media:photos".into())).unwrap();
    s.picked(vec![Ok(photo)]);
    wait_until("the photo to be staged", || {
        s.poll();
        s.media_ids("photos").len() == 1
    });
    let id = s.media_ids("photos")[0].clone();
    let url = s.data.entities[&format!("construct://attachments/{id}")]
        .get_str("url")
        .unwrap()
        .to_owned();
    assert!(url.starts_with("file://"), "{url}");
    assert!(shows(&mut s, "Photo 1 of 1"));

    // Share: the engine uploads it, then creates the post with its upload id.
    s.tap(&Target::Text("Share".into())).unwrap();
    wait_until("the post", || !stub.pod.created.lock().unwrap().is_empty());
    assert_eq!(stub.pod.uploads.lock().unwrap().len(), 1);
    let post = stub.pod.created.lock().unwrap()[0].clone();
    assert_eq!(post["media"], json!(["up-1"]), "{post}");
    assert!(s.media_ids("photos").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
