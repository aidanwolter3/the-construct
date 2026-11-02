//! M5: persistent counters and the fault-injection test hook.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use protocol::WorkspaceSpec;
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

async fn op(app: &Router, op: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::post(format!("/v1/counters/home/{op}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await
}

async fn home_count(app: &Router) -> i64 {
    let (status, v) = send(
        app,
        Request::get("/v1/workspaces/home")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let spec: WorkspaceSpec = serde_json::from_value(v).unwrap();
    spec.entities["core://counters/home"]
        .get_i64("count")
        .unwrap()
}

async fn set_fault(app: &Router, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::post("/v1/test/fault")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn increment_decrement_reset() {
    let app = server::app_in_memory();
    assert_eq!(home_count(&app).await, 0);

    assert_eq!(
        op(&app, "increment").await,
        (StatusCode::OK, json!({"ok": true, "count": 1}))
    );
    assert_eq!(
        op(&app, "increment").await,
        (StatusCode::OK, json!({"ok": true, "count": 2}))
    );
    assert_eq!(
        op(&app, "increment").await,
        (StatusCode::OK, json!({"ok": true, "count": 3}))
    );
    assert_eq!(home_count(&app).await, 3);

    assert_eq!(
        op(&app, "decrement").await,
        (StatusCode::OK, json!({"ok": true, "count": 2}))
    );
    assert_eq!(home_count(&app).await, 2);

    assert_eq!(
        op(&app, "reset").await,
        (StatusCode::OK, json!({"ok": true, "count": 0}))
    );
    assert_eq!(home_count(&app).await, 0);
}

#[tokio::test]
async fn decrement_at_zero_is_422_and_unchanged() {
    let app = server::app_in_memory();
    assert_eq!(
        op(&app, "decrement").await,
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "count_below_zero", "count": 0})
        )
    );
    assert_eq!(home_count(&app).await, 0);

    op(&app, "increment").await;
    assert_eq!(op(&app, "decrement").await.0, StatusCode::OK);
    assert_eq!(
        op(&app, "decrement").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(home_count(&app).await, 0);
}

#[tokio::test]
async fn count_persists_across_app_instances() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("nested/data"); // created on open
    {
        let app = server::app(Arc::new(AppState::open_dir(&data_dir).unwrap()));
        op(&app, "increment").await;
        op(&app, "increment").await;
        assert_eq!(home_count(&app).await, 2);
    }
    assert!(data_dir.join("server.db").exists());
    let app = server::app(Arc::new(AppState::open_dir(&data_dir).unwrap()));
    assert_eq!(home_count(&app).await, 2);
    assert_eq!(op(&app, "decrement").await.1["count"], 1);

    // Two live instances over one file see each other's writes.
    let other = server::app(Arc::new(AppState::open_dir(&data_dir).unwrap()));
    assert_eq!(home_count(&other).await, 1);
}

#[tokio::test]
async fn fault_hook_disabled_is_404() {
    let app = server::app_in_memory();
    let (status, _) = set_fault(&app, json!({"status": 503, "count": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // And mutations are unaffected.
    assert_eq!(op(&app, "increment").await.0, StatusCode::OK);
}

#[tokio::test]
async fn fault_hook_fails_next_n_mutations_without_applying() {
    let app = server::app(Arc::new(AppState::in_memory().with_test_hooks(true)));
    op(&app, "increment").await; // count 1

    let (status, v) = set_fault(&app, json!({"status": 503, "count": 3})).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    // GETs are not mutations and do not consume the fault.
    assert_eq!(home_count(&app).await, 1);
    for name in ["increment", "reset", "decrement"] {
        let (status, v) = op(&app, name).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{name}");
        assert_eq!(v["error"], "injected_fault");
        assert_eq!(home_count(&app).await, 1, "{name} must not apply");
    }
    // Fault exhausted: back to normal.
    assert_eq!(
        op(&app, "increment").await,
        (StatusCode::OK, json!({"ok": true, "count": 2}))
    );

    // A 4xx fault works too, and count 0 clears a pending fault.
    set_fault(&app, json!({"status": 400, "count": 5})).await;
    assert_eq!(op(&app, "increment").await.0, StatusCode::BAD_REQUEST);
    set_fault(&app, json!({"status": 400, "count": 0})).await;
    assert_eq!(op(&app, "increment").await.1["count"], 3);

    // Invalid fault specs are rejected.
    for bad in [
        json!({"status": 200, "count": 1}),
        json!({"status": 503}),
        json!({}),
    ] {
        assert_eq!(set_fault(&app, bad).await.0, StatusCode::BAD_REQUEST);
    }
}
