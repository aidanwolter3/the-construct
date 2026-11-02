//! Versioned workspace layouts and the layout push channel (M7, D22).
//!
//! Only the workspace root [`UiNode`] is versioned; entities still come from
//! fixtures / the upstream. History is append-only: version 1 is the built-in
//! layout (inserted lazily on first use), and every change (agent edit,
//! rollback) appends `max + 1`. Committing does not validate anything; the
//! caller (agent endpoint) validates before it calls [`commit`].

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use protocol::UiNode;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::pixelfed::routes::{bearer, session_hash, unauthorized};
use crate::store::{LayoutMeta, LayoutVersionInfo, Store};
use crate::{ApiError, AppState, blocking, json_error, parse_json_body};

/// Response header carrying the served layout version.
pub const VERSION_HEADER: &str = "x-canvas-layout-version";
/// Default SSE keepalive interval.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Capacity of the layout broadcast channel (a lagging subscriber is caught
/// up with the current version, so losing intermediate events is harmless).
pub const CHANNEL_CAPACITY: usize = 64;
/// The `provider` recorded on versions created by `POST .../rollback`.
pub const ROLLBACK_PROVIDER: &str = "rollback";

/// Workspaces whose layout is versioned.
pub const VERSIONED: [&str; 3] = [
    "home",
    "pixelfed_demo",
    crate::pixelfed::layout::WORKSPACE_ID,
];

pub fn is_versioned(workspace_id: &str) -> bool {
    VERSIONED.contains(&workspace_id)
}

/// A new layout version was committed (what the SSE channel carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEvent {
    pub workspace_id: String,
    pub version: i64,
}

/// The built-in (version 1) layout of a versioned workspace.
pub fn builtin(workspace_id: &str) -> Option<UiNode> {
    match workspace_id {
        "home" | "pixelfed_demo" => crate::workspace(workspace_id).map(|s| s.workspace.clone()),
        crate::pixelfed::layout::WORKSPACE_ID => {
            Some(crate::pixelfed::layout::home_feed(Default::default(), Vec::new()).workspace)
        }
        _ => None,
    }
}

fn to_json(node: &UiNode) -> rusqlite::Result<String> {
    serde_json::to_string(node).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn from_json(json: &str) -> rusqlite::Result<UiNode> {
    serde_json::from_str(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Insert version 1 if the workspace has no versions yet. `Ok(false)` if the
/// workspace is not versioned.
fn ensure_seeded(store: &Store, workspace_id: &str) -> rusqlite::Result<bool> {
    let Some(node) = builtin(workspace_id) else {
        return Ok(false);
    };
    if store.seed_layout(workspace_id, &to_json(&node)?)? {
        tracing::info!(
            workspace = workspace_id,
            "layout v1 seeded from the built-in layout"
        );
    }
    Ok(true)
}

/// The newest `(version, layout)` of a versioned workspace (seeding v1 on
/// first use); `None` if the workspace is not versioned. Blocking.
pub fn latest(store: &Store, workspace_id: &str) -> rusqlite::Result<Option<(i64, UiNode)>> {
    if !ensure_seeded(store, workspace_id)? {
        return Ok(None);
    }
    match store.latest_layout(workspace_id)? {
        Some((v, json)) => Ok(Some((v, from_json(&json)?))),
        None => Ok(None),
    }
}

/// Append `layout` as the next version and return its number. Does not
/// validate and does not broadcast (see [`commit_and_publish`]). The caller
/// must pass a versioned workspace (v1 is seeded first so history starts
/// with the built-in layout). Blocking.
pub fn commit(
    store: &Store,
    workspace_id: &str,
    layout: &UiNode,
    prompt: Option<&str>,
    provider: Option<&str>,
    patch: Option<&Value>,
) -> rusqlite::Result<i64> {
    ensure_seeded(store, workspace_id)?;
    let meta = LayoutMeta {
        prompt: prompt.map(str::to_owned),
        provider: provider.map(str::to_owned),
        patch_json: patch.map(Value::to_string),
    };
    store.append_layout(workspace_id, &to_json(layout)?, &meta)
}

/// [`commit`] off the async workers, then broadcast the new version.
pub async fn commit_and_publish(
    state: &Arc<AppState>,
    workspace_id: &str,
    layout: UiNode,
    prompt: Option<String>,
    provider: Option<String>,
    patch: Option<Value>,
) -> Result<i64, ApiError> {
    let ws = workspace_id.to_owned();
    let version = blocking(state, move |s| {
        commit(
            s,
            &ws,
            &layout,
            prompt.as_deref(),
            provider.as_deref(),
            patch.as_ref(),
        )
    })
    .await?;
    state.publish_layout(workspace_id, version);
    Ok(version)
}

/// Async [`latest`] for handlers.
pub async fn latest_async(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Result<Option<(i64, UiNode)>, ApiError> {
    let ws = workspace_id.to_owned();
    blocking(state, move |s| latest(s, &ws)).await
}

/// `X-Canvas-Layout-Version: <version>`.
pub fn version_header(version: i64) -> (axum::http::HeaderName, HeaderValue) {
    (
        axum::http::HeaderName::from_static(VERSION_HEADER),
        HeaderValue::from(version),
    )
}

// ---------- routes ----------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/workspaces/{id}/versions", get(versions))
        .route("/v1/workspaces/{id}/rollback", post(rollback))
        .route("/v1/workspaces/{id}/events", get(events))
}

/// Auth for layout-changing / history endpoints (versions, rollback, agent):
/// a valid pod session (bearer), unless `CANVAS_AGENT_OPEN=1`. Returns the
/// rejection to send (401, or 500 on a store error), `None` when allowed.
pub async fn require_agent_auth(state: &Arc<AppState>, headers: &HeaderMap) -> Option<Response> {
    if state.agent_open() {
        return None;
    }
    match valid_session(state, headers).await {
        Ok(Some(true)) => None,
        Ok(_) => Some(unauthorized()),
        Err(e) => Some(e.into_response()),
    }
}

/// `None`: no bearer; `Some(valid)` otherwise.
async fn valid_session(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<Option<bool>, ApiError> {
    let Some(token) = bearer(headers) else {
        return Ok(None);
    };
    let hash = session_hash(token);
    let ttl = state.pixelfed_config().session_ttl_secs;
    blocking(state, move |s| s.session_valid(&hash, ttl))
        .await
        .map(Some)
}

fn not_versioned(id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "workspace_not_found", "id": id,
                "versioned": VERSIONED }),
    )
}

/// `GET /v1/workspaces/:id/versions` -> `[{version, created_at, prompt, provider}]`, newest first.
async fn versions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_versioned(&id) {
        return not_versioned(&id);
    }
    if let Some(r) = require_agent_auth(&state, &headers).await {
        return r;
    }
    let ws = id.clone();
    let list: Result<Vec<LayoutVersionInfo>, ApiError> = blocking(&state, move |s| {
        ensure_seeded(s, &ws)?;
        s.layout_versions(&ws)
    })
    .await;
    match list {
        Ok(list) => Json(list).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/workspaces/:id/rollback {"version": N}`: appends a copy of
/// version N (append-only), broadcasts it, and answers
/// `{version, rolled_back_to, workspace}`. Unknown N -> 404.
async fn rollback(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_versioned(&id) {
        return not_versioned(&id);
    }
    if let Some(r) = require_agent_auth(&state, &headers).await {
        return r;
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let Some(target) = payload["version"].as_i64().filter(|v| *v >= 1) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "integer >= 1" } }),
        );
    };
    let ws = id.clone();
    let result = blocking(&state, move |s| {
        ensure_seeded(s, &ws)?;
        let meta = LayoutMeta {
            prompt: Some(format!("rollback to v{target}")),
            provider: Some(ROLLBACK_PROVIDER.to_owned()),
            patch_json: None,
        };
        match s.copy_layout(&ws, target, &meta)? {
            Some((v, json)) => Ok(Some((v, from_json(&json)?))),
            None => Ok(None),
        }
    })
    .await;
    let (version, layout) = match result {
        Ok(Some(x)) => x,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "version_not_found", "workspace_id": id, "version": target }),
            );
        }
        Err(e) => return e.into_response(),
    };
    state.publish_layout(&id, version);
    tracing::info!(workspace = %id, rolled_back_to = target, version, "layout rolled back");
    (
        [version_header(version)],
        Json(json!({ "version": version, "rolled_back_to": target, "workspace": layout })),
    )
        .into_response()
}

fn layout_event(workspace_id: &str, version: i64) -> Event {
    Event::default()
        .event("layout")
        .data(json!({ "workspace_id": workspace_id, "version": version }).to_string())
}

/// `GET /v1/workspaces/:id/events` (SSE). First message: the current version;
/// then one `layout` event per new version of this workspace; a `: keepalive`
/// comment every [`AppState::sse_keepalive`].
///
/// Auth mirrors `GET /v1/workspaces/:id`: no bearer is fine (events carry only
/// a workspace id and a version number, no content); for `pixelfed_home` a
/// bearer that is not a valid session is 401 so the client learns it is stale.
async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_versioned(&id) {
        return not_versioned(&id);
    }
    if id == crate::pixelfed::layout::WORKSPACE_ID {
        match valid_session(&state, &headers).await {
            Ok(Some(false)) => return unauthorized(),
            Ok(_) => {}
            Err(e) => return e.into_response(),
        }
    }
    // Subscribe before reading the current version so nothing committed in
    // between is missed (a duplicate version is harmless to clients).
    let rx = state.subscribe_layouts();
    let current = match latest_async(&state, &id).await {
        Ok(Some((v, _))) => v,
        Ok(None) => return not_versioned(&id),
        Err(e) => return e.into_response(),
    };
    tracing::info!(workspace = %id, version = current, "layout events subscriber connected");
    let first = layout_event(&id, current);
    struct S {
        first: Option<Event>,
        rx: broadcast::Receiver<LayoutEvent>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        state: Arc<AppState>,
        id: String,
    }
    let stream = futures_util::stream::unfold(
        S {
            first: Some(first),
            rx,
            shutdown: state.shutdown_signal(),
            state: state.clone(),
            id,
        },
        |mut s| async move {
            if let Some(e) = s.first.take() {
                return Some((Ok::<_, std::convert::Infallible>(e), s));
            }
            loop {
                let received = tokio::select! {
                    r = s.rx.recv() => r,
                    // Server shutting down: end the stream so graceful
                    // shutdown is not held open by subscribers.
                    _ = s.shutdown.wait_for(|stop| *stop) => return None,
                };
                match received {
                    Ok(ev) if ev.workspace_id == s.id => {
                        let e = layout_event(&s.id, ev.version);
                        return Some((Ok(e), s));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(workspace = %s.id, skipped = n, "layout subscriber lagged; sending current");
                        if let Ok(Some((v, _))) = latest_async(&s.state, &s.id).await {
                            let e = layout_event(&s.id, v);
                            return Some((Ok(e), s));
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(state.sse_keepalive())
                .text("keepalive"),
        )
        .into_response()
}
