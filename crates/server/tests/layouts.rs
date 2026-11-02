//! Versioned layouts (history, rollback, auth) and the SSE push channel (M7).

use std::sync::Arc;
use std::time::Duration;

use protocol::{TextStyle, UiNode, WorkspaceSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
use server::layouts;
use server::pixelfed::routes::session_hash;
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

/// A pod in open mode (`CANVAS_AGENT_OPEN=1`).
async fn open_pod() -> Pod {
    spawn(AppState::in_memory().with_agent_open(true)).await
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn get(url: &str, bearer: Option<&str>) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let version = resp
        .headers()
        .get(layouts::VERSION_HEADER)
        .map(|v| v.to_str().unwrap().to_owned());
    (
        resp.status(),
        version,
        resp.json().await.unwrap_or(Value::Null),
    )
}

async fn rollback(pod: &Pod, ws: &str, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = client()
        .post(format!("{}/v1/workspaces/{ws}/rollback", pod.url))
        .json(&body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    (resp.status(), resp.json().await.unwrap_or(Value::Null))
}

/// A recognizably different `home` layout: the built-in one retitled.
fn retitled(ws: &str, title: &str) -> UiNode {
    let mut node = layouts::builtin(ws).unwrap();
    if let UiNode::Workspace { title: t, .. } = &mut node {
        *t = title.to_owned();
    }
    node
}

fn title(spec: &Value) -> String {
    spec["workspace"]["title"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn v1_is_seeded_lazily_with_the_builtin_layout() {
    let pod = open_pod().await;
    for ws in ["home", "pixelfed_demo"] {
        let (status, version, body) = get(&format!("{}/v1/workspaces/{ws}", pod.url), None).await;
        assert_eq!(status, StatusCode::OK, "{ws}");
        assert_eq!(version.as_deref(), Some("1"), "{ws}");
        let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
        assert_eq!(spec.workspace, layouts::builtin(ws).unwrap());
        assert_eq!(spec, {
            // Entities unchanged (home also gets the stored count overlay).
            let mut fixture = server::workspace(ws).unwrap().clone();
            if ws == "home" {
                fixture
                    .entities
                    .entry("core://counters/home".into())
                    .or_default()
                    .set("count", 0);
            }
            fixture
        });
    }
    // Seeding happens once.
    let (_, version, _) = get(&format!("{}/v1/workspaces/home", pod.url), None).await;
    assert_eq!(version.as_deref(), Some("1"));
    let rows = pod.state.store().layout_versions("home").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prompt, None);
    // pixelfed_home v1 is the generated feed layout.
    let (v, node) = layouts::latest(pod.state.store(), "pixelfed_home")
        .unwrap()
        .unwrap();
    assert_eq!(v, 1);
    assert!(matches!(&node, UiNode::Workspace { id, slots, .. }
        if id == "pixelfed_home" && matches!(slots[0].content, UiNode::FeedList { .. })));
    // Unversioned / unknown workspaces.
    assert!(
        layouts::latest(pod.state.store(), "nope")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn commit_appends_and_get_serves_the_latest() {
    let pod = open_pod().await;
    let store = pod.state.store();
    let patch = json!([{ "op": "replace", "path": "/title", "value": "Two" }]);
    let v = layouts::commit(
        store,
        "home",
        &retitled("home", "Two"),
        Some("rename"),
        Some("rules"),
        Some(&patch),
    )
    .unwrap();
    assert_eq!(v, 2, "v1 is seeded before the first commit");
    let (status, version, body) = get(&format!("{}/v1/workspaces/home", pod.url), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(version.as_deref(), Some("2"));
    assert_eq!(title(&body), "Two");
    // Entities still come from the fixture + counter.
    assert_eq!(body["entities"]["core://counters/home"]["count"], 0);
    // Other workspaces are independent.
    let (_, version, _) = get(&format!("{}/v1/workspaces/pixelfed_demo", pod.url), None).await;
    assert_eq!(version.as_deref(), Some("1"));
}

#[tokio::test]
async fn versions_lists_newest_first() {
    let pod = open_pod().await;
    let url = format!("{}/v1/workspaces/home/versions", pod.url);
    let (status, _, body) = get(&url, None).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["version"], 1);
    assert!(list[0]["created_at"].as_i64().unwrap() > 0);
    assert_eq!(list[0]["prompt"], Value::Null);
    assert_eq!(list[0]["provider"], Value::Null);

    layouts::commit(
        pod.state.store(),
        "home",
        &retitled("home", "Two"),
        Some("rename it"),
        Some("rules"),
        None,
    )
    .unwrap();
    let (_, _, body) = get(&url, None).await;
    let versions: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(versions, [2, 1]);
    assert_eq!(body[0]["prompt"], "rename it");
    assert_eq!(body[0]["provider"], "rules");
    assert_eq!(body[0].as_object().unwrap().len(), 4, "{body}");
}

#[tokio::test]
async fn rollback_appends_a_copy_and_get_reflects_it() {
    let pod = open_pod().await;
    for t in ["Two", "Three"] {
        layouts::commit(
            pod.state.store(),
            "home",
            &retitled("home", t),
            None,
            None,
            None,
        )
        .unwrap();
    }
    let (status, body) = rollback(&pod, "home", None, json!({ "version": 1 })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 4);
    assert_eq!(body["rolled_back_to"], 1);
    let original = serde_json::to_value(layouts::builtin("home").unwrap()).unwrap();
    assert_eq!(body["workspace"], original);

    let (_, version, spec) = get(&format!("{}/v1/workspaces/home", pod.url), None).await;
    assert_eq!(version.as_deref(), Some("4"));
    assert_eq!(spec["workspace"], original);

    // Roll back to a rolled-back-from version: history stays append-only.
    let (status, body) = rollback(&pod, "home", None, json!({ "version": 2 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], 5);
    assert_eq!(body["workspace"]["title"], "Two");
    let (_, _, list) = get(&format!("{}/v1/workspaces/home/versions", pod.url), None).await;
    assert_eq!(list.as_array().unwrap().len(), 5);
    assert_eq!(list[0]["provider"], layouts::ROLLBACK_PROVIDER);
    assert_eq!(list[0]["prompt"], "rollback to v2");
}

#[tokio::test]
async fn rollback_errors() {
    let pod = open_pod().await;
    // Unknown version.
    let (status, body) = rollback(&pod, "home", None, json!({ "version": 9 })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "version_not_found");
    // Nothing was appended.
    assert_eq!(pod.state.store().layout_versions("home").unwrap().len(), 1);
    // Bad body.
    for bad in [
        json!({}),
        json!({ "version": 0 }),
        json!({ "version": "1" }),
    ] {
        let (status, body) = rollback(&pod, "home", None, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], "invalid_request");
    }
    // Unknown / unversioned workspace.
    let (status, body) = rollback(&pod, "nope", None, json!({ "version": 1 })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "workspace_not_found");
    let (status, _, _) = get(&format!("{}/v1/workspaces/nope/versions", pod.url), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versions_and_rollback_need_a_session_unless_open() {
    let pod = spawn(AppState::in_memory()).await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    let versions = format!("{}/v1/workspaces/pixelfed_home/versions", pod.url);
    for bearer in [None, Some("wrong")] {
        let (status, _, body) = get(&versions, bearer).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?}");
        assert_eq!(body["error"], "unauthorized");
        let (status, _) = rollback(&pod, "pixelfed_home", bearer, json!({ "version": 1 })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?}");
    }
    assert_eq!(
        pod.state
            .store()
            .layout_versions("pixelfed_home")
            .unwrap()
            .len(),
        0,
        "rejected requests change nothing"
    );
    let (status, _, _) = get(&versions, Some("tok")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) =
        rollback(&pod, "pixelfed_home", Some("tok"), json!({ "version": 1 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], 2);

    // Open mode: no session needed.
    let open = open_pod().await;
    let (status, _, _) = get(&format!("{}/v1/workspaces/home/versions", open.url), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rollback(&open, "home", None, json!({ "version": 1 })).await;
    assert_eq!(status, StatusCode::OK);
}

/// Reads SSE messages (blocks separated by a blank line) from a streaming response.
struct SseReader {
    resp: reqwest::Response,
    buf: String,
}

impl SseReader {
    async fn connect(url: &str, bearer: Option<&str>) -> Self {
        let mut req = client().get(url);
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

fn layout_msg(ws: &str, version: i64) -> String {
    format!(
        "event: layout\ndata: {}",
        json!({ "workspace_id": ws, "version": version })
    )
}

#[tokio::test]
async fn sse_sends_current_version_then_new_versions_of_its_workspace() {
    let pod = open_pod().await;
    layouts::commit(
        pod.state.store(),
        "home",
        &retitled("home", "Two"),
        None,
        None,
        None,
    )
    .unwrap();
    let mut home =
        SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(home.next_event().await, layout_msg("home", 2));
    let mut demo = SseReader::connect(
        &format!("{}/v1/workspaces/pixelfed_demo/events", pod.url),
        None,
    )
    .await;
    assert_eq!(demo.next_event().await, layout_msg("pixelfed_demo", 1));

    // A version of another workspace is filtered out; the rollback arrives.
    let other = layouts::commit_and_publish(
        &pod.state,
        "pixelfed_demo",
        retitled("pixelfed_demo", "X"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(other, 2);
    let (status, _) = rollback(&pod, "home", None, json!({ "version": 1 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(home.next_event().await, layout_msg("home", 3));
    assert_eq!(demo.next_event().await, layout_msg("pixelfed_demo", 2));

    // A reconnecting client catches up with the current version.
    let mut again =
        SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(again.next_event().await, layout_msg("home", 3));
}

#[tokio::test]
async fn sse_keepalive_comment() {
    let pod = spawn(AppState::in_memory().with_sse_keepalive(Duration::from_millis(100))).await;
    let mut sse = SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(sse.next().await, layout_msg("home", 1));
    assert_eq!(sse.next().await, ": keepalive");
    assert_eq!(sse.next().await, ": keepalive");
}

#[tokio::test]
async fn sse_auth_matches_the_workspace_get() {
    let pod = spawn(AppState::in_memory()).await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    let url = format!("{}/v1/workspaces/pixelfed_home/events", pod.url);
    // No bearer: allowed (events carry only version numbers).
    let mut anon = SseReader::connect(&url, None).await;
    assert_eq!(anon.next_event().await, layout_msg("pixelfed_home", 1));
    // Valid session: allowed.
    let mut signed_in = SseReader::connect(&url, Some("tok")).await;
    assert_eq!(signed_in.next_event().await, layout_msg("pixelfed_home", 1));
    // Stale bearer: 401, like GET /v1/workspaces/pixelfed_home.
    let resp = client()
        .get(&url)
        .bearer_auth("stale")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Unknown workspace: 404.
    let resp = client()
        .get(format!("{}/v1/workspaces/nope/events", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn committed_layout_json_round_trips() {
    // A layout with a blanked caption survives the store as-is.
    let pod = open_pod().await;
    let mut node = layouts::builtin("pixelfed_home").unwrap();
    if let UiNode::Workspace { slots, .. } = &mut node
        && let UiNode::FeedList { item_template, .. } = &mut slots[0].content
        && let UiNode::PostCard { caption_slot, .. } = item_template.as_mut()
    {
        **caption_slot = UiNode::Text {
            value_expr: String::new(),
            style: TextStyle::Caption,
        };
    }
    let v = layouts::commit(pod.state.store(), "pixelfed_home", &node, None, None, None).unwrap();
    let (latest_v, latest) = layouts::latest(pod.state.store(), "pixelfed_home")
        .unwrap()
        .unwrap();
    assert_eq!((latest_v, latest), (v, node));
}

#[tokio::test]
async fn shutdown_ends_open_streams() {
    let pod = open_pod().await;
    let mut sse = SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(sse.next_event().await, layout_msg("home", 1));
    pod.state.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(5), sse.resp.chunk())
        .await
        .expect("stream did not end after shutdown");
    assert!(matches!(end, Ok(None)), "{end:?}");
}
