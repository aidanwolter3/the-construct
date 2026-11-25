//! A fake taste service on 127.0.0.1, and a pod to point at it. Its data is
//! made up.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use source_taste::TasteAdapter;
use tower::ServiceExt;

pub const TOKEN: &str = "test-taste-token";

#[derive(Default)]
pub struct Fake {
    /// How many feed pages were served.
    pub pages: AtomicUsize,
    /// Every event as sent.
    pub events: Mutex<Vec<Value>>,
    /// The `q` of each search and steer.
    pub queries: Mutex<Vec<String>>,
    pub summaries: AtomicUsize,
    /// Answer 502, like a service that's down.
    pub down: AtomicBool,
    /// The box of each map request.
    pub boxes: Mutex<Vec<Box4>>,
    /// How many times the whole map was asked for.
    pub maps: AtomicUsize,
    /// Pieces the service no longer has: a reaction to one answers 404.
    pub gone: Mutex<Vec<String>>,
    /// Behave like a service that caches each pod's history (decision
    /// `taste-history-in-the-pod`); otherwise like one from before, which
    /// has no `/api/state` and ignores `X-Taste-State`.
    pub keeps_states: AtomicBool,
    /// The service's own history (no header): what a pod takes over.
    pub own: Mutex<Vec<Value>>,
    /// The histories it holds, by log: each reaction in order.
    pub states: Mutex<std::collections::HashMap<String, Vec<Value>>>,
    /// Every `PUT /api/state` body.
    pub uploads: Mutex<Vec<Value>>,
    /// Every `X-Taste-State` sent.
    pub tags: Mutex<Vec<String>>,
}

impl Fake {
    /// Lose every history it held, like a service on a new machine.
    pub fn forget(&self) {
        self.states.lock().unwrap().clear();
    }
}

fn state_unknown() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": "state_unknown", "detail": "send it"})),
    )
        .into_response()
}

/// The `X-Taste-State` a request names, noted: `(log, n)`.
fn named(f: &Fake, headers: &HeaderMap) -> Option<(String, usize)> {
    let raw = headers.get("x-taste-state")?.to_str().ok()?.to_owned();
    f.tags.lock().unwrap().push(raw.clone());
    let (log, n) = raw.split_once('/')?;
    Some((log.to_owned(), n.parse().ok()?))
}

async fn get_state(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if !f.keeps_states.load(Ordering::SeqCst) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"}))).into_response();
    }
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    let own = f.own.lock().unwrap().clone();
    Json(json!({"version": own.len(), "events": own, "served": {"shop0.example:1003": 2}}))
        .into_response()
}

async fn put_state(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !f.keeps_states.load(Ordering::SeqCst) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"}))).into_response();
    }
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"detail": "no"}))).into_response();
    }
    let log = body["log"].as_str().unwrap_or_default().to_owned();
    let events = body["events"].as_array().cloned().unwrap_or_default();
    let version = events.len();
    f.states.lock().unwrap().insert(log, events);
    f.uploads.lock().unwrap().push(body);
    Json(json!({"ok": true, "version": version})).into_response()
}

/// The whole map: five made-up pieces of page 0 along a diagonal, the
/// first liked, the last skipped.
async fn map(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    f.maps.fetch_add(1, Ordering::SeqCst);
    let ids: Vec<String> = (0..5)
        .map(|n| format!("shop0.example:{}", 1000 + n))
        .collect();
    Json(json!({
        "ids": ids,
        "xy": (0..5).map(|n| [0.1 + 0.2 * n as f64, 0.9 - 0.2 * n as f64]).collect::<Vec<_>>(),
        "cluster": [0, 0, 1, 1, 2],
        "thumb": (0..5).map(|n| format!("https://cdn.shop0.example/files/coat-{n}.jpg")).collect::<Vec<_>>(),
        "liked": [0],
        "skipped": [4],
        "p": [0.9, 0.7, 0.5, null, 0.1],
    }))
    .into_response()
}

/// Made-up product `n` of page `page`.
pub fn product(page: usize, n: usize) -> Value {
    let brand = ["Fernhill Supply", "Okra & Wool", "Northgate"][n % 3];
    let why = ["boundary", "explore", "likely"][n % 3];
    json!({
        "id": format!("shop{page}.example:{}", 1000 + n),
        "brand": brand,
        "title": format!("Chore Coat {page}-{n}"),
        "url": format!("https://shop{page}.example/products/coat-{n}"),
        "price": 98.0 + n as f64,
        "currency": "USD",
        "category": "jacket",
        "images": [
            format!("https://cdn.shop{page}.example/files/coat-{n}-worn.jpg?v=1"),
            format!("https://cdn.shop{page}.example/files/coat-{n}.jpg?v=1"),
            format!("https://cdn.shop{page}.example/files/coat-{n}-back.jpg?v=1"),
        ],
        "worn": true,
        "description": "A boxy chore coat in heavy cotton canvas, with three patch pockets.",
        "p": 0.5,
        "liked": false,
        "skipped": false,
        "why": why,
    })
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn check(f: &Fake, headers: &HeaderMap) -> Result<(), Response> {
    if !authorized(headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "missing or wrong token"})),
        )
            .into_response());
    }
    if f.down.load(Ordering::SeqCst) {
        return Err((StatusCode::BAD_GATEWAY, Json(json!({"detail": "down"}))).into_response());
    }
    // A history named must be one it holds at exactly that version.
    if f.keeps_states.load(Ordering::SeqCst)
        && let Some((log, n)) = named(f, headers)
        && f.states.lock().unwrap().get(&log).map(Vec::len) != Some(n)
    {
        return Err(state_unknown());
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct Q {
    #[serde(default)]
    n: Option<usize>,
    #[serde(default)]
    q: Option<String>,
}

#[derive(serde::Deserialize, Debug, Clone, Copy)]
pub struct Box4 {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    #[serde(default)]
    pub n: Option<usize>,
}

/// An n × n map over the box: the top-left tile is crowded, the bottom-right
/// one empty, the rest hold a few.
async fn map_tiles(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Query(b): Query<Box4>,
) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    f.boxes.lock().unwrap().push(b);
    let n = b.n.unwrap_or(4);
    let (w, h) = ((b.x1 - b.x0) / n as f64, (b.y1 - b.y0) / n as f64);
    let mut tiles = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let count = if (i, j) == (0, 0) {
                40
            } else if (i, j) == (n - 1, n - 1) {
                0
            } else {
                3
            };
            tiles.push(json!({
                "i": i, "j": j,
                "x0": b.x0 + i as f64 * w, "y0": b.y0 + j as f64 * h,
                "x1": b.x0 + (i + 1) as f64 * w, "y1": b.y0 + (j + 1) as f64 * h,
                "count": count, "likes": usize::from(i == 0), "skips": usize::from(j == 1),
                "p": if count > 0 { json!(0.5) } else { Value::Null },
                "color": "#C98A5B",
                "item": if count > 0 { product(6, i + n * j) } else { Value::Null },
            }));
        }
    }
    Json(
        json!({"bounds": {"x0": b.x0, "y0": b.y0, "x1": b.x1, "y1": b.y1}, "n": n, "tiles": tiles}),
    )
    .into_response()
}

async fn map_items(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Query(b): Query<Box4>,
) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    f.boxes.lock().unwrap().push(b);
    Json(json!({"items": (0..b.n.unwrap_or(36).min(5)).map(|i| product(7, i)).collect::<Vec<_>>()}))
        .into_response()
}

async fn feed(State(f): State<Arc<Fake>>, headers: HeaderMap, Query(q): Query<Q>) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    let page = f.pages.fetch_add(1, Ordering::SeqCst);
    let n = q.n.unwrap_or(24).min(6);
    Json(json!({"items": (0..n).map(|i| product(page, i)).collect::<Vec<_>>()})).into_response()
}

async fn event(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"detail": "no"}))).into_response();
    }
    if f.down.load(Ordering::SeqCst) {
        return (StatusCode::BAD_GATEWAY, Json(json!({"detail": "down"}))).into_response();
    }
    // A reaction names the version it makes: taken when the service holds
    // one fewer, a repeat when it holds that many already.
    if f.keeps_states.load(Ordering::SeqCst)
        && let Some((log, n)) = named(&f, &headers)
    {
        let mut states = f.states.lock().unwrap();
        let Some(history) = states.get_mut(&log) else {
            return state_unknown();
        };
        let repeat = history.len() == n
            && history
                .last()
                .is_some_and(|l| l["id"] == body["id"] && l["kind"] == body["kind"]);
        if !repeat && history.len() + 1 != n {
            return state_unknown();
        }
        if !repeat
            && !body["id"]
                .as_str()
                .is_some_and(|id| f.gone.lock().unwrap().iter().any(|g| g == id))
        {
            history.push(json!({"id": body["id"], "kind": body["kind"], "t": 0}));
        }
    }
    if body["id"]
        .as_str()
        .is_some_and(|id| f.gone.lock().unwrap().iter().any(|g| g == id))
    {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"detail": "no such item"})),
        )
            .into_response();
    }
    let mut events = f.events.lock().unwrap();
    events.push(body);
    let likes = events.iter().filter(|e| e["kind"] == "like").count();
    let skips = events.iter().filter(|e| e["kind"] == "skip").count();
    Json(json!({"ok": true, "likes": likes, "skips": skips})).into_response()
}

async fn item(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Path(rest): Path<String>,
    Query(q): Query<Q>,
) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    let (id, list) = match rest.rsplit_once('/') {
        Some((id, l)) if ["similar", "contrast", "steer"].contains(&l) => (id.to_owned(), Some(l)),
        _ => (rest.clone(), None),
    };
    if !id.starts_with("shop") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "no such item"})),
        )
            .into_response();
    }
    match list {
        None => {
            let mut p = product(0, 0);
            p["id"] = json!(id);
            Json(p).into_response()
        }
        Some(l) => {
            if let Some(words) = q.q {
                f.queries.lock().unwrap().push(words);
            }
            let base = match l {
                "similar" => 7,
                "contrast" => 8,
                _ => 9,
            };
            Json(json!({"items": (0..q.n.unwrap_or(3).min(3)).map(|i| product(base, i)).collect::<Vec<_>>()}))
                .into_response()
        }
    }
}

async fn search(State(f): State<Arc<Fake>>, headers: HeaderMap, Query(q): Query<Q>) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    f.queries.lock().unwrap().push(q.q.unwrap_or_default());
    Json(json!({"items": [product(5, 0), product(5, 1)]})).into_response()
}

async fn likes(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    let mut p = product(0, 1);
    p["liked"] = json!(true);
    Json(json!({"items": [p]})).into_response()
}

async fn taste(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    let summary = (f.summaries.load(Ordering::SeqCst) > 0)
        .then(|| json!({"text": "You like warm, textured workwear.", "likes": 2}));
    Json(json!({
        "likes": 2, "skips": 1,
        "tastes": [{"size": 2, "attributes": ["rugged workwear", "earth tones", "cotton canvas"],
                    "brands": ["Fernhill Supply"], "categories": ["jacket"],
                    "items": [product(0, 1), product(0, 2)]}],
        "summary": summary,
    }))
    .into_response()
}

/// One taste to browse: its two likes, then the nearest pieces; only taste 0.
async fn one_taste(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Path(index): Path<String>,
) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    if index != "0" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "no such taste"})),
        )
            .into_response();
    }
    let mut likes = vec![product(0, 1), product(0, 2)];
    for l in &mut likes {
        l["liked"] = json!(true);
    }
    Json(json!({
        "index": 0, "size": 2, "attributes": ["rugged workwear", "earth tones"],
        "brands": ["Fernhill Supply"], "categories": ["jacket"],
        "items": likes, "more": [product(6, 0), product(6, 1), product(6, 2)],
    }))
    .into_response()
}

async fn summary(State(f): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check(&f, &headers) {
        return r;
    }
    f.summaries.fetch_add(1, Ordering::SeqCst);
    Json(json!({"text": "You like warm, textured workwear.", "likes": 2})).into_response()
}

/// Serve `fake`; its base URL.
pub async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new()
        .route("/api/feed", get(feed))
        .route("/api/event", post(event))
        .route("/api/item/{*rest}", get(item))
        .route("/api/search", get(search))
        .route("/api/likes", get(likes))
        .route("/api/taste", get(taste))
        .route("/api/taste/summary", post(summary))
        .route("/api/taste/{index}", get(one_taste))
        .route("/api/map", get(map))
        .route("/api/map/tiles", get(map_tiles))
        .route("/api/map/items", get(map_items))
        .route("/api/state", get(get_state).put(put_state))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

pub fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(TasteAdapter::new(true)),
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

/// A layout that is the whole Taste core layout; its id.
pub async fn taste_layout(pod: &Pod) -> String {
    let app = tool(
        pod,
        "create_layout",
        json!({"title": "Taste", "feeds": [["taste", "feed"]],
            "layout": {"type": "core://Use", "ref": "@core/taste"}}),
    )
    .await;
    app["id"].as_str().unwrap().to_owned()
}

/// The data of layout `app` (its feeds merged).
pub async fn workspace(pod: &Pod, app: &str) -> Value {
    let (status, ws) =
        pod_core::testing::layout_for(&pod_core::app(pod.clone()), app, Some("phone")).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    ws
}

/// A fake service and a pod signed in to it.
pub async fn signed_in() -> (Pod, Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let url = serve(fake.clone()).await;
    let pod = pod();
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (pod, fake, url)
}
