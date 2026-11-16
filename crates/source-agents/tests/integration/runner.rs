//! A fake agent runner speaking CAP v3 on 127.0.0.1, and a pod to point at
//! it. Its data is made up.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use source_agents::{AgentsAdapter, AgentsConfig};
use source_collections::CollectionsAdapter;
use tower::ServiceExt;

pub const TOKEN: &str = "test-runner-token";

#[derive(Default)]
pub struct Fake {
    pub snapshot: Mutex<Value>,
    pub events: Mutex<Vec<Value>>,
    /// Each run as last sent, for [`Fake::run`].
    pub runs: Mutex<HashMap<String, Value>>,
    /// `(since, Last-Event-ID)` of each stream request.
    pub streams: Mutex<Vec<(i64, Option<String>)>>,
    /// `since` of each events page asked for.
    pub pages: Mutex<Vec<i64>>,
    /// How many snapshots were taken.
    pub snapshots: AtomicI64,
    /// The oldest seq the log keeps: a page from before it is 410.
    pub oldest: AtomicI64,
    /// The runs each request id answered.
    pub requests: Mutex<HashMap<String, String>>,
    pub commands: Mutex<Vec<Value>>,
    /// Answers for the next commands; then the runner's usual answer.
    pub answers: Mutex<VecDeque<(u16, Value)>>,
    /// Run ids `start_run` hands out, in order (then `run_new<n>`).
    pub next_runs: Mutex<VecDeque<String>>,
    /// How many times each run's log was read.
    pub log_reads: Mutex<HashMap<String, usize>>,
    /// End each stream once its backlog is sent.
    pub end_streams: AtomicBool,
    /// Answer 502 and end streams, like a runner that's gone.
    pub down: AtomicBool,
}

impl Fake {
    /// Send an event of `kind` (`run.updated`, ...) carrying `record`.
    pub fn push(&self, kind: &str, record: Value) {
        let mut events = self.events.lock().unwrap();
        let seq = events.len() as i64
            + 1
            + self.snapshot.lock().unwrap()["cursor"]
                .as_i64()
                .unwrap_or(0);
        events.push(
            json!({"v": 3, "seq": seq, "ts": 1_800_000_000_000_i64 + seq * 1000,
            "type": kind, "record": record}),
        );
    }

    /// The seq of the last event sent.
    pub fn last_seq(&self) -> i64 {
        let events = self.events.lock().unwrap();
        events
            .last()
            .and_then(|e| e["seq"].as_i64())
            .unwrap_or_else(|| {
                self.snapshot.lock().unwrap()["cursor"]
                    .as_i64()
                    .unwrap_or(0)
            })
    }

    /// Change run `id` (starting from its last record, or a new one) by
    /// `fields`, raise its version and send it.
    pub fn run(&self, id: &str, fields: Value) {
        let record = {
            let mut runs = self.runs.lock().unwrap();
            let r = runs.entry(id.to_owned()).or_insert_with(|| {
                json!({"id": id, "version": 0, "request_id": null, "work_item_id": null,
                    "occurrence": null, "title": "", "origin": "task",
                    "workspace": "sample-repo", "sprite": "box-1", "model": null,
                    "max_runtime_min": 240, "state": "queued", "state_detail": null,
                    "queue_reason": "waiting_for_slot", "needs_you": null, "end_reason": null,
                    "error": null, "deadline_at": null, "attempt": 1, "max_attempts": 3,
                    "current_tool": null, "summary": null, "claim": null,
                    "outcome": {"state": "none"}, "claude_url": null, "session_id": null,
                    "resumed_from": null, "branch": null,
                    "cost_usd": 0, "input_tokens": 0, "output_tokens": 0,
                    "queued_at": 1_800_000_000_000_i64, "started_at": null,
                    "updated_at": 1_800_000_000_000_i64, "ended_at": null})
            });
            for (k, v) in fields.as_object().unwrap() {
                r[k] = v.clone();
            }
            r["version"] = json!(r["version"].as_i64().unwrap() + 1);
            r.clone()
        };
        self.push("run.updated", record);
    }

    pub fn sinces(&self) -> Vec<i64> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect()
    }

    /// The commands of type `kind` sent so far.
    pub fn sent(&self, kind: &str) -> Vec<Value> {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == kind)
            .cloned()
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
    f.snapshots.fetch_add(1, Ordering::SeqCst);
    let mut s = f.snapshot.lock().unwrap().clone();
    s["v"] = json!(3);
    // The snapshot is the state now: what the events changed since too.
    let runs = f.runs.lock().unwrap().clone();
    let mut listed: Vec<Value> = s["runs"].as_array().cloned().unwrap_or_default();
    for (id, r) in runs {
        listed.retain(|x| x["id"] != id.as_str());
        listed.push(r);
    }
    s["runs"] = json!(listed);
    s["cursor"] = json!(f.last_seq());
    s["oldest_seq"] = json!(f.oldest.load(Ordering::SeqCst));
    Json(s).into_response()
}

#[derive(serde::Deserialize)]
struct PageQuery {
    since: Option<i64>,
    limit: Option<usize>,
}

async fn events(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    if !authorized(&headers) {
        return refuse(401, "unauthorized");
    }
    if f.down.load(Ordering::SeqCst) {
        return refuse(502, "down");
    }
    let since = q.since.unwrap_or(0);
    f.pages.lock().unwrap().push(since);
    if since < f.oldest.load(Ordering::SeqCst) {
        return refuse(410, "cursor_too_old");
    }
    let limit = q.limit.unwrap_or(500).min(500);
    // Small pages, so catching up reads more than one.
    let limit = limit.min(3);
    let all: Vec<Value> = f
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["seq"].as_i64().unwrap() > since)
        .cloned()
        .collect();
    let page: Vec<Value> = all.iter().take(limit).cloned().collect();
    let next = page.last().and_then(|e| e["seq"].as_i64()).unwrap_or(since);
    Json(json!({"events": page, "next": next, "more": all.len() > limit})).into_response()
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
                let msg = format!(
                    "id: {since}\nevent: {}\ndata: {e}\n\n",
                    e["type"].as_str().unwrap()
                );
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
    if let Some((status, answer)) = f.answers.lock().unwrap().pop_front() {
        return (StatusCode::from_u16(status).unwrap(), Json(answer)).into_response();
    }
    let n = f.commands.lock().unwrap().len();
    match body["type"].as_str().unwrap_or("") {
        "start_run" => {
            let request = body["request_id"].as_str().unwrap_or("").to_owned();
            if let Some(id) = f.requests.lock().unwrap().get(&request).cloned() {
                // The same request again: the same run (200).
                let run = f
                    .runs
                    .lock()
                    .unwrap()
                    .get(&id)
                    .cloned()
                    .unwrap_or(json!({"id": id}));
                return Json(json!({"run": run})).into_response();
            }
            let id = f
                .next_runs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| format!("run_new{n}"));
            f.requests
                .lock()
                .unwrap()
                .insert(request.clone(), id.clone());
            let run = json!({"id": id, "version": 1, "state": "queued",
                "queue_reason": "waiting_for_slot", "request_id": request,
                "work_item_id": body["work_item_id"], "occurrence": body["occurrence"],
                "origin": body["origin"], "title": body["title"], "model": body["model"],
                "max_runtime_min": body["max_runtime_min"], "resumed_from": body["resume_from"],
                "outcome": {"state": "none"}, "queued_at": 1_800_000_000_000_i64});
            // Kept as sent, so `Fake::run` changes it from here.
            f.runs.lock().unwrap().insert(id, run.clone());
            (StatusCode::ACCEPTED, Json(json!({"run": run}))).into_response()
        }
        "stop_run" => {
            Json(json!({"run": {"id": body["run_id"], "state": "stopping"}})).into_response()
        }
        "wake_sprite" => {
            Json(json!({"sprite": {"name": body["workspace"], "state": "waking"}})).into_response()
        }
        "repair_sprite" => (
            StatusCode::ACCEPTED,
            Json(json!({"sprite": {"name": body["workspace"], "state": "provisioning"}})),
        )
            .into_response(),
        "mark_reviewed" => {
            let id = body["run_id"].as_str().unwrap_or("").to_owned();
            let current = f.runs.lock().unwrap().get(&id).cloned();
            match current {
                Some(r) if r["outcome"]["state"] == "needs_review" => {
                    let mut outcome = r["outcome"].clone();
                    outcome["state"] = json!("done");
                    outcome["reviewed"] = json!(true);
                    f.run(&id, json!({"outcome": outcome}));
                    let run = f.runs.lock().unwrap()[&id].clone();
                    Json(json!({"run": run})).into_response()
                }
                Some(_) => refuse(409, "the run doesn't need review"),
                None => refuse(404, "no such run"),
            }
        }
        _ => refuse(400, "unknown command"),
    }
}

async fn log(State(f): State<Arc<Fake>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !authorized(&headers) {
        return refuse(401, "unauthorized");
    }
    if f.down.load(Ordering::SeqCst) {
        return refuse(502, "down");
    }
    *f.log_reads.lock().unwrap().entry(id.clone()).or_default() += 1;
    if id == "run_unknown" {
        return refuse(404, "no such run");
    }
    Json(json!({"entries": [
        {"ts": 1_800_000_000_000_i64, "kind": "state", "text": "Queued"},
        {"ts": 1_800_000_060_000_i64, "kind": "state", "text": "Working"},
        {"ts": 1_800_000_120_000_i64, "kind": "tool", "text": "Edit: CHANGELOG.md"}]}))
    .into_response()
}

/// Serve `fake`; its base URL.
pub async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new()
        .route("/v3/snapshot", get(snapshot))
        .route("/v3/events", get(events))
        .route("/v3/stream", get(stream))
        .route("/v3/commands", post(command))
        .route("/v3/runs/{id}/log", get(log))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

pub fn snapshot_data() -> Value {
    json!({
        "v": 3,
        "cursor": 10,
        "oldest_seq": 0,
        "runs": [
            {"id": "run_work", "version": 3, "request_id": "start_1", "work_item_id": "pod/1",
             "title": "Tidy the readme", "origin": "task", "workspace": "sample-repo",
             "sprite": "box-1", "state": "working", "state_detail": "waiting on 1 background task",
             "needs_you": null, "end_reason": null, "outcome": {"state": "none"},
             "current_tool": "Bash: cargo test", "session_id": "session_01Work",
             "attempt": 1, "max_attempts": 3,
             "queued_at": 1_800_000_000_000_i64, "started_at": 1_800_000_000_000_i64,
             "updated_at": 1_800_000_000_000_i64},
            {"id": "run_old", "version": 9, "request_id": "sch_x_20261106T0300",
             "work_item_id": "sch_gone", "occurrence": "2026-11-06T03:00",
             "title": "nightly-deps", "origin": "schedule", "workspace": "sample-repo",
             "sprite": "box-1", "state": "ended", "end_reason": "reported",
             "claim": {"status": "done", "summary": "Bumped two packages.", "pr_url": null},
             "outcome": {"state": "done"},
             "cost_usd": 0.31, "queued_at": 1_799_990_000_000_i64,
             "started_at": 1_799_990_000_000_i64, "ended_at": 1_799_990_600_000_i64,
             "updated_at": 1_799_990_600_000_i64}
        ],
        "sprites": [
            {"name": "box-1", "workspace": "sample-repo", "version": 4, "state": "awake",
             "slots": 2, "slots_used": 1, "preflight": {"ok": true, "failures": []},
             "agentd_version": "a1b2c3d4", "last_heartbeat_at": 1_800_000_000_000_i64},
            {"name": "box-2", "version": 1, "state": "asleep", "agentd_version": null,
             "last_heartbeat_at": 1_799_999_000_000_i64}
        ],
        "checks": [{"id": "egress", "title": "Network egress", "status": "ok",
            "detail": "Allowlist only", "version": 1}]
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
        source_shell::adapter(),
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
