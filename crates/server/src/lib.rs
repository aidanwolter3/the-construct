//! Project Canvas backend (Milestone 2): serves fixture hydration bundles
//! and accepts counter mutations.
//!
//! Fixtures are embedded at compile time and parsed into
//! [`protocol::WorkspaceSpec`] once, so an invalid fixture fails loudly
//! (panics on first access, which the integration tests exercise).

use std::sync::LazyLock;

use axum::{
    Json, Router,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode, header},
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
        None => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "workspace_not_found", "id": id }),
        ),
    }
}

fn json_error(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// Accepts an optional JSON body. The path is validated before the body so an
/// unknown endpoint is always 404. An empty body is treated as `null` even
/// when `Content-Type: application/json` is set (common for HTTP clients that
/// add the header unconditionally). A non-empty body must be JSON-typed and
/// valid; failures are reported as JSON errors like the other endpoints.
async fn counter_mutation(
    Path((id, op)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if id != "home" || !matches!(op.as_str(), "increment" | "reset") {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "unknown_counter_endpoint", "id": id, "op": op }),
        );
    }
    let payload = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        let is_json = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|mime| {
                let mime = mime.trim();
                mime.eq_ignore_ascii_case("application/json")
                    || (mime.starts_with("application/") && mime.ends_with("+json"))
            });
        if !is_json {
            return json_error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                json!({ "error": "expected_application_json" }),
            );
        }
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) => v,
            Err(e) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "invalid_json", "detail": e.to_string() }),
                );
            }
        }
    };
    tracing::info!(counter = %id, %op, %payload, "counter mutation");
    Json(json!({ "ok": true })).into_response()
}
