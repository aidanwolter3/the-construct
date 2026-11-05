use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use protocol::{UiNode, WorkspaceSpec};
use serde_json::Value;
use tower::ServiceExt;

async fn send(req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = server::app_in_memory().oneshot(req).await.unwrap();
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
async fn pixelfed_workspace_parses() {
    let (status, body) = get("/v1/workspaces/pixelfed_sample_no_login").await;
    assert_eq!(status, StatusCode::OK);
    let spec: WorkspaceSpec = serde_json::from_slice(&body).unwrap();
    assert!(matches!(spec.workspace, UiNode::Workspace { .. }));
    assert!(!spec.entities.is_empty());
}

#[tokio::test]
async fn removed_counter_app_is_gone() {
    // `home` is now every pod's home app (decision `blueprints-and-home`),
    // not the counter that once had the id.
    let (status, body) = get("/v1/workspaces/home").await;
    assert_eq!(status, StatusCode::OK);
    let spec: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        spec["workspace"]["slots"][0]["content"]["type"],
        "core://Dock"
    );
    let (status, _) = send(
        Request::post("/v1/counters/home/increment")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_workspace_is_404_json() {
    let (status, body) = get("/v1/workspaces/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "workspace_not_found");
}

#[tokio::test]
async fn wrong_method_is_405() {
    let (status, _) = send(
        Request::post("/v1/workspaces/pixelfed_sample_no_login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = get("/v1/pixelfed_sample_no_login/ack").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn workspace_response_is_json_content_type() {
    let resp = server::app_in_memory()
        .oneshot(
            Request::get("/v1/workspaces/pixelfed_sample_no_login")
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
    let handle = tokio::spawn(async move { axum::serve(listener, server::app_in_memory()).await });

    let spec: WorkspaceSpec = reqwest::get(format!(
        "http://{addr}/v1/workspaces/pixelfed_sample_no_login"
    ))
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
