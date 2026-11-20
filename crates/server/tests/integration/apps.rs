//! The construct through the real server (decision
//! `content-addressed-construct`): its event stream (`/v1/construct/events`),
//! the old layout system's routes gone, and this build's core layouts.

use std::sync::Arc;
use std::time::Duration;

use protocol::UiNode;
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
use server::session::session_hash;
use tokio::task::JoinHandle;

struct Pod {
    url: String,
    state: Arc<AppState>,
    handle: JoinHandle<()>,
}

impl Drop for Pod {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn(state: AppState) -> Pod {
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::app(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Pod { url, state, handle }
}

async fn pod() -> Pod {
    spawn(server::state_in_memory()).await
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Reads SSE messages (blocks separated by a blank line) from a streaming response.
struct SseReader {
    resp: reqwest::Response,
    buf: String,
}

impl SseReader {
    /// As this build (its features), with `bearer`.
    async fn connect(url: &str, bearer: Option<&str>) -> Self {
        let mut req = client()
            .get(url)
            .header(protocol::FEATURES_HEADER, protocol::features_header());
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        Self {
            resp,
            buf: String::new(),
        }
    }

    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(i) = self.buf.find("\n\n") {
                    let msg = self.buf[..i].to_owned();
                    self.buf.drain(..i + 2);
                    return msg;
                }
                let chunk = self.resp.chunk().await.unwrap().expect("stream ended");
                self.buf.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no SSE message within 5 s")
    }

    /// The next message that is not a keepalive comment.
    async fn next_event(&mut self) -> String {
        loop {
            let msg = self.next().await;
            if msg != ": keepalive" {
                return msg;
            }
        }
    }
}

/// Home's root for this build (`GET /v1/construct`).
async fn root(pod: &Pod) -> (String, i64) {
    let v: Value = client()
        .get(format!("{}/v1/construct", pod.url))
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        v["root"].as_str().unwrap().to_owned(),
        v["generation"].as_i64().unwrap(),
    )
}

fn construct_msg(root: &str, generation: i64) -> String {
    format!(
        "event: construct\ndata: {}",
        json!({ "root": root, "generation": generation })
    )
}

#[tokio::test]
async fn the_stream_starts_with_the_root_and_sends_each_new_one() {
    let pod = pod().await;
    let url = format!("{}/v1/construct/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    let (r0, g0) = root(&pod).await;
    assert_eq!(sse.next_event().await, construct_msg(&r0, g0));
    // A write moves home's name (its feeds grow): the new root, once.
    let made = pod_core::construct::write::mcp(
        &pod.state,
        "create_layout",
        &json!({"title": "Note", "feeds": [["pixelfed", "home"]],
            "layout": {"type": "core://Page", "title": "Note", "root": {"type": "core://List",
                "query_key": "timeline:home",
                "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}}),
    )
    .await
    .unwrap();
    assert!(made.is_ok(), "{made:?}");
    let (r1, g1) = root(&pod).await;
    assert_ne!(r1, r0);
    assert_eq!(sse.next_event().await, construct_msg(&r1, g1));
}

#[tokio::test]
async fn the_streams_keepalive_comment() {
    let pod = spawn(server::state_in_memory().with_sse_keepalive(Duration::from_millis(100))).await;
    let url = format!("{}/v1/construct/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert!(sse.next().await.starts_with("event: construct"));
    assert_eq!(sse.next().await, ": keepalive");
    assert_eq!(sse.next().await, ": keepalive");
}

/// The construct's stream is home's as before: no bearer, a session with
/// or without a source's sign-in (D46), even a stale bearer (without its
/// session's events) all get the root.
#[tokio::test]
async fn anyone_may_follow_the_root() {
    let pod = pod().await;
    let store = pod.state.store();
    store.create_session(&session_hash("tok")).unwrap();
    store
        .attach_sign_in(&session_hash("tok"), server::pixelfed::SIGN_IN)
        .unwrap();
    store.create_session(&session_hash("other")).unwrap();
    let url = format!("{}/v1/construct/events", pod.url);
    for bearer in [None, Some("tok"), Some("other"), Some("stale")] {
        let mut sse = SseReader::connect(&url, bearer).await;
        assert!(
            sse.next_event().await.starts_with("event: construct"),
            "{bearer:?}"
        );
    }
}

#[tokio::test]
async fn shutdown_ends_open_streams() {
    let pod = pod().await;
    let url = format!("{}/v1/construct/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert!(sse.next_event().await.starts_with("event: construct"));
    pod.state.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(5), sse.resp.chunk())
        .await
        .expect("stream did not end after shutdown");
    assert!(matches!(end, Ok(None)), "{end:?}");
}

/// The old layout system's routes are gone (issue #300).
#[tokio::test]
async fn the_old_layout_systems_routes_are_gone() {
    let pod = pod().await;
    pod.state
        .store()
        .create_pinned_session(&session_hash("pod-token"))
        .unwrap();
    for (method, path) in [
        ("GET", "/v1/apps"),
        ("GET", "/v1/apps/home/versions"),
        ("GET", "/v1/apps/home/versions/1"),
        ("PUT", "/v1/apps/home/hidden"),
        ("GET", "/v1/workspaces/home"),
        ("GET", "/v1/workspaces/home/events"),
        ("GET", "/v1/custom-apps/home"),
        ("POST", "/v1/custom-apps"),
    ] {
        let resp = client()
            .request(method.parse().unwrap(), format!("{}{path}", pod.url))
            .bearer_auth("pod-token")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    // The construct answers instead, never `not_switched`.
    let resp = client()
        .get(format!("{}/v1/construct", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = client()
        .get(format!("{}/v1/construct/names/nope", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// This build's newest element of a core layout, as clients draw it.
fn newest(core_layout: &str) -> UiNode {
    let core = pod_core::construct::core::load(&server::catalog()).unwrap();
    let el = core.current(&format!("@core/{core_layout}")).unwrap();
    serde_json::from_value(construct_elements::materialize(&core, el).unwrap().node).unwrap()
}

/// Decision `named-vocabulary`: no core layout's newest file uses vocabulary
/// current clients no longer draw.
#[test]
fn newest_layouts_use_no_removed_vocabulary() {
    let core = pod_core::construct::core::load(&server::catalog()).unwrap();
    let mut core_layouts = 0;
    for b in server::catalog().core_layouts() {
        let el = core.current(&format!("@core/{}", b.id)).unwrap();
        let removed: Vec<&str> = core.features[el]
            .split(',')
            .filter(|f| protocol::is_retired(f))
            .collect();
        assert!(removed.is_empty(), "{}: {removed:?}", b.id);
        core_layouts += 1;
    }
    assert!(core_layouts > 5);
}

/// `pixelfed`'s Share is a sequence (feature `action_sequence`).
#[test]
fn share_is_a_create_then_back() {
    let layout = newest("pixelfed");
    let share = layout
        .walk()
        .into_iter()
        .flat_map(UiNode::own_intents)
        .find(|i| matches!(i, protocol::ActionIntent::Sequence { .. }))
        .expect("a sequence");
    let protocol::ActionIntent::Sequence { steps } = share else {
        unreachable!()
    };
    assert!(matches!(
        steps.as_slice(),
        [
            protocol::ActionIntent::Create { dismiss: false, .. },
            protocol::ActionIntent::NavigateBack
        ]
    ));
    assert!(
        protocol::required_features(&layout).contains(&protocol::feature::ACTION_SEQUENCE),
        "{:?}",
        protocol::required_features(&layout)
    );
}

/// Settings has no Layouts section or versions screen any more (no layout
/// versions on the device), names channels by their layout, and asks
/// before Sign out and Leave pod.
#[test]
fn settings_names_channels_by_layout_and_shows_no_versions() {
    let text = serde_json::to_string(&newest("settings")).unwrap();
    for gone in [
        "local://layouts",
        "local:versions",
        "show_versions",
        "$item.app_id",
    ] {
        assert!(!text.contains(gone), "{gone}");
    }
    assert!(text.contains("$item.layout"));
    assert!(text.contains("action.confirm"));
    assert!(text.contains("leave_pod"));
}
