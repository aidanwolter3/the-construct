//! End-to-end tests for `ConstructEngine` against a tiny hand-rolled HTTP server.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use client_core::store::{Store, StoreOps};
use client_core::{ConstructEngine, StateSnapshot};
use protocol::WorkspaceSpec;
use serde_json::Value;

const FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);

const INCREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"#;
const RESET: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"0","rollback_value":0,"endpoint":"/v1/counters/home/reset","payload":{}}"#;

/// Serve the counter fixture at `/v1/workspaces/home` (404 elsewhere).
/// POSTs (outbox flushes) get 503, so queued rows stay PENDING in these
/// tests; tests/outbox.rs covers flushing against the real server.
/// Returns the base URL.
fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            handle(stream);
        }
    });
    format!("http://{addr}")
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_nonblocking(false);
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers, then the body (so closing never resets the connection).
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
    let mut body_buf = vec![0u8; content_length];
    let _ = std::io::Read::read_exact(&mut reader, &mut body_buf);
    let (status, body) = if request_line.starts_with("POST ") {
        (
            "503 Service Unavailable",
            "fixture server does not accept mutations",
        )
    } else if request_line.starts_with("GET /v1/workspaces/home ") {
        ("200 OK", FIXTURE)
    } else if request_line.starts_with("GET /healthz ") {
        ("200 OK", "ok")
    } else {
        ("404 Not Found", "not found")
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

/// A fixture server that can be stopped (port closed) and restarted on the
/// same address.
struct StoppableServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StoppableServer {
    fn start(addr: &str) -> Self {
        let listener = TcpListener::bind(addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle(stream),
                    Err(_) => thread::sleep(Duration::from_millis(20)),
                }
            }
            // Listener dropped here: the port is closed.
        });
        Self {
            addr,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn stop(mut self) -> SocketAddr {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
        self.addr
    }
}

/// Wait until a snapshot with the given online state arrives.
fn wait_for_online_state(rx: &mpsc::Receiver<StateSnapshot>, online: bool) -> StateSnapshot {
    loop {
        let s = rx
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("no snapshot with is_online={online}"));
        if s.is_online == online {
            return s;
        }
    }
}

/// Regression: after a successful fetch, killing the server must flip the
/// engine offline, and bringing it back must flip it online without
/// discarding local optimistic changes.
#[test]
fn server_going_away_mid_session_reports_offline_then_online() {
    let server = StoppableServer::start("127.0.0.1:0");
    let engine = ConstructEngine::init("", &server.url());
    let rx = subscribe_channel(&engine);
    wait_online(&rx);

    engine.dispatch(INCREMENT);
    assert_eq!(count(&wait_for_online_state(&rx, true)), 1);

    let addr = server.stop();
    let offline = wait_for_online_state(&rx, false);
    assert_ne!(offline.workspace_json, "null", "layout kept while offline");
    assert_eq!(count(&offline), 1, "local state kept while offline");

    let server = StoppableServer::start(&addr.to_string());
    let back = wait_for_online_state(&rx, true);
    assert_eq!(count(&back), 1, "reconnect must not discard local changes");
    server.stop();
}

/// A brief outage (one missed probe) must not flip the engine offline:
/// the offline badge would flash for a network blip.
#[test]
fn brief_outage_does_not_report_offline() {
    let server = StoppableServer::start("127.0.0.1:0");
    let engine = ConstructEngine::init("", &server.url());
    let rx = subscribe_channel(&engine);
    wait_online(&rx);

    // Down for about one probe interval (2 s), then back.
    let addr = server.stop();
    thread::sleep(Duration::from_millis(2500));
    let server = StoppableServer::start(&addr.to_string());
    thread::sleep(Duration::from_secs(3));

    assert!(
        rx.try_iter().all(|s| s.is_online),
        "a single missed probe reported offline"
    );
    assert!(engine.snapshot().is_online);
    server.stop();
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "client-core-test-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

/// Write the counter fixture into `<dir>/cache.db` as workspace `id`, the
/// way a successful fetch would.
fn seed_cache(dir: &std::path::Path, id: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let spec: WorkspaceSpec = serde_json::from_str(FIXTURE).unwrap();
    let mut store = Store::open(&dir.join("cache.db")).unwrap();
    let uris: Vec<String> = spec.entities.keys().cloned().collect();
    store
        .transaction(|tx| {
            tx.save_workspace(id, &spec.workspace, &uris, &spec.queries, 1)?;
            for (uri, e) in &spec.entities {
                tx.upsert_entity(uri, e, 1)?;
            }
            Ok(())
        })
        .unwrap();
}

fn count(snap: &StateSnapshot) -> i64 {
    let entities: Value = serde_json::from_str(&snap.entities_json).unwrap();
    entities["core://counters/home"]["count"].as_i64().unwrap()
}

fn subscribe_channel(engine: &ConstructEngine) -> mpsc::Receiver<StateSnapshot> {
    let (tx, rx) = mpsc::channel();
    engine.subscribe(move |s| {
        let _ = tx.send(s);
    });
    rx
}

/// Wait for the first online snapshot that has a workspace.
fn wait_online(rx: &mpsc::Receiver<StateSnapshot>) -> StateSnapshot {
    loop {
        let s = rx.recv_timeout(TIMEOUT).expect("no online snapshot");
        if s.is_online && s.workspace_json != "null" {
            return s;
        }
    }
}

#[test]
fn fetch_increment_reset() {
    let base = serve_fixture();
    let dir = temp_dir("counter");
    let engine = ConstructEngine::init(dir.to_str().unwrap(), &base);
    let rx = subscribe_channel(&engine);

    let first = wait_online(&rx);
    let ws: Value = serde_json::from_str(&first.workspace_json).unwrap();
    assert_eq!(ws["type"], "core://Workspace");
    assert_eq!(ws["id"], "home");
    assert_eq!(count(&first), 0);

    engine.dispatch(INCREMENT);
    assert_eq!(count(&rx.recv_timeout(TIMEOUT).unwrap()), 1);
    engine.dispatch(INCREMENT);
    let s = rx.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(count(&s), 2);
    assert!(s.is_online);
    assert_eq!(count(&engine.snapshot()), 2);

    engine.dispatch(RESET);
    assert_eq!(count(&rx.recv_timeout(TIMEOUT).unwrap()), 0);

    // The SQLite cache was written: layout, entity URI list, entity, and
    // one outbox row per mutation.
    let store = Store::open(&dir.join("cache.db")).unwrap();
    let cached = store
        .load_workspace("home")
        .unwrap()
        .expect("workspace cached");
    assert_eq!(
        serde_json::to_string(&cached.layout).unwrap(),
        first.workspace_json
    );
    assert_eq!(cached.entity_uris, vec!["core://counters/home".to_string()]);
    assert_eq!(
        store
            .get_entity("core://counters/home")
            .unwrap()
            .unwrap()
            .get_i64("count"),
        Some(0)
    );
    assert_eq!(store.pending_count().unwrap(), 3);
    assert!(
        !dir.join("workspace_home.json").exists(),
        "no JSON cache written"
    );

    // A second engine on the same storage dir with the server unreachable
    // starts from the cached workspace, offline.
    let offline = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let snap = offline.snapshot();
    assert_eq!(snap.workspace_json, first.workspace_json);
    assert_eq!(count(&snap), 0);
    assert_eq!(snap.pending_actions, 3);
    assert!(!snap.is_online);
    offline.refresh();
    let snap = offline.snapshot();
    assert_eq!(snap.workspace_json, first.workspace_json);
    assert!(!snap.is_online);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_and_unsupported_intents_change_nothing() {
    let base = serve_fixture();
    let engine = ConstructEngine::init("", &base);
    let rx = subscribe_channel(&engine);
    wait_online(&rx);

    engine.dispatch("not json");
    engine.dispatch(r#"{"intent":"action.unknown"}"#);
    engine.dispatch(r#"{"intent":"action.navigation.back"}"#);
    engine.dispatch(&INCREMENT.replace("entity.count + 1", "entity.count * 2"));
    engine.dispatch("");
    engine.dispatch("{}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    assert_eq!(count(&engine.snapshot()), 0);
}

#[test]
fn mutate_creates_missing_entity() {
    let engine = ConstructEngine::init("", "http://127.0.0.1:1");
    engine.dispatch(&INCREMENT.replace("core://counters/home", "core://counters/new"));
    let entities: Value = serde_json::from_str(&engine.snapshot().entities_json).unwrap();
    assert_eq!(entities["core://counters/new"]["count"], 1);
}

#[test]
fn snapshot_before_fetch_is_empty() {
    // Port 1 refuses fast, but the snapshot is taken synchronously before
    // the background fetch can finish or not; either way no workspace.
    let engine = ConstructEngine::init("", "http://127.0.0.1:1");
    let s = engine.snapshot();
    assert_eq!(s.workspace_json, "null");
    assert_eq!(s.entities_json, "{}");
    assert!(!s.is_online);
}

#[test]
fn engine_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ConstructEngine>();
}

#[test]
fn unreachable_server_reports_offline() {
    let engine = ConstructEngine::init("", "http://127.0.0.1:1");
    let rx = subscribe_channel(&engine);
    // Immediate snapshot on subscribe, then (possibly the same) after the
    // failed fetch; all are offline and empty.
    let s = rx.recv_timeout(TIMEOUT).expect("no offline snapshot");
    assert!(!s.is_online);
    assert_eq!(s.workspace_json, "null");
    assert_eq!(s.entities_json, "{}");
    engine.refresh(); // synchronous, must not panic
    let s = engine.snapshot();
    assert!(!s.is_online);
    assert!(s.online_known, "a failed fetch is an answer: offline");
    engine.dispatch(INCREMENT); // must not panic
}

#[test]
fn connectivity_is_unknown_until_the_first_fetch() {
    // A cached workspace is shown at once; it says nothing about the server
    // yet, so UIs must not flash "offline" (e.g. when switching apps).
    let dir = temp_dir("unknown");
    seed_cache(&dir, "home");
    let url = serve_fixture();
    let engine = ConstructEngine::init(dir.to_str().unwrap(), &url);
    let rx = subscribe_channel(&engine);
    let first = rx.recv_timeout(TIMEOUT).expect("initial snapshot");
    if first.seq == 1 {
        // Only the cache load so far (the fetch may already have won the race).
        assert_ne!(first.workspace_json, "null");
        assert!(!first.online_known);
    }
    let online = wait_online(&rx);
    assert!(online.online_known);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn offline_falls_back_to_cache() {
    let dir = temp_dir("cache");
    seed_cache(&dir, "home");

    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    // Cached state is available synchronously.
    let snap = engine.snapshot();
    assert_ne!(snap.workspace_json, "null");
    assert_eq!(count(&snap), 0);
    assert_eq!(snap.seq, 1);
    assert!(!snap.is_online);
    assert!(
        !snap.online_known,
        "the cache says nothing about the server"
    );

    engine.dispatch(INCREMENT);
    assert_eq!(count(&engine.snapshot()), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn observer_may_dispatch_reentrantly() {
    use std::sync::{Arc, Mutex};
    let engine = Arc::new(ConstructEngine::init("", "http://127.0.0.1:1"));
    // Weak ref avoids an Arc cycle (engine -> observer -> engine).
    let weak = Arc::downgrade(&engine);
    let fired = Arc::new(Mutex::new(false));
    let fired2 = Arc::clone(&fired);
    let (tx, rx) = mpsc::channel();
    engine.subscribe(move |s| {
        let entities: Value = serde_json::from_str(&s.entities_json).unwrap();
        let c = entities["core://counters/home"]["count"].as_i64();
        if c == Some(1) {
            let mut f = fired2.lock().unwrap();
            if !*f {
                *f = true;
                drop(f);
                if let Some(e) = weak.upgrade() {
                    e.dispatch(INCREMENT); // must not deadlock
                }
            }
        }
        let _ = tx.send(c);
    });
    engine.dispatch(INCREMENT);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let c = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if c == Some(2) {
            break;
        }
    }
    assert_eq!(count(&engine.snapshot()), 2);
}

#[test]
fn panicking_observer_does_not_propagate() {
    let engine = ConstructEngine::init("", "http://127.0.0.1:1");
    engine.subscribe(|_| panic!("observer boom"));
    let rx = subscribe_channel(&engine);
    engine.dispatch(INCREMENT);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let s = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if s.entities_json != "{}" {
            assert_eq!(count(&s), 1);
            break;
        }
    }
}

#[test]
fn cache_is_keyed_by_workspace_id() {
    let dir = temp_dir("keyed");
    seed_cache(&dir, "home");

    // A different workspace must not pick up the "home" layout/entities.
    let other = ConstructEngine::init_with_workspace(
        dir.to_str().unwrap(),
        "http://127.0.0.1:1",
        "pixelfed_demo",
    );
    let snap = other.snapshot();
    assert_eq!(snap.workspace_json, "null");
    assert_eq!(snap.entities_json, "{}");

    // Ids are stored verbatim (no file-name sanitizing any more).
    seed_cache(&dir, "a/b..c");
    let odd =
        ConstructEngine::init_with_workspace(dir.to_str().unwrap(), "http://127.0.0.1:1", "a/b..c");
    assert_ne!(odd.snapshot().workspace_json, "null");
    let near =
        ConstructEngine::init_with_workspace(dir.to_str().unwrap(), "http://127.0.0.1:1", "a_b__c");
    assert_eq!(near.snapshot().workspace_json, "null");

    // "home" still finds its own cache.
    let home =
        ConstructEngine::init_with_workspace(dir.to_str().unwrap(), "http://127.0.0.1:1", "home");
    assert_ne!(home.snapshot().workspace_json, "null");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remembered_form_values_survive_a_restart_per_app() {
    let dir = temp_dir("form-values");
    let storage = dir.to_str().unwrap();
    let base = "http://127.0.0.1:1";
    {
        let home = ConstructEngine::init_with_workspace(storage, base, "home");
        assert!(home.remembered_form_values("").is_empty());
        home.remember_form_value("", "view", Some("cards"));
        home.remember_form_value("post", "view", Some("grid"));
        // Empty, oversized and nameless values are not stored.
        home.remember_form_value("", "sort", Some(""));
        home.remember_form_value("", "big", Some(&"x".repeat(2000)));
        home.remember_form_value("", "", Some("x"));
    }
    let home = ConstructEngine::init_with_workspace(storage, base, "home");
    let root = home.remembered_form_values("");
    assert_eq!(root.len(), 1);
    assert_eq!(root["view"], "cards");
    assert_eq!(home.remembered_form_values("post")["view"], "grid");
    // Another app on the same storage has its own values.
    let other = ConstructEngine::init_with_workspace(storage, base, "recipes");
    assert!(other.remembered_form_values("").is_empty());
    // None forgets.
    home.remember_form_value("", "view", None);
    assert!(home.remembered_form_values("").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_404_keeps_engine_online() {
    let base = serve_fixture();
    let engine = ConstructEngine::init_with_workspace("", &base, "missing");
    let rx = subscribe_channel(&engine);
    let deadline = std::time::Instant::now() + TIMEOUT;
    let s = loop {
        let s = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("no online snapshot after 404");
        if s.is_online {
            break s;
        }
    };
    assert_eq!(s.workspace_json, "null");
    assert_eq!(s.entities_json, "{}");
    engine.refresh();
    assert!(engine.snapshot().is_online);
}

fn outbox_rows(dir: &std::path::Path) -> Vec<client_core::store::OutboxRow> {
    Store::open(&dir.join("cache.db"))
        .unwrap()
        .pending_outbox()
        .unwrap()
}

#[test]
fn json_cache_files_are_ignored() {
    let dir = temp_dir("jsonignored");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("workspace_home.json"), FIXTURE).unwrap();
    std::fs::write(dir.join("last_workspace.json"), FIXTURE).unwrap();

    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let snap = engine.snapshot();
    assert_eq!(
        snap.workspace_json, "null",
        "old JSON cache must not be read"
    );
    assert_eq!(snap.entities_json, "{}");
    assert!(dir.join("workspace_home.json").exists(), "not deleted");
    assert!(dir.join("last_workspace.json").exists(), "not deleted");
    assert!(dir.join("cache.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_persists_entities_workspace_and_outbox() {
    let base = serve_fixture();
    let dir = temp_dir("restart");
    let first_ws;
    {
        let engine = ConstructEngine::init(dir.to_str().unwrap(), &base);
        let rx = subscribe_channel(&engine);
        first_ws = wait_online(&rx).workspace_json;
        engine.dispatch(INCREMENT);
        engine.dispatch(INCREMENT);
        let s = engine.snapshot();
        assert_eq!(count(&s), 2);
        assert_eq!(s.pending_actions, 2);
    }

    let rows = outbox_rows(&dir);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].id < rows[1].id);
    let r = &rows[0];
    assert_eq!(r.entity_uri, "core://counters/home");
    assert_eq!(r.endpoint, "/v1/counters/home/increment");
    assert_eq!(r.payload_json, "{}");
    assert_eq!(r.field.as_deref(), Some("count"));
    assert_eq!(r.value_expr.as_deref(), Some("entity.count + 1"));
    assert_eq!(r.prev_value_json.as_deref(), Some("0"));
    assert_eq!(rows[1].prev_value_json.as_deref(), Some("1"));
    assert_eq!(r.rollback_value_json.as_deref(), Some("0"));
    assert_eq!(r.status, "PENDING");
    // The sync worker may already have tried once (the fixture answers 503).
    assert!(r.retry_count <= 1, "retry_count {}", r.retry_count);
    assert_eq!(r.next_attempt_at == 0, r.retry_count == 0);
    assert_eq!(
        rows[1].retry_count, 0,
        "FIFO: the second row is never tried first"
    );
    assert!(r.created_at > 1_600_000_000, "created_at is unix seconds");

    // "Restart": a fresh engine on the same dir, server unreachable.
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let snap = engine.snapshot();
    assert_eq!(snap.workspace_json, first_ws);
    assert_eq!(count(&snap), 2);
    assert_eq!(snap.pending_actions, 2);
    engine.dispatch(INCREMENT);
    assert_eq!(count(&engine.snapshot()), 3);
    assert_eq!(engine.snapshot().pending_actions, 3);
    assert_eq!(outbox_rows(&dir).len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pending_entity_outside_workspace_survives_restart() {
    let dir = temp_dir("pendingextra");
    seed_cache(&dir, "home");
    let new_uri = "core://counters/new";
    {
        let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
        engine.dispatch(&INCREMENT.replace("core://counters/home", new_uri));
    }
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let entities: Value = serde_json::from_str(&engine.snapshot().entities_json).unwrap();
    assert_eq!(entities[new_uri]["count"], 1);
    assert_eq!(entities["core://counters/home"]["count"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// If the outbox insert fails, the entity update in the same transaction
/// must roll back and the in-memory state must not change.
#[test]
fn failed_mutate_transaction_changes_nothing() {
    let dir = temp_dir("atomic");
    seed_cache(&dir, "home");
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let rx = subscribe_channel(&engine);
    let before = engine.snapshot();
    assert_eq!(count(&before), 0);

    // Break the outbox from a second connection.
    let raw = rusqlite::Connection::open(dir.join("cache.db")).unwrap();
    raw.execute_batch("ALTER TABLE action_outbox RENAME TO action_outbox_broken")
        .unwrap();

    engine.dispatch(INCREMENT);
    let after = engine.snapshot();
    assert_eq!(
        after.entities_json, before.entities_json,
        "memory unchanged"
    );
    assert_eq!(after.pending_actions, 0);
    let stored: String = raw
        .query_row(
            "SELECT payload_json FROM entities WHERE uri = 'core://counters/home'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["count"], 0, "entity update rolled back");
    let rows: i64 = raw
        .query_row("SELECT COUNT(*) FROM action_outbox_broken", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "no outbox row");
    while let Ok(s) = rx.try_recv() {
        assert_eq!(count(&s), 0, "no snapshot with the failed mutation");
    }

    // Repair: the engine recovers and the next mutate goes through.
    raw.execute_batch("ALTER TABLE action_outbox_broken RENAME TO action_outbox")
        .unwrap();
    engine.dispatch(INCREMENT);
    let s = engine.snapshot();
    assert_eq!(count(&s), 1);
    assert_eq!(s.pending_actions, 1);
    assert_eq!(outbox_rows(&dir).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cache.db that can be read but not written: the engine falls back to an
/// in-memory copy (cached workspace still shown) and clicks keep working
/// for the session; the file is never modified.
#[cfg(unix)]
#[test]
fn read_only_cache_falls_back_to_an_in_memory_copy() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("readonly");
    seed_cache(&dir, "home");
    let db = dir.join("cache.db");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Root (e.g. cloud sessions, containers) ignores file permissions, so
    // the file is still writable and there is no read-only case to test.
    // CI runs as a normal user and covers it.
    if std::fs::OpenOptions::new().write(true).open(&db).is_ok() {
        eprintln!("skipping: a 0444 file is still writable (running as root?)");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let s = engine.snapshot();
    assert_ne!(
        s.workspace_json, "null",
        "cached workspace copied into memory"
    );
    assert_eq!(count(&s), 0);
    engine.dispatch(INCREMENT);
    engine.dispatch(INCREMENT);
    let s = engine.snapshot();
    assert_eq!(
        (count(&s), s.pending_actions),
        (2, 2),
        "mutations are not no-ops"
    );

    let raw =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let rows: i64 = raw
        .query_row("SELECT COUNT(*) FROM action_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "the read-only file is untouched");
    drop(raw);
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Another connection holds the write lock on cache.db: dispatch (UI
/// thread) gives up after the short busy timeout instead of freezing, and
/// the failed mutate changes nothing.
#[test]
fn locked_cache_dispatch_returns_quickly_and_changes_nothing() {
    use std::time::Instant;

    let dir = temp_dir("locked");
    seed_cache(&dir, "home");
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let rx = subscribe_channel(&engine);
    let before = engine.snapshot();

    let raw = rusqlite::Connection::open(dir.join("cache.db")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    engine.dispatch(INCREMENT);
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(1),
        "dispatch blocked for {took:?}"
    );
    assert!(
        took >= client_core::store::BUSY_TIMEOUT / 2,
        "the write did wait for the lock: {took:?}"
    );
    let after = engine.snapshot();
    assert_eq!(after.entities_json, before.entities_json, "state unchanged");
    assert_eq!(after.pending_actions, 0);
    while let Ok(s) = rx.try_recv() {
        assert_eq!(count(&s), 0, "no snapshot with the failed mutation");
    }

    // Lock released: the next click goes through.
    raw.execute_batch("ROLLBACK").unwrap();
    engine.dispatch(INCREMENT);
    let s = engine.snapshot();
    assert_eq!((count(&s), s.pending_actions), (1, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn value_token_is_substituted_in_outbox_payload() {
    let dir = temp_dir("valuetoken");
    seed_cache(&dir, "home");
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let intent = INCREMENT.replace(
        r#""payload":{}"#,
        r#""payload":{"count":"$value","nested":["$value","$values"]}"#,
    );
    engine.dispatch(&intent);
    engine.dispatch(&intent);
    let rows = outbox_rows(&dir);
    assert_eq!(rows.len(), 2);
    let p0: Value = serde_json::from_str(&rows[0].payload_json).unwrap();
    let p1: Value = serde_json::from_str(&rows[1].payload_json).unwrap();
    assert_eq!(
        p0,
        serde_json::json!({"count": 1, "nested": [1, "$values"]})
    );
    assert_eq!(
        p1,
        serde_json::json!({"count": 2, "nested": [2, "$values"]})
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_reports_seq_and_pending_actions() {
    let engine = ConstructEngine::init("", "http://127.0.0.1:1");
    let rx = subscribe_channel(&engine);
    for expected in 1..=3u32 {
        engine.dispatch(INCREMENT);
        assert_eq!(engine.snapshot().pending_actions, expected);
    }
    engine.dispatch("not json"); // no change, no seq bump
    engine.refresh(); // failed fetch: still bumps seq

    let mut seqs = Vec::new();
    let mut pendings = Vec::new();
    while let Ok(s) = rx.recv_timeout(Duration::from_millis(200)) {
        seqs.push(s.seq);
        pendings.push(s.pending_actions);
    }
    assert!(seqs.len() >= 5, "initial + 3 mutations + refresh: {seqs:?}");
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq strictly increases: {seqs:?}"
    );
    assert!(pendings.contains(&1) && pendings.contains(&2));
    assert_eq!(*pendings.last().unwrap(), 3);
    assert_eq!(engine.snapshot().seq, *seqs.last().unwrap());
}

/// DESIGN §2.3 target: an optimistic mutate reaches observers in < 5 ms.
/// Asserted with a generous bound to avoid flakiness on loaded machines.
#[test]
fn mutate_dispatch_to_notify_latency() {
    use std::sync::Mutex;
    use std::time::Instant;

    let dir = temp_dir("latency");
    seed_cache(&dir, "home");
    let engine = ConstructEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let seen: Arc<Mutex<Vec<(i64, Instant)>>> = Arc::default();
    let seen2 = Arc::clone(&seen);
    engine.subscribe(move |s| {
        let at = Instant::now();
        seen2.lock().unwrap().push((count(&s), at));
    });

    let mut samples = Vec::new();
    for i in 1..=50i64 {
        let t0 = Instant::now();
        engine.dispatch(INCREMENT);
        let at = seen
            .lock()
            .unwrap()
            .iter()
            .find(|(c, _)| *c == i)
            .map(|(_, at)| *at)
            .expect("observer notified synchronously");
        samples.push(at.duration_since(t0));
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    println!(
        "mutate dispatch->notify over {} file-backed mutations: median {} us, max {} us",
        samples.len(),
        median.as_micros(),
        samples.last().unwrap().as_micros()
    );
    assert!(median < Duration::from_millis(20), "median {median:?}");
    assert_eq!(engine.snapshot().pending_actions, 50);
    let _ = std::fs::remove_dir_all(&dir);
}
