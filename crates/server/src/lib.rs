//! Project Canvas backend (Milestone 2): serves fixture hydration bundles
//! and accepts counter mutations.
//!
//! Fixtures are embedded at compile time and parsed into
//! [`protocol::WorkspaceSpec`] once, so an invalid fixture fails loudly
//! (panics on first access, which the integration tests exercise).

use std::sync::LazyLock;

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use protocol::WorkspaceSpec;
use serde_json::{Value, json};

static COUNTER_HOME_JSON: &str = include_str!("../../../fixtures/counter_home_bundle.json");
static PIXELFED_HOME_JSON: &str = include_str!("../../../fixtures/pixelfed_home_bundle.json");

static COUNTER_HOME: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(COUNTER_HOME_JSON).expect("fixtures/counter_home_bundle.json is invalid")
});
static PIXELFED_HOME: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(PIXELFED_HOME_JSON).expect("fixtures/pixelfed_home_bundle.json is invalid")
});

/// Look up a workspace bundle by id.
pub fn workspace(id: &str) -> Option<&'static WorkspaceSpec> {
    match id {
        "home" => Some(&COUNTER_HOME),
        "pixelfed_demo" => Some(&PIXELFED_HOME),
        _ => None,
    }
}

/// Force-parse all embedded fixtures. Call at startup to fail fast.
pub fn validate_fixtures() {
    LazyLock::force(&COUNTER_HOME);
    LazyLock::force(&PIXELFED_HOME);
}

/// Build the HTTP router.
pub fn app() -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/workspaces/{id}", get(get_workspace))
        .route("/v1/counters/{id}/{op}", post(counter_mutation))
}

async fn get_workspace(Path(id): Path<String>) -> Response {
    match workspace(&id) {
        Some(spec) => Json(spec).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "workspace_not_found", "id": id })),
        )
            .into_response(),
    }
}

async fn counter_mutation(
    Path((id, op)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    if id != "home" || !matches!(op.as_str(), "increment" | "reset") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown_counter_endpoint", "id": id, "op": op })),
        )
            .into_response();
    }
    let payload = body.map(|Json(v)| v).unwrap_or(Value::Null);
    tracing::info!(counter = %id, %op, %payload, "counter mutation");
    Json(json!({ "ok": true })).into_response()
}
