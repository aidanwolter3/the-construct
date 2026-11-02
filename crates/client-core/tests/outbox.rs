//! Sync worker and refetch merge (docs/DECISIONS.md D14) against the real
//! server (`server::app`, in-process on a local port, test hooks enabled).
//! Intervals are shortened through `EngineConfig`; waits poll with
//! timeouts instead of fixed sleeps.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::Request;
use axum::middleware::{self, Next};
use client_core::store::{OutboxRow, Store, StoreOps};
use client_core::{CanvasEngine, EngineConfig, StateSnapshot};
use protocol::WorkspaceSpec;
use serde_json::Value;
use server::AppState;

const FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);
const URI: &str = "core://counters/home";

const INCREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"#;
const DECREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count - 1","rollback_value":0,"endpoint":"/v1/counters/home/decrement","payload":{}}"#;

const INC: &str = "/v1/counters/home/increment";
const DEC: &str = "/v1/counters/home/decrement";

fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(20),
        backoff_cap: Duration::from_millis(200),
    }
}

/// One POST the server saw: path, response status, arrival time.
#[derive(Clone, Debug)]
struct Hit {
    path: String,
    status: u16,
    at: Instant,
}

/// The real server on a local port, recording every POST it answers.
/// The same app also listens on a private admin port (used for the fault
/// hook), so a test can prepare state before the engine's port opens.
/// Dropping it closes both ports.
struct TestServer {
    state: Arc<AppState>,
    addr: SocketAddr,
    admin: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    /// Mutation POSTs that have arrived (counted before any delay).
    arrivals: Arc<AtomicUsize>,
    rt: Option<tokio::runtime::Runtime>,
}

fn spawn_serve(rt: &tokio::runtime::Runtime, addr: SocketAddr, app: axum::Router) -> SocketAddr {
    let std_listener = TcpListener::bind(addr).unwrap();
    std_listener.set_nonblocking(true).unwrap();
    let addr = std_listener.local_addr().unwrap();
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

impl TestServer {
    fn start() -> Self {
        Self::start_on("127.0.0.1:0".parse().unwrap())
    }

    fn start_on(addr: SocketAddr) -> Self {
        Self::start_on_with(addr, |_| {})
    }

    /// Start, running `setup` (e.g. `set_count`, `fault`) before `addr`
    /// accepts connections.
    fn start_on_with(addr: SocketAddr, setup: impl FnOnce(&TestServer)) -> Self {
        Self::build(addr, Duration::ZERO, setup)
    }

    /// Like [`TestServer::start`], but every mutation POST waits
    /// `post_delay` before it is handled.
    fn start_slow(post_delay: Duration) -> Self {
        Self::build("127.0.0.1:0".parse().unwrap(), post_delay, |_| {})
    }

    fn build(addr: SocketAddr, post_delay: Duration, setup: impl FnOnce(&TestServer)) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(AppState::in_memory().with_test_hooks(true));
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let arrivals: Arc<AtomicUsize> = Arc::default();
        let log = Arc::clone(&hits);
        let arrived = Arc::clone(&arrivals);
        let app = server::app(Arc::clone(&state)).layer(middleware::from_fn(
            move |req: Request, next: Next| {
                let log = Arc::clone(&log);
                let arrived = Arc::clone(&arrived);
                async move {
                    let post = req.method() == axum::http::Method::POST;
                    let path = req.uri().path().to_string();
                    if post && !path.starts_with("/v1/test/") {
                        arrived.fetch_add(1, AtomicOrdering::SeqCst);
                        if !post_delay.is_zero() {
                            tokio::time::sleep(post_delay).await;
                        }
                    }
                    let resp = next.run(req).await;
                    if post && !path.starts_with("/v1/test/") {
                        log.lock().unwrap().push(Hit {
                            path,
                            status: resp.status().as_u16(),
                            at: Instant::now(),
                        });
                    }
                    resp
                }
            },
        ));
        let admin = spawn_serve(&rt, "127.0.0.1:0".parse().unwrap(), app.clone());
        let mut server = Self {
            state,
            addr: admin,
            admin,
            hits,
            arrivals,
            rt: Some(rt),
        };
        setup(&server);
        server.addr = spawn_serve(server.rt.as_ref().unwrap(), addr, app);
        server
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn count(&self) -> i64 {
        self.state.store().counter("home").unwrap()
    }

    fn set_count(&self, n: i64) {
        self.state.store().reset("home").unwrap();
        for _ in 0..n {
            self.state.store().increment("home").unwrap();
        }
    }

    /// Make the next `count` mutation POSTs answer `status`.
    fn fault(&self, status: u16, count: u32) {
        let resp = reqwest::blocking::Client::new()
            .post(format!("http://{}/v1/test/fault", self.admin))
            .header("Content-Type", "application/json")
            .body(format!(r#"{{"status":{status},"count":{count}}}"#))
            .send()
            .unwrap();
        assert!(resp.status().is_success(), "fault hook: {}", resp.status());
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn hit_list(&self) -> Vec<(String, u16)> {
        self.hits()
            .into_iter()
            .map(|h| (h.path, h.status))
            .collect()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// A local address with nothing listening on it (yet).
fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "client-core-outbox-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

/// Write the counter fixture (count 0) into `<dir>/cache.db`, the way a
/// successful fetch would, so an offline engine has the entity.
fn seed_cache(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let spec: WorkspaceSpec = serde_json::from_str(FIXTURE).unwrap();
    let mut store = Store::open(&dir.join("cache.db")).unwrap();
    let uris: Vec<String> = spec.entities.keys().cloned().collect();
    store
        .transaction(|tx| {
            tx.save_workspace("home", &spec.workspace, &uris, 1)?;
            for (uri, e) in &spec.entities {
                tx.upsert_entity(uri, e, 1)?;
            }
            Ok(())
        })
        .unwrap();
}

fn rows(dir: &Path) -> Vec<OutboxRow> {
    Store::open(&dir.join("cache.db"))
        .unwrap()
        .pending_outbox()
        .unwrap()
}

fn count(snap: &StateSnapshot) -> i64 {
    let entities: Value = serde_json::from_str(&snap.entities_json).unwrap();
    entities[URI]["count"].as_i64().unwrap()
}

fn engine(dir: &Path, base: &str, config: EngineConfig) -> CanvasEngine {
    CanvasEngine::init_with_config(dir.to_str().unwrap(), base, "home", config)
}

/// Poll `cond` every 10 ms until it holds; panic with `what` after TIMEOUT.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_online(engine: &CanvasEngine) {
    wait_until("online with a workspace", || {
        let s = engine.snapshot();
        s.is_online && s.workspace_json != "null"
    });
}

/// Wait until the engine has no pending rows and the server count is `n`.
fn wait_flushed(engine: &CanvasEngine, server: &TestServer, n: i64) {
    wait_until(&format!("outbox flushed, server count {n}"), || {
        engine.snapshot().pending_actions == 0 && server.count() == n
    });
}

#[test]
fn online_dispatches_flush_fifo_and_server_count_matches() {
    let server = TestServer::start();
    let dir = temp_dir("fifo");
    let engine = engine(&dir, &server.url(), fast_config());
    wait_online(&engine);

    for intent in [INCREMENT, INCREMENT, DECREMENT, INCREMENT] {
        engine.dispatch(intent);
    }
    assert_eq!(count(&engine.snapshot()), 2, "optimistic value immediately");
    wait_flushed(&engine, &server, 2);
    assert_eq!(
        server.hit_list(),
        vec![
            (INC.into(), 200),
            (INC.into(), 200),
            (DEC.into(), 200),
            (INC.into(), 200)
        ],
        "server saw the POSTs in dispatch order"
    );
    assert!(rows(&dir).is_empty(), "accepted rows are deleted");
    let s = engine.snapshot();
    assert_eq!(count(&s), 2);
    assert!(s.is_online);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn offline_rows_stay_then_flush_fifo_when_server_starts() {
    let addr = free_addr();
    let dir = temp_dir("offline");
    seed_cache(&dir);
    let engine = engine(&dir, &format!("http://{addr}"), fast_config());
    for _ in 0..3 {
        engine.dispatch(INCREMENT);
    }
    engine.dispatch(DECREMENT);
    // Give the probe/worker a few cycles: nothing is sent, nothing is lost.
    thread::sleep(Duration::from_millis(300));
    let s = engine.snapshot();
    assert!(!s.is_online);
    assert_eq!((count(&s), s.pending_actions), (2, 4));
    let queued = rows(&dir);
    assert_eq!(queued.len(), 4);
    assert!(
        queued.iter().all(|r| r.retry_count == 0),
        "no attempts while offline"
    );

    let server = TestServer::start_on(addr);
    wait_flushed(&engine, &server, 2);
    assert_eq!(
        server.hit_list(),
        vec![
            (INC.into(), 200),
            (INC.into(), 200),
            (INC.into(), 200),
            (DEC.into(), 200)
        ]
    );
    assert_eq!(count(&engine.snapshot()), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_decrement_rolls_back() {
    let server = TestServer::start();
    let dir = temp_dir("reject");
    let engine = engine(&dir, &server.url(), fast_config());
    wait_online(&engine);
    let (tx, rx) = mpsc::channel();
    engine.subscribe(move |s| {
        let _ = tx.send(s);
    });

    engine.dispatch(DECREMENT);
    wait_until("rejected row removed", || {
        engine.snapshot().pending_actions == 0 && !server.hits().is_empty()
    });
    assert_eq!(server.hit_list(), vec![(DEC.into(), 422)]);
    assert_eq!(server.count(), 0);
    assert_eq!(count(&engine.snapshot()), 0, "rolled back to prev_value");
    let seen: Vec<i64> = rx.try_iter().map(|s| count(&s)).collect();
    assert!(seen.contains(&-1), "optimistic -1 was shown first: {seen:?}");
    assert_eq!(*seen.last().unwrap(), 0);
    assert!(rows(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_row_reapplies_later_pending_rows() {
    // Queue offline (decrement at 0, then increment), then bring the server
    // up: the decrement is rejected, the count rolls back to 0 and the
    // later increment is re-applied on top (-> 1), which the server accepts.
    let addr = free_addr();
    let dir = temp_dir("reapply");
    seed_cache(&dir);
    let engine = engine(&dir, &format!("http://{addr}"), fast_config());
    engine.dispatch(DECREMENT);
    engine.dispatch(INCREMENT);
    assert_eq!(count(&engine.snapshot()), 0);
    let queued = rows(&dir);
    assert_eq!(queued[1].prev_value_json.as_deref(), Some("-1"));

    let server = TestServer::start_on(addr);
    wait_flushed(&engine, &server, 1);
    assert_eq!(
        server.hit_list(),
        vec![(DEC.into(), 422), (INC.into(), 200)]
    );
    assert_eq!(count(&engine.snapshot()), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn server_errors_retry_with_capped_backoff_then_flush() {
    let server = TestServer::start();
    let dir = temp_dir("retry");
    let config = EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(10),
        backoff_cap: Duration::from_millis(80),
    };
    let cap_ms = 80;
    let engine = engine(&dir, &server.url(), config.clone());
    wait_online(&engine);
    server.fault(503, 4);

    engine.dispatch(INCREMENT);
    // Watch the row while it is retried.
    let mut observed = Vec::new();
    wait_until("row flushed after the faults", || {
        // Sample the clock after the read: next_attempt_at was written
        // before it, so next_attempt_at <= now_ms + cap must hold.
        let first = rows(&dir).into_iter().next();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        match first.as_ref() {
            Some(r) => {
                observed.push((r.retry_count, r.next_attempt_at, now_ms, r.last_error.clone()));
                false
            }
            None => true,
        }
    });
    wait_flushed(&engine, &server, 1);

    let hits = server.hits();
    let statuses: Vec<u16> = hits.iter().map(|h| h.status).collect();
    assert_eq!(statuses, vec![503, 503, 503, 503, 200]);
    assert!(hits.iter().all(|h| h.path == INC));

    // retry_count only grows; last_error names the status; next_attempt_at
    // is never further out than the cap.
    let counts: Vec<i64> = observed.iter().map(|o| o.0).collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
    assert!(counts.iter().any(|&c| c >= 1), "a retry was recorded: {counts:?}");
    for (retry_count, next, now_ms, last_error) in &observed {
        if *retry_count > 0 {
            assert!(last_error.as_deref().unwrap_or("").contains("503"), "{last_error:?}");
            assert!(next - now_ms <= cap_ms, "next_attempt_at beyond the cap: {next} vs {now_ms}");
        }
    }

    // Gaps between attempts follow the backoff: 20, 40, 80 (capped from
    // 80), 80 (capped from 160) ms. Timers only fire late, so each gap is
    // at least its delay (minus ms rounding).
    for (i, pair) in hits.windows(2).enumerate() {
        let gap = pair[1].at.duration_since(pair[0].at);
        let expected = config.backoff(i as i64 + 1);
        assert!(
            gap + Duration::from_millis(5) >= expected,
            "attempt gap {i}: {gap:?} < backoff {expected:?}"
        );
        assert!(gap < expected + Duration::from_secs(2), "gap {i} too long: {gap:?}");
    }
    assert_eq!(count(&engine.snapshot()), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Serves the fixture and /healthz, but drops every POST without answering
/// (a network error for the client).
fn serve_dropping_posts() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim().is_empty() => break,
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
            let _ = reader.read_exact(&mut body);
            if request_line.starts_with("POST ") {
                continue; // dropped: connection closed with no response
            }
            let body = if request_line.starts_with("GET /v1/workspaces/home ") {
                FIXTURE
            } else {
                "ok"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

#[test]
fn network_error_on_flush_marks_offline_and_keeps_row() {
    let base = serve_dropping_posts();
    let dir = temp_dir("neterr");
    // A long probe interval: /healthz succeeds anyway, so going offline
    // can only come from the failed POST.
    let config = EngineConfig {
        poll_interval: Duration::from_secs(30),
        ..fast_config()
    };
    let engine = engine(&dir, &base, config);
    wait_online(&engine);
    engine.dispatch(INCREMENT);
    wait_until("offline after the failed POST", || !engine.snapshot().is_online);
    let s = engine.snapshot();
    assert_eq!((count(&s), s.pending_actions), (1, 1), "optimistic value and row kept");
    let r = &rows(&dir)[0];
    assert_eq!(r.retry_count, 1);
    assert!(r.next_attempt_at > 0);
    assert!(r.last_error.as_deref().is_some_and(|e| !e.is_empty()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_queued_before_restart_flush_on_next_launch() {
    let addr = free_addr();
    let dir = temp_dir("restart");
    seed_cache(&dir);
    {
        let a = engine(&dir, &format!("http://{addr}"), fast_config());
        a.dispatch(INCREMENT);
        a.dispatch(INCREMENT);
        assert_eq!(a.snapshot().pending_actions, 2);
    } // engine A dropped (app closed) with the rows still queued
    assert_eq!(rows(&dir).len(), 2);

    let server = TestServer::start_on(addr);
    let b = engine(&dir, &server.url(), fast_config());
    wait_flushed(&b, &server, 2);
    assert_eq!(server.hit_list(), vec![(INC.into(), 200), (INC.into(), 200)]);
    assert_eq!(count(&b.snapshot()), 2);
    assert!(rows(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refetch_reapplies_pending_rows_over_fetched_entity() {
    // Server count 5; three increments queued offline on a cache that says
    // 0. On reconnect the fetched 5 must show as 8 (never 5), and after the
    // flush the server has 8 too.
    let addr = free_addr();
    let dir = temp_dir("merge");
    seed_cache(&dir);
    let engine = engine(
        &dir,
        &format!("http://{addr}"),
        EngineConfig {
            backoff_base: Duration::from_millis(150),
            ..fast_config()
        },
    );
    for _ in 0..3 {
        engine.dispatch(INCREMENT);
    }
    assert_eq!(count(&engine.snapshot()), 3);
    let (tx, rx) = mpsc::channel();
    engine.subscribe(move |s| {
        let _ = tx.send(s);
    });

    // Hold the first flush back briefly so the merged rows can be checked.
    let server = TestServer::start_on_with(addr, |s| {
        s.set_count(5);
        s.fault(503, 1);
    });
    wait_online(&engine);
    let held = rows(&dir);
    assert_eq!(held.len(), 3);
    let prevs: Vec<&str> = held
        .iter()
        .map(|r| r.prev_value_json.as_deref().unwrap())
        .collect();
    assert_eq!(prevs, vec!["5", "6", "7"], "prev_value_json rebased on the fetch");
    assert_eq!(count(&engine.snapshot()), 8);

    wait_flushed(&engine, &server, 8);
    assert_eq!(count(&engine.snapshot()), 8);
    let online_counts: Vec<i64> = rx
        .try_iter()
        .filter(|s| s.is_online)
        .map(|s| count(&s))
        .collect();
    assert!(!online_counts.is_empty());
    assert!(
        online_counts.iter().all(|&c| c == 8),
        "pending values never clobbered: {online_counts:?}"
    );
    // The cache holds the merged value too.
    let cached = Store::open(&dir.join("cache.db"))
        .unwrap()
        .get_entity(URI)
        .unwrap()
        .unwrap();
    assert_eq!(cached.get_i64("count"), Some(8));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A server whose first `GET /v1/workspaces/home` blocks until `release`
/// is signalled (`fetching` is signalled when it arrives), then serves the
/// fixture. Answers /healthz and records every POST (on arrival, before the
/// 200 reply). One thread per connection.
struct HeldFetchServer {
    base: String,
    fetching: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    posts: Arc<Mutex<Vec<String>>>,
}

fn serve_held_fetch() -> HeldFetchServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (fetching_tx, fetching_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let posts: Arc<Mutex<Vec<String>>> = Arc::default();
    let posts2 = Arc::clone(&posts);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let fetching_tx = fetching_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            let posts = Arc::clone(&posts2);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.trim().is_empty() => break,
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
                let _ = reader.read_exact(&mut body);
                let body = if request_line.starts_with("POST ") {
                    posts.lock().unwrap().push(request_line.trim().to_string());
                    "{}"
                } else if request_line.starts_with("GET /v1/workspaces/home ") {
                    // Only the first fetch is held.
                    let held = release_rx.lock().unwrap().take();
                    if let Some(release) = held {
                        let _ = fetching_tx.send(());
                        let _ = release.recv_timeout(TIMEOUT);
                    }
                    FIXTURE
                } else {
                    "ok"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    HeldFetchServer {
        base: format!("http://{addr}"),
        fetching: fetching_rx,
        release: release_tx,
        posts,
    }
}

#[test]
fn dropping_the_engine_stops_the_worker() {
    // The engine is dropped while the fetch thread is inside its first
    // fetch, holding the last strong reference. That fetch then succeeds
    // (the engine would go online with a pending row), but no POST may start
    // after the drop. Deterministic: the fetch is held open by the server
    // until after the drop, and the check runs once the engine's state has
    // been freed (an observer's token is released), i.e. once no thread can
    // POST any more.
    let HeldFetchServer {
        base,
        fetching,
        release,
        posts,
    } = serve_held_fetch();
    let dir = temp_dir("drop");
    seed_cache(&dir);
    let token = Arc::new(());
    {
        let e = engine(&dir, &base, fast_config());
        fetching.recv_timeout(TIMEOUT).expect("first fetch in flight");
        let held = Arc::clone(&token);
        e.subscribe(move |_| {
            let _ = &held;
        });
        e.dispatch(INCREMENT);
        assert_eq!(e.snapshot().pending_actions, 1);
        assert!(!e.snapshot().is_online);
    } // dropped with the fetch still in flight
    release.send(()).unwrap();
    wait_until("engine state freed", || Arc::strong_count(&token) == 1);
    assert!(posts.lock().unwrap().is_empty(), "POST after drop: {:?}", posts.lock().unwrap());
    assert_eq!(rows(&dir).len(), 1, "the row stays queued for the next launch");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn throttled_and_timed_out_posts_are_retried_not_rolled_back() {
    // 429 and 408 are transient: the row is kept and retried with backoff
    // (no rollback), then accepted.
    let server = TestServer::start();
    let dir = temp_dir("transient4xx");
    let engine = engine(&dir, &server.url(), fast_config());
    wait_online(&engine);
    let (tx, rx) = mpsc::channel();
    engine.subscribe(move |s| {
        let _ = tx.send(s);
    });

    server.fault(429, 2);
    engine.dispatch(INCREMENT);
    wait_flushed(&engine, &server, 1);
    server.fault(408, 1);
    engine.dispatch(INCREMENT);
    wait_flushed(&engine, &server, 2);

    assert_eq!(
        server.hit_list(),
        vec![
            (INC.into(), 429),
            (INC.into(), 429),
            (INC.into(), 200),
            (INC.into(), 408),
            (INC.into(), 200)
        ]
    );
    let seen: Vec<i64> = rx.try_iter().map(|s| count(&s)).collect();
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]),
        "never rolled back: {seen:?}"
    );
    assert_eq!(count(&engine.snapshot()), 2);
    assert!(rows(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_an_engine_mid_flush_never_double_sends_with_its_successor() {
    // Engine A is mid-pass (a slow POST in flight) when it is dropped and
    // engine B opens the same cache.db (Android activity relaunch). A must
    // stop after its in-flight row and B must not resend that row.
    let server = TestServer::start_slow(Duration::from_millis(250));
    let dir = temp_dir("handover");
    let a = engine(&dir, &server.url(), fast_config());
    wait_online(&a);
    for _ in 0..3 {
        a.dispatch(INCREMENT);
    }
    wait_until("first POST in flight", || {
        server.arrivals.load(AtomicOrdering::SeqCst) >= 1
    });
    drop(a);
    let b = engine(&dir, &server.url(), fast_config());
    wait_until("all rows flushed", || {
        rows(&dir).is_empty() && b.snapshot().pending_actions == 0
    });
    // Let any stray duplicate POST land before checking.
    thread::sleep(Duration::from_millis(600));
    assert_eq!(server.count(), 3, "POSTs: {:?}", server.hit_list());
    assert_eq!(server.hit_list(), vec![(INC.into(), 200); 3]);
    assert_eq!(count(&b.snapshot()), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn row_due_beyond_the_backoff_cap_is_sent_anyway() {
    // A next_attempt_at a day ahead (the wall clock moved backwards since it
    // was written) must not stall the FIFO outbox for a day.
    let addr = free_addr();
    let dir = temp_dir("skew");
    seed_cache(&dir);
    {
        let a = engine(&dir, &format!("http://{addr}"), fast_config());
        a.dispatch(INCREMENT);
    }
    let id = rows(&dir)[0].id;
    let day_ahead = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 86_400_000;
    Store::open(&dir.join("cache.db"))
        .unwrap()
        .mark_retry(id, day_ahead, "HTTP 503")
        .unwrap();

    let server = TestServer::start_on(addr);
    let b = engine(&dir, &server.url(), fast_config());
    wait_flushed(&b, &server, 1);
    assert_eq!(server.hit_list(), vec![(INC.into(), 200)]);
    let _ = std::fs::remove_dir_all(&dir);
}
