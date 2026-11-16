//! `event: data` reloads (D50, decision `scoped-data-events`): an event
//! that says what changed reloads only the cached data it matches (on
//! screen first, the rest when shown again), an event that doesn't say
//! reloads everything, loads run a few at a time, and a path the pod
//! answers 404 for is dropped. An event sent while the stream was down is
//! caught up on when it reconnects. Against a small stub pod.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use client_core::{ConstructEngine, EngineConfig};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../protocol/tests/data/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long the stub takes to answer a data path (to see loads overlap).
const SLOW: Duration = Duration::from_millis(150);

/// The data paths the stub serves, and the entity each one holds.
const PATHS: [(&str, &str, &str); 3] = [
    ("/v1/data/a", "t://groceries/1", "list:groceries"),
    ("/v1/data/b", "t://meals/1", "list:meals"),
    ("/v1/data/c", "t://groceries/2", "list:groceries:row:2"),
];

#[derive(Default)]
struct StubState {
    /// Requests per path.
    hits: Mutex<BTreeMap<String, usize>>,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
    event_conns: AtomicUsize,
    streams: Mutex<Vec<mpsc::Sender<String>>>,
    /// `/v1/data/gone` answers 404.
    gone: AtomicBool,
    /// Part of every row's name: bumped to change the data.
    generation: AtomicUsize,
    construct: crate::construct_stub::Construct,
}

/// Layouts `home` and `sub`, as bundles.
fn layout(id: &str) -> crate::construct_stub::Answer {
    match id {
        "home" => Some((200, FIXTURE.to_owned())),
        "sub" => {
            let mut b: Value = serde_json::from_str(FIXTURE).unwrap();
            b["workspace"]["id"] = json!("sub");
            b["entities"] = json!({ "t://groceries/9": { "name": "nine" } });
            b["queries"] = json!({});
            Some((200, b.to_string()))
        }
        _ => None,
    }
}

struct Stub {
    base: String,
    st: Arc<StubState>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(StubState::default());
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

    /// Requests for a data path, or fetches of a layout (`layout:<id>`).
    fn hits(&self, path: &str) -> usize {
        match path.strip_prefix("layout:") {
            Some(id) => self.st.construct.fetches(id),
            None => self.st.hits.lock().unwrap().get(path).copied().unwrap_or(0),
        }
    }

    /// Every request counted so far.
    fn total(&self) -> usize {
        self.st.hits.lock().unwrap().values().sum::<usize>()
            + self.hits("layout:home")
            + self.hits("layout:sub")
    }

    /// Ends every open events stream (the engine reconnects after its
    /// backoff); events sent until then reach no one.
    fn cut_streams(&self) {
        self.st.streams.lock().unwrap().clear();
    }

    fn data_event(&self, body: Value) {
        let text = format!("event: data\ndata: {body}\n\n");
        for tx in self.st.streams.lock().unwrap().iter() {
            let _ = tx.send(text.clone());
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn bundle(uri: &str, key: &str, generation: usize) -> String {
    let name = format!("{uri} v{generation}");
    json!({ "entities": { uri: { "name": name } }, "queries": { key: [uri] } }).to_string()
}

fn handle(mut stream: TcpStream, st: &StubState) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = std::io::Read::read_exact(&mut reader, &mut body);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    if path == "/healthz" {
        return respond(&mut stream, "200 OK", "ok");
    }
    if path == crate::construct_stub::EVENTS {
        return serve_events(stream, st);
    }
    let now = st.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    st.most_in_flight.fetch_max(now, Ordering::SeqCst);
    if let Some((code, answer)) =
        st.construct
            .answer(&method, &path, &String::from_utf8_lossy(&body), &mut layout)
    {
        let status = format!("{code} {}", crate::construct_stub::reason(code));
        respond(&mut stream, &status, &answer);
        st.in_flight.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    *st.hits.lock().unwrap().entry(path.clone()).or_default() += 1;
    thread::sleep(SLOW);
    let generation = st.generation.load(Ordering::SeqCst);
    match path.as_str() {
        "/v1/data/gone" if st.gone.load(Ordering::SeqCst) => {
            respond(&mut stream, "404 Not Found", "{}")
        }
        // Two answers listing the same key (each layout binding a feed
        // carries its query keys).
        "/v1/data/x" | "/v1/data/y" => {
            let uri = format!("t://{}/1", &path[9..]);
            let body = json!({ "entities": { &uri: { "name": format!("{uri} v{generation}") } },
                "queries": { "list:shared": [uri] } });
            respond(&mut stream, "200 OK", &body.to_string())
        }
        "/v1/data/gone" => respond(
            &mut stream,
            "200 OK",
            &bundle("t://other/1", "list:other", generation),
        ),
        p => match PATHS.iter().find(|(q, _, _)| *q == p) {
            Some((_, uri, key)) => respond(&mut stream, "200 OK", &bundle(uri, key, generation)),
            None => respond(&mut stream, "404 Not Found", "{}"),
        },
    }
    st.in_flight.fetch_sub(1, Ordering::SeqCst);
}

fn serve_events(mut stream: TcpStream, st: &StubState) {
    let (tx, rx) = mpsc::channel::<String>();
    st.streams.lock().unwrap().push(tx);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
        st.construct.event(&mut layout)
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    st.event_conns.fetch_add(1, Ordering::SeqCst);
    loop {
        let text = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(t) => t,
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

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn loaded(engine: &ConstructEngine, key: &str) -> bool {
    let loads: Value = serde_json::from_str(&engine.snapshot().loads_json).unwrap();
    loads[key]["state"] == "ready"
}

/// An engine with the workspace, the three data paths and layout `sub`
/// loaded, and the events stream open.
fn ready(stub: &Stub) -> ConstructEngine {
    let config = EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_secs(5),
        backoff_cap: Duration::from_secs(5),
    };
    let engine = ConstructEngine::init_with_config("", &stub.base, "home", config);
    wait_until("the workspace", || stub.hits("layout:home") >= 1);
    for (path, _, _) in PATHS {
        engine.dispatch(&json!({ "intent": "action.data.load", "path": path }).to_string());
    }
    engine.open_layout("sub");
    for (path, _, _) in PATHS {
        wait_until(path, || loaded(&engine, path));
    }
    wait_until("layout sub", || loaded(&engine, "layout:sub"));
    wait_until("events connection", || {
        stub.st.event_conns.load(Ordering::SeqCst) >= 1
    });
    engine
}

/// Settles: no request is under way, and none starts for a while.
fn settle(stub: &Stub) {
    loop {
        let before = stub.total();
        thread::sleep(SLOW * 3);
        if stub.total() == before && stub.st.in_flight.load(Ordering::SeqCst) == 0 {
            return;
        }
    }
}

fn groceries_changed() -> Value {
    json!({
        "workspace_id": "home",
        "source": "collections",
        "changes": [{ "collection": "groceries", "rows": ["1"] }],
        "uri_prefixes": ["t://groceries/"],
        "query_prefixes": ["list:groceries"],
    })
}

#[test]
fn scoped_data_event_reloads_only_the_matching_paths() {
    let stub = Stub::start();
    let _engine = ready(&stub);
    settle(&stub);
    let before = |p: &str| stub.hits(p);
    let (a, b, c, sub, home) = (
        before("/v1/data/a"),
        before("/v1/data/b"),
        before("/v1/data/c"),
        before("layout:sub"),
        before("layout:home"),
    );
    stub.data_event(groceries_changed());
    // a: an entity under the prefix; c: a query key under it; sub: a
    // layout holding one.
    wait_until("a reloaded", || stub.hits("/v1/data/a") == a + 1);
    wait_until("c reloaded", || stub.hits("/v1/data/c") == c + 1);
    wait_until("sub reloaded", || stub.hits("layout:sub") == sub + 1);
    settle(&stub);
    assert_eq!(stub.hits("/v1/data/b"), b, "b holds only meals");
    assert_eq!(
        stub.hits("layout:home"),
        home,
        "the workspace holds no groceries"
    );
    assert_eq!(stub.hits("/v1/data/a"), a + 1);
}

#[test]
fn off_screen_matches_are_reloaded_when_shown_again() {
    let stub = Stub::start();
    let engine = ready(&stub);
    engine.set_visible(vec!["/v1/data/a".into(), "layout:home".into()]);
    settle(&stub);
    let (a, c, sub) = (
        stub.hits("/v1/data/a"),
        stub.hits("/v1/data/c"),
        stub.hits("layout:sub"),
    );
    stub.data_event(groceries_changed());
    wait_until("a reloaded", || stub.hits("/v1/data/a") == a + 1);
    settle(&stub);
    assert_eq!(
        stub.hits("/v1/data/c"),
        c,
        "c is off screen: only marked stale"
    );
    assert_eq!(stub.hits("layout:sub"), sub);
    // Back to a screen showing c: it is reloaded once; sub stays stale.
    engine.set_visible(vec!["/v1/data/c".into(), "layout:home".into()]);
    wait_until("c reloaded", || stub.hits("/v1/data/c") == c + 1);
    settle(&stub);
    assert_eq!(stub.hits("layout:sub"), sub);
    // Shown again without a change: nothing to reload.
    engine.set_visible(vec!["/v1/data/a".into(), "layout:home".into()]);
    engine.set_visible(vec!["/v1/data/c".into(), "layout:home".into()]);
    settle(&stub);
    assert_eq!(stub.hits("/v1/data/c"), c + 1);
    assert_eq!(stub.hits("/v1/data/a"), a + 1);
}

#[test]
fn unscoped_data_event_reloads_everything_a_few_at_a_time() {
    let stub = Stub::start();
    let engine = ready(&stub);
    // Visibility only orders scoped reloads: an unscoped event reloads all.
    engine.set_visible(vec!["/v1/data/a".into()]);
    settle(&stub);
    stub.st.most_in_flight.store(0, Ordering::SeqCst);
    let before: Vec<usize> = ["/v1/data/a", "/v1/data/b", "/v1/data/c", "layout:sub"]
        .iter()
        .map(|p| stub.hits(p))
        .collect();
    let home = stub.hits("layout:home");
    stub.data_event(json!({ "workspace_id": "home" }));
    for (p, n) in ["/v1/data/a", "/v1/data/b", "/v1/data/c", "layout:sub"]
        .iter()
        .zip(before)
    {
        wait_until(p, || stub.hits(p) == n + 1);
    }
    wait_until("the workspace", || stub.hits("layout:home") > home);
    let most = stub.st.most_in_flight.load(Ordering::SeqCst);
    assert!(
        (2..=client_core::reload::PARALLEL_LOADS).contains(&most),
        "{most} requests at once"
    );
}

#[test]
fn a_path_the_pod_no_longer_has_is_dropped_from_reloads() {
    let stub = Stub::start();
    let engine = ready(&stub);
    engine.dispatch(&json!({ "intent": "action.data.load", "path": "/v1/data/gone" }).to_string());
    wait_until("gone loaded", || loaded(&engine, "/v1/data/gone"));
    settle(&stub);
    stub.st.gone.store(true, Ordering::SeqCst);
    let gone = stub.hits("/v1/data/gone");
    let a = stub.hits("/v1/data/a");
    stub.data_event(json!({ "workspace_id": "home" }));
    wait_until("gone asked once", || stub.hits("/v1/data/gone") == gone + 1);
    settle(&stub);
    stub.data_event(json!({ "workspace_id": "home" }));
    wait_until("a reloaded twice", || stub.hits("/v1/data/a") == a + 2);
    settle(&stub);
    assert_eq!(stub.hits("/v1/data/gone"), gone + 1, "not asked again");
}

fn name_of(engine: &ConstructEngine, uri: &str) -> String {
    let entities: Value = serde_json::from_str(&engine.snapshot().entities_json).unwrap();
    entities[uri]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_data_event_sent_while_the_stream_was_down_is_caught_up_on_reconnect() {
    let stub = Stub::start();
    let engine = ready(&stub);
    engine.set_visible(vec!["/v1/data/a".into(), "layout:home".into()]);
    settle(&stub);
    assert_eq!(name_of(&engine, "t://groceries/1"), "t://groceries/1 v0");
    let (b, c) = (stub.hits("/v1/data/b"), stub.hits("/v1/data/c"));
    // The stream drops (the app in the background); the data changes and
    // the pod's event reaches no one.
    stub.cut_streams();
    stub.st.generation.store(1, Ordering::SeqCst);
    stub.data_event(json!({ "workspace_id": "home" }));
    wait_until("the stream reconnected", || {
        stub.st.event_conns.load(Ordering::SeqCst) >= 2
    });
    // What's on screen shows the change without anything opening it.
    wait_until("a caught up", || {
        name_of(&engine, "t://groceries/1") == "t://groceries/1 v1"
    });
    settle(&stub);
    // The rest is only marked stale: reloaded once shown again.
    assert_eq!(stub.hits("/v1/data/b"), b, "b is off screen");
    assert_eq!(stub.hits("/v1/data/c"), c, "c is off screen");
    engine.set_visible(vec!["/v1/data/c".into(), "layout:home".into()]);
    wait_until("c caught up", || {
        name_of(&engine, "t://groceries/2") == "t://groceries/2 v1"
    });
}

#[test]
fn the_first_connection_reloads_nothing() {
    let stub = Stub::start();
    let _engine = ready(&stub);
    settle(&stub);
    let a = stub.hits("/v1/data/a");
    thread::sleep(SLOW * 4);
    assert_eq!(stub.st.event_conns.load(Ordering::SeqCst), 1);
    assert_eq!(stub.hits("/v1/data/a"), a);
}

fn shared(engine: &ConstructEngine) -> Value {
    let q: Value = serde_json::from_str(&engine.snapshot().queries_json).unwrap();
    q["list:shared"].clone()
}

fn load(engine: &ConstructEngine, path: &str) {
    engine.dispatch(&json!({ "intent": "action.data.load", "path": path }).to_string());
}

/// Decision `newest-query-answer-wins`: a key two answers list is the newest
/// answer's, whichever path sorts last.
#[test]
fn a_key_two_answers_list_is_the_newest_answers() {
    let stub = Stub::start();
    let engine = ready(&stub);
    load(&engine, "/v1/data/y");
    wait_until("y", || loaded(&engine, "/v1/data/y"));
    load(&engine, "/v1/data/x");
    wait_until("x", || shared(&engine) == json!(["t://x/1"]));
    load(&engine, "/v1/data/y");
    wait_until("y again", || shared(&engine) == json!(["t://y/1"]));
}

/// Decision `newest-query-answer-wins`: answers restored at launch are older
/// than any fetched since, and are loaded again once shown.
#[test]
fn a_restored_answer_never_hides_a_fresh_one() {
    let stub = Stub::start();
    let dir = std::env::temp_dir().join(format!("construct-restored-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = || EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_secs(5),
        backoff_cap: Duration::from_secs(5),
    };
    let first =
        ConstructEngine::init_with_config(dir.to_str().unwrap(), &stub.base, "home", config());
    load(&first, "/v1/data/x");
    wait_until("x", || loaded(&first, "/v1/data/x"));
    load(&first, "/v1/data/y");
    wait_until("y last", || shared(&first) == json!(["t://y/1"]));
    drop(first);

    let again =
        ConstructEngine::init_with_config(dir.to_str().unwrap(), &stub.base, "home", config());
    assert_eq!(
        shared(&again),
        json!(["t://y/1"]),
        "both restored: path order"
    );
    let y = stub.hits("/v1/data/y");
    load(&again, "/v1/data/x");
    wait_until("the fresh x wins", || shared(&again) == json!(["t://x/1"]));
    assert_eq!(stub.hits("/v1/data/y"), y, "y isn't shown");
    again.set_visible(vec!["/v1/data/y".into(), "layout:home".into()]);
    wait_until("y, shown, is loaded again", || {
        stub.hits("/v1/data/y") == y + 1
    });
    wait_until("and is the newest", || shared(&again) == json!(["t://y/1"]));
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}
