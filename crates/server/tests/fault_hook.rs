//! The fault-injection test hook (`POST /v1/test/fault`, `CONSTRUCT_TEST_HOOKS=1`)
//! and JSON body parsing, exercised through `POST /v1/demo/ack`.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use server::AppState;
use tower::ServiceExt;

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

fn post(uri: &str, content_type: Option<&str>, body: impl Into<Body>) -> Request<Body> {
    let mut b = Request::post(uri);
    if let Some(ct) = content_type {
        b = b.header("content-type", ct);
    }
    b.body(body.into()).unwrap()
}

async fn ack(app: &Router) -> (StatusCode, Value) {
    send(app, post("/v1/demo/ack", Some("application/json"), "{}")).await
}

async fn set_fault(app: &Router, body: Value) -> (StatusCode, Value) {
    send(
        app,
        post("/v1/test/fault", Some("application/json"), body.to_string()),
    )
    .await
}

fn hooked() -> Router {
    server::app(Arc::new(AppState::in_memory().with_test_hooks(true)))
}

#[tokio::test]
async fn fault_hook_disabled_is_404() {
    let app = server::app_in_memory();
    let (status, _) = set_fault(&app, json!({"status": 503, "count": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // And mutations are unaffected.
    assert_eq!(ack(&app).await.0, StatusCode::OK);
}

#[tokio::test]
async fn fault_hook_fails_next_n_mutations() {
    let app = hooked();
    assert_eq!(ack(&app).await, (StatusCode::OK, json!({"ok": true})));

    let (status, v) = set_fault(&app, json!({"status": 503, "count": 3})).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    // GETs are not mutations and do not consume the fault.
    let (status, _) = send(
        &app,
        Request::get("/v1/workspaces/pixelfed_demo")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for i in 0..3 {
        let (status, v) = ack(&app).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "attempt {i}");
        assert_eq!(v["error"], "injected_fault");
    }
    // Fault exhausted: back to normal.
    assert_eq!(ack(&app).await.0, StatusCode::OK);

    // A 4xx fault works too, and count 0 clears a pending fault.
    set_fault(&app, json!({"status": 400, "count": 5})).await;
    assert_eq!(ack(&app).await.0, StatusCode::BAD_REQUEST);
    set_fault(&app, json!({"status": 400, "count": 0})).await;
    assert_eq!(ack(&app).await.0, StatusCode::OK);

    // Invalid fault specs are rejected.
    for bad in [
        json!({"status": 200, "count": 1}),
        json!({"status": 503}),
        json!({}),
    ] {
        assert_eq!(set_fault(&app, bad).await.0, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn json_body_edge_cases() {
    // The fault hook parses its body like every JSON endpoint of the pod.
    let app = hooked();
    const URI: &str = "/v1/test/fault";
    // Empty body with a JSON content-type reads as null (clients often always
    // set it): it gets past parsing to the fault validation.
    let (status, v) = send(&app, post(URI, Some("application/json"), "")).await;
    assert_eq!(
        (status, &v["error"]),
        (StatusCode::BAD_REQUEST, &json!("invalid_fault"))
    );
    // Content-type parameters are tolerated.
    let (status, _) = send(
        &app,
        post(
            URI,
            Some("application/json; charset=utf-8"),
            r#"{"status":503,"count":0}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Invalid JSON -> 400 with a JSON error body.
    let (status, v) = send(&app, post(URI, Some("application/json"), "{bad")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "invalid_json");

    // Non-empty body without a JSON content-type -> 415 JSON error.
    for ct in [None, Some("text/plain")] {
        let (status, v) = send(&app, post(URI, ct, r#"{"status":503,"count":0}"#)).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "ct {ct:?}");
        assert_eq!(v["error"], "expected_application_json");
    }
}
