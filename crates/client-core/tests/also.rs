//! Multi-field optimistic mutations (`OptimisticMutate.also`, docs/DECISIONS.md
//! D15) and bundle `queries` (M6) against a small scriptable HTTP server:
//! it serves a Pixelfed-like bundle and answers POSTs with queued statuses
//! (200 once the queue is empty).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use client_core::store::{OutboxRow, Store, StoreOps};
use client_core::{CanvasEngine, EngineConfig, StateSnapshot};
use protocol::WorkspaceSpec;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);
const POST1: &str = "pixelfed://posts/1";
const POST2: &str = "pixelfed://posts/2";
const LIKE_EP: &str = "/v1/pixelfed/posts/like";

/// The pixelfed_home like intent: toggle `is_liked`, adjust `like_count`.
const LIKE: &str = r#"{"intent":"action.mutation.optimistic","entity_uri":"pixelfed://posts/1","field":"is_liked","value_expr":"!entity.is_liked","rollback_value":false,"endpoint":"/v1/pixelfed/posts/like","payload":{"id":"1","liked":"$value"},"also":[{"field":"like_count","value_expr":"entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"}]}"#;

fn bundle(is_liked: bool, like_count: i64) -> String {
    json!({
        "version": "1",
        "workspace": {"type": "core://Workspace", "id": "feed", "title": "Feed", "slots": []},
        "entities": {
            POST1: {"id": "1", "is_liked": is_liked, "like_count": like_count},
            POST2: {"id": "2", "is_liked": false, "like_count": 0},
        },
        "queries": {"timeline:home": [POST2, POST1]},
    })
    .to_string()
}

fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(20),
        backoff_cap: Duration::from_millis(200),
    }
}

/// Scriptable server: `bundle` for `GET /v1/workspaces/feed`, POST statuses
/// popped from `statuses` (200 when empty); records POST (path, body, status).
#[derive(Clone)]
struct Script {
    bundle: Arc<Mutex<String>>,
    statuses: Arc<Mutex<VecDeque<u16>>>,
    posts: Arc<Mutex<Vec<(String, Value, u16)>>>,
}

impl Script {
    fn new(bundle: String, statuses: &[u16]) -> Self {
        Self {
            bundle: Arc::new(Mutex::new(bundle)),
            statuses: Arc::new(Mutex::new(statuses.iter().copied().collect())),
            posts: Arc::default(),
        }
    }

    fn posts(&self) -> Vec<(String, Value, u16)> {
        self.posts.lock().unwrap().clone()
    }

    /// Serve on `addr` (use [`free_addr`] to start it after the engine).
    fn serve(&self, addr: SocketAddr) -> String {
        let listener = TcpListener::bind(addr).unwrap();
        let addr = listener.local_addr().unwrap();
        let script = self.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let script = script.clone();
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
                    let mut parts = request_line.split_whitespace();
                    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    let (status, resp) = if method == "POST" {
                        let status = script.statuses.lock().unwrap().pop_front().unwrap_or(200);
                        let sent = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        script.posts.lock().unwrap().push((path.to_string(), sent, status));
                        (status, r#"{"ok":true}"#.to_string())
                    } else if path == "/v1/workspaces/feed" {
                        (200, script.bundle.lock().unwrap().clone())
                    } else {
                        (200, "ok".to_string())
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                        resp.len()
                    );
                });
            }
        });
        format!("http://{addr}")
    }
}

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
    std::env::temp_dir().join(format!("client-core-also-{tag}-{}-{nanos}", std::process::id()))
}

/// Store `bundle_json` in `<dir>/cache.db` as workspace "feed", the way a
/// fetch would.
fn seed_cache(dir: &Path, bundle_json: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let spec: WorkspaceSpec = serde_json::from_str(bundle_json).unwrap();
    let mut store = Store::open(&dir.join("cache.db")).unwrap();
    let uris: Vec<String> = spec.entities.keys().cloned().collect();
    store
        .transaction(|tx| {
            tx.save_workspace("feed", &spec.workspace, &uris, &spec.queries, 1)?;
            for (uri, e) in &spec.entities {
                tx.upsert_entity(uri, e, 1)?;
            }
            Ok(())
        })
        .unwrap();
}

fn engine(dir: &Path, base: &str, config: EngineConfig) -> CanvasEngine {
    CanvasEngine::init_with_config(dir.to_str().unwrap(), base, "feed", config)
}

fn rows(dir: &Path) -> Vec<OutboxRow> {
    Store::open(&dir.join("cache.db"))
        .unwrap()
        .pending_outbox()
        .unwrap()
}

/// `(is_liked, like_count)` of post 1.
fn post1(snap: &StateSnapshot) -> (bool, i64) {
    let entities: Value = serde_json::from_str(&snap.entities_json).unwrap();
    let p = &entities[POST1];
    (p["is_liked"].as_bool().unwrap(), p["like_count"].as_i64().unwrap())
}

/// Stored `(prev is_liked, [(also field, prev value)])` of a row.
fn prevs(row: &OutboxRow) -> (Value, Vec<(String, Value)>) {
    let prev = serde_json::from_str(row.prev_value_json.as_deref().unwrap()).unwrap();
    let also = row
        .also
        .iter()
        .map(|a| (a.field.clone(), a.prev_value.clone()))
        .collect();
    (prev, also)
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn like_with_also_updates_both_fields_and_stores_the_outbox_row() {
    let dir = temp_dir("like");
    seed_cache(&dir, &bundle(false, 5));
    let engine = engine(&dir, &format!("http://{}", free_addr()), fast_config());
    assert_eq!(post1(&engine.snapshot()), (false, 5));

    engine.dispatch(LIKE);
    let s = engine.snapshot();
    assert_eq!(post1(&s), (true, 6), "both fields applied at once");
    assert_eq!(s.pending_actions, 1);

    engine.dispatch(LIKE);
    assert_eq!(post1(&engine.snapshot()), (false, 5), "unlike evaluates against pre-mutation entity");

    let queued = rows(&dir);
    assert_eq!(queued.len(), 2);
    let r = &queued[0];
    assert_eq!(r.field.as_deref(), Some("is_liked"));
    assert_eq!(r.endpoint, LIKE_EP);
    assert_eq!(
        serde_json::from_str::<Value>(&r.payload_json).unwrap(),
        json!({"id": "1", "liked": true}),
        "$value is the primary field's new value"
    );
    assert_eq!(prevs(r), (json!(false), vec![("like_count".into(), json!(5))]));
    assert_eq!(
        r.also[0].value_expr,
        "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
    );
    assert_eq!(prevs(&queued[1]), (json!(true), vec![("like_count".into(), json!(6))]));
    assert_eq!(
        serde_json::from_str::<Value>(&queued[1].payload_json).unwrap(),
        json!({"id": "1", "liked": false})
    );

    // Persisted: a restarted engine shows the optimistic values.
    drop(engine);
    let again = CanvasEngine::init_with_config(
        dir.to_str().unwrap(),
        &format!("http://{}", free_addr()),
        "feed",
        fast_config(),
    );
    assert_eq!(post1(&again.snapshot()), (false, 5));
    assert_eq!(again.snapshot().pending_actions, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failing_also_expression_changes_nothing() {
    let dir = temp_dir("badalso");
    seed_cache(&dir, &bundle(false, 5));
    let engine = engine(&dir, &format!("http://{}", free_addr()), fast_config());
    let before = engine.snapshot();
    engine.dispatch(
        r#"{"intent":"action.mutation.optimistic","entity_uri":"pixelfed://posts/1","field":"is_liked","value_expr":"!entity.is_liked","rollback_value":false,"endpoint":"/x","payload":{},"also":[{"field":"like_count","value_expr":"!entity.missing"}]}"#,
    );
    assert_eq!(engine.snapshot(), before, "no change, no snapshot bump");
    assert!(rows(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_like_restores_both_fields() {
    let script = Script::new(bundle(false, 5), &[403]);
    let base = script.serve("127.0.0.1:0".parse().unwrap());
    let dir = temp_dir("reject");
    let engine = engine(&dir, &base, fast_config());
    wait_until("online", || {
        let s = engine.snapshot();
        s.is_online && s.workspace_json != "null"
    });
    engine.dispatch(LIKE);
    assert_eq!(post1(&engine.snapshot()), (true, 6));
    wait_until("rejected row removed", || {
        engine.snapshot().pending_actions == 0 && !script.posts().is_empty()
    });
    assert_eq!(post1(&engine.snapshot()), (false, 5), "both fields rolled back");
    assert_eq!(
        script.posts(),
        vec![(LIKE_EP.to_string(), json!({"id": "1", "liked": true}), 403)]
    );
    assert!(rows(&dir).is_empty());
    let cached = Store::open(&dir.join("cache.db"))
        .unwrap()
        .get_entity(POST1)
        .unwrap()
        .unwrap();
    assert_eq!(
        (cached.get_bool("is_liked"), cached.get_i64("like_count")),
        (Some(false), Some(5))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_row_reapplies_later_rows_with_all_their_fields() {
    // Offline: like (false/5 -> true/6), then a second like toggles back
    // (-> false/5). Online: the first is rejected; the entity is restored to
    // false/5 and the second row is re-evaluated on top (-> true/6), with its
    // stored prevs rebased to false/5. A 503 holds the second row so the
    // rebased prevs can be checked.
    let addr = free_addr();
    let dir = temp_dir("chain");
    seed_cache(&dir, &bundle(false, 5));
    let config = EngineConfig {
        backoff_base: Duration::from_millis(400),
        backoff_cap: Duration::from_millis(2000),
        ..fast_config()
    };
    let engine = engine(&dir, &format!("http://{addr}"), config);
    engine.dispatch(LIKE);
    engine.dispatch(LIKE);
    assert_eq!(post1(&engine.snapshot()), (false, 5));
    assert_eq!(prevs(&rows(&dir)[1]), (json!(true), vec![("like_count".into(), json!(6))]));

    let script = Script::new(bundle(false, 5), &[422, 503]);
    script.serve(addr);
    wait_until("second row retried once", || {
        rows(&dir).first().is_some_and(|r| r.retry_count >= 1)
    });
    let held = rows(&dir);
    assert_eq!(held.len(), 1);
    assert_eq!(prevs(&held[0]), (json!(false), vec![("like_count".into(), json!(5))]));
    assert_eq!(post1(&engine.snapshot()), (true, 6));

    wait_until("flushed", || engine.snapshot().pending_actions == 0);
    let statuses: Vec<u16> = script.posts().iter().map(|p| p.2).collect();
    assert_eq!(statuses, vec![422, 503, 200]);
    assert_eq!(post1(&engine.snapshot()), (true, 6));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refetch_reapplies_all_fields_of_pending_rows() {
    // Offline like on a cached false/5. The server says false/10 (others
    // liked meanwhile): after the fetch the post must show true/11, with the
    // row's prevs rebased to false/10, and never the bare fetched values.
    let addr = free_addr();
    let dir = temp_dir("merge");
    seed_cache(&dir, &bundle(false, 5));
    let config = EngineConfig {
        backoff_base: Duration::from_millis(300),
        backoff_cap: Duration::from_millis(2000),
        ..fast_config()
    };
    let engine = engine(&dir, &format!("http://{addr}"), config);
    engine.dispatch(LIKE);
    assert_eq!(post1(&engine.snapshot()), (true, 6));
    let seen: Arc<Mutex<Vec<StateSnapshot>>> = Arc::default();
    let log = Arc::clone(&seen);
    engine.subscribe(move |s| log.lock().unwrap().push(s));

    let script = Script::new(bundle(false, 10), &[503]);
    script.serve(addr);
    wait_until("online", || engine.snapshot().is_online);
    wait_until("fetched", || {
        rows(&dir).first().is_some_and(|r| prevs(r).1 == vec![("like_count".into(), json!(10))])
    });
    assert_eq!(post1(&engine.snapshot()), (true, 11));
    assert_eq!(prevs(&rows(&dir)[0]).0, json!(false));

    wait_until("flushed", || engine.snapshot().pending_actions == 0);
    assert_eq!(post1(&engine.snapshot()), (true, 11));
    let online: Vec<(bool, i64)> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.is_online)
        .map(post1)
        .collect();
    assert!(online.iter().all(|&v| v == (true, 11)), "never clobbered: {online:?}");
    let cached = Store::open(&dir.join("cache.db"))
        .unwrap()
        .get_entity(POST1)
        .unwrap()
        .unwrap();
    assert_eq!(cached.get_i64("like_count"), Some(11));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn queries_are_persisted_and_exposed_in_the_snapshot() {
    let script = Script::new(bundle(false, 5), &[]);
    let base = script.serve("127.0.0.1:0".parse().unwrap());
    let dir = temp_dir("queries");
    let engine = engine(&dir, &base, fast_config());
    assert_eq!(engine.snapshot().queries_json, "{}");
    assert_eq!(engine.snapshot().auth_json, r#"{"state":"signed_out"}"#);
    wait_until("fetched", || engine.snapshot().workspace_json != "null");
    let expected = json!({"timeline:home": [POST2, POST1]});
    let s = engine.snapshot();
    assert_eq!(serde_json::from_str::<Value>(&s.queries_json).unwrap(), expected);
    drop(engine);

    // Restart offline: queries and every entity they reference come from
    // the cache.
    let offline = CanvasEngine::init_with_config(
        dir.to_str().unwrap(),
        &format!("http://{}", free_addr()),
        "feed",
        fast_config(),
    );
    let s = offline.snapshot();
    assert!(!s.is_online);
    assert_eq!(serde_json::from_str::<Value>(&s.queries_json).unwrap(), expected);
    let entities: Value = serde_json::from_str(&s.entities_json).unwrap();
    assert!(entities.get(POST1).is_some() && entities.get(POST2).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_uris_outside_the_entity_list_are_loaded_from_the_cache() {
    // A cached workspace whose queries name an entity that is in the cache
    // but not in its entity URI list: the snapshot still includes it.
    let dir = temp_dir("queryuris");
    std::fs::create_dir_all(&dir).unwrap();
    let spec: WorkspaceSpec = serde_json::from_str(&bundle(false, 5)).unwrap();
    let mut store = Store::open(&dir.join("cache.db")).unwrap();
    store
        .transaction(|tx| {
            tx.save_workspace("feed", &spec.workspace, &[POST1.to_string()], &spec.queries, 1)?;
            for (uri, e) in &spec.entities {
                tx.upsert_entity(uri, e, 1)?;
            }
            Ok(())
        })
        .unwrap();
    drop(store);
    let engine = engine(&dir, &format!("http://{}", free_addr()), fast_config());
    let entities: Value = serde_json::from_str(&engine.snapshot().entities_json).unwrap();
    assert!(entities.get(POST2).is_some(), "{entities}");
    let _ = std::fs::remove_dir_all(&dir);
}
