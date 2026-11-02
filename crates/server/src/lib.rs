//! Project Canvas backend: serves fixture hydration bundles and persists
//! counter mutations in SQLite (M5, see [`store`]).
//!
//! Fixtures are embedded at compile time and parsed into
//! [`protocol::WorkspaceSpec`] once, so an invalid fixture fails loudly
//! (panics on first access, which the integration tests exercise).

pub mod store;

use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use protocol::WorkspaceSpec;
use serde_json::{Value, json};
use store::Store;

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

/// The counter entity injected into the `home` bundle.
const HOME_COUNTER_URI: &str = "core://counters/home";

/// A pending injected fault (test hook): the next `remaining` mutation
/// requests get `status` without being applied.
#[derive(Debug, Clone, Copy)]
struct Fault {
    status: StatusCode,
    remaining: u32,
}

/// Shared server state.
pub struct AppState {
    store: Store,
    test_hooks: bool,
    fault: Mutex<Option<Fault>>,
}

impl AppState {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            test_hooks: false,
            fault: Mutex::new(None),
        }
    }

    /// State backed by `<dir>/server.db` (the directory is created).
    pub fn open_dir(dir: &FsPath) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(dir)?;
        Ok(Self::new(Store::open(&dir.join("server.db"))?))
    }

    /// Private in-memory database (tests).
    pub fn in_memory() -> Self {
        Self::new(Store::open_in_memory().expect("in-memory SQLite"))
    }

    /// Enable `POST /v1/test/fault` (`CANVAS_TEST_HOOKS=1`).
    pub fn with_test_hooks(mut self, enabled: bool) -> Self {
        self.test_hooks = enabled;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Consume one injected fault, if any is pending.
    fn take_fault(&self) -> Option<StatusCode> {
        let mut guard = self.fault.lock().unwrap_or_else(|e| e.into_inner());
        let fault = guard.as_mut()?;
        let status = fault.status;
        fault.remaining -= 1;
        if fault.remaining == 0 {
            *guard = None;
        }
        Some(status)
    }
}

/// Build the HTTP router.
pub fn app(state: Arc<AppState>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/workspaces/{id}", get(get_workspace))
        .route("/v1/counters/{id}/{op}", post(counter_mutation));
    if state.test_hooks {
        router = router.route("/v1/test/fault", post(set_fault));
    }
    router
        .route_layer(middleware::from_fn_with_state(state.clone(), fault_layer))
        .with_state(state)
}

/// Convenience for tests: router over a fresh in-memory store, hooks off.
pub fn app_in_memory() -> Router {
    app(Arc::new(AppState::in_memory()))
}

/// Run a blocking store operation off the async workers.
async fn blocking<T: Send + 'static>(
    state: &Arc<AppState>,
    f: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let state = state.clone();
    match tokio::task::spawn_blocking(move || f(&state.store)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "store error");
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "store_error", "detail": e.to_string() }),
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "store task failed");
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "store_error" }),
            ))
        }
    }
}

async fn get_workspace(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(spec) = workspace(&id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "workspace_not_found", "id": id }),
        );
    };
    if id != "home" {
        return Json(spec).into_response();
    }
    let count = match blocking(&state, |s| s.counter("home")).await {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    let mut spec = spec.clone();
    spec.entities
        .entry(HOME_COUNTER_URI.to_owned())
        .or_default()
        .set("count", count);
    Json(spec).into_response()
}

/// A JSON error response, kept small so it can travel in `Result::Err`.
type ApiError = (StatusCode, Json<Value>);

fn api_error(status: StatusCode, body: Value) -> ApiError {
    (status, Json(body))
}

fn json_error(status: StatusCode, body: Value) -> Response {
    api_error(status, body).into_response()
}

/// Test hook: while a fault is pending, every POST except `/v1/test/*`
/// returns the injected status without reaching its handler.
async fn fault_layer(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if req.method() == Method::POST
        && !req.uri().path().starts_with("/v1/test/")
        && let Some(status) = state.take_fault()
    {
        tracing::info!(
            path = %req.uri().path(),
            status = status.as_u16(),
            "injected fault: request not applied"
        );
        return json_error(
            status,
            json!({ "error": "injected_fault", "status": status.as_u16() }),
        );
    }
    next.run(req).await
}

/// `POST /v1/test/fault {"status":503,"count":3}` (only with test hooks on).
/// `count` 0 clears any pending fault.
async fn set_fault(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let status = payload["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .filter(|s| s.as_u16() >= 400);
    let count = payload["count"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok());
    let (Some(status), Some(count)) = (status, count) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_fault", "expected": {"status": "400..=599", "count": "u32"} }),
        );
    };
    let fault = (count > 0).then_some(Fault {
        status,
        remaining: count,
    });
    *state.fault.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    tracing::info!(
        status = status.as_u16(),
        count,
        "fault injection armed for the next mutations"
    );
    Json(json!({ "ok": true, "status": status.as_u16(), "count": count })).into_response()
}

/// An empty body is treated as `null` even when `Content-Type:
/// application/json` is set (common for HTTP clients that add the header
/// unconditionally). A non-empty body must be JSON-typed and valid; failures
/// are reported as JSON errors like the other endpoints.
fn parse_json_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
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
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({ "error": "expected_application_json" }),
        ));
    }
    serde_json::from_slice::<Value>(body).map_err(|e| {
        api_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": e.to_string() }),
        )
    })
}

/// Accepts an optional JSON body (ignored beyond validation). The path is
/// validated before the body so an unknown endpoint is always 404.
async fn counter_mutation(
    State(state): State<Arc<AppState>>,
    Path((id, op)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if id != "home" || !matches!(op.as_str(), "increment" | "decrement" | "reset") {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "unknown_counter_endpoint", "id": id, "op": op }),
        );
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let key = id.clone();
    let result = match op.as_str() {
        "increment" => blocking(&state, move |s| s.increment(&key).map(Ok)).await,
        "decrement" => blocking(&state, move |s| s.decrement(&key)).await,
        _ => blocking(&state, move |s| s.reset(&key).map(Ok)).await,
    };
    match result {
        Ok(Ok(count)) => {
            tracing::info!(counter = %id, %op, %payload, count, "counter mutation applied");
            Json(json!({ "ok": true, "count": count })).into_response()
        }
        Ok(Err(store::BelowZero { count })) => {
            tracing::info!(counter = %id, %op, count, "counter mutation rejected: count_below_zero");
            json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({ "error": "count_below_zero", "count": count }),
            )
        }
        Err(e) => e.into_response(),
    }
}
