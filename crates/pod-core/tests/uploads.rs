//! Uploads (D47), `event: data` (D48) and the adapters' `resume` hook, with
//! test adapters only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::{
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, Hydration, Pod, RequestCtx, Session,
};
use protocol::DataBundle;
use serde_json::Value;
use tower::ServiceExt;

static FILES: &[LayoutSource] = &[LayoutSource {
    app_id: "up",
    name: "0001",
    text: r#"{"note": "First", "schema": 1, "layout": {"type": "core://Workspace", "id": "up",
        "title": "T", "slots": [{"slot_id": "main", "provider_uri": "core://test",
        "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#,
}];

struct Up(Arc<AtomicUsize>);

impl AppAdapter for Up {
    fn name(&self) -> &'static str {
        "up"
    }
    fn apps(&self) -> Vec<AppInfo> {
        vec![AppInfo {
            id: "up",
            title: "Up",
            icon: "photo",
            dev: false,
            auth: AuthPolicy::Public,
        }]
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }
    fn hydrate<'a>(
        &'a self,
        _: &'a Pod,
        _: &'a str,
        _: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async { Hydration::Data(DataBundle::default()) })
    }
    fn resume<'a>(&'a self, _pod: &'a Pod) -> BoxFuture<'a, ()> {
        Box::pin(async {
            self.0.fetch_add(1, Ordering::SeqCst);
        })
    }
}

fn pod_counting(max: usize, resumed: Arc<AtomicUsize>) -> Pod {
    let state = AppState::in_memory(vec![Arc::new(Up(resumed))]).with_upload_max_bytes(max);
    for t in ["alice", "bob"] {
        state.store().create_session(&session_hash(t)).unwrap();
    }
    Arc::new(state)
}

fn pod(max: usize) -> Pod {
    pod_counting(max, Arc::default())
}

fn session(token: &str) -> Session {
    Session {
        token_hash: session_hash(token),
        sign_ins: Vec::new(),
        pinned: false,
    }
}

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1];

async fn upload(
    pod: &Pod,
    bearer: Option<&str>,
    key: Option<&str>,
    ct: &str,
    body: &[u8],
) -> (StatusCode, Value) {
    let mut req = Request::post("/v1/uploads").header("content-type", ct);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    let resp = pod_core::app(pod.clone())
        .oneshot(req.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(pod: &Pod, path: &str) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let resp = pod_core::app(pod.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

#[tokio::test]
async fn uploads_need_a_session_a_key_and_an_image() {
    let pod = pod(1024);
    let ok = |r: &(StatusCode, Value)| r.1["upload_id"].as_str().unwrap().to_owned();
    assert_eq!(
        upload(&pod, None, Some("k"), "image/jpeg", JPEG).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upload(&pod, Some("nobody"), Some("k"), "image/jpeg", JPEG)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upload(&pod, Some("alice"), None, "image/jpeg", JPEG)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        upload(&pod, Some("alice"), Some("k"), "image/svg+xml", b"<svg/>")
            .await
            .0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    // The declared type must be what the bytes are.
    assert_eq!(
        upload(&pod, Some("alice"), Some("k"), "image/png", JPEG)
            .await
            .0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let too_big = [JPEG, &[0u8; 2048]].concat();
    assert_eq!(
        upload(&pod, Some("alice"), Some("k"), "image/jpeg", &too_big)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let a = upload(&pod, Some("alice"), Some("k1"), "image/jpeg", JPEG).await;
    assert_eq!(a.0, StatusCode::OK);
    let id = ok(&a);
    assert_eq!(id.len(), 32);
    // Same key, same session: the same upload. Another session: its own.
    assert_eq!(
        ok(&upload(&pod, Some("alice"), Some("k1"), "image/jpeg", JPEG).await),
        id
    );
    let bobs = ok(&upload(&pod, Some("bob"), Some("k1"), "image/jpeg", JPEG).await);
    assert_ne!(bobs, id);

    // Readable without a bearer, as the image it is.
    let (status, headers, bytes) = get(&pod, &format!("/v1/uploads/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(bytes, JPEG);
    assert_eq!(get(&pod, "/v1/uploads/0123").await.0, StatusCode::NOT_FOUND);
    assert!(pod.catalog().is_public_path(&format!("/v1/uploads/{id}")));
    assert!(!pod.catalog().is_public_path("/v1/uploads/../x"));

    // Adapters: only the uploader's session sees it; claims are all or nothing.
    let up = pod.uploads();
    assert!(up.get(&id, &session("bob")).await.unwrap().is_none());
    let r = up.get(&id, &session("alice")).await.unwrap().unwrap();
    assert_eq!(
        (r.content_type.as_str(), r.size),
        ("image/jpeg", JPEG.len() as i64)
    );
    assert!(
        !up.claim(&[id.clone(), bobs.clone()], &session("alice"), "t:1")
            .await
            .unwrap()
    );
    assert!(
        up.get(&id, &session("alice"))
            .await
            .unwrap()
            .unwrap()
            .owner
            .is_none()
    );
    assert!(
        up.claim(std::slice::from_ref(&id), &session("alice"), "t:1")
            .await
            .unwrap()
    );
    assert!(
        !up.claim(std::slice::from_ref(&id), &session("alice"), "t:2")
            .await
            .unwrap()
    );
    assert_eq!(up.owned("t:1").await.unwrap().len(), 1);
    assert_eq!(up.bytes(&id).await.unwrap().unwrap(), JPEG);
    up.release("t:1").await.unwrap();
    assert_eq!(
        get(&pod, &format!("/v1/uploads/{id}")).await.0,
        StatusCode::NOT_FOUND
    );

    // The sweeper deletes old unclaimed uploads only.
    let kept = ok(&upload(&pod, Some("alice"), Some("k2"), "image/jpeg", JPEG).await);
    assert!(
        up.claim(std::slice::from_ref(&kept), &session("alice"), "t:3")
            .await
            .unwrap()
    );
    let later = pod_core::util::now_secs() + pod_core::uploads::UNCLAIMED_TTL_SECS + 10;
    assert_eq!(
        pod_core::uploads::sweep(pod.store(), later).unwrap(),
        1,
        "bob's"
    );
    assert_eq!(
        get(&pod, &format!("/v1/uploads/{bobs}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&pod, &format!("/v1/uploads/{kept}")).await.0,
        StatusCode::OK
    );
}

/// Read the SSE body until `want` shows up or `wait` passes.
async fn sse_text(body: &mut Body, wait: Duration) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await {
        if let Some(d) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(d));
        }
    }
    text
}

#[tokio::test]
async fn data_events_reach_only_their_session_and_resume_runs_once() {
    let resumed = Arc::new(AtomicUsize::new(0));
    let pod = pod_counting(1024, Arc::clone(&resumed));
    let connect = |token: &'static str| {
        let pod = pod.clone();
        async move {
            pod_core::app(pod)
                .oneshot(
                    Request::get("/v1/workspaces/up/events")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .into_body()
        }
    };
    let mut alice = connect("alice").await;
    let mut bob = connect("bob").await;
    // The first message (the layout version) arrives on connecting.
    assert!(
        sse_text(&mut alice, Duration::from_millis(200))
            .await
            .contains("event: layout")
    );
    assert!(
        sse_text(&mut bob, Duration::from_millis(200))
            .await
            .contains("event: layout")
    );

    pod.publish_data("up", &session("alice"));
    pod.publish_data("other", &session("bob"));
    let a = sse_text(&mut alice, Duration::from_millis(300)).await;
    assert!(
        a.contains("event: data") && a.contains(r#"{"workspace_id":"up"}"#),
        "{a}"
    );
    let b = sse_text(&mut bob, Duration::from_millis(300)).await;
    assert!(!b.contains("event: data"), "{b}");

    // Every `app()` above was for the same pod: resume ran once.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(resumed.load(Ordering::SeqCst), 1);
}
