//! The events stream's session handling (security audit #31, M1 and M2):
//! a stream ends once its session is revoked (logout, a pod token rotation
//! from another process, expiry) and forwards nothing private after that,
//! and one session token may hold only a few streams open.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::store::Store;
use pod_core::{
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, FeedSpec, Hydration, NewNotification,
    Pod, RequestCtx, SourceManifest,
};
use protocol::{DataBundle, Importance, NotificationChannel};
use tower::ServiceExt;

static FILES: &[LayoutSource] = &[LayoutSource {
    app_id: "bell",
    name: "0001",
    text: r#"{"note": "First", "schema": 1, "layout": {"type": "core://Workspace", "id": "bell",
        "title": "T", "slots": [{"slot_id": "main", "provider_uri": "core://test",
        "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#,
}];

struct Bell;

impl AppAdapter for Bell {
    fn name(&self) -> &'static str {
        "bell"
    }
    fn apps(&self) -> Vec<AppInfo> {
        vec![AppInfo {
            id: "bell",
            title: "Bell",
            icon: "photo",
            dev: false,
            auth: AuthPolicy::Public,
            feeds: &[("bell", "all")],
        }]
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }
    fn manifest(&self) -> SourceManifest {
        SourceManifest {
            feeds: &[FeedSpec {
                name: "all",
                query_keys: &[],
                doc: "",
            }],
            ..SourceManifest::new("T")
        }
    }
    fn feed<'a>(&'a self, _: &'a Pod, _: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async { Hydration::Data(DataBundle::default()) })
    }
    fn notification_channels(&self) -> Vec<NotificationChannel> {
        vec![NotificationChannel {
            app_id: "bell".into(),
            id: "likes".into(),
            title: "likes".into(),
            description: String::new(),
            default_enabled: true,
            importance: Importance::Default,
        }]
    }
}

fn note(key: &str) -> NewNotification {
    NewNotification {
        app_id: "bell".into(),
        channel: "likes".into(),
        dedupe_key: key.into(),
        title: format!("title {key}"),
        body: "private body".into(),
        image_url: None,
        target_uri: None,
        group_key: None,
        created_at: 1_700_000_000,
        quiet: false,
    }
}

fn state() -> AppState {
    AppState::in_memory(vec![Arc::new(Bell)])
}

async fn open_stream(pod: &Pod, token: Option<&str>) -> Response {
    let mut req = Request::get("/v1/workspaces/bell/events");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    pod_core::app(pod.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Read the SSE body until it ends; panics if it is still open after 5 s.
async fn read_to_end(body: &mut Body) -> String {
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .unwrap_or_else(|_| panic!("the stream is still open; got {seen:?}"));
        match frame {
            None => return seen,
            Some(f) => {
                if let Some(data) = f.unwrap().data_ref() {
                    seen.push_str(&String::from_utf8_lossy(data));
                }
            }
        }
    }
}

/// Read the first event (the current version) off a fresh stream.
async fn first_event(body: &mut Body) {
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("no first event")
        .expect("stream ended")
        .unwrap();
    let text = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
    assert!(text.contains("event: layout"), "{text}");
}

#[tokio::test]
async fn logout_closes_the_stream() {
    let pod: Pod = Arc::new(state().with_sse_keepalive(Duration::from_millis(100)));
    pod.store().create_session(&session_hash("tok")).unwrap();
    let resp = open_stream(&pod, Some("tok")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    first_event(&mut body).await;

    let logout = pod_core::app(pod.clone())
        .oneshot(
            Request::post("/v1/auth/logout")
                .header("authorization", "Bearer tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    read_to_end(&mut body).await;
}

/// `server pods rotate` replaces the pod token's pinned session from
/// another process (its own connection to the pod's database); the running
/// server's stream for the old token must still end.
#[tokio::test]
async fn rotating_the_pod_token_closes_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let pod: Pod = Arc::new(
        AppState::open_dir(dir.path(), vec![Arc::new(Bell)])
            .unwrap()
            .with_sse_keepalive(Duration::from_millis(100)),
    );
    pod.store()
        .create_pinned_session(&session_hash("old"))
        .unwrap();
    let resp = open_stream(&pod, Some("old")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    first_event(&mut body).await;

    let path = dir.path().join("server.db");
    tokio::task::spawn_blocking(move || {
        Store::open(&path)
            .unwrap()
            .replace_pinned_session(&session_hash("new"))
            .unwrap()
    })
    .await
    .unwrap();
    read_to_end(&mut body).await;

    // The new token streams as usual.
    let resp = open_stream(&pod, Some("new")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_expired_session_closes_the_stream() {
    let pod: Pod = Arc::new(
        state()
            .with_session_ttl_secs(2)
            .with_sse_keepalive(Duration::from_millis(100)),
    );
    pod.store().create_session(&session_hash("tok")).unwrap();
    let resp = open_stream(&pod, Some("tok")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    first_event(&mut body).await;
    read_to_end(&mut body).await;
}

/// Between two re-checks a revoked stream still forwards nothing private:
/// the session is checked before each notification.
#[tokio::test]
async fn no_notification_reaches_a_revoked_stream() {
    let pod: Pod = Arc::new(state().with_sse_keepalive(Duration::from_secs(600)));
    pod.store().create_session(&session_hash("tok")).unwrap();
    let resp = open_stream(&pod, Some("tok")).await;
    let mut body = resp.into_body();
    first_event(&mut body).await;

    pod.store().delete_session(&session_hash("tok")).unwrap();
    pod.notify(vec![note("secret")]).await.unwrap();
    let rest = read_to_end(&mut body).await;
    assert!(!rest.contains("notification"), "{rest}");
    assert!(!rest.contains("private body"), "{rest}");
}

#[tokio::test]
async fn streams_are_capped_per_token() {
    let pod: Pod = Arc::new(state().with_sse_streams_per_token(2));
    for t in ["a", "b"] {
        pod.store().create_session(&session_hash(t)).unwrap();
    }
    let first = open_stream(&pod, Some("a")).await;
    let second = open_stream(&pod, Some("a")).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);

    let third = open_stream(&pod, Some("a")).await;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = third.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "too_many_streams");
    assert_eq!(body["limit"], 2);

    // Other tokens have their own count.
    assert_eq!(open_stream(&pod, Some("b")).await.status(), StatusCode::OK);

    // Closing a stream frees its slot.
    drop(first);
    assert_eq!(open_stream(&pod, Some("a")).await.status(), StatusCode::OK);
    drop(second);
}
