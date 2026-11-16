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

/// Home as a device draws it (`/v1/construct/names/home`, its objects and
/// its data).
#[tokio::test]
async fn home_is_served_by_the_construct() {
    let (status, body) =
        pod_core::testing::layout_for(&server::app_in_memory(), "home", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    assert!(
        matches!(spec.workspace, UiNode::Page { .. }),
        "{:?}",
        spec.workspace
    );
    assert!(spec.entities.contains_key("core://theme"));
}

#[tokio::test]
async fn removed_counter_app_is_gone() {
    let (status, _) = send(
        Request::post("/v1/counters/home/increment")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_name_is_404_json() {
    let (status, body) = get("/v1/construct/names/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].is_string(), "{v}");
}

#[tokio::test]
async fn wrong_method_is_405() {
    let (status, _) = send(Request::post("/v1/construct").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = get("/v1/pixelfed_sample_no_login/ack").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn construct_response_is_json_content_type() {
    let resp = server::app_in_memory()
        .oneshot(Request::get("/v1/construct").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.headers()["content-type"], "application/json");
}

#[tokio::test]
async fn real_tcp_listener() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, server::app_in_memory()).await });

    let root: Value = reqwest::get(format!("http://{addr}/v1/construct"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let root = root["root"].as_str().unwrap();
    let objects: Value = reqwest::Client::new()
        .post(format!("http://{addr}/v1/objects"))
        .json(&serde_json::json!({ "have": [], "want": [root] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(objects["objects"].get(root).is_some(), "{objects}");
    handle.abort();
}
