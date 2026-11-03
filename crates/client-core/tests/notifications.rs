//! Notifications in the engine (D52): the device header and registration,
//! the events stream's `notification` / `notifications_read` events, the
//! catch-up fetch on (re)connect, mark-read, push messages and sign-out,
//! against a small hand-rolled stub of the pod that records every request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use client_core::store::{KV_NOTIFICATION_UNREAD, Store, StoreOps};
use client_core::{
    ConstructEngine, EngineConfig, EngineOptions, MemoryStore, SecureStore, StateSnapshot,
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);
const INCREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"#;

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    /// Lower-case names.
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

enum StreamCmd {
    Send(String),
    Close,
}

struct StubState {
    reqs: Mutex<Vec<Req>>,
    streams: Mutex<Vec<mpsc::Sender<StreamCmd>>>,
    event_conns: AtomicUsize,
    /// `GET /v1/notifications` answer.
    list: Mutex<Value>,
    list_status: AtomicU16,
    /// `POST /v1/notifications/read` answer.
    read_answer: Mutex<Value>,
    read_status: AtomicU16,
}

struct Stub {
    base: String,
    st: Arc<StubState>,
}

fn notification(id: &str, app: &str, read: bool) -> Value {
    json!({
        "id": id, "app_id": app, "channel": "likes", "title": format!("Title {id}"),
        "body": "", "target_uri": format!("{app}://posts/{id}"), "created_at": 100, "read": read
    })
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(StubState {
            reqs: Mutex::default(),
            streams: Mutex::default(),
            event_conns: AtomicUsize::new(0),
            list: Mutex::new(
                json!({ "notifications": [], "unread_total": 0, "unread_by_app": {} }),
            ),
            list_status: AtomicU16::new(200),
            read_answer: Mutex::new(json!({ "unread_total": 0, "unread_by_app": {} })),
            read_status: AtomicU16::new(200),
        });
        let shared = Arc::clone(&st);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let st = Arc::clone(&shared);
                thread::spawn(move || handle(stream, &st));
            }
        });
        Self { base, st }
    }

    fn reqs(&self) -> Vec<Req> {
        self.st.reqs.lock().unwrap().clone()
    }

    fn find(&self, method: &str, prefix: &str) -> Vec<Req> {
        self.reqs()
            .into_iter()
            .filter(|r| r.method == method && r.path.starts_with(prefix))
            .collect()
    }

    fn push(&self, event: &str, data: &Value) {
        for tx in self.st.streams.lock().unwrap().iter() {
            let _ = tx.send(StreamCmd::Send(format!("event: {event}\ndata: {data}\n\n")));
        }
    }

    fn close_streams(&self) {
        for tx in self.st.streams.lock().unwrap().drain(..) {
            let _ = tx.send(StreamCmd::Close);
        }
    }

    fn event_conns(&self) -> usize {
        self.st.event_conns.load(Ordering::SeqCst)
    }

    fn set_list(&self, items: Vec<Value>, total: u32, by_app: Value) {
        *self.st.list.lock().unwrap() =
            json!({ "notifications": items, "unread_total": total, "unread_by_app": by_app });
    }
}

fn respond(stream: &mut TcpStream, status: u16, extra: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn handle(mut stream: TcpStream, st: &StubState) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {
                if let Some((k, v)) = line.split_once(':') {
                    headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
                }
            }
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let req = Req {
        method: method.clone(),
        path: path.clone(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    st.reqs.lock().unwrap().push(req.clone());
    let route = path.split('?').next().unwrap_or("");
    match (method.as_str(), route) {
        ("GET", "/healthz") => respond(&mut stream, 200, "", "ok"),
        ("GET", "/v1/workspaces/home") => respond(
            &mut stream,
            200,
            "X-Construct-Layout-Version: 1\r\n",
            FIXTURE,
        ),
        ("GET", "/v1/workspaces/home/events") => serve_events(stream, st),
        ("GET", "/v1/notifications") => {
            let status = st.list_status.load(Ordering::SeqCst);
            respond(
                &mut stream,
                status,
                "",
                &st.list.lock().unwrap().to_string(),
            )
        }
        ("POST", "/v1/notifications/read") => {
            let status = st.read_status.load(Ordering::SeqCst);
            respond(
                &mut stream,
                status,
                "",
                &st.read_answer.lock().unwrap().to_string(),
            )
        }
        ("GET", "/v1/notifications/channels") => respond(
            &mut stream,
            200,
            "",
            &json!({ "channels": [{ "app_id": "a", "id": "likes", "title": "Likes",
                "description": "", "default_enabled": true, "importance": "default",
                "enabled": true }] })
            .to_string(),
        ),
        ("GET", "/v1/push/key") => respond(&mut stream, 200, "", r#"{"vapid_public_key":"BKey"}"#),
        ("PUT", p) if p.starts_with("/v1/devices/") => {
            let push = req.json().get("push").is_some_and(Value::is_object);
            respond(
                &mut stream,
                200,
                "",
                &json!({ "ok": true, "push": push }).to_string(),
            )
        }
        ("DELETE", p) if p.starts_with("/v1/devices/") => {
            respond(&mut stream, 200, "", r#"{"ok":true,"deleted":true}"#)
        }
        ("POST", "/v1/pods/temporary") => {
            respond(&mut stream, 200, "", r#"{"session_token":"tok-new"}"#)
        }
        ("POST", _) => respond(&mut stream, 200, "", "{}"),
        _ => respond(&mut stream, 404, "", r#"{"error":"not_found"}"#),
    }
}

fn serve_events(mut stream: TcpStream, st: &StubState) {
    let (tx, rx) = mpsc::channel();
    st.streams.lock().unwrap().push(tx);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                Connection: close\r\n\r\n: hello\n\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    st.event_conns.fetch_add(1, Ordering::SeqCst);
    loop {
        let text = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(StreamCmd::Send(t)) => t,
            Ok(StreamCmd::Close) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => ": keepalive\n\n".to_owned(),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if stream
            .write_all(text.as_bytes())
            .and_then(|_| stream.flush())
            .is_err()
        {
            return;
        }
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "client-core-notify-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

/// An engine; `token`: a session already in the secure store.
fn engine(stub: &Stub, storage: &str, token: Option<&str>) -> ConstructEngine {
    let store = Arc::new(MemoryStore::new());
    if let Some(t) = token {
        store.set(&client_core::auth::session_key(&stub.base), t);
    }
    ConstructEngine::init_with_options(
        storage,
        &stub.base,
        "home",
        EngineOptions {
            config: EngineConfig {
                poll_interval: Duration::from_millis(100),
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(200),
            },
            secure_store: Some(store),
            ..EngineOptions::default()
        },
    )
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn counts(s: &StateSnapshot) -> Value {
    serde_json::from_str(&s.notifications_json).unwrap()
}

fn total(engine: &ConstructEngine) -> u64 {
    counts(&engine.snapshot())["unread_total"].as_u64().unwrap()
}

/// Collects the listener's calls.
fn listen(engine: &ConstructEngine) -> Arc<Mutex<Vec<String>>> {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&heard);
    engine.set_notification_listener(move |n| h.lock().unwrap().push(n.id.clone()));
    heard
}

/// Online, the stream open and its catch-up fetch done.
fn connected(stub: &Stub, engine: &ConstructEngine) {
    wait_until("the workspace", || {
        engine.snapshot().workspace_json != "null"
    });
    wait_until("the events stream", || stub.event_conns() >= 1);
    wait_until("the catch-up fetch", || {
        !stub.find("GET", "/v1/notifications?").is_empty()
    });
    // Its answer (a local stub, so quick) is merged.
    thread::sleep(Duration::from_millis(300));
}

#[test]
fn every_pod_call_carries_the_device_header_and_the_id_persists() {
    let stub = Stub::start();
    let dir = temp_dir("header");
    let engine = engine(&stub, dir.to_str().unwrap(), Some("tok-1"));
    let id = engine.device_id();
    assert!(client_core::notify::valid_device_id(&id), "{id}");
    connected(&stub, &engine);
    wait_until("device registration", || {
        !stub.find("PUT", "/v1/devices/").is_empty()
    });
    engine.dispatch(INCREMENT);
    wait_until("the outbox POST", || {
        !stub.find("POST", "/v1/counters/").is_empty()
    });
    let _ = engine.list_apps();
    let _ = engine.notification_channels();
    let reqs = stub.reqs();
    for path in [
        "/v1/workspaces/home",
        "/v1/workspaces/home/events",
        "/v1/notifications?",
        "/v1/devices/",
        "/v1/counters/home/increment",
        "/v1/apps",
        "/v1/notifications/channels",
    ] {
        assert!(
            reqs.iter().any(|r| r.path.starts_with(path)),
            "no request to {path}"
        );
    }
    for r in reqs.iter().filter(|r| r.path != "/healthz") {
        assert_eq!(
            r.header("x-construct-device"),
            Some(id.as_str()),
            "{} {}",
            r.method,
            r.path
        );
    }
    let put = &stub.find("PUT", "/v1/devices/")[0];
    assert_eq!(put.path, format!("/v1/devices/{id}"));
    assert_eq!(put.header("authorization"), Some("Bearer tok-1"));
    assert_eq!(put.json()["platform"], "unknown");
    drop(engine);
    // The same id after a restart.
    let again = self::engine(&stub, dir.to_str().unwrap(), None);
    assert_eq!(again.device_id(), id);
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_device_is_registered_after_a_sign_in_and_when_its_info_changes() {
    let stub = Stub::start();
    let engine = engine(&stub, "", None);
    wait_until("the events stream", || stub.event_conns() >= 1);
    thread::sleep(Duration::from_millis(200));
    assert!(
        stub.find("PUT", "/v1/devices/").is_empty(),
        "no session yet"
    );
    assert!(
        stub.find("GET", "/v1/notifications").is_empty(),
        "no catch-up without a session"
    );
    assert_eq!(
        counts(&engine.snapshot()),
        json!({ "unread_total": 0, "unread_by_app": {} })
    );
    engine.dispatch(
        &json!({ "intent": "action.auth.session", "path": "/v1/pods/temporary", "payload": {} })
            .to_string(),
    );
    wait_until("registration with the new session", || {
        stub.find("PUT", "/v1/devices/")
            .iter()
            .any(|r| r.header("authorization") == Some("Bearer tok-new"))
    });
    let n = stub.find("PUT", "/v1/devices/").len();
    engine.set_device_info("android", "Pixel 9");
    wait_until("registration with the new info", || {
        stub.find("PUT", "/v1/devices/")
            .iter()
            .any(|r| r.json()["platform"] == "android" && r.json()["name"] == "Pixel 9")
    });
    // Unchanged info: no new registration.
    let m = stub.find("PUT", "/v1/devices/").len();
    assert!(m > n);
    engine.set_device_info("android", "Pixel 9");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(stub.find("PUT", "/v1/devices/").len(), m);

    // Push subscription and channel choices go to this device.
    let id = engine.device_id();
    engine
        .register_push("https://push.example/x", "p", "a")
        .unwrap();
    let put = stub.find("PUT", "/v1/devices/").pop().unwrap();
    assert_eq!(
        put.json()["push"],
        json!({ "endpoint": "https://push.example/x", "p256dh": "p", "auth": "a" })
    );
    engine.set_channel_enabled("a", "likes", false).unwrap();
    let put = stub.find("PUT", "/v1/devices/").pop().unwrap();
    assert_eq!(put.path, format!("/v1/devices/{id}/channels"));
    assert_eq!(put.json(), json!({ "a.likes": false }));
    assert_eq!(engine.push_public_key().unwrap(), "BKey");
    engine.unregister_push().unwrap();
    let put = stub.find("PUT", "/v1/devices/").pop().unwrap();
    assert_eq!(put.json()["push"], Value::Null);
    assert!(put.json().as_object().unwrap().contains_key("push"));
}

#[test]
fn a_streamed_notification_updates_counts_and_calls_the_listener_once() {
    let stub = Stub::start();
    let engine = engine(&stub, "", Some("tok-1"));
    let heard = listen(&engine);
    connected(&stub, &engine);
    let ev = json!({ "notification": notification("5", "a", false), "unread_total": 1 });
    stub.push("notification", &ev);
    wait_until("the count", || total(&engine) == 1);
    assert_eq!(
        counts(&engine.snapshot()),
        json!({ "unread_total": 1, "unread_by_app": { "a": 1 } })
    );
    wait_until("the listener", || heard.lock().unwrap().len() == 1);
    assert_eq!(heard.lock().unwrap()[0], "5");
    // The same one again (a duplicate): no second call.
    stub.push("notification", &ev);
    // Then a read event clears the counts.
    stub.push(
        "notifications_read",
        &json!({ "unread_total": 0, "unread_by_app": {} }),
    );
    wait_until("read", || total(&engine) == 0);
    assert_eq!(heard.lock().unwrap().len(), 1);
    // The cache has it, read.
    stub.st.list_status.store(503, Ordering::SeqCst);
    let list = engine.list_notifications(None, 20, None).unwrap();
    assert_eq!(list.notifications.len(), 1);
    assert!(list.notifications[0].read);
}

#[test]
fn the_catch_up_fetch_after_a_reconnect_never_calls_the_listener() {
    let stub = Stub::start();
    stub.set_list(
        vec![notification("7", "a", false), notification("6", "b", false)],
        3,
        json!({ "a": 2, "b": 1 }),
    );
    let engine = engine(&stub, "", Some("tok-1"));
    let heard = listen(&engine);
    connected(&stub, &engine);
    wait_until("counts from the fetch", || total(&engine) == 3);
    assert_eq!(counts(&engine.snapshot())["unread_by_app"]["b"], 1);
    // Missed while disconnected: the next catch-up brings it, quietly.
    stub.set_list(
        vec![
            notification("8", "a", false),
            notification("7", "a", false),
            notification("6", "b", false),
        ],
        4,
        json!({ "a": 3, "b": 1 }),
    );
    let fetches = stub.find("GET", "/v1/notifications?").len();
    stub.close_streams();
    wait_until("reconnect", || stub.event_conns() >= 2);
    wait_until("the second catch-up", || {
        stub.find("GET", "/v1/notifications?").len() > fetches
    });
    wait_until("new counts", || total(&engine) == 4);
    let get = stub.find("GET", "/v1/notifications?").pop().unwrap();
    assert_eq!(get.path, "/v1/notifications?limit=20");
    assert_eq!(get.header("authorization"), Some("Bearer tok-1"));
    thread::sleep(Duration::from_millis(200));
    assert!(
        heard.lock().unwrap().is_empty(),
        "{:?}",
        heard.lock().unwrap()
    );
}

#[test]
fn mark_read_is_optimistic_then_posted_and_resent_after_a_failure() {
    let stub = Stub::start();
    stub.set_list(
        vec![notification("2", "a", false), notification("1", "a", false)],
        2,
        json!({ "a": 2 }),
    );
    let engine = engine(&stub, "", Some("tok-1"));
    connected(&stub, &engine);
    wait_until("counts", || total(&engine) == 2);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    engine.subscribe(move |snap| {
        s.lock()
            .unwrap()
            .push(counts(&snap)["unread_total"].clone());
    });

    // The pod has a problem: the local counts stand, the request is queued.
    stub.st.read_status.store(503, Ordering::SeqCst);
    let ev = engine.mark_notifications_read(&["1".to_owned()]).unwrap();
    assert_eq!(ev.unread_total, 1);
    assert_eq!(total(&engine), 1);
    assert!(seen.lock().unwrap().contains(&json!(1)));
    let posts = stub.find("POST", "/v1/notifications/read");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].json(), json!({ "ids": ["1"] }));
    assert_eq!(posts[0].header("authorization"), Some("Bearer tok-1"));
    // Offline answers come from the cache, with "1" read.
    stub.st.list_status.store(503, Ordering::SeqCst);
    let list = engine.list_notifications(None, 20, None).unwrap();
    let read: Vec<bool> = list.notifications.iter().map(|n| n.read).collect();
    assert_eq!(read, vec![false, true]);

    // Sent again when the stream reconnects.
    stub.st.read_status.store(200, Ordering::SeqCst);
    stub.st.list_status.store(200, Ordering::SeqCst);
    *stub.st.read_answer.lock().unwrap() =
        json!({ "unread_total": 1, "unread_by_app": { "a": 1 } });
    stub.set_list(
        vec![notification("2", "a", false), notification("1", "a", true)],
        1,
        json!({ "a": 1 }),
    );
    stub.close_streams();
    wait_until("the resend", || {
        stub.find("POST", "/v1/notifications/read").len() == 2
    });
    assert_eq!(
        stub.find("POST", "/v1/notifications/read")[1].json(),
        json!({ "ids": ["1"] })
    );

    // Mark all: the pod's answer comes back.
    *stub.st.read_answer.lock().unwrap() = json!({ "unread_total": 0, "unread_by_app": {} });
    let ev = engine.mark_all_notifications_read(None).unwrap();
    assert_eq!(ev.unread_total, 0);
    assert_eq!(total(&engine), 0);
    assert_eq!(
        stub.find("POST", "/v1/notifications/read")
            .pop()
            .unwrap()
            .json(),
        json!({ "all": true })
    );
    engine.mark_all_notifications_read(Some("a")).unwrap();
    assert_eq!(
        stub.find("POST", "/v1/notifications/read")
            .pop()
            .unwrap()
            .json(),
        json!({ "all": true, "app": "a" })
    );
}

#[test]
fn a_push_message_is_cached_and_counted_without_the_listener() {
    let stub = Stub::start();
    let engine = engine(&stub, "", Some("tok-1"));
    let heard = listen(&engine);
    connected(&stub, &engine);
    let payload =
        json!({ "v": 1, "notification": notification("9", "a", false), "unread_total": 1 });
    let n = engine
        .handle_push_message(&payload.to_string())
        .expect("a notification");
    assert_eq!((n.id.as_str(), n.app_id.as_str()), ("9", "a"));
    assert_eq!(n.target_uri.as_deref(), Some("a://posts/9"));
    assert_eq!(
        counts(&engine.snapshot()),
        json!({ "unread_total": 1, "unread_by_app": { "a": 1 } })
    );
    assert!(engine.handle_push_message("{\"v\":1}").is_none());
    assert!(engine.handle_push_message("garbage").is_none());
    // Offline, the inbox shows it from the cache.
    stub.st.list_status.store(503, Ordering::SeqCst);
    let list = engine.list_notifications(None, 20, None).unwrap();
    assert_eq!(list.notifications.len(), 1);
    assert_eq!(list.unread_total, 1);
    // Arriving on the stream afterwards: already known, no listener call.
    stub.push(
        "notification",
        &json!({ "notification": notification("9", "a", false), "unread_total": 1 }),
    );
    thread::sleep(Duration::from_millis(300));
    assert!(heard.lock().unwrap().is_empty());
}

#[test]
fn logout_deletes_the_device_first_and_clears_the_cache() {
    let stub = Stub::start();
    stub.set_list(vec![notification("3", "a", false)], 1, json!({ "a": 1 }));
    let dir = temp_dir("logout");
    let engine = engine(&stub, dir.to_str().unwrap(), Some("tok-1"));
    connected(&stub, &engine);
    wait_until("counts", || total(&engine) == 1);
    let _ = engine.notification_channels().unwrap();
    let id = engine.device_id();

    engine.logout();
    assert_eq!(total(&engine), 0, "zero at once");
    wait_until("the logout call", || {
        !stub.find("POST", "/v1/auth/logout").is_empty()
    });
    let reqs = stub.reqs();
    let delete = reqs
        .iter()
        .position(|r| r.method == "DELETE" && r.path == format!("/v1/devices/{id}"))
        .expect("device deleted");
    let logout = reqs
        .iter()
        .position(|r| r.path == "/v1/auth/logout")
        .unwrap();
    assert!(
        delete < logout,
        "the device goes while the session is valid"
    );
    assert_eq!(reqs[delete].header("authorization"), Some("Bearer tok-1"));

    let db = Store::open(&dir.join("cache.db")).unwrap();
    assert!(db.load_notifications(None, 50, None).unwrap().is_empty());
    assert_eq!(db.kv_get(KV_NOTIFICATION_UNREAD).unwrap(), None);
    assert_eq!(
        db.kv_get(client_core::store::KV_NOTIFICATION_CHANNELS)
            .unwrap(),
        None
    );
    assert!(
        engine
            .list_notifications(None, 20, None)
            .unwrap()
            .notifications
            .is_empty()
    );
    drop(db);
    drop(engine);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn leaving_the_pod_deletes_the_device() {
    let stub = Stub::start();
    let engine = engine(&stub, "", Some("tok-1"));
    connected(&stub, &engine);
    engine.forget_session();
    wait_until("the device deleted", || {
        stub.find("DELETE", "/v1/devices/")
            .iter()
            .any(|r| r.header("authorization") == Some("Bearer tok-1"))
    });
    assert!(stub.find("POST", "/v1/auth/logout").is_empty());
}
