//! A fake agent runner speaking CAP v1 on 127.0.0.1, and a pod to point at
//! it. Its data is made up.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_agents::{AgentsAdapter, AgentsConfig};
use app_collections::CollectionsAdapter;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

pub const TOKEN: &str = "test-runner-token";

#[derive(Default)]
pub struct Fake {
    pub snapshot: Mutex<Value>,
    pub events: Mutex<Vec<Value>>,
    /// `(since, Last-Event-ID)` of each stream request.
    pub streams: Mutex<Vec<(i64, Option<String>)>>,
    pub commands: Mutex<Vec<Value>>,
    /// Answers for the next commands; then 202 `{ok}`.
    pub answers: Mutex<VecDeque<(u16, Value)>>,
    /// End each stream once its backlog is sent.
    pub end_streams: AtomicBool,
    /// Answer 502 and end streams, like a runner that's gone.
    pub down: AtomicBool,
}

impl Fake {
    pub fn push(&self, mut e: Value) {
        let mut events = self.events.lock().unwrap();
        let seq = events.len() as i64
            + 1
            + self.snapshot.lock().unwrap()["cursor"]
                .as_i64()
                .unwrap_or(0);
        e["v"] = json!(1);
        e["seq"] = json!(seq);
        if e.get("ts").is_none() {
            e["ts"] = json!(1_800_000_000_000_i64 + seq * 1000);
        }
        if e.get("data").is_none() {
            e["data"] = json!({});
        }
        events.push(e);
    }

    pub fn sinces(&self) -> Vec<i64> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect()
    }
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn refuse(status: u16, error: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"error": error})),
    )
        .into_response()
}

async fn snapshot(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return refuse(401, "unauthorized");
    }
    if f.down.load(Ordering::SeqCst) {
        return refuse(502, "down");
    }
    let mut s = f.snapshot.lock().unwrap().clone();
    s["v"] = json!(1);
    Json(s).into_response()
}

#[derive(serde::Deserialize)]
struct Since {
    since: Option<i64>,
}

async fn stream(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Query(q): Query<Since>,
) -> Response {
    if !authorized(&headers) {
        return refuse(401, "unauthorized");
    }
    if f.down.load(Ordering::SeqCst) {
        return refuse(502, "down");
    }
    let since = q.since.unwrap_or(0);
    let last = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    f.streams.lock().unwrap().push((since, last));
    let s = futures_util::stream::unfold((f, since), |(f, mut since)| async move {
        loop {
            if f.down.load(Ordering::SeqCst) {
                return None;
            }
            let next = f
                .events
                .lock()
                .unwrap()
                .iter()
                .find(|e| e["seq"].as_i64().unwrap() > since)
                .cloned();
            if let Some(e) = next {
                since = e["seq"].as_i64().unwrap();
                let msg = format!("id: {since}\nevent: cap\ndata: {e}\n\n");
                return Some((Ok::<_, Infallible>(Bytes::from(msg)), (f, since)));
            }
            if f.end_streams.load(Ordering::SeqCst) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(s))
        .unwrap()
}

async fn command(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return refuse(401, "unauthorized");
    }
    if f.down.load(Ordering::SeqCst) {
        return refuse(502, "down");
    }
    f.commands.lock().unwrap().push(body.clone());
    match f.answers.lock().unwrap().pop_front() {
        Some((status, answer)) => {
            (StatusCode::from_u16(status).unwrap(), Json(answer)).into_response()
        }
        None => {
            let status = match body["type"].as_str() {
                Some("pause_schedule" | "resume_schedule" | "stop_session") => StatusCode::OK,
                _ => StatusCode::ACCEPTED,
            };
            let mut answer = json!({"ok": true});
            if body["type"] == "run_schedule" {
                answer["run_key"] = json!(format!("{}/manual-1", body["name"].as_str().unwrap()));
                answer["task_id"] = json!(format!(
                    "schedule/{}/manual-1",
                    body["name"].as_str().unwrap()
                ));
            }
            (status, Json(answer)).into_response()
        }
    }
}

/// Serve `fake`; its base URL.
pub async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new()
        .route("/v1/snapshot", get(snapshot))
        .route("/v1/stream", get(stream))
        .route("/v1/commands", post(command))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

pub fn snapshot_data() -> Value {
    json!({
        "v": 1,
        "cursor": 10,
        "sprites": [
            {"name": "box-1", "state": "awake", "last_seen": 1_800_000_000_000_i64, "workspace": "sample-repo"},
            {"name": "box-2", "state": "asleep", "last_seen": 1_799_999_000_000_i64}
        ],
        "sessions": [
            {"id": "session_01Work", "title": "Tidy the readme", "status": "working",
             "sprite": "box-1", "workspace": "sample-repo", "task_id": "pod/1",
             "started_at": 1_800_000_000_000_i64}
        ],
        "checks": [{"id": "egress", "title": "Network egress", "status": "ok", "detail": "Allowlist only"}],
        "schedules": [
            {"name": "nightly-deps", "cron": "0 3 * * *", "timezone": "UTC", "workspace": "sample-repo",
             "model": "a-model", "max_runtime_minutes": 30, "enabled": true, "paused": false,
             "next_run": 1_800_010_000_000_i64,
             "last_run": {"run_key": "nightly-deps/2026-11-06T03:00", "due_at": 1_799_990_000_000_i64,
                          "status": "finished", "error": null, "session_id": "session_01Old",
                          "summary": "Bumped two packages.", "cost_usd": 0.31}}
        ]
    })
}

pub fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(AgentsAdapter::new(AgentsConfig {
            allow_private_hosts: true,
            tasks_every: Duration::from_millis(100),
            backoff_max: Duration::from_millis(100),
        })),
        Arc::new(CollectionsAdapter::new()),
        app_shell::adapter(),
    ]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

pub async fn call(
    pod: &Pod,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(protocol::FEATURES_HEADER, protocol::features_header());
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = pod_core::app(pod.clone())
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

/// An MCP tool call as the pod's owner; the tool's result.
pub async fn tool(pod: &Pod, name: &str, args: Value) -> Value {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        Some("pod"),
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": args}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["result"];
    assert_ne!(r["isError"], true, "{name}: {body}");
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// A layout that is the whole Agents blueprint; its id.
pub async fn agents_layout(pod: &Pod) -> String {
    let app = tool(
        pod,
        "create_layout",
        json!({"title": "Agents",
            "feeds": [["agents", "sessions"], ["agents", "sandboxes"], ["agents", "checks"], ["agents", "schedules"]],
            "layout": {"type": "core://Ref", "app": "agents", "part": "layout"}}),
    )
    .await;
    app["id"].as_str().unwrap().to_owned()
}

/// A fake runner with the made-up snapshot, and a pod signed in to it.
pub async fn signed_in() -> (Pod, Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    *fake.snapshot.lock().unwrap() = snapshot_data();
    let url = serve(fake.clone()).await;
    let pod = pod();
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/agents/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (pod, fake, url)
}

/// Wait up to 5 s for `f` to hold.
pub async fn until<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..250 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// The data of layout `app` (its feeds merged).
pub async fn workspace(pod: &Pod, app: &str) -> Value {
    let (status, ws) = call(
        pod,
        "GET",
        &format!("/v1/workspaces/{app}"),
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    ws
}
