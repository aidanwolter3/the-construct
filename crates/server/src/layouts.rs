//! Layout catalog, version history and the layout push channel (M7 D22, D26).
//!
//! Each versioned workspace has several layouts (built-ins seeded from
//! `fixtures/layouts/`, ids like `pixelfed_home.editorial`), each with its own
//! append-only, gapless version history, and exactly one ACTIVE layout. The
//! workspace REVISION (one monotonic number per workspace, bumped on every
//! visible change: an edit or restore of the active layout, or a switch) is
//! what `X-Construct-Layout-Version` and SSE `version` carry, so clients keep
//! one number per workspace. Only the workspace root [`UiNode`] is versioned;
//! entities still come from fixtures / the upstream. Committing does not
//! validate anything; the caller (agent endpoint) validates first.

// Helpers return a ready error `Response` for the handler to send as is.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
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
use crate::store::{
    Activated, LayoutRow, LayoutVersionInfo, Restored, SeedLayout, Store, VersionRow,
};
pub use crate::store::{ActiveLayout, LayoutMeta};
use crate::{ApiError, AppState, blocking, json_error, parse_json_body};

/// Response header carrying the served workspace REVISION (the client's
/// one monotonic "layout version" per workspace, D24).
pub const VERSION_HEADER: &str = "x-construct-layout-version";
/// Response header naming the served (active) layout.
pub const LAYOUT_ID_HEADER: &str = "x-construct-layout-id";
/// Response header carrying the served layout's own version number.
pub const LAYOUT_ID_VERSION_HEADER: &str = "x-construct-layout-id-version";
/// Default SSE keepalive interval.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Capacity of the layout broadcast channel (a lagging subscriber is caught
/// up with the current revision, so losing intermediate events is harmless).
pub const CHANNEL_CAPACITY: usize = 64;
/// The `provider` recorded on versions created by `POST .../rollback`.
pub const ROLLBACK_PROVIDER: &str = "rollback";
/// Default and largest page of `GET /v1/layouts/:id/versions`.
pub const DEFAULT_VERSIONS_LIMIT: i64 = 100;
pub const MAX_VERSIONS_LIMIT: i64 = 1000;

/// Workspaces whose layout is versioned.
pub const VERSIONED: [&str; 3] = [
    "home",
    "pixelfed_demo",
    crate::pixelfed::layout::WORKSPACE_ID,
];

pub fn is_versioned(workspace_id: &str) -> bool {
    VERSIONED.contains(&workspace_id)
}

/// Longest accepted workspace id.
pub const MAX_WORKSPACE_ID_LEN: usize = 64;
/// Longest accepted layout id (`<workspace>.<name>`).
pub const MAX_LAYOUT_ID_LEN: usize = 128;

/// 1..=64 characters of `[A-Za-z0-9_-]`.
pub fn is_valid_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_WORKSPACE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 1..=128 characters of `[A-Za-z0-9_.-]`.
pub fn is_valid_layout_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LAYOUT_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// `id` cut to [`MAX_WORKSPACE_ID_LEN`] characters, for echoing in errors.
pub fn truncated_id(id: &str) -> String {
    let mut chars = id.chars();
    let mut out: String = chars.by_ref().take(MAX_WORKSPACE_ID_LEN).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

/// 400 for a malformed workspace id (`None` when it is well-formed). The
/// echo is truncated so a huge id is not reflected back.
pub fn reject_invalid_workspace_id(id: &str) -> Option<Response> {
    if is_valid_workspace_id(id) {
        return None;
    }
    Some(json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_workspace_id", "id": truncated_id(id),
                "detail": format!("a workspace id is 1..={MAX_WORKSPACE_ID_LEN} characters of [A-Za-z0-9_-]") }),
    ))
}

/// A workspace's visible layout changed (what the SSE channel carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEvent {
    pub workspace_id: String,
    /// The workspace revision.
    pub version: i64,
    pub layout_id: String,
    pub layout_version: i64,
}

impl From<&ActiveLayout> for LayoutEvent {
    fn from(a: &ActiveLayout) -> Self {
        Self {
            workspace_id: a.workspace_id.clone(),
            version: a.revision,
            layout_id: a.layout_id.clone(),
            layout_version: a.layout_version,
        }
    }
}

/// The built-in default layout of a versioned workspace (the classic feed /
/// the counter; version 1 of [`default_layout_id`]).
pub fn builtin(workspace_id: &str) -> Option<UiNode> {
    match workspace_id {
        "home" | "pixelfed_demo" => crate::workspace(workspace_id).map(|s| s.workspace.clone()),
        crate::pixelfed::layout::WORKSPACE_ID => {
            Some(crate::pixelfed::layout::home_feed(Default::default(), Vec::new()).workspace)
        }
        _ => None,
    }
}

/// The layout a workspace starts on (and where pre-catalog history went).
pub fn default_layout_id(workspace_id: &str) -> String {
    crate::store::default_layout_id(workspace_id)
}

/// A built-in variant: `(name, title, description, preview_hint, json)`;
/// `json` `None` = the workspace's [`builtin`] layout.
type Variant = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

const FEED_VARIANTS_HOME: [Variant; 4] = [
    (
        "classic",
        "Classic feed",
        "Author, square photo, like/bookmark/comment bar, caption.",
        "feed-classic",
        None,
    ),
    (
        "editorial",
        "Editorial journal",
        "Large portrait photo first, then a byline and the caption; no action bar.",
        "feed-editorial",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_home.editorial.json"
        )),
    ),
    (
        "compact",
        "Compact",
        "Smaller inset wide photos and small captions for a denser list.",
        "feed-compact",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_home.compact.json"
        )),
    ),
    (
        "minimal",
        "Minimal",
        "Author, photo and caption only; no action bar.",
        "feed-minimal",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_home.minimal.json"
        )),
    ),
];

const FEED_VARIANTS_DEMO: [Variant; 4] = [
    FEED_VARIANTS_HOME[0],
    (
        "editorial",
        "Editorial journal",
        "Large portrait photo first, then a byline and the caption; no action bar.",
        "feed-editorial",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_demo.editorial.json"
        )),
    ),
    (
        "compact",
        "Compact",
        "Smaller inset wide photos and small captions for a denser list.",
        "feed-compact",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_demo.compact.json"
        )),
    ),
    (
        "minimal",
        "Minimal",
        "Author, photo and caption only; no action bar.",
        "feed-minimal",
        Some(include_str!(
            "../../../fixtures/layouts/pixelfed_demo.minimal.json"
        )),
    ),
];

const COUNTER_VARIANTS: [Variant; 2] = [
    (
        "counter",
        "Counter",
        "Title, count and the increment / decrement / reset buttons.",
        "counter",
        None,
    ),
    (
        "big_counter",
        "Big counter",
        "The count first, the buttons below it and a small title at the bottom.",
        "counter-big",
        Some(include_str!(
            "../../../fixtures/layouts/home.big_counter.json"
        )),
    ),
];

fn variants(workspace_id: &str) -> &'static [Variant] {
    match workspace_id {
        "home" => &COUNTER_VARIANTS,
        "pixelfed_demo" => &FEED_VARIANTS_DEMO,
        crate::pixelfed::layout::WORKSPACE_ID => &FEED_VARIANTS_HOME,
        _ => &[],
    }
}

static CATALOG: LazyLock<Vec<SeedLayout>> = LazyLock::new(|| {
    VERSIONED
        .iter()
        .flat_map(|ws| {
            variants(ws)
                .iter()
                .map(move |(name, title, description, hint, json)| {
                    let node = match json {
                        None => builtin(ws).expect("versioned workspaces have a built-in layout"),
                        Some(text) => serde_json::from_str(text)
                            .unwrap_or_else(|e| panic!("fixtures/layouts/{ws}.{name}.json: {e}")),
                    };
                    SeedLayout {
                        id: format!("{ws}.{name}"),
                        workspace_id: (*ws).to_owned(),
                        title: (*title).to_owned(),
                        description: (*description).to_owned(),
                        preview_hint: (*hint).to_owned(),
                        json: to_json(&node).expect("a UiNode serializes"),
                    }
                })
        })
        .collect()
});

/// The built-in layouts (parsed once; an invalid fixture panics, tested).
pub fn catalog() -> &'static [SeedLayout] {
    &CATALOG
}

fn to_json(node: &UiNode) -> rusqlite::Result<String> {
    serde_json::to_string(node).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// A layout as JSON, through its text form: `serde_json::to_value` would
/// widen `f32` fields (`aspect_ratio` 0.8 -> 0.800000011920929).
pub fn node_value(node: &UiNode) -> Value {
    serde_json::to_string(node)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn from_json(json: &str) -> rusqlite::Result<UiNode> {
    serde_json::from_str(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Seed the built-in layouts once per store (idempotent: existing history
/// and the active layout are kept; new built-ins are added). Blocking.
pub fn ensure_seeded(store: &Store) -> rusqlite::Result<()> {
    if store.catalog_seeded() {
        return Ok(());
    }
    let ids: Vec<(&str, String)> = VERSIONED
        .iter()
        .map(|ws| (*ws, default_layout_id(ws)))
        .collect();
    let defaults: Vec<(&str, &str)> = ids.iter().map(|(ws, id)| (*ws, id.as_str())).collect();
    let inserted = store.seed_catalog(catalog(), &defaults)?;
    if inserted > 0 {
        tracing::info!(inserted, "built-in layouts seeded");
    }
    Ok(())
}

/// What a workspace serves: its latest revision and the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Served {
    pub active: ActiveLayout,
    pub layout: UiNode,
}

/// The active layout of a versioned workspace (seeding on first use);
/// `None` if the workspace is not versioned. Blocking.
pub fn active(store: &Store, workspace_id: &str) -> rusqlite::Result<Option<Served>> {
    if !is_versioned(workspace_id) {
        return Ok(None);
    }
    ensure_seeded(store)?;
    match store.active_layout(workspace_id)? {
        Some((active, json)) => Ok(Some(Served {
            active,
            layout: from_json(&json)?,
        })),
        None => Ok(None),
    }
}

/// `(revision, layout)` of a versioned workspace; see [`active`]. Blocking.
pub fn latest(store: &Store, workspace_id: &str) -> rusqlite::Result<Option<(i64, UiNode)>> {
    Ok(active(store, workspace_id)?.map(|s| (s.active.revision, s.layout)))
}

fn source_for(provider: Option<&str>) -> &'static str {
    if provider.is_some() {
        crate::store::SOURCE_AGENT
    } else {
        crate::store::SOURCE_MANUAL
    }
}

fn commit_active(
    store: &Store,
    workspace_id: &str,
    layout: &UiNode,
    meta: &LayoutMeta,
) -> rusqlite::Result<ActiveLayout> {
    if !is_versioned(workspace_id) {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    ensure_seeded(store)?;
    store
        .append_active(
            workspace_id,
            None,
            &to_json(layout)?,
            source_for(meta.provider.as_deref()),
            meta,
        )?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// Append `layout` as the next version of the workspace's ACTIVE layout and
/// return the new workspace revision. Does not validate and does not
/// broadcast (see [`commit_and_publish`]). Source `agent` when a provider is
/// given, else `manual`. Blocking.
pub fn commit(
    store: &Store,
    workspace_id: &str,
    layout: &UiNode,
    prompt: Option<&str>,
    provider: Option<&str>,
    patch: Option<&Value>,
) -> rusqlite::Result<i64> {
    let meta = LayoutMeta {
        prompt: prompt.map(str::to_owned),
        provider: provider.map(str::to_owned),
        patch_json: patch.map(Value::to_string),
    };
    Ok(commit_active(store, workspace_id, layout, &meta)?.revision)
}

/// [`commit`] off the async workers, then broadcast the new revision.
pub async fn commit_and_publish(
    state: &Arc<AppState>,
    workspace_id: &str,
    layout: UiNode,
    prompt: Option<String>,
    provider: Option<String>,
    patch: Option<Value>,
) -> Result<i64, ApiError> {
    let ws = workspace_id.to_owned();
    let meta = LayoutMeta {
        prompt,
        provider,
        patch_json: patch.as_ref().map(Value::to_string),
    };
    let active = blocking(state, move |s| commit_active(s, &ws, &layout, &meta)).await?;
    state.publish_layout(&active);
    Ok(active.revision)
}

/// Like [`commit_and_publish`], but only on top of `base` (the active
/// layout and version the change was made from): `Ok(None)` if the layout
/// was edited, restored or switched meanwhile (nothing committed), so the
/// caller can redo the change on the new active layout instead of silently
/// overwriting it.
pub async fn commit_on_and_publish(
    state: &Arc<AppState>,
    base: &ActiveLayout,
    layout: UiNode,
    meta: LayoutMeta,
) -> Result<Option<ActiveLayout>, ApiError> {
    let base = base.clone();
    let result = blocking(state, move |s| {
        s.append_active(
            &base.workspace_id,
            Some((&base.layout_id, base.layout_version)),
            &to_json(&layout)?,
            crate::store::SOURCE_AGENT,
            &meta,
        )
    })
    .await?;
    if let Some(a) = &result {
        state.publish_layout(a);
    }
    Ok(result)
}

/// Async [`active`] for handlers.
pub async fn active_async(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Result<Option<Served>, ApiError> {
    let ws = workspace_id.to_owned();
    blocking(state, move |s| active(s, &ws)).await
}

/// `X-Construct-Layout-Version: <revision>`, `X-Construct-Layout-Id: <layout id>`,
/// `X-Construct-Layout-Id-Version: <that layout's version>`.
pub fn served_headers(active: &ActiveLayout) -> [(HeaderName, HeaderValue); 3] {
    [
        (
            HeaderName::from_static(VERSION_HEADER),
            HeaderValue::from(active.revision),
        ),
        (
            HeaderName::from_static(LAYOUT_ID_HEADER),
            HeaderValue::from_str(&active.layout_id)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        ),
        (
            HeaderName::from_static(LAYOUT_ID_VERSION_HEADER),
            HeaderValue::from(active.layout_version),
        ),
    ]
}

// ---------- routes ----------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/workspaces/{id}/versions", get(versions))
        .route("/v1/workspaces/{id}/rollback", post(rollback))
        .route("/v1/workspaces/{id}/events", get(events))
        .route("/v1/layouts", get(list_layouts))
        .route("/v1/layouts/{id}", get(get_layout))
        .route("/v1/layouts/{id}/versions", get(layout_versions))
        .route("/v1/layouts/{id}/versions/{n}", get(layout_version))
        .route("/v1/layouts/{id}/activate", post(activate))
        .route("/v1/layouts/{id}/restore", post(restore))
}

/// Auth for layout-changing / history endpoints (versions, rollback, agent,
/// activate, restore): a valid pod session (bearer), unless
/// `CONSTRUCT_AGENT_OPEN=1`. Returns the rejection to send (401, or 500 on a
/// store error), `None` when allowed.
pub async fn require_agent_auth(state: &Arc<AppState>, headers: &HeaderMap) -> Option<Response> {
    if state.agent_open() {
        return None;
    }
    match valid_session(state, headers).await {
        Ok(Some(true)) => None,
        Ok(_) => Some(agent_unauthorized()),
        Err(e) => Some(e.into_response()),
    }
}

/// 401 for the agent / history routes, saying what would be accepted.
fn agent_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized",
                     "detail": "a valid pod session is required (or CONSTRUCT_AGENT_OPEN=1 for local demos)" })),
    )
        .into_response()
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

/// Who may read `pixelfed_home` layouts (the others are public, like their
/// workspace GET): `Ok(true)` with a valid session or in open mode,
/// `Ok(false)` without a bearer, `Err(401)` for a stale bearer.
async fn private_reads(state: &Arc<AppState>, headers: &HeaderMap) -> Result<bool, Response> {
    if state.agent_open() {
        return Ok(true);
    }
    match valid_session(state, headers).await {
        Ok(Some(true)) => Ok(true),
        Ok(Some(false)) => Err(unauthorized()),
        Ok(None) => Ok(false),
        Err(e) => Err(e.into_response()),
    }
}

/// Read access to one workspace's layouts.
async fn require_read(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Option<Response> {
    if workspace_id != crate::pixelfed::layout::WORKSPACE_ID {
        return None;
    }
    match private_reads(state, headers).await {
        Ok(true) => None,
        Ok(false) => Some(agent_unauthorized()),
        Err(r) => Some(r),
    }
}

fn not_versioned(id: &str) -> Response {
    if let Some(r) = reject_invalid_workspace_id(id) {
        return r;
    }
    json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "workspace_not_found", "id": truncated_id(id),
                "versioned": VERSIONED }),
    )
}

/// `GET /v1/workspaces/:id/versions` -> `[{version, created_at, prompt, provider}]`,
/// newest first: the history of the workspace's ACTIVE layout.
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
        ensure_seeded(s)?;
        s.layout_versions(&ws)
    })
    .await;
    match list {
        Ok(list) => Json(list).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `{"version": N}` (N >= 1) from a request body. The body must be an
/// object (or empty, when the version is optional): `[1]` or `"x"` is a 400,
/// not a silent "no version".
fn version_arg(payload: &Value, required: bool) -> Result<Option<i64>, Response> {
    let invalid = || {
        json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "integer >= 1" } }),
        )
    };
    if !payload.is_object() && !payload.is_null() {
        return Err(invalid());
    }
    match &payload["version"] {
        Value::Null if !required => Ok(None),
        v => v.as_i64().filter(|v| *v >= 1).map(Some).ok_or_else(invalid),
    }
}

/// `POST /v1/workspaces/:id/rollback {"version": N}`: appends a copy of
/// version N of the ACTIVE layout (append-only), broadcasts it, and answers
/// `{version, rolled_back_to, workspace, revision, layout_id}` (`version` =
/// the active layout's new version; the header carries the revision).
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
    let target = match version_arg(&payload, true) {
        Ok(v) => v.unwrap_or(1),
        Err(r) => return r,
    };
    let ws = id.clone();
    let result = blocking(&state, move |s| {
        ensure_seeded(s)?;
        let meta = LayoutMeta {
            prompt: Some(format!("rollback to v{target}")),
            provider: Some(ROLLBACK_PROVIDER.to_owned()),
            patch_json: None,
        };
        match s.rollback_active(&ws, target, &meta)? {
            Some(Restored::Appended {
                version,
                json,
                active: Some(active),
            }) => Ok(Some((version, from_json(&json)?, active))),
            _ => Ok(None),
        }
    })
    .await;
    let (version, layout, active) = match result {
        Ok(Some(x)) => x,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "version_not_found", "workspace_id": id, "version": target }),
            );
        }
        Err(e) => return e.into_response(),
    };
    state.publish_layout(&active);
    tracing::info!(workspace = %id, layout = %active.layout_id, rolled_back_to = target, version,
                   revision = active.revision, "layout rolled back");
    (
        served_headers(&active),
        Json(
            json!({ "version": version, "rolled_back_to": target, "workspace": node_value(&layout),
                     "revision": active.revision, "layout_id": active.layout_id }),
        ),
    )
        .into_response()
}

// ---------- catalog ----------

fn workspace_json(a: &ActiveLayout) -> Value {
    json!({ "workspace_id": a.workspace_id, "revision": a.revision,
            "layout_id": a.layout_id, "layout_version": a.layout_version })
}

fn layout_json(row: &LayoutRow, active: Option<&ActiveLayout>) -> Value {
    let is_active = active.is_some_and(|a| a.layout_id == row.id);
    let mut v = json!({
        "id": row.id,
        "workspace_id": row.workspace_id,
        "title": row.title,
        "description": row.description,
        "preview_hint": row.preview_hint,
        "builtin": row.builtin,
        "current_version": row.current_version,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "active": is_active,
    });
    if let Some(a) = active.filter(|_| is_active) {
        v["active_version"] = json!(a.layout_version);
    }
    v
}

fn version_json(row: &VersionRow) -> Value {
    let mut v = json!({
        "version": row.version,
        "created_at": row.created_at,
        "source": row.source,
        "hash": row.hash,
        "bytes": row.bytes,
    });
    if let Some(p) = &row.prompt {
        v["prompt"] = json!(p);
    }
    if let Some(p) = &row.provider {
        v["provider"] = json!(p);
    }
    v
}

fn invalid_layout_id(id: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_layout_id", "id": truncated_id(id),
                "detail": format!("a layout id is 1..={MAX_LAYOUT_ID_LEN} characters of [A-Za-z0-9_.-]") }),
    )
}

fn layout_not_found(id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "layout_not_found", "id": id }),
    )
}

/// The layout's row (seeding first) and its workspace's active layout, or
/// the error response (400 / 404 / read auth).
async fn load_layout(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    id: &str,
) -> Result<(LayoutRow, Option<ActiveLayout>), Response> {
    if !is_valid_layout_id(id) {
        return Err(invalid_layout_id(id));
    }
    let lid = id.to_owned();
    let found = blocking(state, move |s| {
        ensure_seeded(s)?;
        let Some(row) = s.layout(&lid)? else {
            return Ok(None);
        };
        let active = s.active(&row.workspace_id)?;
        Ok(Some((row, active)))
    })
    .await
    .map_err(IntoResponse::into_response)?;
    let Some((row, active)) = found else {
        return Err(layout_not_found(id));
    };
    if let Some(r) = require_read(state, headers, &row.workspace_id).await {
        return Err(r);
    }
    Ok((row, active))
}

/// `GET /v1/layouts[?workspace_id=<ws>]` -> `{"layouts": [...], "workspaces":
/// [{workspace_id, revision, layout_id, layout_version}]}` (filtered: plus
/// `workspace_id` and `workspace_revision`). Unfiltered, anonymous callers do
/// not see `pixelfed_home` layouts.
async fn list_layouts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let filter = query.get("workspace_id").cloned();
    let include_private = match &filter {
        Some(ws) => {
            if !is_versioned(ws) {
                return not_versioned(ws);
            }
            if let Some(r) = require_read(&state, &headers, ws).await {
                return r;
            }
            true
        }
        None => match private_reads(&state, &headers).await {
            Ok(v) => v,
            Err(r) => return r,
        },
    };
    let ws_filter = filter.clone();
    let result = blocking(&state, move |s| {
        ensure_seeded(s)?;
        let rows = s.layouts(ws_filter.as_deref())?;
        let mut actives = Vec::new();
        for ws in VERSIONED {
            if ws_filter.as_deref().is_none_or(|f| f == ws)
                && let Some(a) = s.active(ws)?
            {
                actives.push(a);
            }
        }
        Ok((rows, actives))
    })
    .await;
    let (rows, actives) = match result {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let visible = |ws: &str| include_private || ws != crate::pixelfed::layout::WORKSPACE_ID;
    let layouts: Vec<Value> = rows
        .iter()
        .filter(|r| visible(&r.workspace_id))
        .map(|r| {
            let active = actives.iter().find(|a| a.workspace_id == r.workspace_id);
            layout_json(r, active)
        })
        .collect();
    let workspaces: Vec<Value> = actives
        .iter()
        .filter(|a| visible(&a.workspace_id))
        .map(workspace_json)
        .collect();
    let mut body = json!({ "layouts": layouts, "workspaces": workspaces });
    if let Some(ws) = filter {
        body["workspace_id"] = json!(ws);
        body["workspace_revision"] = json!(actives.first().map(|a| a.revision));
    }
    Json(body).into_response()
}

/// `GET /v1/layouts/:id` -> the layout object plus `workspace_revision`.
async fn get_layout(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match load_layout(&state, &headers, &id).await {
        Ok((row, active)) => {
            let mut v = layout_json(&row, active.as_ref());
            v["workspace_revision"] = json!(active.map(|a| a.revision));
            Json(v).into_response()
        }
        Err(r) => r,
    }
}

/// `GET /v1/layouts/:id/versions[?limit=N&before=V]` -> `{"layout_id",
/// "versions": [{version, created_at, source, prompt?, provider?, hash,
/// bytes}]}` newest first; `next_before` when older versions remain.
async fn layout_versions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let parse = |k: &str| {
        query
            .get(k)
            .map(|v| v.parse::<i64>().ok().filter(|n| *n >= 1))
    };
    let limit = match parse("limit") {
        None => DEFAULT_VERSIONS_LIMIT,
        Some(Some(n)) => n.min(MAX_VERSIONS_LIMIT),
        Some(None) => return bad_query(),
    };
    let before = match parse("before") {
        None => None,
        Some(Some(n)) => Some(n),
        Some(None) => return bad_query(),
    };
    let (row, _) = match load_layout(&state, &headers, &id).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let lid = id.clone();
    let rows = match blocking(&state, move |s| {
        s.layout_version_rows(&lid, before, limit + 1)
    })
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let more = rows.len() as i64 > limit;
    let page = &rows[..rows.len().min(limit as usize)];
    let mut body = json!({
        "layout_id": row.id,
        "workspace_id": row.workspace_id,
        "current_version": row.current_version,
        "versions": page.iter().map(version_json).collect::<Vec<_>>(),
    });
    if more && let Some(last) = page.last() {
        body["next_before"] = json!(last.version);
    }
    Json(body).into_response()
}

fn bad_query() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request",
                "expected": { "limit": format!("integer 1..={MAX_VERSIONS_LIMIT}"), "before": "integer >= 1" } }),
    )
}

/// `GET /v1/layouts/:id/versions/:n` -> `{layout_id, version, workspace_id,
/// source, created_at, prompt?, provider?, hash, bytes, layout}`.
async fn layout_version(
    State(state): State<Arc<AppState>>,
    Path((id, n)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let (row, _) = match load_layout(&state, &headers, &id).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(n) = n.parse::<i64>().ok().filter(|n| *n >= 1) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "integer >= 1" } }),
        );
    };
    let lid = id.clone();
    let found = blocking(&state, move |s| match s.layout_version(&lid, n)? {
        Some((v, json)) => Ok(Some((v, from_json(&json)?))),
        None => Ok(None),
    })
    .await;
    match found {
        Ok(Some((v, layout))) => {
            let mut body = version_json(&v);
            body["layout_id"] = json!(row.id);
            body["workspace_id"] = json!(row.workspace_id);
            body["layout"] = node_value(&layout);
            Json(body).into_response()
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "layout_id": id, "version": n }),
        ),
        Err(e) => e.into_response(),
    }
}

/// Common start of the write endpoints: id check, write auth, body.
async fn write_preamble(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    id: &str,
    body: &Bytes,
) -> Result<Value, Response> {
    if !is_valid_layout_id(id) {
        return Err(invalid_layout_id(id));
    }
    if let Some(r) = require_agent_auth(state, headers).await {
        return Err(r);
    }
    parse_json_body(headers, body).map_err(IntoResponse::into_response)
}

/// `POST /v1/layouts/:id/activate` `{}` or `{"version": n}`: make the layout
/// its workspace's active one (with `version`: restore that version into it
/// first, append-only). -> `{workspace_id, revision, layout_id,
/// layout_version, unchanged, restored_version?}`; broadcast when it changed.
async fn activate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match write_preamble(&state, &headers, &id, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let version = match version_arg(&payload, false) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let lid = id.clone();
    let result = blocking(&state, move |s| {
        ensure_seeded(s)?;
        s.activate_layout(&lid, version)
    })
    .await;
    match result {
        Ok(Activated::Done {
            active,
            restored,
            unchanged,
        }) => {
            if !unchanged {
                state.publish_layout(&active);
                tracing::info!(workspace = %active.workspace_id, layout = %active.layout_id,
                               layout_version = active.layout_version, revision = active.revision,
                               ?restored, "layout activated");
            }
            let mut body = workspace_json(&active);
            body["unchanged"] = json!(unchanged);
            if let Some(v) = restored {
                body["restored_version"] = json!(v);
            }
            (served_headers(&active), Json(body)).into_response()
        }
        Ok(Activated::LayoutNotFound) => layout_not_found(&id),
        Ok(Activated::VersionNotFound) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "layout_id": id, "version": version }),
        ),
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/layouts/:id/restore {"version": n}`: append a copy of version n
/// (source `restore`, prompt `restore v<n>`), so a restore is itself
/// undoable. Content equal to the newest version -> 200 `"unchanged": true`.
/// -> `{layout_id, workspace_id, version, restored_from, unchanged, active,
/// revision?}`; broadcast when the layout is active.
async fn restore(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match write_preamble(&state, &headers, &id, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let from = match version_arg(&payload, true) {
        Ok(v) => v.unwrap_or(1),
        Err(r) => return r,
    };
    let lid = id.clone();
    let result = blocking(&state, move |s| {
        ensure_seeded(s)?;
        let meta = LayoutMeta {
            prompt: Some(format!("restore v{from}")),
            ..Default::default()
        };
        let out = s.restore_version(&lid, from, crate::store::SOURCE_RESTORE, &meta, true)?;
        let row = s.layout(&lid)?;
        let active = match &row {
            Some(r) => s.active(&r.workspace_id)?,
            None => None,
        };
        Ok((out, row, active))
    })
    .await;
    let (out, row, active) = match result {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let Some(row) = row else {
        return layout_not_found(&id);
    };
    // An appended restore reports the revision it appended itself (decided
    // in its transaction): the `active` read after it may already see a
    // concurrent switch, which would hide the change from the caller.
    let active = match &out {
        Restored::Appended { active, .. } => active.clone(),
        _ => active.filter(|a| a.layout_id == row.id),
    };
    let is_active = active.is_some();
    let (version, unchanged) = match out {
        Restored::LayoutNotFound => return layout_not_found(&id),
        Restored::VersionNotFound => {
            return json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "version_not_found", "layout_id": id, "version": from }),
            );
        }
        Restored::Unchanged { version } => (version, true),
        Restored::Appended {
            version,
            active: new_active,
            ..
        } => {
            if let Some(a) = &new_active {
                state.publish_layout(a);
            }
            tracing::info!(layout = %id, restored_from = from, version, active = is_active, "layout restored");
            (version, false)
        }
    };
    let mut body = json!({
        "layout_id": row.id,
        "workspace_id": row.workspace_id,
        "version": version,
        "restored_from": from,
        "unchanged": unchanged,
        "active": is_active,
    });
    if let Some(a) = active {
        body["revision"] = json!(a.revision);
    }
    Json(body).into_response()
}

// ---------- push ----------

fn layout_event(ev: &LayoutEvent) -> Event {
    Event::default().event("layout").data(
        json!({ "workspace_id": ev.workspace_id, "version": ev.version,
                "layout_id": ev.layout_id, "layout_version": ev.layout_version })
        .to_string(),
    )
}

/// `GET /v1/workspaces/:id/events` (SSE). First message: the current
/// revision; then one `layout` event per new revision of this workspace
/// (`{workspace_id, version: <revision>, layout_id, layout_version}`); a
/// `: keepalive` comment every [`AppState::sse_keepalive`].
///
/// Auth mirrors `GET /v1/workspaces/:id`: no bearer is fine (events carry only
/// ids and numbers, no content); for `pixelfed_home` a bearer that is not a
/// valid session is 401 so the client learns it is stale.
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
    // Subscribe before reading the current revision so nothing committed in
    // between is missed (a duplicate revision is harmless to clients).
    let rx = state.subscribe_layouts();
    let current = match active_async(&state, &id).await {
        Ok(Some(s)) => LayoutEvent::from(&s.active),
        Ok(None) => return not_versioned(&id),
        Err(e) => return e.into_response(),
    };
    tracing::info!(workspace = %id, revision = current.version, "layout events subscriber connected");
    let first = layout_event(&current);
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
                        return Some((Ok(layout_event(&ev)), s));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(workspace = %s.id, skipped = n, "layout subscriber lagged; sending current");
                        if let Ok(Some(served)) = active_async(&s.state, &s.id).await {
                            let e = layout_event(&LayoutEvent::from(&served.active));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_deserializes_and_validates_against_its_default() {
        let all = catalog();
        assert_eq!(all.len(), 10);
        for ws in VERSIONED {
            let default = serde_json::to_value(builtin(ws).unwrap()).unwrap();
            let mine: Vec<&SeedLayout> = all.iter().filter(|s| s.workspace_id == ws).collect();
            assert!(mine.iter().any(|s| s.id == default_layout_id(ws)), "{ws}");
            let mut hashes = std::collections::HashSet::new();
            for seed in mine {
                assert!(is_valid_layout_id(&seed.id), "{}", seed.id);
                assert!(seed.id.starts_with(&format!("{ws}.")), "{}", seed.id);
                let doc: Value = serde_json::from_str(&seed.json).unwrap();
                let node = crate::agent::validate(&default, &doc)
                    .unwrap_or_else(|e| panic!("{} does not validate: {e}", seed.id));
                assert_eq!(
                    to_json(&node).unwrap(),
                    seed.json,
                    "{} is canonical",
                    seed.id
                );
                assert!(
                    hashes.insert(crate::store::content_hash(&seed.json)),
                    "{} is a duplicate",
                    seed.id
                );
            }
        }
    }

    #[test]
    fn layout_ids() {
        assert!(is_valid_layout_id("pixelfed_home.editorial"));
        assert!(!is_valid_layout_id(""));
        assert!(!is_valid_layout_id("a/b"));
        assert!(!is_valid_layout_id(&"a".repeat(129)));
    }
}
