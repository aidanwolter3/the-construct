//! Layouts as code, the read-only apps / versions API and the layout push
//! channel.
//!
//! The pod serves a few apps (one workspace each; only `pixelfed_home` is
//! listed unless `CONSTRUCT_SHOW_DEV_APPS=1`). Each app has ONE linear,
//! append-only layout history (`app_versions`, store migration v7). New
//! versions come only from the developer: files `layouts/<app_id>/NNNN.json`
//! in the repo, embedded in the binary ([`FILES`]) and appended at startup by
//! [`sync`] before the server listens. No HTTP route changes a layout.
//!
//! # Adding a layout version
//!
//! 1. Copy the newest file, e.g. `layouts/pixelfed_home/0001.json` to
//!    `layouts/pixelfed_home/0002.json` (4 digits, the next number: numbering
//!    is contiguous from 0001), and edit it. The format is
//!    `{"note": "short changelog line", "layout": <UiNode>}`; the layout root
//!    is the app's `core://Workspace` (id = the app id).
//! 2. Add one line to [`FILES`]: `layout_file!("pixelfed_home", "0002"),`
//!    (a test fails if a file in `layouts/` is not listed).
//! 3. `cargo test -p server` (validates every file), deploy. On start the pod
//!    appends it as the app's next version and pushes it to connected clients
//!    on their next fetch.
//!
//! Files are append-only: once deployed, a file must never change (the pod
//! refuses to start, naming the file and how to recover, if its content
//! differs from what it recorded: [`file_hash`], the hash of the file's own
//! note and layout JSON, independent of how the Rust types serialize it); add
//! a new file instead. A gap in the numbering, a file that is not a valid
//! layout (structure checked by [`crate::agent::validate_structure`]), an
//! unknown action intent tag, a root id that is not the app id, or a new file
//! numbered below a deployed one also stop the start. A deployed file missing
//! from the build (a rollback deploy, an older branch) only logs a warning:
//! the recorded history is served unchanged.
//!
//! `X-Construct-Layout-Version` on `GET /v1/workspaces/:id` and the SSE
//! `version` carry the app's latest version number.

// Helpers return a ready error `Response` for the handler to send as is.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use protocol::UiNode;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::pixelfed::routes::{bearer, session_hash, unauthorized};
use crate::store::{AppVersion, FileVersion, Store, SyncReport};
use crate::{ApiError, AppState, blocking, json_error};

/// Response header carrying the served app version (the client's one
/// monotonic "layout version" per workspace).
pub const VERSION_HEADER: &str = "x-construct-layout-version";
/// Default SSE keepalive interval.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Capacity of the layout broadcast channel (a lagging subscriber is caught
/// up with the current version, so losing intermediate events is harmless).
pub const CHANNEL_CAPACITY: usize = 64;
/// Default and largest page of `GET /v1/apps/:id/versions`.
pub const DEFAULT_VERSIONS_LIMIT: i64 = 20;
pub const MAX_VERSIONS_LIMIT: i64 = 100;

/// An app the pod serves (one workspace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct App {
    pub id: &'static str,
    pub title: &'static str,
    /// Icon name for clients (`photo`, `counter`).
    pub icon: &'static str,
    /// Listed by `GET /v1/apps` only with `CONSTRUCT_SHOW_DEV_APPS=1` (its
    /// other endpoints always work: tests, CI and the simulator use them).
    pub dev: bool,
}

/// Every app, in `GET /v1/apps` order.
pub const APPS: [App; 3] = [
    App {
        id: crate::pixelfed::layout::WORKSPACE_ID,
        title: "Pixelfed",
        icon: "photo",
        dev: false,
    },
    App {
        id: "pixelfed_demo",
        title: "Pixelfed demo",
        icon: "photo",
        dev: true,
    },
    App {
        id: "home",
        title: "Counter",
        icon: "counter",
        dev: true,
    },
];

pub fn app(id: &str) -> Option<&'static App> {
    APPS.iter().find(|a| a.id == id)
}

/// Whether the workspace's layout is versioned (it is an app).
pub fn is_versioned(workspace_id: &str) -> bool {
    app(workspace_id).is_some()
}

// ---------- layout files ----------

/// One embedded layout file: `layouts/<app>/<name>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSource {
    pub app_id: &'static str,
    /// `NNNN` (4 digits).
    pub name: &'static str,
    pub text: &'static str,
}

/// `layout_file!("app", "NNNN")` embeds `layouts/app/NNNN.json`.
macro_rules! layout_file {
    ($app:literal, $name:literal) => {
        LayoutSource {
            app_id: $app,
            name: $name,
            text: include_str!(concat!("../../../layouts/", $app, "/", $name, ".json")),
        }
    };
}

/// Every layout file, per app in number order. Add new files here (see the
/// module docs).
pub const FILES: &[LayoutSource] = &[
    layout_file!("pixelfed_home", "0001"),
    layout_file!("pixelfed_home", "0002"),
    layout_file!("pixelfed_demo", "0001"),
    layout_file!("home", "0001"),
];

/// A parsed, validated layout file.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutFile {
    pub app_id: String,
    pub number: u32,
    /// `layouts/<app>/NNNN.json`.
    pub path: String,
    pub note: String,
    pub layout: UiNode,
    /// [`file_hash`] of the file's note and layout.
    pub file_hash: String,
}

impl LayoutFile {
    fn to_version(&self) -> FileVersion {
        FileVersion {
            key: format!("{}/{:04}", self.app_id, self.number),
            number: self.number,
            path: self.path.clone(),
            note: self.note.clone(),
            json: to_json(&self.layout),
            file_hash: self.file_hash.clone(),
        }
    }
}

/// The hash the append-only check compares: SHA-256 of the canonical JSON
/// (sorted keys, no whitespace) of `{"layout": <the file's layout as
/// written>, "note": <trimmed note>}`. Reformatting or reordering keys in the
/// file does not change it, and neither does a change to the Rust types
/// (field order, a new defaulted field), unlike a hash of the re-serialized
/// `UiNode`.
pub fn file_hash(note: &str, layout: &Value) -> String {
    let doc = json!({ "layout": layout, "note": note });
    let mut out = String::new();
    write_canonical(&doc, &mut out);
    crate::store::content_hash(&out)
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Whether this build knows the intent tag (an unknown one, and the
/// `action.unknown` fallback tag itself, deserializes as
/// [`protocol::ActionIntent::Unknown`]; layout files must not use one).
fn is_known_intent(tag: &str) -> bool {
    !matches!(
        serde_json::from_value::<protocol::ActionIntent>(json!({ "intent": tag })),
        Ok(protocol::ActionIntent::Unknown)
    )
}

/// `(JSON pointer, tag)` of each intent in `v` (the value of an `on_click`
/// or `*_intent` key) whose `"intent"` tag is not known to this build.
fn unknown_intent_tags(v: &Value, at: &str, out: &mut Vec<(String, String)>) {
    collect_unknown_intent_tags(v, at, false, out);
}

fn collect_unknown_intent_tags(
    v: &Value,
    at: &str,
    intent_position: bool,
    out: &mut Vec<(String, String)>,
) {
    match v {
        Value::Object(map) => {
            if intent_position
                && let Some(Value::String(tag)) = map.get("intent")
                && !is_known_intent(tag)
            {
                out.push((at.to_owned(), tag.clone()));
            }
            for (k, child) in map {
                let is_intent = k == "on_click" || k.ends_with("_intent");
                let k = k.replace('~', "~0").replace('/', "~1");
                collect_unknown_intent_tags(child, &format!("{at}/{k}"), is_intent, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                collect_unknown_intent_tags(child, &format!("{at}/{i}"), false, out);
            }
        }
        _ => {}
    }
}

/// Parse and validate one file (`Err` names the file).
pub fn parse_file(src: &LayoutSource) -> Result<LayoutFile, String> {
    let path = format!("layouts/{}/{}.json", src.app_id, src.name);
    let fail = |e: String| format!("{path}: {e}");
    if app(src.app_id).is_none() {
        return Err(fail(format!("'{}' is not an app", src.app_id)));
    }
    let number = match src.name.parse::<u32>() {
        Ok(n) if n >= 1 && src.name.len() == 4 && src.name.bytes().all(|b| b.is_ascii_digit()) => n,
        _ => {
            return Err(fail(
                "file names are 4-digit version numbers from 0001 (NNNN.json)".into(),
            ));
        }
    };
    let doc: Value =
        serde_json::from_str(src.text).map_err(|e| fail(format!("not valid JSON: {e}")))?;
    let Value::Object(obj) = &doc else {
        return Err(fail(r#"expected {"note": "...", "layout": {...}}"#.into()));
    };
    if let Some(k) = obj.keys().find(|k| *k != "note" && *k != "layout") {
        return Err(fail(format!(
            "unknown key '{k}' (only \"note\" and \"layout\")"
        )));
    }
    let note = obj
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| fail("\"note\" must be a non-empty string".into()))?
        .to_owned();
    let layout = obj
        .get("layout")
        .ok_or_else(|| fail("\"layout\" is missing".into()))?;
    let parsed = crate::agent::validate_structure(src.app_id, layout)
        .map_err(|e| fail(format!("invalid layout: {e}")))?;
    // An unknown intent tag would deserialize as `ActionIntent::Unknown` and
    // ship as a dead action (lenient only for legacy stored versions).
    if crate::agent::intents(&parsed)
        .iter()
        .any(|i| matches!(i, protocol::ActionIntent::Unknown))
    {
        let mut found = Vec::new();
        unknown_intent_tags(layout, "/layout", &mut found);
        let what = match found.first() {
            Some((at, tag)) => format!("unknown action intent '{tag}' at {at}"),
            None => "unknown action intent".to_owned(),
        };
        return Err(fail(format!(
            "invalid layout: {what} (a typo? this build does not know it)"
        )));
    }
    let file_hash = file_hash(&note, layout);
    Ok(LayoutFile {
        app_id: src.app_id.to_owned(),
        number,
        path,
        note,
        layout: parsed,
        file_hash,
    })
}

/// Parse every source; per app the numbers must be exactly 1..=N.
pub fn parse_files(sources: &[LayoutSource]) -> Result<Vec<LayoutFile>, String> {
    let mut files = sources
        .iter()
        .map(parse_file)
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|a, b| (a.app_id.as_str(), a.number).cmp(&(b.app_id.as_str(), b.number)));
    for a in APPS {
        for (expected, f) in (1..).zip(files.iter().filter(|f| f.app_id == a.id)) {
            if f.number != expected {
                return Err(if f.number < expected {
                    format!("{} is listed twice", f.path)
                } else {
                    format!(
                        "{}: layout files are numbered without gaps, expected {:04}.json next",
                        f.path, expected
                    )
                });
            }
        }
    }
    Ok(files)
}

/// Append every new layout file (all apps, one transaction) to the store;
/// see the module docs. Blocking; idempotent. `Err` is the message to stop
/// the start with.
pub fn sync(store: &Store) -> Result<SyncReport, String> {
    sync_sources(store, FILES)
}

/// [`sync`] over any set of sources (tests).
pub fn sync_sources(store: &Store, sources: &[LayoutSource]) -> Result<SyncReport, String> {
    let files = parse_files(sources)?;
    let apps: Vec<(&str, Vec<FileVersion>)> = APPS
        .iter()
        .map(|a| {
            let versions = files
                .iter()
                .filter(|f| f.app_id == a.id)
                .map(LayoutFile::to_version)
                .collect();
            (a.id, versions)
        })
        .collect();
    store
        .sync_layout_files(&apps)
        .map_err(|e| format!("syncing layouts/: {e}"))?
}

/// The original built-in default layout of an app (what each `0001.json`
/// holds: the classic feed / the counter), built from code. (The
/// `pixelfed_demo` fixture's root id is `home_stream`; its file uses the app
/// id, as every layout file must.)
pub fn builtin(workspace_id: &str) -> Option<UiNode> {
    match workspace_id {
        "home" | "pixelfed_demo" => crate::workspace(workspace_id).map(|s| s.workspace.clone()),
        crate::pixelfed::layout::WORKSPACE_ID => {
            Some(crate::pixelfed::layout::home_feed(Default::default(), Vec::new()).workspace)
        }
        _ => None,
    }
}

fn to_json(node: &UiNode) -> String {
    serde_json::to_string(node).expect("a UiNode serializes")
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

// ---------- serving ----------

/// What a workspace serves: its app's latest version and layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Served {
    pub version: i64,
    pub layout: UiNode,
}

/// The latest layout of an app; `None` if the workspace is not an app or has
/// no versions (never after [`sync`]). Blocking.
pub fn active(store: &Store, workspace_id: &str) -> rusqlite::Result<Option<Served>> {
    if !is_versioned(workspace_id) {
        return Ok(None);
    }
    match store.app_latest(workspace_id)? {
        Some((v, json)) => Ok(Some(Served {
            version: v.version,
            layout: from_json(&json)?,
        })),
        None => Ok(None),
    }
}

/// The client's schema version from [`protocol::SCHEMA_HEADER`] (1 when
/// absent or unreadable: clients from before screens send none).
pub fn client_schema(headers: &HeaderMap) -> u32 {
    headers
        .get(protocol::SCHEMA_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .filter(|v: &u32| *v >= 1)
        .unwrap_or(1)
}

/// The newest version of an app a client of schema `schema` can draw
/// ([`protocol::required_schema`]); `None` if the workspace is not an app or
/// no version fits (version 1 of every app is schema 1). Blocking.
pub fn active_for(
    store: &Store,
    workspace_id: &str,
    schema: u32,
) -> rusqlite::Result<Option<Served>> {
    let Some(latest) = active(store, workspace_id)? else {
        return Ok(None);
    };
    if protocol::required_schema(&latest.layout) <= schema {
        return Ok(Some(latest));
    }
    for version in (1..latest.version).rev() {
        if let Some((_, json)) = store.app_version(workspace_id, version)? {
            let layout = from_json(&json)?;
            if protocol::required_schema(&layout) <= schema {
                tracing::info!(
                    app = workspace_id,
                    latest = latest.version,
                    served = version,
                    schema,
                    "served an older layout version the client can draw"
                );
                return Ok(Some(Served { version, layout }));
            }
        }
    }
    Ok(None)
}

/// Async [`active_for`] with the schema read from the request headers.
pub async fn active_for_client(
    state: &Arc<AppState>,
    workspace_id: &str,
    headers: &HeaderMap,
) -> Result<Option<Served>, ApiError> {
    let ws = workspace_id.to_owned();
    let schema = client_schema(headers);
    blocking(state, move |s| active_for(s, &ws, schema)).await
}

/// `(version, layout)`; see [`active`]. Blocking.
pub fn latest(store: &Store, workspace_id: &str) -> rusqlite::Result<Option<(i64, UiNode)>> {
    Ok(active(store, workspace_id)?.map(|s| (s.version, s.layout)))
}

/// Append `layout` as the app's next version (source `manual`) WITHOUT a
/// layout file. Tests and tooling only: no HTTP route reaches it, and a real
/// pod gets new versions from files. Does not broadcast (see
/// [`AppState::publish_layout`]). Blocking.
pub fn append_version(
    store: &Store,
    app_id: &str,
    layout: &UiNode,
    note: Option<&str>,
) -> rusqlite::Result<i64> {
    store.append_app_version(app_id, &to_json(layout), crate::store::SOURCE_MANUAL, note)
}

/// Async [`active`] for handlers.
pub async fn active_async(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Result<Option<Served>, ApiError> {
    let ws = workspace_id.to_owned();
    blocking(state, move |s| active(s, &ws)).await
}

/// `X-Construct-Layout-Version: <app version>`.
pub fn served_headers(version: i64) -> [(HeaderName, HeaderValue); 1] {
    [(
        HeaderName::from_static(VERSION_HEADER),
        HeaderValue::from(version),
    )]
}

// ---------- ids ----------

/// Longest accepted workspace id.
pub const MAX_WORKSPACE_ID_LEN: usize = 64;

/// 1..=64 characters of `[A-Za-z0-9_-]`.
pub fn is_valid_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_WORKSPACE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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

// ---------- time ----------

/// Unix seconds as RFC 3339 UTC (`2026-11-02T10:20:30Z`).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// ---------- routes ----------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/apps", get(list_apps))
        .route("/v1/apps/{id}/versions", get(app_versions))
        .route("/v1/apps/{id}/versions/{n}", get(app_version))
        .route("/v1/workspaces/{id}/events", get(events))
}

/// 400 for a malformed id, 404 `app_not_found` for an unknown one.
fn known_app(id: &str) -> Result<&'static App, Response> {
    if let Some(r) = reject_invalid_workspace_id(id) {
        return Err(r);
    }
    app(id).ok_or_else(|| {
        json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "app_not_found", "id": id }),
        )
    })
}

fn version_json(v: &AppVersion) -> Value {
    let mut out = json!({
        "version": v.version,
        "created_at": rfc3339(v.created_at),
        "source": v.source,
    });
    if let Some(n) = &v.note {
        out["note"] = json!(n);
    }
    out
}

/// `GET /v1/apps` -> `{"apps": [{id, title, icon, latest_version,
/// updated_at}]}` (public). Dev apps only with `CONSTRUCT_SHOW_DEV_APPS=1`.
async fn list_apps(State(state): State<Arc<AppState>>) -> Response {
    let shown: Vec<&'static App> = APPS
        .iter()
        .filter(|a| !a.dev || state.show_dev_apps())
        .collect();
    let ids: Vec<&'static str> = shown.iter().map(|a| a.id).collect();
    let latest = blocking(&state, move |s| {
        ids.iter()
            .map(|id| s.app_latest_meta(id))
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .await;
    let latest = match latest {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    let apps: Vec<Value> = shown
        .iter()
        .zip(latest)
        .map(|(a, v)| {
            json!({
                "id": a.id,
                "title": a.title,
                "icon": a.icon,
                "latest_version": v.as_ref().map(|v| v.version),
                "updated_at": v.as_ref().map(|v| rfc3339(v.created_at)),
            })
        })
        .collect();
    Json(json!({ "apps": apps })).into_response()
}

fn bad_query() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request",
                "expected": { "limit": format!("integer 1..={MAX_VERSIONS_LIMIT}"), "before": "integer >= 1" } }),
    )
}

/// `GET /v1/apps/:id/versions[?limit=1..=100 (20)&before=N]` ->
/// `{app_id, latest_version, versions: [{version, created_at, source,
/// note?}], next_before?}` newest first; `next_before` iff older versions
/// exist (pass it as `before` for the next page).
async fn app_versions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let app = match known_app(&id) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let parse = |k: &str| {
        query
            .get(k)
            .map(|v| v.trim().parse::<i64>().ok().filter(|n| *n >= 1))
    };
    let limit = match parse("limit") {
        None => DEFAULT_VERSIONS_LIMIT,
        Some(Some(n)) if n <= MAX_VERSIONS_LIMIT => n,
        Some(_) => return bad_query(),
    };
    let before = match parse("before") {
        None => None,
        Some(Some(n)) => Some(n),
        Some(None) => return bad_query(),
    };
    let result = blocking(&state, move |s| {
        let latest = s.app_latest_meta(app.id)?.map(|v| v.version);
        let rows = s.app_versions(app.id, before, limit + 1)?;
        Ok((latest, rows))
    })
    .await;
    let (latest, rows) = match result {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let more = rows.len() as i64 > limit;
    let page = &rows[..rows.len().min(limit as usize)];
    let mut body = json!({
        "app_id": app.id,
        "latest_version": latest,
        "versions": page.iter().map(version_json).collect::<Vec<_>>(),
    });
    if more && let Some(last) = page.last() {
        body["next_before"] = json!(last.version);
    }
    Json(body).into_response()
}

/// `GET /v1/apps/:id/versions/:n` -> `{app_id, version, created_at, source,
/// note?, layout}`.
async fn app_version(
    State(state): State<Arc<AppState>>,
    Path((id, n)): Path<(String, String)>,
) -> Response {
    let app = match known_app(&id) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(n) = n.parse::<i64>().ok().filter(|n| *n >= 1) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "integer >= 1" } }),
        );
    };
    let found = blocking(&state, move |s| s.app_version(app.id, n)).await;
    match found {
        Ok(Some((v, stored))) => {
            let mut body = version_json(&v);
            body["app_id"] = json!(app.id);
            body["layout"] = match from_json(&stored) {
                Ok(layout) => node_value(&layout),
                // A legacy version this build cannot read as a UiNode: serve
                // what was stored rather than fail (it is history to look at).
                Err(e) => match serde_json::from_str::<Value>(&stored) {
                    Ok(raw) => {
                        tracing::warn!(
                            app = app.id,
                            version = n,
                            "serving a stored layout that is not a valid UiNode as is: {e}"
                        );
                        raw
                    }
                    Err(e) => {
                        tracing::error!(
                            app = app.id,
                            version = n,
                            "stored layout is not JSON: {e}"
                        );
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            json!({ "error": "corrupt_version", "app_id": app.id, "version": n,
                                    "detail": "the stored layout of this version is not valid JSON" }),
                        );
                    }
                },
            };
            Json(body).into_response()
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "app_id": app.id, "version": n }),
        ),
        Err(e) => e.into_response(),
    }
}

// ---------- push ----------

/// An app's latest version changed (what the SSE channel carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEvent {
    pub workspace_id: String,
    pub version: i64,
}

fn layout_event(ev: &LayoutEvent) -> Event {
    Event::default()
        .event("layout")
        .data(json!({ "workspace_id": ev.workspace_id, "version": ev.version }).to_string())
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

fn workspace_not_found(id: &str) -> Response {
    if let Some(r) = reject_invalid_workspace_id(id) {
        return r;
    }
    json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "workspace_not_found", "id": truncated_id(id) }),
    )
}

/// `GET /v1/workspaces/:id/events` (SSE). First message: the current
/// version; then one `layout` event per new version of this app
/// (`{workspace_id, version}`); a `: keepalive` comment every
/// [`AppState::sse_keepalive`]. New versions normally arrive with a deploy
/// (a restart), so clients mostly see the first message after reconnecting.
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
        return workspace_not_found(&id);
    }
    if id == crate::pixelfed::layout::WORKSPACE_ID {
        match valid_session(&state, &headers).await {
            Ok(Some(false)) => return unauthorized(),
            Ok(_) => {}
            Err(e) => return e.into_response(),
        }
    }
    // Subscribe before reading the current version so nothing published in
    // between is missed (a duplicate version is harmless to clients).
    let rx = state.subscribe_layouts();
    let current = match active_async(&state, &id).await {
        Ok(Some(s)) => LayoutEvent {
            workspace_id: id.clone(),
            version: s.version,
        },
        Ok(None) => return workspace_not_found(&id),
        Err(e) => return e.into_response(),
    };
    tracing::info!(workspace = %id, version = current.version, "layout events subscriber connected");
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
                            let e = layout_event(&LayoutEvent {
                                workspace_id: s.id.clone(),
                                version: served.version,
                            });
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
    fn every_file_in_layouts_is_embedded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../layouts");
        let mut on_disk = Vec::new();
        for app_dir in std::fs::read_dir(&root).unwrap() {
            let app_dir = app_dir.unwrap().path();
            let app_id = app_dir.file_name().unwrap().to_str().unwrap().to_owned();
            for f in std::fs::read_dir(&app_dir).unwrap() {
                let name = f.unwrap().file_name().into_string().unwrap();
                on_disk.push(format!("{app_id}/{name}"));
            }
        }
        on_disk.sort();
        let mut embedded: Vec<String> = FILES
            .iter()
            .map(|f| format!("{}/{}.json", f.app_id, f.name))
            .collect();
        embedded.sort();
        assert_eq!(
            on_disk, embedded,
            "every file in layouts/ must be listed in layouts::FILES (and vice versa)"
        );
        for f in FILES {
            let text =
                std::fs::read_to_string(root.join(f.app_id).join(format!("{}.json", f.name)))
                    .unwrap();
            assert_eq!(text, f.text);
        }
    }

    #[test]
    fn every_file_parses_and_validates() {
        let files = parse_files(FILES).unwrap();
        for a in APPS {
            assert!(
                files.iter().any(|f| f.app_id == a.id && f.number == 1),
                "{} has a 0001.json",
                a.id
            );
        }
    }

    #[test]
    fn first_files_are_the_original_builtin_layouts() {
        let files = parse_files(FILES).unwrap();
        for (app_id, note) in [
            ("pixelfed_home", "Classic feed"),
            ("pixelfed_demo", "Classic feed"),
            ("home", "Counter"),
        ] {
            let f = files
                .iter()
                .find(|f| f.app_id == app_id && f.number == 1)
                .unwrap();
            let mut want = builtin(app_id).unwrap();
            // The demo fixture's root id was `home_stream`; files use the app id.
            if let UiNode::Workspace { id, .. } = &mut want {
                *id = app_id.to_owned();
            }
            assert_eq!(f.layout, want, "{app_id}");
            assert_eq!(f.note, note);
        }
    }

    fn src(app_id: &'static str, name: &'static str, text: &'static str) -> LayoutSource {
        LayoutSource { app_id, name, text }
    }

    #[test]
    fn invalid_files_are_refused_with_their_path() {
        let ok = FILES.iter().find(|f| f.app_id == "home").unwrap().text;
        let cases: [(LayoutSource, &str); 8] = [
            (
                src("home", "0001", "{"),
                "layouts/home/0001.json: not valid JSON",
            ),
            (
                src("home", "1", ok),
                "layouts/home/1.json: file names are 4-digit",
            ),
            (
                src("nope", "0001", ok),
                "layouts/nope/0001.json: 'nope' is not an app",
            ),
            (
                src("home", "0001", r#"{"layout": {}}"#),
                "\"note\" must be a non-empty string",
            ),
            (
                src("home", "0001", r#"{"note": "x"}"#),
                "\"layout\" is missing",
            ),
            (
                src("home", "0001", r#"{"note": "x", "layout": {}, "extra": 1}"#),
                "unknown key 'extra'",
            ),
            (
                src(
                    "home",
                    "0001",
                    r#"{"note": "x", "layout": {"type": "core://Text", "value_expr": "", "style": "Body"}}"#,
                ),
                "layouts/home/0001.json: invalid layout: root must stay a core://Workspace",
            ),
            (
                src("pixelfed_demo", "0001", ok),
                "invalid layout: workspace id changed from 'pixelfed_demo' to 'home'",
            ),
        ];
        for (s, want) in cases {
            let err = parse_files(&[s]).unwrap_err();
            assert!(err.contains(want), "{err} does not contain {want}");
        }
        // Gaps and duplicates.
        let err = parse_files(&[src("home", "0001", ok), src("home", "0003", ok)]).unwrap_err();
        assert_eq!(
            err,
            "layouts/home/0003.json: layout files are numbered without gaps, expected 0002.json next"
        );
        let err = parse_files(&[src("home", "0002", ok)]).unwrap_err();
        assert!(err.contains("expected 0001.json next"), "{err}");
        let err = parse_files(&[src("home", "0001", ok), src("home", "0001", ok)]).unwrap_err();
        assert!(err.contains("listed twice"), "{err}");
        // Structural rules from the agent validator (endpoint prefix).
        let bad = ok.replace("/v1/counters/home/reset", "https://evil/reset");
        let bad: &'static str = Box::leak(bad.into_boxed_str());
        let err = parse_files(&[src("home", "0001", bad)]).unwrap_err();
        assert!(
            err.contains("must be a pod path starting with /v1/"),
            "{err}"
        );
        // New intents are fine in developer-authored files.
        let new_intent = ok.replace("/v1/counters/home/reset", "/v1/counters/home/increment");
        let new_intent: &'static str = Box::leak(new_intent.into_boxed_str());
        parse_files(&[src("home", "0001", new_intent)]).unwrap();
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn unknown_intent_tags_are_refused_in_files() {
        let ok = FILES
            .iter()
            .find(|f| f.app_id == "pixelfed_home")
            .unwrap()
            .text;
        // A typo'd tag (would deserialize as ActionIntent::Unknown).
        let typo = leak(ok.replacen(
            "\"action.navigation.open\"",
            "\"action.navigation.opne\"",
            1,
        ));
        let err = parse_files(&[src("pixelfed_home", "0001", typo)]).unwrap_err();
        assert!(
            err.starts_with(
                "layouts/pixelfed_home/0001.json: invalid layout: unknown action intent \
                 'action.navigation.opne' at /layout/"
            ),
            "{err}"
        );
        // The pointer leads to the object holding the tag.
        let at = err
            .split(" at ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .unwrap();
        let doc: Value = serde_json::from_str(typo).unwrap();
        assert_eq!(doc.pointer(at).unwrap()["intent"], "action.navigation.opne");
        // The removed agent intent and the fallback tag itself.
        for tag in ["action.agent.prompt", "action.unknown"] {
            let bad = leak(ok.replacen("\"action.mutation.optimistic\"", &format!("\"{tag}\""), 1));
            let err = parse_files(&[src("pixelfed_home", "0001", bad)]).unwrap_err();
            assert!(
                err.contains(&format!("unknown action intent '{tag}'")),
                "{err}"
            );
        }
        // Every tag in the shipped files is known.
        for f in FILES {
            let mut found = Vec::new();
            unknown_intent_tags(&serde_json::from_str(f.text).unwrap(), "", &mut found);
            assert!(found.is_empty(), "{}/{}: {found:?}", f.app_id, f.name);
        }
    }

    #[test]
    fn file_hash_ignores_formatting_and_key_order_but_not_content() {
        let one = FILES.iter().find(|f| f.app_id == "home").unwrap().text;
        let parsed = parse_file(&src("home", "0001", one)).unwrap();
        // Compact, keys sorted (serde_json's map), top-level keys reversed.
        let v: Value = serde_json::from_str(one).unwrap();
        let compact = leak(format!(
            "{{\"layout\":{},\"note\":{}}}",
            serde_json::to_string(&v["layout"]).unwrap(),
            v["note"]
        ));
        let pretty = leak(serde_json::to_string_pretty(&v).unwrap());
        for text in [compact, pretty] {
            assert_eq!(
                parse_file(&src("home", "0001", text)).unwrap().file_hash,
                parsed.file_hash
            );
        }
        // Note whitespace is trimmed before hashing (the served note is trimmed too).
        let padded = leak(one.replacen("\"Counter\"", "\"  Counter \"", 1));
        assert_eq!(
            parse_file(&src("home", "0001", padded)).unwrap().file_hash,
            parsed.file_hash
        );
        // Content changes change it: the note, a value, an added explicit default.
        let renoted = leak(one.replacen("\"Counter\"", "\"Counter!\"", 1));
        let retitled = leak(one.replace("The Construct Counter", "Counter v2"));
        for text in [renoted, retitled] {
            assert_ne!(
                parse_file(&src("home", "0001", text)).unwrap().file_hash,
                parsed.file_hash
            );
        }
        assert_eq!(parsed.file_hash.len(), 64);
    }

    #[test]
    fn sync_appends_new_files_once_and_refuses_changes() {
        let store = Store::open_in_memory().unwrap();
        let report = sync(&store).unwrap();
        assert_eq!(report.appended.len(), FILES.len());
        assert!(report.adopted.is_empty() && report.missing.is_empty());
        assert_eq!(sync(&store).unwrap(), SyncReport::default(), "idempotent");
        let (v, layout) = latest(&store, "pixelfed_home").unwrap().unwrap();
        assert_eq!(v, 2, "0002 adds the comments screen");
        let mut feed = layout.clone();
        if let UiNode::Workspace { screens, .. } = &mut feed {
            assert_eq!(screens.len(), 1);
            screens.clear();
        }
        assert_eq!(feed, builtin("pixelfed_home").unwrap(), "same feed as 0001");
        // A client from before screens (schema 1) still gets version 1.
        let old = active_for(&store, "pixelfed_home", 1).unwrap().unwrap();
        assert_eq!(old.version, 1);
        assert_eq!(protocol::required_schema(&old.layout), 1);
        let new = active_for(&store, "pixelfed_home", protocol::SCHEMA_VERSION)
            .unwrap()
            .unwrap();
        assert_eq!(new.version, 2);
        assert_eq!(protocol::required_schema(&new.layout), 2);
        // A second home file.
        let one = FILES.iter().find(|f| f.app_id == "home").copied().unwrap();
        let two_text = one.text.replace("The Construct Counter", "Counter v2");
        let two_text: &'static str = Box::leak(two_text.into_boxed_str());
        let two = src("home", "0002", two_text);
        let mut sources = FILES.to_vec();
        sources.push(two);
        assert_eq!(
            sync_sources(&store, &sources).unwrap().appended,
            [("layouts/home/0002.json".to_owned(), 2)]
        );
        // Changing a deployed file stops the start, naming it.
        let mut changed = FILES.to_vec();
        changed.push(src("home", "0002", one.text));
        assert_eq!(
            sync_sources(&store, &changed).unwrap_err(),
            "layouts/home/0002.json changed after it was deployed as version 2; layout files are \
             append-only. To recover, restore the deployed content of layouts/home/0002.json \
             (git log -p -- layouts/home/0002.json) and put the change in a new file, 0003.json"
        );
        // Reformatting a file (same layout and note) is not a change.
        let reformatted: Value = serde_json::from_str(two_text).unwrap();
        let reformatted: &'static str = Box::leak(
            serde_json::to_string(&reformatted)
                .unwrap()
                .into_boxed_str(),
        );
        let mut same = FILES.to_vec();
        same.push(src("home", "0002", reformatted));
        assert_eq!(sync_sources(&store, &same).unwrap(), SyncReport::default());
        // A rollback deploy without 0002.json starts, serving v2 unchanged.
        let report = sync(&store).unwrap();
        assert_eq!(
            report.missing,
            [
                "layouts/home/0002.json was deployed as version 2 but is missing from this build; \
              serving the recorded history unchanged"
            ]
        );
        assert!(report.appended.is_empty());
        assert_eq!(latest(&store, "home").unwrap().unwrap().0, 2);
    }

    #[test]
    fn rfc3339_formats_utc() {
        for secs in [0, 951_782_400, 1_793_620_960, 4_102_444_799] {
            let s = rfc3339(secs);
            assert_eq!(crate::pixelfed::parse_rfc3339(&s), Some(secs), "{s}");
        }
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }
}
