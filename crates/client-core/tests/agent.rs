//! M7 client: `PromptAgent` dispatch, layout version tracking and the SSE
//! push channel, against a small hand-rolled stub of the pod (workspace
//! bundle with `X-Canvas-Layout-Version`, `/events` SSE stream that stays
//! open, `/v1/agent/modify_layout`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use client_core::store::{STATUS_PENDING, Store, StoreOps};
use client_core::{
    AGENT_OFFLINE, CanvasEngine, EngineConfig, EngineOptions, MemoryStore, SecureStore,
    StateSnapshot,
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);
const INCREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"#;

fn prompt(text: &str) -> String {
    json!({ "intent": "action.agent.prompt", "prompt_text": text }).to_string()
}

/// The fixture's layout for `version`: the title says which one it is.
fn layout(version: u64) -> Value {
    let mut ws: Value = serde_json::from_str::<Value>(FIXTURE).unwrap()["workspace"].clone();
    ws["title"] = json!(format!("Home v{version}"));
    ws
}

enum StreamCmd {
    Send(String),
    Close,
}

#[derive(Default)]
struct StubState {
    version: Mutex<u64>,
    fetches: AtomicUsize,
    event_conns: AtomicUsize,
    closed_streams: AtomicUsize,
    streams: Mutex<Vec<mpsc::Sender<StreamCmd>>>,
    /// `None`: success (bumps the version, no broadcast).
    agent_reply: Mutex<Option<(u16, String)>>,
    /// (authorization header, body) per agent request.
    agent_requests: Mutex<Vec<(String, String)>>,
    /// Authorization header per events connection.
    event_auth: Mutex<Vec<String>>,
    /// Workspace fetches answer an unversioned "Signed out" screen (no
    /// version header), like the signed-out `pixelfed_home`.
    unversioned: std::sync::atomic::AtomicBool,
}

struct Stub {
    base: String,
    st: Arc<StubState>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(StubState {
            version: Mutex::new(1),
            ..StubState::default()
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

    fn set_version(&self, v: u64) {
        *self.st.version.lock().unwrap() = v;
    }

    /// Send raw SSE text to every open stream.
    fn push_raw(&self, text: &str) {
        for tx in self.st.streams.lock().unwrap().iter() {
            let _ = tx.send(StreamCmd::Send(text.to_owned()));
        }
    }

    /// A `layout` event, with the data split over two `data:` lines.
    fn push_layout(&self, version: u64) {
        self.push_raw(&format!(
            "event: layout\ndata: {{\"workspace_id\":\"home\",\ndata: \"version\":{version}}}\n\n"
        ));
    }

    fn close_streams(&self) {
        for tx in self.st.streams.lock().unwrap().drain(..) {
            let _ = tx.send(StreamCmd::Close);
        }
    }

    fn fetches(&self) -> usize {
        self.st.fetches.load(Ordering::SeqCst)
    }

    fn event_conns(&self) -> usize {
        self.st.event_conns.load(Ordering::SeqCst)
    }
}

fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\
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
    let mut content_length = 0usize;
    let mut auth = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {
                if let Some((k, v)) = line.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_length = v.trim().parse().unwrap_or(0);
                    } else if k.eq_ignore_ascii_case("authorization") {
                        auth = v.trim().to_owned();
                    }
                }
            }
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).into_owned();
    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_owned();
    let get = request_line.starts_with("GET ");

    match (get, path.as_str()) {
        (true, "/healthz") => respond(&mut stream, "200 OK", "", "ok"),
        (true, "/v1/workspaces/home") => {
            st.fetches.fetch_add(1, Ordering::SeqCst);
            let v = *st.version.lock().unwrap();
            let mut bundle: Value = serde_json::from_str(FIXTURE).unwrap();
            bundle["workspace"] = layout(v);
            if st.unversioned.load(Ordering::SeqCst) {
                bundle["workspace"]["title"] = json!("Signed out");
                respond(&mut stream, "200 OK", "", &bundle.to_string());
                return;
            }
            respond(
                &mut stream,
                "200 OK",
                &format!("X-Canvas-Layout-Version: {v}\r\n"),
                &bundle.to_string(),
            );
        }
        (true, "/v1/workspaces/home/events") => serve_events(stream, st, auth),
        (false, "/v1/agent/modify_layout") => {
            st.agent_requests.lock().unwrap().push((auth, body));
            let reply = st.agent_reply.lock().unwrap().clone();
            match reply {
                Some((code, body)) => respond(&mut stream, &format!("{code} Error"), "", &body),
                None => {
                    let v = {
                        let mut v = st.version.lock().unwrap();
                        *v += 1;
                        *v
                    };
                    let resp = json!({
                        "version": "1.0.0", "workspace": layout(v), "entities": {},
                        "layout_version": v, "provider": "rules", "model": null,
                        "patch": [{"op": "replace", "path": "/title", "value": format!("Home v{v}")}],
                    });
                    respond(&mut stream, "200 OK", "", &resp.to_string());
                }
            }
        }
        // Outbox POSTs stay PENDING.
        (false, _) => respond(&mut stream, "503 Service Unavailable", "", "{}"),
        _ => respond(&mut stream, "404 Not Found", "", "not found"),
    }
}

/// Keep an SSE stream open: the current version first, then whatever the
/// test pushes, and a `: keepalive` comment every 100 ms (a failed write
/// means the client closed the connection).
fn serve_events(mut stream: TcpStream, st: &StubState, auth: String) {
    st.event_auth.lock().unwrap().push(auth);
    let (tx, rx) = mpsc::channel();
    st.streams.lock().unwrap().push(tx);
    let v = *st.version.lock().unwrap();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
         Connection: close\r\n\r\n: hello\n\nevent: layout\r\ndata: {{\"workspace_id\":\"home\",\"version\":{v}}}\r\n\r\n"
    );
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
        if stream.write_all(text.as_bytes()).and_then(|_| stream.flush()).is_err() {
            st.closed_streams.fetch_add(1, Ordering::SeqCst);
            return;
        }
    }
}

fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_secs(5),
        backoff_cap: Duration::from_secs(5),
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("client-core-agent-{tag}-{}-{nanos}", std::process::id()))
}

fn engine(stub: &Stub, storage: &str) -> CanvasEngine {
    CanvasEngine::init_with_config(storage, &stub.base, "home", fast_config())
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn title(s: &StateSnapshot) -> String {
    let ws: Value = serde_json::from_str(&s.workspace_json).unwrap();
    ws["title"].as_str().unwrap_or_default().to_owned()
}

fn agent(s: &StateSnapshot) -> Value {
    serde_json::from_str(&s.agent_json).unwrap()
}

fn count(s: &StateSnapshot) -> i64 {
    let entities: Value = serde_json::from_str(&s.entities_json).unwrap();
    entities["core://counters/home"]["count"].as_i64().unwrap()
}

/// Online with layout v1 and the events stream connected.
fn ready(stub: &Stub, engine: &CanvasEngine) {
    wait_until("layout v1", || title(&engine.snapshot()) == "Home v1");
    wait_until("events connection", || stub.event_conns() >= 1);
}

#[test]
fn prompt_success_applies_layout_and_reports_done() {
    let stub = Stub::start();
    let dir = temp_dir("prompt");
    let store = Arc::new(MemoryStore::new());
    store.set(&client_core::auth::session_key(&stub.base), "tok-1");
    let engine = CanvasEngine::init_with_options(
        dir.to_str().unwrap(),
        &stub.base,
        "home",
        EngineOptions {
            config: fast_config(),
            secure_store: Some(store),
            ..EngineOptions::default()
        },
    );
    ready(&stub, &engine);
    assert_eq!(agent(&engine.snapshot()), json!({"state": "idle"}));
    let fetches = stub.fetches();

    engine.dispatch(&prompt("  Hide post captions "));
    wait_until("agent done", || agent(&engine.snapshot())["state"] == "done");
    let snap = engine.snapshot();
    assert_eq!(title(&snap), "Home v2", "returned layout applied immediately");
    let a = agent(&snap);
    assert_eq!(a["provider"], "rules");
    assert_eq!(a["layout_version"], 2);
    assert_eq!(a["message"], "Layout updated (v2, rules)");
    assert_eq!(count(&snap), 0, "entities kept");
    assert_eq!(stub.fetches(), fetches, "applied without a refetch");

    let (auth, body) = stub.st.agent_requests.lock().unwrap()[0].clone();
    assert_eq!(auth, "Bearer tok-1");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body, json!({"prompt": "Hide post captions", "workspace_id": "home"}));
    assert_eq!(stub.st.event_auth.lock().unwrap()[0], "Bearer tok-1");

    // Persisted with the cached entity URIs and queries intact.
    drop(engine);
    let cached = Store::open(&dir.join("cache.db")).unwrap().load_workspace("home").unwrap().unwrap();
    assert_eq!(serde_json::to_value(&cached.layout).unwrap()["title"], "Home v2");
    assert_eq!(cached.entity_uris, vec!["core://counters/home".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prompt_response_is_not_painted_over_an_unversioned_screen() {
    // An open agent endpoint answers a signed-out client with the feed
    // layout; the signed-out screen (no version header) must stay.
    let stub = Stub::start();
    stub.st.unversioned.store(true, Ordering::SeqCst);
    let engine = engine(&stub, "");
    wait_until("signed-out screen", || title(&engine.snapshot()) == "Signed out");
    let fetches = stub.fetches();
    engine.dispatch(&prompt("Hide post captions"));
    wait_until("agent done", || agent(&engine.snapshot())["state"] == "done");
    wait_until("refetch", || stub.fetches() > fetches);
    assert_eq!(title(&engine.snapshot()), "Signed out", "response layout not applied");
}

#[test]
fn prompt_errors_show_the_pods_message_and_keep_the_layout() {
    let stub = Stub::start();
    let engine = engine(&stub, "");
    ready(&stub, &engine);

    *stub.st.agent_reply.lock().unwrap() = Some((
        422,
        json!({"error": "invalid_patch", "detail": "missing field `action_slot`", "patch": [],
               "provider": "rules"})
        .to_string(),
    ));
    engine.dispatch(&prompt("remove the action bar"));
    wait_until("agent error", || agent(&engine.snapshot())["state"] == "error");
    let snap = engine.snapshot();
    let a = agent(&snap);
    assert_eq!(a["message"], "invalid_patch: missing field `action_slot`");
    assert_eq!(a["provider"], "rules");
    assert_eq!(title(&snap), "Home v1", "layout unchanged");

    *stub.st.agent_reply.lock().unwrap() = Some((
        422,
        json!({"error": "unsupported_prompt", "supported": ["Hide post captions", "Photos first"]})
            .to_string(),
    ));
    engine.dispatch(&prompt("make it pop"));
    wait_until("agent error", || {
        agent(&engine.snapshot())["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("unsupported_prompt"))
    });
    let msg = agent(&engine.snapshot())["message"].as_str().unwrap().to_owned();
    assert!(msg.contains("Hide post captions") && msg.contains("Photos first"), "{msg}");

    *stub.st.agent_reply.lock().unwrap() = Some((503, "upstream down".into()));
    engine.dispatch(&prompt("Hide post captions"));
    wait_until("agent error", || {
        agent(&engine.snapshot())["message"] == "HTTP 503: upstream down"
    });

    engine.dispatch(&prompt("   "));
    assert_eq!(agent(&engine.snapshot())["state"], "error");
    assert_eq!(title(&engine.snapshot()), "Home v1");

    // The pod cannot be reached at all.
    let offline = CanvasEngine::init_with_config("", "http://127.0.0.1:1", "home", fast_config());
    offline.dispatch(&prompt("Hide post captions"));
    assert_eq!(agent(&offline.snapshot())["state"], "running");
    wait_until("offline error", || agent(&offline.snapshot())["state"] == "error");
    assert_eq!(agent(&offline.snapshot())["message"], AGENT_OFFLINE);
}

#[test]
fn sse_layout_event_triggers_refetch_and_stale_or_equal_versions_are_ignored() {
    let stub = Stub::start();
    let engine = engine(&stub, "");
    ready(&stub, &engine);
    let fetches = stub.fetches();

    // Equal and older versions (with comments in between): no refetch.
    stub.push_layout(1);
    stub.push_raw(": keepalive\n\n");
    stub.push_layout(0);
    stub.push_raw("event: other\ndata: {\"version\":99}\n\n");
    // Then a newer one: exactly one refetch, which brings v3.
    stub.set_version(3);
    stub.push_layout(3);
    wait_until("layout v3", || title(&engine.snapshot()) == "Home v3");
    assert_eq!(stub.fetches(), fetches + 1);

    // Repeating the applied version does nothing.
    stub.push_layout(3);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(stub.fetches(), fetches + 1);

    // After a prompt applied v4, the SSE echo of v4 is ignored too.
    engine.dispatch(&prompt("Photos first"));
    wait_until("agent done", || agent(&engine.snapshot())["state"] == "done");
    assert_eq!(title(&engine.snapshot()), "Home v4");
    stub.push_layout(4);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(stub.fetches(), fetches + 1);
}

#[test]
fn reconnects_after_the_stream_is_dropped_and_catches_up() {
    let stub = Stub::start();
    let engine = engine(&stub, "");
    ready(&stub, &engine);

    stub.close_streams();
    // Changed while disconnected: the first message after reconnecting
    // carries v2, which triggers the refetch.
    stub.set_version(2);
    wait_until("reconnect", || stub.event_conns() >= 2);
    wait_until("layout v2", || title(&engine.snapshot()) == "Home v2");
}

#[test]
fn dropping_the_engine_closes_the_stream_and_stops_the_thread() {
    let stub = Stub::start();
    let engine = engine(&stub, "");
    ready(&stub, &engine);
    // The thread is alive and reading.
    stub.set_version(2);
    stub.push_layout(2);
    wait_until("layout v2", || title(&engine.snapshot()) == "Home v2");
    assert_eq!(stub.st.closed_streams.load(Ordering::SeqCst), 0);
    drop(engine);
    wait_until("stream closed by the client", || {
        stub.st.closed_streams.load(Ordering::SeqCst) >= 1
    });
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(stub.event_conns(), 1, "no reconnect after drop");
}

#[test]
fn pending_outbox_row_survives_a_pushed_layout_change() {
    let stub = Stub::start();
    let dir = temp_dir("pending");
    let engine = engine(&stub, dir.to_str().unwrap());
    ready(&stub, &engine);

    engine.dispatch(INCREMENT);
    let snap = engine.snapshot();
    assert_eq!((count(&snap), snap.pending_actions), (1, 1));

    let fetches = stub.fetches();
    stub.set_version(2);
    stub.push_layout(2);
    wait_until("layout v2", || title(&engine.snapshot()) == "Home v2");
    assert_eq!(stub.fetches(), fetches + 1);
    let snap = engine.snapshot();
    assert_eq!(count(&snap), 1, "optimistic value re-applied on the pushed refetch");
    assert_eq!(snap.pending_actions, 1);

    drop(engine);
    let rows = Store::open(&dir.join("cache.db")).unwrap().pending_outbox().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, STATUS_PENDING);
    assert_eq!(rows[0].endpoint, "/v1/counters/home/increment");
    let _ = std::fs::remove_dir_all(&dir);
}
