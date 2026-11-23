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
}

/// Made-up product `n` of page `page`.
pub fn product(page: usize, n: usize) -> Value {
    let brand = ["Fernhill Supply", "Okra & Wool", "Northgate"][n % 3];
    let why = ["near:0", "adjacent", "explore"][n % 3];
    json!({
        "id": format!("shop{page}.example:{}", 1000 + n),
        "brand": brand,
        "title": format!("Chore Coat {page}-{n}"),
        "url": format!("https://shop{page}.example/products/coat-{n}"),
        "price": 98.0 + n as f64,
        "currency": "USD",
        "category": "jacket",
        "images": [format!("https://cdn.shop{page}.example/files/coat-{n}.jpg?v=1")],
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
    Ok(())
}

#[derive(serde::Deserialize)]
struct Q {
    #[serde(default)]
    n: Option<usize>,
    #[serde(default)]
    q: Option<String>,
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
    if let Err(r) = check(&f, &headers) {
        return r;
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
