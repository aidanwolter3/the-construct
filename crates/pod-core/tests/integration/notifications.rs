//! Notifications from apps (D52) with a test adapter: storing once, unread
//! counts and reads, devices and channel choices, polling and its backoff,
//! the events stream, and Web Push to a local receiver.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Router,
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::push::{PushSender, test_device_keys};
use pod_core::session::session_hash;
use pod_core::{
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, Catalog, FeedSpec, Hydration,
    NewNotification, Pod, PollCtx, PollOutcome, PollStatus, RequestCtx, SourceManifest,
};
use protocol::{DataBundle, Importance, NotificationChannel, PushPayload};
use serde_json::{Value, json};
use tower::ServiceExt;

static FILES: &[LayoutSource] = &[LayoutSource {
    dir: "layouts",
    app_id: "bell",
    name: "0001",
    text: r#"{"note": "First", "schema": 1, "layout": {"type": "core://Workspace", "id": "bell",
        "title": "T", "slots": [{"slot_id": "main", "provider_uri": "core://test",
        "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#,
}];

const BELL: AppInfo = AppInfo {
    id: "bell",
    title: "Bell",
    icon: "photo",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[("bell", "all")],
};

/// Polls answer from a queue of outcomes (empty queue: nothing new).
#[derive(Default)]
struct Bell {
    outcomes: Mutex<VecDeque<PollOutcome>>,
    polls: Mutex<u32>,
}

impl AppAdapter for Bell {
    fn name(&self) -> &'static str {
        "bell"
    }
    fn apps(&self) -> Vec<AppInfo> {
        vec![BELL]
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
        vec![channel("likes", true), channel("other", false)]
    }
    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }
    fn poll_notifications<'a>(&'a self, _: &'a Pod, _: &'a PollCtx) -> BoxFuture<'a, PollOutcome> {
        *self.polls.lock().unwrap() += 1;
        let next = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        Box::pin(async move { next })
    }
}

fn channel(id: &str, on: bool) -> NotificationChannel {
    NotificationChannel {
        app_id: "bell".into(),
        id: id.into(),
        title: id.into(),
        description: String::new(),
        default_enabled: on,
        importance: Importance::Default,
    }
}

fn note(key: &str, channel: &str) -> NewNotification {
    NewNotification {
        app_id: "bell".into(),
        channel: channel.into(),
        dedupe_key: key.into(),
        title: format!("title {key}"),
        body: "body".into(),
        image_url: None,
        target_uri: Some(format!("bell://items/{key}")),
        group_key: None,
        created_at: 1_700_000_000,
        quiet: false,
    }
}

struct Pod1 {
    pod: Pod,
    bell: Arc<Bell>,
    token: String,
}

fn pod_with(push: Option<Arc<PushSender>>) -> Pod1 {
    let bell = Arc::new(Bell::default());
    let state = AppState::in_memory(vec![bell.clone()]).with_push(push);
    let token = "tok-1".to_owned();
    state.store().create_session(&session_hash(&token)).unwrap();
    Pod1 {
        pod: Arc::new(state),
        bell,
        token,
    }
}

async fn call(
    p: &Pod1,
    method: &str,
    path: &str,
    body: Option<Value>,
    device: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {}", p.token));
    if let Some(d) = device {
        req = req.header("x-construct-device", d);
    }
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = pod_core::app(p.pod.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[test]
fn the_catalog_checks_channels() {
    struct Wrong;
    impl AppAdapter for Wrong {
        fn name(&self) -> &'static str {
            "wrong"
        }
        fn apps(&self) -> Vec<AppInfo> {
            vec![BELL]
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
        fn feed<'a>(
            &'a self,
            _: &'a Pod,
            _: &'a str,
            _: &'a RequestCtx,
        ) -> BoxFuture<'a, Hydration> {
            Box::pin(async { Hydration::Data(DataBundle::default()) })
        }
        fn notification_channels(&self) -> Vec<NotificationChannel> {
            let mut c = channel("x", true);
            c.app_id = "elsewhere".into();
            vec![c]
        }
    }
    let err = Catalog::new(vec![Arc::new(Wrong)]).err().unwrap();
    assert!(err.contains("not one of its apps"), "{err}");

    struct Silent;
    impl AppAdapter for Silent {
        fn name(&self) -> &'static str {
            "silent"
        }
        fn apps(&self) -> Vec<AppInfo> {
            vec![BELL]
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
        fn feed<'a>(
            &'a self,
            _: &'a Pod,
            _: &'a str,
            _: &'a RequestCtx,
        ) -> BoxFuture<'a, Hydration> {
            Box::pin(async { Hydration::Data(DataBundle::default()) })
        }
        fn poll_interval(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }
    }
    let err = Catalog::new(vec![Arc::new(Silent)]).err().unwrap();
    assert!(err.contains("declares no channel"), "{err}");
}

#[tokio::test]
async fn routes_need_a_session() {
    let p = pod_with(None);
    let resp = pod_core::app(p.pod.clone())
        .oneshot(
            Request::get("/v1/notifications")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let stale = Pod1 {
        token: "nope".into(),
        ..p
    };
    let (status, _) = call(&stale, "GET", "/v1/notifications", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn notifications_are_stored_once_and_read() {
    let p = pod_with(None);
    let mut quiet = note("old", "likes");
    quiet.quiet = true;
    let stored = p
        .pod
        .notify(vec![
            note("a", "likes"),
            note("b", "other"),
            quiet,
            note("x", "nope"),
        ])
        .await
        .unwrap();
    assert_eq!(stored.len(), 3, "the undeclared channel is dropped");
    // Reporting the same keys again stores nothing.
    assert!(
        p.pod
            .notify(vec![note("a", "likes")])
            .await
            .unwrap()
            .is_empty()
    );

    let (status, list) = call(&p, "GET", "/v1/notifications", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["unread_total"], 2);
    assert_eq!(list["unread_by_app"]["bell"], 2);
    let items = list["notifications"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items.iter().filter(|n| n["read"] == true).count(), 1);
    assert!(
        items[0]["target_uri"]
            .as_str()
            .unwrap()
            .starts_with("bell://items/")
    );

    // Paging.
    let (_, page) = call(&p, "GET", "/v1/notifications?limit=2", None, None).await;
    assert_eq!(page["notifications"].as_array().unwrap().len(), 2);
    let before = page["next_before"].as_str().unwrap().to_owned();
    let (_, rest) = call(
        &p,
        "GET",
        &format!("/v1/notifications?limit=2&before={before}"),
        None,
        None,
    )
    .await;
    assert_eq!(rest["notifications"].as_array().unwrap().len(), 1);
    assert!(rest.get("next_before").is_none());

    let first = items.iter().find(|n| n["read"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, counts) = call(
        &p,
        "POST",
        "/v1/notifications/read",
        Some(json!({"ids": [first]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(counts["unread_total"], 1);
    let (_, counts) = call(
        &p,
        "POST",
        "/v1/notifications/read",
        Some(json!({"all": true, "app": "bell"})),
        None,
    )
    .await;
    assert_eq!(counts["unread_total"], 0);
    let (status, _) = call(&p, "POST", "/v1/notifications/read", Some(json!({})), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn devices_and_channel_choices() {
    let p = pod_with(None);
    let (status, _) = call(
        &p,
        "PUT",
        "/v1/devices/bad",
        Some(json!({"platform": "android"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "device ids are 8+ characters"
    );
    let (status, v) = call(
        &p,
        "PUT",
        "/v1/devices/device-0001",
        Some(json!({"platform": "android", "name": "Phone"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["push"], false);
    // No push sender: a subscription is refused.
    let (_, p256dh, auth) = test_device_keys();
    let (status, v) = call(&p, "PUT", "/v1/devices/device-0001",
        Some(json!({"platform": "android", "push": {"endpoint": "https://push.example.com/x", "p256dh": p256dh, "auth": auth}})), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["error"], "push_unavailable");

    let (_, chans) = call(
        &p,
        "GET",
        "/v1/notifications/channels",
        None,
        Some("device-0001"),
    )
    .await;
    let enabled: Vec<(String, bool)> = chans["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["enabled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        enabled,
        vec![("likes".into(), true), ("other".into(), false)]
    );
    let (status, _) = call(
        &p,
        "PUT",
        "/v1/devices/device-0001/channels",
        Some(json!({"bell.likes": false, "bell.other": true})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, chans) = call(
        &p,
        "GET",
        "/v1/notifications/channels",
        None,
        Some("device-0001"),
    )
    .await;
    assert_eq!(chans["channels"][0]["enabled"], false);
    assert_eq!(chans["channels"][1]["enabled"], true);
    let (status, v) = call(
        &p,
        "PUT",
        "/v1/devices/device-0001/channels",
        Some(json!({"bell.nope": true})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "unknown_channel");
    let (status, _) = call(
        &p,
        "PUT",
        "/v1/devices/device-9999/channels",
        Some(json!({"bell.likes": true})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, v) = call(&p, "DELETE", "/v1/devices/device-0001", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["deleted"], true);
}

#[tokio::test]
async fn polling_needs_a_device_and_backs_off() {
    let p = pod_with(None);
    assert_eq!(p.pod.poll_due().await, None, "no device: no polls");
    assert_eq!(*p.bell.polls.lock().unwrap(), 0);
    call(
        &p,
        "PUT",
        "/v1/devices/device-0001",
        Some(json!({"platform": "desktop"})),
        None,
    )
    .await;
    assert_eq!(p.pod.next_wake().await, Some(0), "due at once");

    p.bell.outcomes.lock().unwrap().push_back(PollOutcome {
        new: vec![note("p1", "likes")],
        ..Default::default()
    });
    let now = pod_core::util::now_secs();
    let next = p.pod.poll_due().await.unwrap();
    assert!(
        (now + 119..=now + 121).contains(&next),
        "the adapter's interval"
    );
    assert_eq!(*p.bell.polls.lock().unwrap(), 1);
    let (_, list) = call(&p, "GET", "/v1/notifications?limit=0", None, None).await;
    assert_eq!(list["unread_total"], 1);
    // Not due yet: no poll.
    p.pod.poll_due().await;
    assert_eq!(*p.bell.polls.lock().unwrap(), 1);

    // A failure backs off (at least the interval), a lost sign-in stops polls.
    p.pod
        .store()
        .with_conn(|c| c.execute("UPDATE notification_polls SET next_at = 0", []))
        .unwrap();
    p.bell.outcomes.lock().unwrap().push_back(PollOutcome {
        status: PollStatus::Transient("down".into()),
        ..Default::default()
    });
    let next = p.pod.poll_due().await.unwrap();
    assert!(next >= now + 120);
    p.pod
        .store()
        .with_conn(|c| c.execute("UPDATE notification_polls SET next_at = 0", []))
        .unwrap();
    p.bell.outcomes.lock().unwrap().push_back(PollOutcome {
        status: PollStatus::SignInGone("test".into()),
        ..Default::default()
    });
    assert_eq!(p.pod.poll_due().await, None);
    assert_eq!(p.pod.next_wake().await, None);
    // Signing in again resumes them.
    p.pod
        .store()
        .attach_sign_in(&session_hash(&p.token), "test")
        .unwrap();
    assert_eq!(p.pod.next_wake().await, Some(0));
}

/// Read the SSE body until `needle` shows up (or time runs out).
async fn read_until(body: &mut Body, needle: &str) -> String {
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seen.contains(needle) {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .expect("timed out waiting for the event")
            .expect("stream ended")
            .unwrap();
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
        }
    }
    seen
}

#[tokio::test]
async fn the_events_stream_carries_notifications() {
    let p = pod_with(None);
    let resp = pod_core::app(p.pod.clone())
        .oneshot(
            Request::get("/v1/workspaces/bell/events")
                .header("authorization", format!("Bearer {}", p.token))
                .header("x-construct-device", "device-0001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    read_until(&mut body, "event: layout").await;
    p.pod.notify(vec![note("live", "likes")]).await.unwrap();
    let text = read_until(&mut body, "event: notification").await;
    assert!(text.contains("title live"), "{text}");
    assert!(text.contains("\"unread_total\":1"), "{text}");
    call(
        &p,
        "POST",
        "/v1/notifications/read",
        Some(json!({"all": true})),
        None,
    )
    .await;
    read_until(&mut body, "event: notifications_read").await;

    // Without a session: layout events only.
    let resp = pod_core::app(p.pod.clone())
        .oneshot(
            Request::get("/v1/workspaces/bell/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut anon = resp.into_body();
    read_until(&mut anon, "event: layout").await;
    p.pod.notify(vec![note("private", "likes")]).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(300), anon.frame()).await;
    assert!(got.is_err(), "an anonymous stream hears no notification");
}

/// A local push service: records each delivery's body; answers `status`.
async fn receiver(status: StatusCode) -> (String, Arc<Mutex<Vec<Bytes>>>) {
    let got: Arc<Mutex<Vec<Bytes>>> = Arc::default();
    let g = got.clone();
    let app = Router::new().route(
        "/push/{id}",
        axum::routing::post(move |body: Bytes| {
            let g = g.clone();
            async move {
                g.lock().unwrap().push(body);
                status
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/push/1"), got)
}

async fn wait_for<F: Fn() -> bool>(f: F) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not met in time");
}

#[tokio::test]
async fn web_push_reaches_devices_that_want_it() {
    let sender = Arc::new(
        PushSender::new(
            &PushSender::generate_private_key(),
            "mailto:test@example.com",
            true,
        )
        .unwrap(),
    );
    let p = pod_with(Some(sender.clone()));
    let (_, key) = call(&p, "GET", "/v1/push/key", None, None).await;
    assert_eq!(key["vapid_public_key"], sender.public_key());

    let (endpoint, got) = receiver(StatusCode::CREATED).await;
    let (private, p256dh, auth) = test_device_keys();
    let (status, v) = call(&p, "PUT", "/v1/devices/device-0001",
        Some(json!({"platform": "android", "push": {"endpoint": endpoint, "p256dh": p256dh, "auth": auth}})), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["push"], true);

    p.pod
        .notify(vec![note("p1", "likes"), note("p2", "other")])
        .await
        .unwrap();
    wait_for(|| !got.lock().unwrap().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bodies = got.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "'other' is off by default");
    use base64::Engine;
    let auth_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&auth)
        .unwrap();
    let plain = pod_core::push::decrypt(&bodies[0], &private, &auth_bytes).unwrap();
    let payload: PushPayload = serde_json::from_slice(&plain).unwrap();
    assert_eq!(payload.v, 1);
    assert_eq!(payload.event.notification.title, "title p1");
    assert_eq!(
        payload.event.notification.target_uri.as_deref(),
        Some("bell://items/p1")
    );
    assert_eq!(payload.event.unread_total, 2);

    // A device with its stream open gets no push.
    got.lock().unwrap().clear();
    {
        let _live = p.pod.device_live("device-0001");
        p.pod.notify(vec![note("p3", "likes")]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(got.lock().unwrap().is_empty());
    }
    // Turning the channel off stops pushes too.
    call(
        &p,
        "PUT",
        "/v1/devices/device-0001/channels",
        Some(json!({"bell.likes": false})),
        None,
    )
    .await;
    p.pod.notify(vec![note("p4", "likes")]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(got.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_gone_subscription_is_dropped() {
    let sender = Arc::new(
        PushSender::new(
            &PushSender::generate_private_key(),
            "mailto:test@example.com",
            true,
        )
        .unwrap(),
    );
    let p = pod_with(Some(sender));
    let (endpoint, got) = receiver(StatusCode::GONE).await;
    let (_, p256dh, auth) = test_device_keys();
    call(&p, "PUT", "/v1/devices/device-0001",
        Some(json!({"platform": "android", "push": {"endpoint": endpoint, "p256dh": p256dh, "auth": auth}})), None).await;
    p.pod.notify(vec![note("g1", "likes")]).await.unwrap();
    wait_for(|| !got.lock().unwrap().is_empty()).await;
    let pod = p.pod.clone();
    wait_for(move || {
        pod.store()
            .with_conn(|c| {
                c.query_row(
                    "SELECT push_endpoint IS NULL FROM devices WHERE id = 'device-0001'",
                    [],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap()
    })
    .await;
}

#[tokio::test]
async fn a_public_server_refuses_private_endpoints() {
    let sender = Arc::new(
        PushSender::new(
            &PushSender::generate_private_key(),
            "mailto:test@example.com",
            false,
        )
        .unwrap(),
    );
    let p = pod_with(Some(sender));
    let (_, p256dh, auth) = test_device_keys();
    let (status, v) = call(&p, "PUT", "/v1/devices/device-0001",
        Some(json!({"platform": "android", "push": {"endpoint": "http://127.0.0.1:9/x", "p256dh": p256dh, "auth": auth}})), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "push_endpoint_not_allowed");
}
