use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use protocol::{UiNode, WorkspaceSpec};
use serde_json::Value;
use tower::ServiceExt;

async fn send(req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = server::app().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn get(uri: &str) -> (StatusCode, Vec<u8>) {
    send(Request::get(uri).body(Body::empty()).unwrap()).await
}

fn collect_button_labels(node: &UiNode, out: &mut Vec<String>) {
    match node {
        UiNode::Workspace { slots, .. } => {
            for s in slots {
                collect_button_labels(&s.content, out);
            }
        }
        UiNode::Container { children, .. } => {
            for c in children {
                collect_button_labels(c, out);
            }
        }
        UiNode::Button { label, .. } => out.push(label.clone()),
        _ => {}
    }
}

#[test]
fn fixtures_parse() {
    server::validate_fixtures();
}

#[tokio::test]
async fn healthz() {
    let (status, body) = get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"ok");
}

#[tokio::test]
async fn home_workspace_is_counter_bundle() {
    let (status, body) = get("/v1/workspaces/home").await;
    assert_eq!(status, StatusCode::OK);
    let spec: WorkspaceSpec = serde_json::from_slice(&body).unwrap();
    assert!(matches!(&spec.workspace, UiNode::Workspace { id, .. } if id == "home"));
    let mut labels = Vec::new();
    collect_button_labels(&spec.workspace, &mut labels);
    assert!(
        labels.iter().any(|l| l == "Increment"),
        "labels: {labels:?}"
    );
    assert!(labels.iter().any(|l| l == "Reset"), "labels: {labels:?}");
    assert_eq!(
        spec.entities["core://counters/home"].get_i64("count"),
        Some(0)
    );
}

#[tokio::test]
async fn pixelfed_workspace_parses() {
    let (status, body) = get("/v1/workspaces/pixelfed_demo").await;
    assert_eq!(status, StatusCode::OK);
    let spec: WorkspaceSpec = serde_json::from_slice(&body).unwrap();
    assert!(matches!(spec.workspace, UiNode::Workspace { .. }));
    assert!(!spec.entities.is_empty());
}

#[tokio::test]
async fn unknown_workspace_is_404_json() {
    let (status, body) = get("/v1/workspaces/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "workspace_not_found");
}

#[tokio::test]
async fn counter_mutations_return_ok() {
    for op in ["increment", "reset"] {
        let req = Request::post(format!("/v1/counters/home/{op}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"anything":1}"#))
            .unwrap();
        let (status, body) = send(req).await;
        assert_eq!(status, StatusCode::OK, "op {op}");
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }
    // Empty body also accepted.
    let (status, _) = send(
        Request::post("/v1/counters/home/increment")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        Request::post("/v1/counters/home/explode")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn post(uri: &str, content_type: Option<&str>, body: &'static str) -> Request<Body> {
    let mut b = Request::post(uri);
    if let Some(ct) = content_type {
        b = b.header("content-type", ct);
    }
    b.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn counter_mutation_body_edge_cases() {
    const URI: &str = "/v1/counters/home/increment";
    // Empty body with JSON content-type is accepted (clients often always set it).
    let (status, _) = send(post(URI, Some("application/json"), "")).await;
    assert_eq!(status, StatusCode::OK);
    // Content-type parameters are tolerated.
    let (status, _) = send(post(URI, Some("application/json; charset=utf-8"), "{}")).await;
    assert_eq!(status, StatusCode::OK);

    // Invalid JSON -> 400 with a JSON error body.
    let (status, body) = send(post(URI, Some("application/json"), "{bad")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "invalid_json");

    // Non-empty body without a JSON content-type -> 415 JSON error.
    for ct in [None, Some("text/plain")] {
        let (status, body) = send(post(URI, ct, r#"{"a":1}"#)).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "ct {ct:?}");
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "expected_application_json");
    }

    // Unknown counter id is 404 even with a malformed body (path checked first).
    let (status, body) = send(post(
        "/v1/counters/other/increment",
        Some("application/json"),
        "{bad",
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "unknown_counter_endpoint");
}

#[tokio::test]
async fn wrong_method_is_405() {
    let (status, _) = send(
        Request::post("/v1/workspaces/home")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = get("/v1/counters/home/increment").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn workspace_response_is_json_content_type() {
    let resp = server::app()
        .oneshot(
            Request::get("/v1/workspaces/home")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.headers()["content-type"], "application/json");
}

#[tokio::test]
async fn real_tcp_listener() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, server::app()).await });

    let spec: WorkspaceSpec = reqwest::get(format!("http://{addr}/v1/workspaces/home"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(spec.workspace, UiNode::Workspace { .. }));
    handle.abort();
}
