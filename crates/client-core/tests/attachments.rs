//! Attachments (docs/DECISIONS.md D49): staging a photo, a create that carries
//! it, the outbox uploading it before the POST (once, even across retries),
//! and its files being cleaned up. Against a small mock pod.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use client_core::{ConstructEngine, EngineConfig, StateSnapshot};
use serde_json::{Value, json};

#[derive(Default)]
struct Pod {
    /// (idempotency key, content type, bytes) per upload request.
    uploads: Vec<(String, String, usize)>,
    /// Bodies POSTed to the create endpoint.
    creates: Vec<Value>,
    /// Answer this many uploads with 503 first.
    fail_uploads: usize,
    /// Answer creates with this status.
    create_status: u16,
}

type Shared = Arc<Mutex<Pod>>;

fn workspace() -> Value {
    json!({
        "version": "1",
        "workspace": {"type": "core://Workspace", "id": "home", "title": "Home", "slots": [
            {"slot_id": "s", "provider_uri": "x://feed", "content": {"type": "core://List",
             "query_key": "feed", "item_template": {"type": "core://Divider"}}}]},
        "entities": {"x://posts/1": {"caption": "old"}},
        "queries": {"feed": ["x://posts/1"]}
    })
}

async fn upload(State(pod): State<Shared>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let mut p = pod.lock().unwrap();
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    p.uploads.push((key.clone(), ct, body.len()));
    if p.fail_uploads > 0 {
        p.fail_uploads -= 1;
        return (StatusCode::SERVICE_UNAVAILABLE, "busy".to_owned()).into_response();
    }
    axum::Json(json!({"upload_id": format!("up-{key}")})).into_response()
}

async fn create(State(pod): State<Shared>, body: Bytes) -> impl IntoResponse {
    let mut p = pod.lock().unwrap();
    p.creates
        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    let code = StatusCode::from_u16(p.create_status).unwrap_or(StatusCode::ACCEPTED);
    (code, axum::Json(json!({"ok": code.is_success()})))
}

fn serve(pod: Shared) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new()
                .route("/healthz", get(|| async { "ok" }))
                .route(
                    "/v1/workspaces/home",
                    get(|| async { axum::Json(workspace()) }),
                )
                .route("/v1/uploads", post(upload))
                .route("/v1/create", post(create))
                .with_state(pod);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "construct-attach-{tag}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn photo(dir: &std::path::Path, name: &str, w: u32, h: u32) -> String {
    let p = dir.join(name);
    image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]))
        .save(&p)
        .unwrap();
    p.to_string_lossy().into_owned()
}

fn fast() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(50),
        backoff_base: Duration::from_millis(20),
        backoff_cap: Duration::from_millis(100),
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn queries(s: &StateSnapshot) -> Value {
    serde_json::from_str(&s.queries_json).unwrap()
}

fn entities(s: &StateSnapshot) -> Value {
    serde_json::from_str(&s.entities_json).unwrap()
}

fn share(ids: &[&str]) -> String {
    json!({
        "intent": "action.mutation.create", "query_key": "feed", "position": "start",
        "entity_uri": "x://posts/pending-n1",
        "entity": {"caption": "hi", "media_uris": "$attachments.photos", "meta": "Posting…"},
        "attachments": {"photos": ids},
        "endpoint": "/v1/create",
        "payload": {"caption": "hi", "media": "$attachments.photos", "client_id": "n1"},
        "dismiss": true
    })
    .to_string()
}

#[test]
fn a_create_uploads_its_photos_first_and_once() {
    let pod: Shared = Arc::new(Mutex::new(Pod {
        fail_uploads: 1,
        create_status: 202,
        ..Pod::default()
    }));
    let base = serve(Arc::clone(&pod));
    let dir = temp_dir("upload");
    let engine = ConstructEngine::init_with_config(
        dir.join("store").to_str().unwrap(),
        &base,
        "home",
        fast(),
    );
    wait_until("online", || engine.snapshot().is_online);

    let a = engine
        .stage_attachment(&photo(&dir, "a.png", 3000, 1500))
        .unwrap();
    let b = engine
        .stage_attachment(&photo(&dir, "b.jpg", 100, 100))
        .unwrap();
    assert_eq!((a.width, a.height), (2048, 1024));
    assert_eq!(a.uri, format!("construct://attachments/{}", a.id));
    assert!(a.url.starts_with("file://"), "{}", a.url);
    let snap = engine.snapshot();
    assert_eq!(entities(&snap)[&a.uri]["aspect_ratio"], 2.0);

    // Pause the flush while checking the optimistic state: the first upload
    // answers 503, so the row waits for its backoff.
    engine.dispatch(&share(&[&a.id, &b.id]));
    let snap = engine.snapshot();
    assert_eq!(
        queries(&snap)["feed"],
        json!(["x://posts/pending-n1", "x://posts/1"])
    );
    assert_eq!(
        entities(&snap)["x://posts/pending-n1"]["media_uris"],
        json!([a.uri, b.uri])
    );

    wait_until("the create", || !pod.lock().unwrap().creates.is_empty());
    wait_until("flushed", || engine.snapshot().pending_actions == 0);
    let p = pod.lock().unwrap();
    // a: 503 then OK (same key); b: OK. Then one create.
    let keys: Vec<&str> = p.uploads.iter().map(|u| u.0.as_str()).collect();
    assert_eq!(keys, [a.id.as_str(), a.id.as_str(), b.id.as_str()]);
    assert!(p.uploads.iter().all(|u| u.1 == "image/jpeg" && u.2 > 0));
    assert_eq!(p.creates.len(), 1);
    assert_eq!(
        p.creates[0],
        json!({"caption": "hi", "client_id": "n1",
               "media": [format!("up-{}", a.id), format!("up-{}", b.id)]})
    );
    drop(p);
    // Accepted: the files stay a while (the placeholder may still show them).
    let snap = engine.snapshot();
    assert!(entities(&snap).get(&a.uri).is_some());
    // A used attachment can't be sent twice.
    let before = pod.lock().unwrap().creates.len();
    engine.dispatch(&share(&[&a.id]).replace("pending-n1", "pending-n2"));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(pod.lock().unwrap().creates.len(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_attachments_are_refused_and_refused_creates_delete_their_files() {
    let pod: Shared = Arc::new(Mutex::new(Pod {
        create_status: 422,
        ..Pod::default()
    }));
    let base = serve(Arc::clone(&pod));
    let dir = temp_dir("refuse");
    let engine = ConstructEngine::init_with_config(
        dir.join("store").to_str().unwrap(),
        &base,
        "home",
        fast(),
    );
    wait_until("online", || engine.snapshot().is_online);

    // Not staged: nothing happens.
    engine.dispatch(&share(&["nope"]));
    assert_eq!(engine.snapshot().pending_actions, 0);
    assert_eq!(queries(&engine.snapshot())["feed"], json!(["x://posts/1"]));

    let a = engine
        .stage_attachment(&photo(&dir, "a.png", 50, 80))
        .unwrap();
    let file = PathBuf::from(a.url.strip_prefix("file://").unwrap());
    assert!(file.exists());
    engine.dispatch(&share(&[&a.id]));
    wait_until("the refusal", || {
        let s = engine.snapshot();
        s.pending_actions == 0 && !pod.lock().unwrap().creates.is_empty()
    });
    let snap = engine.snapshot();
    assert_eq!(queries(&snap)["feed"], json!(["x://posts/1"]));
    assert!(entities(&snap).get(&a.uri).is_none());
    assert!(!file.exists(), "a refused create's photos are deleted");

    // A photo removed before sending is deleted too; one that isn't an image
    // is refused with a reason.
    let b = engine
        .stage_attachment(&photo(&dir, "b.png", 10, 10))
        .unwrap();
    engine.discard_attachment(&b.id);
    assert!(entities(&engine.snapshot()).get(&b.uri).is_none());
    std::fs::write(dir.join("x.txt"), "hello").unwrap();
    let err = engine
        .stage_attachment(dir.join("x.txt").to_str().unwrap())
        .unwrap_err();
    assert!(err.contains("isn't supported"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn queued_photos_survive_a_restart_and_upload_later() {
    // No server yet: the create waits offline.
    let dir = temp_dir("restart");
    let store = dir.join("store");
    let id = {
        let engine = ConstructEngine::init_with_config(
            store.to_str().unwrap(),
            "http://127.0.0.1:9",
            "home",
            fast(),
        );
        let a = engine
            .stage_attachment(&photo(&dir, "a.png", 40, 40))
            .unwrap();
        engine.dispatch(&share(&[&a.id]));
        assert_eq!(engine.snapshot().pending_actions, 1);
        a.id
    };
    let pod: Shared = Arc::new(Mutex::new(Pod {
        create_status: 202,
        ..Pod::default()
    }));
    let base = serve(Arc::clone(&pod));
    let engine = ConstructEngine::init_with_config(store.to_str().unwrap(), &base, "home", fast());
    // The attachment entity is back from the store.
    assert!(
        entities(&engine.snapshot())
            .get(format!("construct://attachments/{id}"))
            .is_some()
    );
    wait_until("flushed", || {
        engine.snapshot().pending_actions == 0 && !pod.lock().unwrap().creates.is_empty()
    });
    let p = pod.lock().unwrap();
    assert_eq!(p.uploads.len(), 1);
    assert_eq!(p.creates[0]["media"], json!([format!("up-{id}")]));
    drop(p);
    let _ = std::fs::remove_dir_all(&dir);
}
