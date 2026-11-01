//! End-to-end tests for `CanvasEngine` against a tiny hand-rolled HTTP server.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use client_core::{CanvasEngine, StateSnapshot};
use serde_json::Value;

const FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");
const TIMEOUT: Duration = Duration::from_secs(10);

const INCREMENT: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"#;
const RESET: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"0","rollback_value":0,"endpoint":"/v1/counters/home/reset","payload":{}}"#;

/// Serve the counter fixture at `/v1/workspaces/home` (404 elsewhere).
/// Returns the base URL.
fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // Drain headers.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                }
            }
            let (status, body) = if request_line.starts_with("GET /v1/workspaces/home ") {
                ("200 OK", FIXTURE)
            } else {
                ("404 Not Found", "not found")
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
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

fn count(snap: &StateSnapshot) -> i64 {
    let entities: Value = serde_json::from_str(&snap.entities_json).unwrap();
    entities["core://counters/home"]["count"].as_i64().unwrap()
}

fn subscribe_channel(engine: &CanvasEngine) -> mpsc::Receiver<StateSnapshot> {
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
    let engine = CanvasEngine::init(dir.to_str().unwrap(), &base);
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

    // Cache was written.
    let cached = std::fs::read_to_string(dir.join("last_workspace.json")).unwrap();
    assert_eq!(cached, FIXTURE);

    // A second engine on the same storage dir with the server unreachable
    // starts from the cached workspace, offline.
    let offline = CanvasEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    let snap = offline.snapshot();
    assert_eq!(snap.workspace_json, first.workspace_json);
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
    let engine = CanvasEngine::init("", &base);
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
    let engine = CanvasEngine::init("", "http://127.0.0.1:1");
    engine.dispatch(&INCREMENT.replace("core://counters/home", "core://counters/new"));
    let entities: Value = serde_json::from_str(&engine.snapshot().entities_json).unwrap();
    assert_eq!(entities["core://counters/new"]["count"], 1);
}

#[test]
fn snapshot_before_fetch_is_empty() {
    // Port 1 refuses fast, but the snapshot is taken synchronously before
    // the background fetch can finish or not; either way no workspace.
    let engine = CanvasEngine::init("", "http://127.0.0.1:1");
    let s = engine.snapshot();
    assert_eq!(s.workspace_json, "null");
    assert_eq!(s.entities_json, "{}");
    assert!(!s.is_online);
}

#[test]
fn engine_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CanvasEngine>();
}

#[test]
fn unreachable_server_reports_offline() {
    let engine = CanvasEngine::init("", "http://127.0.0.1:1");
    let rx = subscribe_channel(&engine);
    // Immediate snapshot on subscribe, then (possibly the same) after the
    // failed fetch; all are offline and empty.
    let s = rx.recv_timeout(TIMEOUT).expect("no offline snapshot");
    assert!(!s.is_online);
    assert_eq!(s.workspace_json, "null");
    assert_eq!(s.entities_json, "{}");
    engine.refresh(); // synchronous, must not panic
    assert!(!engine.snapshot().is_online);
    engine.dispatch(INCREMENT); // must not panic
}

#[test]
fn offline_falls_back_to_cache() {
    let dir = temp_dir("cache");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("last_workspace.json"), FIXTURE).unwrap();

    let engine = CanvasEngine::init(dir.to_str().unwrap(), "http://127.0.0.1:1");
    // Cached state is available synchronously.
    let snap = engine.snapshot();
    assert_ne!(snap.workspace_json, "null");
    assert!(!snap.is_online);

    engine.dispatch(INCREMENT);
    assert_eq!(count(&engine.snapshot()), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn observer_may_dispatch_reentrantly() {
    use std::sync::{Arc, Mutex};
    let engine = Arc::new(CanvasEngine::init("", "http://127.0.0.1:1"));
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
        let c = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).unwrap();
        if c == Some(2) {
            break;
        }
    }
    assert_eq!(count(&engine.snapshot()), 2);
}

#[test]
fn panicking_observer_does_not_propagate() {
    let engine = CanvasEngine::init("", "http://127.0.0.1:1");
    engine.subscribe(|_| panic!("observer boom"));
    let rx = subscribe_channel(&engine);
    engine.dispatch(INCREMENT);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let s = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).unwrap();
        if s.entities_json != "{}" {
            assert_eq!(count(&s), 1);
            break;
        }
    }
}
