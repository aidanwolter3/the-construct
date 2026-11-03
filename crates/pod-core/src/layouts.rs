//! Layouts as code, the read-only apps / versions API and the layout push
//! channel.
//!
//! The pod serves the apps its adapters register (one workspace each; dev
//! apps are listed only with `CONSTRUCT_SHOW_DEV_APPS=1`). Each app has ONE
//! linear, append-only layout history (`app_versions`, store migration v7).
//! New versions come only from the developer: files
//! `layouts/<app_id>/NNNN.json` in the repo, embedded in the binary by the
//! app's adapter ([`crate::AppAdapter::layout_files`]) and appended at startup
//! by [`sync`] before the server listens. No HTTP route changes a built-in
//! app's layout; custom apps (the pod user's own, [`crate::custom`]) get
//! their versions from validated writes instead of files.
//!
//! # Adding a layout version
//!
//! 1. Copy the newest file, e.g. `layouts/pixelfed_home/0001.json` to
//!    `layouts/pixelfed_home/0002.json` (4 digits, the next number: numbering
//!    is contiguous from 0001), and edit it. The format is
//!    `{"note": "short changelog line", "schema": N, "layout": <UiNode>}`;
//!    the layout root is the app's `core://Workspace` (id = the app id).
//!    `schema` is the client schema the layout is written for, normally
//!    [`protocol::SCHEMA_VERSION`] (at least what its vocabulary needs,
//!    [`protocol::required_schema`]); clients of other schemas are served the
//!    newest version they can draw ([`protocol::compatibility`], D36).
//! 2. Add one line to the adapter's file list (for Pixelfed, `HOME_FILES` in
//!    `crates/app-pixelfed/src/apps.rs`): `layout_file!("pixelfed_home", "0002"),`
//!    (a test fails if a file in `layouts/` is not listed).
//! 3. `cargo test --workspace` (validates every file), deploy. On start the pod
//!    appends it as the app's next version and pushes it to connected clients
//!    on their next fetch.
//!
//! Files are append-only: once deployed, a file must never change (the pod
//! refuses to start, naming the file and how to recover, if its content
//! differs from what it recorded: [`file_hash`], the hash of the file's own
//! note and layout JSON, independent of how the Rust types serialize it); add
//! a new file instead. `schema` is not part of the hash (it was added to
//! deployed files later) but is recorded and append-only too: to stop clients
//! drawing old versions after a breaking schema change, add an entry to
//! [`protocol::RETIRED`] rather than editing files. A gap in the numbering, a file that is not a valid
//! layout (structure checked by [`crate::validate::validate_structure`]), an
//! unknown action intent tag, a root id that is not the app id, or a new file
//! numbered below a deployed one also stop the start. A deployed file missing
//! from the build (a rollback deploy, an older branch) only logs a warning:
//! the recorded history is served unchanged.
//!
//! `X-Construct-Layout-Version` on `GET /v1/workspaces/:id` carries the
//! served version (the newest the client can draw); the SSE `version` carries
//! the app's latest version number.

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

use crate::adapter::{AppInfo, AuthPolicy, Catalog};
use crate::session::{Caller, unauthorized};
use crate::store::{AppVersion, FileVersion, Store, SyncReport};
use crate::{ApiError, AppState, blocking, json_error};

/// Response header carrying the served app version (the client's one
/// monotonic "layout version" per workspace).
pub const VERSION_HEADER: &str = "x-construct-layout-version";
/// Default SSE keepalive interval.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Default cap on open SSE streams per session token. A client keeps one
/// stream open; the headroom covers a pod token shared by a few devices and
/// dead connections the server has not noticed yet.
pub const DEFAULT_SSE_STREAMS_PER_TOKEN: usize = 8;
/// Capacity of the layout broadcast channel (a lagging subscriber is caught
/// up with the current version, so losing intermediate events is harmless).
pub const CHANNEL_CAPACITY: usize = 64;
/// The version a [`LayoutEvent`] carries when a custom app was deleted:
/// its streams send `event: deleted` and end.
pub const DELETED_VERSION: i64 = -1;
/// Default and largest page of `GET /v1/apps/:id/versions`.
pub const DEFAULT_VERSIONS_LIMIT: i64 = 20;
pub const MAX_VERSIONS_LIMIT: i64 = 100;

// ---------- layout files ----------

/// One embedded layout file: `layouts/<app>/<name>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSource {
    pub app_id: &'static str,
    /// `NNNN` (4 digits).
    pub name: &'static str,
    pub text: &'static str,
}

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
    /// The client schema the layout is written for (`"schema"`).
    pub schema: u32,
    /// Named parts other layouts may reference (`"parts"`, decision
    /// `layout-parts`).
    pub parts: std::collections::BTreeMap<String, UiNode>,
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
            schema: self.schema,
            meta_json: (!self.parts.is_empty()).then(|| {
                let parts: serde_json::Map<String, Value> = self
                    .parts
                    .iter()
                    .map(|(k, v)| (k.clone(), node_value(v)))
                    .collect();
                json!({ "parts": parts }).to_string()
            }),
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
    file_hash_with_parts(note, layout, None)
}

/// [`file_hash`] of a file that also declares `"parts"` (they are hashed
/// with it; files without parts hash as before).
pub fn file_hash_with_parts(note: &str, layout: &Value, parts: Option<&Value>) -> String {
    let mut doc = json!({ "layout": layout, "note": note });
    if let Some(p) = parts {
        doc["parts"] = p.clone();
    }
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
pub fn unknown_intent_tags(v: &Value, at: &str, out: &mut Vec<(String, String)>) {
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
                let is_intent = k == "on_click" || k == "on_submit" || k.ends_with("_intent");
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

/// Parse and validate one file of an app in `catalog` (`Err` names the file).
pub fn parse_file(catalog: &Catalog, src: &LayoutSource) -> Result<LayoutFile, String> {
    let path = format!("layouts/{}/{}.json", src.app_id, src.name);
    let fail = |e: String| format!("{path}: {e}");
    if !catalog.is_app(src.app_id) {
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
        return Err(fail(
            r#"expected {"note": "...", "schema": N, "layout": {...}}"#.into(),
        ));
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "note" | "schema" | "layout" | "parts"))
    {
        return Err(fail(format!(
            "unknown key '{k}' (only \"note\", \"schema\", \"layout\" and \"parts\")"
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
    let parsed = crate::validate::validate_structure(src.app_id, layout)
        .map_err(|e| fail(format!("invalid layout: {e}")))?;
    // An unknown intent tag would deserialize as `ActionIntent::Unknown` and
    // ship as a dead action (lenient only for legacy stored versions).
    if crate::validate::intents(&parsed)
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
    // Node types this build doesn't know are only allowed where they were
    // deployed before they were removed (D37), the adapter's legacy files.
    let unknown = crate::validate::unknown_node_types(&parsed);
    if let Some(t) = unknown.first()
        && !catalog.is_legacy(src.app_id, number)
    {
        return Err(fail(format!(
            "invalid layout: unknown node type '{t}' (a typo? this build does not know it)"
        )));
    }
    // Layouts reach only what their app's data sources declare (decision
    // `custom-apps-and-data-sources`).
    crate::source::check_routes(&parsed, &catalog.app_sources(src.app_id))
        .map_err(|e| fail(format!("invalid layout: {e}")))?;
    let schema = obj
        .get("schema")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| (1..=protocol::SCHEMA_VERSION).contains(n))
        .ok_or_else(|| {
            fail(format!(
                "\"schema\" must be the client schema the layout is written for, 1..={} \
                 (normally the current one, {})",
                protocol::SCHEMA_VERSION,
                protocol::SCHEMA_VERSION
            ))
        })?;
    let needs = protocol::required_schema(&parsed);
    if needs > schema {
        return Err(fail(format!(
            "\"schema\" is {schema} but the layout uses vocabulary from schema {needs}"
        )));
    }
    let raw_parts = obj.get("parts");
    let mut parts = std::collections::BTreeMap::new();
    if let Some(p) = raw_parts {
        let Value::Object(map) = p else {
            return Err(fail("\"parts\" must be an object of named nodes".into()));
        };
        for (name, v) in map {
            if name.is_empty()
                || name.contains(':')
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(fail(format!("part name '{name}' must be [a-z0-9_]+")));
            }
            let node: UiNode = serde_json::from_value(v.clone())
                .map_err(|e| fail(format!("part '{name}' is not a node: {e}")))?;
            if let Some(t) = crate::validate::unknown_node_types(&node).first() {
                return Err(fail(format!("part '{name}': unknown node type '{t}'")));
            }
            crate::source::check_routes(&node, &catalog.app_sources(src.app_id))
                .map_err(|e| fail(format!("part '{name}': {e}")))?;
            let needs = protocol::required_schema(&node);
            if needs > schema {
                return Err(fail(format!(
                    "part '{name}' uses vocabulary from schema {needs}, above the file's {schema}"
                )));
            }
            parts.insert(name.clone(), node);
        }
    }
    let file_hash = file_hash_with_parts(&note, layout, raw_parts);
    Ok(LayoutFile {
        parts,
        app_id: src.app_id.to_owned(),
        number,
        path,
        note,
        layout: parsed,
        file_hash,
        schema,
    })
}

/// Parse every source; per app the numbers must be exactly 1..=N.
pub fn parse_files(catalog: &Catalog, sources: &[LayoutSource]) -> Result<Vec<LayoutFile>, String> {
    let mut files = sources
        .iter()
        .map(|s| parse_file(catalog, s))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|a, b| (a.app_id.as_str(), a.number).cmp(&(b.app_id.as_str(), b.number)));
    for a in catalog.apps() {
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

/// Append every new layout file of the catalog's apps (one transaction) to
/// the store, and record the declared schema of files deployed before it
/// existed; see the module docs. Blocking; idempotent. `Err` is the message
/// to stop the start with.
pub fn sync(store: &Store, catalog: &Catalog) -> Result<SyncReport, String> {
    sync_sources(store, catalog, catalog.files())
}

/// [`sync`] over any set of sources (tests).
pub fn sync_sources(
    store: &Store,
    catalog: &Catalog,
    sources: &[LayoutSource],
) -> Result<SyncReport, String> {
    let files = parse_files(catalog, sources)?;
    let apps: Vec<(&str, Vec<FileVersion>)> = catalog
        .apps()
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

/// An app's first layout (its `0001.json`). Panics if the embedded files are
/// invalid (the pod would not start either).
pub fn builtin(catalog: &Catalog, workspace_id: &str) -> Option<UiNode> {
    let src = catalog
        .files()
        .iter()
        .find(|f| f.app_id == workspace_id && f.name == "0001")?;
    Some(
        parse_file(catalog, src)
            .expect("embedded layout files are valid")
            .layout,
    )
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
pub fn active(
    store: &Store,
    catalog: &Catalog,
    workspace_id: &str,
) -> rusqlite::Result<Option<Served>> {
    if !catalog.is_app(workspace_id) && !crate::custom::is_custom_id(workspace_id) {
        return Ok(None);
    }
    match store.app_latest(workspace_id)? {
        Some((v, json)) => Ok(Some(Served {
            version: v.version,
            layout: from_json(&crate::parts::serve(store, catalog, &json, None))?,
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

/// The newest version of an app a client of schema `schema` can draw (by the
/// version's declared schema, [`protocol::compatibility`]): metadata only.
/// `None` if the app has no version that fits. Blocking.
pub fn newest_compatible(
    store: &Store,
    app_id: &str,
    schema: u32,
) -> rusqlite::Result<Option<AppVersion>> {
    let mut before = None;
    loop {
        let page = store.app_versions(app_id, before, DEFAULT_VERSIONS_LIMIT)?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        before = Some(last.version);
        if let Some(v) = page.into_iter().find(|v| {
            protocol::compatibility(v.schema(), schema) == protocol::Compatibility::Compatible
        }) {
            return Ok(Some(v));
        }
    }
}

/// The newest version of an app a client of schema `schema` can draw
/// ([`newest_compatible`]) with its layout; `None` if the workspace is not an
/// app or no version fits. Blocking.
pub fn active_for(
    store: &Store,
    catalog: &Catalog,
    workspace_id: &str,
    schema: u32,
) -> rusqlite::Result<Option<Served>> {
    if !catalog.is_app(workspace_id) && !crate::custom::is_custom_id(workspace_id) {
        return Ok(None);
    }
    let Some(latest) = store.app_latest_meta(workspace_id)? else {
        return Ok(None);
    };
    let Some(v) = newest_compatible(store, workspace_id, schema)? else {
        return Ok(None);
    };
    let Some((_, json)) = store.app_version(workspace_id, v.version)? else {
        return Ok(None);
    };
    if v.version != latest.version {
        tracing::info!(
            app = workspace_id,
            latest = latest.version,
            served = v.version,
            schema,
            "served an older layout version the client can draw"
        );
    }
    Ok(Some(Served {
        version: v.version,
        layout: from_json(&crate::parts::serve(store, catalog, &json, Some(schema)))?,
    }))
}

/// Async [`active_for`] with the schema read from the request headers.
pub async fn active_for_client(
    state: &Arc<AppState>,
    workspace_id: &str,
    headers: &HeaderMap,
) -> Result<Option<Served>, ApiError> {
    let ws = workspace_id.to_owned();
    let schema = client_schema(headers);
    let st = state.clone();
    blocking(state, move |s| active_for(s, st.catalog(), &ws, schema)).await
}

/// `(version, layout)`; see [`active`]. Blocking.
pub fn latest(
    store: &Store,
    catalog: &Catalog,
    workspace_id: &str,
) -> rusqlite::Result<Option<(i64, UiNode)>> {
    Ok(active(store, catalog, workspace_id)?.map(|s| (s.version, s.layout)))
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

/// 400 for a malformed id, 404 `app_not_found` for an unknown one: the
/// app's id.
async fn known_app(state: &Arc<AppState>, id: &str) -> Result<String, Response> {
    if let Some(r) = reject_invalid_workspace_id(id) {
        return Err(r);
    }
    match state.app(id).await {
        Ok(Some(a)) => Ok(a.id().to_owned()),
        Ok(None) => Err(json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "app_not_found", "id": id }),
        )),
        Err(e) => Err(e.into_response()),
    }
}

/// One version's metadata for a client of schema `client`: `schema` (what it
/// was written for), `compatible`, and `incompatible`
/// (`update_required` | `retired`) when the client cannot draw it.
fn version_json(v: &AppVersion, client: u32) -> Value {
    let compat = protocol::compatibility(v.schema(), client);
    let mut out = json!({
        "version": v.version,
        "created_at": rfc3339(v.created_at),
        "source": v.source,
        "schema": v.schema(),
        "compatible": compat.reason().is_none(),
    });
    if let Some(n) = &v.note {
        out["note"] = json!(n);
    }
    if let Some(a) = &v.author {
        out["author"] = json!(a);
    }
    if let Some(r) = compat.reason() {
        out["incompatible"] = json!(r);
    }
    out
}

/// `GET /v1/apps[?all=1]` -> `{"apps": [{id, title, icon, kind, hidden,
/// latest_version, latest_compatible_version, updated_at}]}` (public):
/// built-in apps, then custom ones (`kind` `builtin` | `custom`) oldest
/// first. Hidden apps (`PUT /v1/apps/:id/hidden`) are listed only with
/// `?all=1`. Dev apps only with `CONSTRUCT_SHOW_DEV_APPS=1`.
/// `latest_compatible_version` is the newest version the client
/// ([`protocol::SCHEMA_HEADER`]) can draw, i.e. what opening the app serves
/// it (null if none fits).
async fn list_apps(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let schema = client_schema(&headers);
    let all = query.get("all").is_some_and(|v| v == "1" || v == "true");
    let builtin: Vec<AppInfo> = state
        .catalog()
        .apps()
        .filter(|a| !a.dev || state.show_dev_apps())
        .copied()
        .collect();
    let st = state.clone();
    let listed = blocking(&state, move |s| {
        let hidden = crate::custom::hidden_apps(s)?;
        let dismissed = crate::custom::dismissed(s)?;
        let custom = s.with_conn(crate::custom::custom_apps_on)?;
        let mut apps: Vec<crate::custom::AppRef> = builtin
            .into_iter()
            .map(crate::custom::AppRef::Builtin)
            .chain(custom.into_iter().map(crate::custom::AppRef::Custom))
            .collect();
        if !all {
            apps.retain(|a| !hidden.iter().any(|h| h == a.id()));
        }
        apps.into_iter()
            .map(|a| {
                let latest = s.app_latest_meta(a.id())?;
                let compatible = newest_compatible(s, a.id(), schema)?.map(|v| v.version);
                let is_hidden = hidden.iter().any(|h| h == a.id());
                let updates = match &a {
                    crate::custom::AppRef::Custom(c) => {
                        crate::custom::updates_from(s, st.catalog(), &c.id, schema, &dismissed)?
                    }
                    crate::custom::AppRef::Builtin(_) => None,
                };
                Ok((a, is_hidden, latest, compatible, updates))
            })
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .await;
    let listed = match listed {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    let apps: Vec<Value> = listed
        .iter()
        .map(|(a, hidden, v, compatible, updates)| {
            let mut row = json!({
                "id": a.id(),
                "title": a.title(),
                "icon": a.icon(),
                "kind": a.kind(),
                "hidden": hidden,
                "latest_version": v.as_ref().map(|v| v.version),
                "latest_compatible_version": compatible,
                "updated_at": v.as_ref().map(|v| rfc3339(v.created_at)),
            });
            if let Some(u) = updates {
                row["updates_from"] = u.clone();
            }
            row
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
/// `{app_id, latest_version, latest_compatible_version, versions: [{version,
/// created_at, source, note?, schema, compatible, incompatible?}],
/// next_before?}` newest first; `next_before` iff older versions exist (pass
/// it as `before` for the next page). `compatible` and
/// `latest_compatible_version` are for the client's
/// [`protocol::SCHEMA_HEADER`].
async fn app_versions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let app_id = match known_app(&state, &id).await {
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
    let schema = client_schema(&headers);
    let lookup = app_id.clone();
    let result = blocking(&state, move |s| {
        let latest = s.app_latest_meta(&lookup)?.map(|v| v.version);
        let compatible = newest_compatible(s, &lookup, schema)?.map(|v| v.version);
        let rows = s.app_versions(&lookup, before, limit + 1)?;
        Ok((latest, compatible, rows))
    })
    .await;
    let (latest, compatible, rows) = match result {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let more = rows.len() as i64 > limit;
    let page = &rows[..rows.len().min(limit as usize)];
    let mut body = json!({
        "app_id": app_id,
        "latest_version": latest,
        "latest_compatible_version": compatible,
        "versions": page
            .iter()
            .map(|v| version_json(v, schema))
            .collect::<Vec<_>>(),
    });
    if more && let Some(last) = page.last() {
        body["next_before"] = json!(last.version);
    }
    Json(body).into_response()
}

/// `GET /v1/apps/:id/versions/:n` -> `{app_id, version, created_at, source,
/// note?, schema, compatible, incompatible?, layout}` (the layout is served
/// even when incompatible: it is history to look at).
async fn app_version(
    State(state): State<Arc<AppState>>,
    Path((id, n)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let app_id = match known_app(&state, &id).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(n) = n.parse::<i64>().ok().filter(|n| *n >= 1) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "integer >= 1" } }),
        );
    };
    let lookup = app_id.clone();
    let schema = client_schema(&headers);
    let st = state.clone();
    let found = blocking(&state, move |s| {
        // A custom version's built-in parts, as this client would be served them.
        Ok(s.app_version(&lookup, n)?.map(|(v, json)| {
            let served = crate::parts::serve(s, st.catalog(), &json, Some(schema));
            (v, served)
        }))
    })
    .await;
    match found {
        Ok(Some((v, stored))) => {
            let mut body = version_json(&v, client_schema(&headers));
            body["app_id"] = json!(app_id);
            body["layout"] = match from_json(&stored) {
                Ok(layout) => node_value(&layout),
                // A legacy version this build cannot read as a UiNode: serve
                // what was stored rather than fail (it is history to look at).
                Err(e) => match serde_json::from_str::<Value>(&stored) {
                    Ok(raw) => {
                        tracing::warn!(
                            app = %app_id,
                            version = n,
                            "serving a stored layout that is not a valid UiNode as is: {e}"
                        );
                        raw
                    }
                    Err(e) => {
                        tracing::error!(
                            app = %app_id,
                            version = n,
                            "stored layout is not JSON: {e}"
                        );
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            json!({ "error": "corrupt_version", "app_id": app_id, "version": n,
                                    "detail": "the stored layout of this version is not valid JSON" }),
                        );
                    }
                },
            };
            Json(body).into_response()
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "app_id": app_id, "version": n }),
        ),
        Err(e) => e.into_response(),
    }
}

// ---------- push ----------

use crate::notify::NotifyEvent;

/// An app's latest version changed (what the SSE channel carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEvent {
    pub workspace_id: String,
    pub version: i64,
}

/// A session's data from one source changed (D50): `event: data` to that
/// session's subscribers of the apps that draw on `source` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataEvent {
    /// A data source id (an adapter's name).
    pub source: String,
    pub token_hash: String,
}

fn data_event(workspace_id: &str) -> Event {
    Event::default()
        .event("data")
        .data(json!({ "workspace_id": workspace_id }).to_string())
}

fn layout_event(ev: &LayoutEvent) -> Event {
    Event::default()
        .event("layout")
        .data(json!({ "workspace_id": ev.workspace_id, "version": ev.version }).to_string())
}

/// The next notification event of a stream that has notifications (never
/// resolves for one that doesn't). A lag only loses events (the client
/// refreshes its counts when it reconnects), so it is skipped; a closed
/// channel stops notifications on this stream.
/// The next tick of a stream's session re-check; never without one.
async fn tick(i: &mut Option<tokio::time::Interval>) {
    match i {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Whether the session an events stream was opened with is still valid
/// (`None`: no session, nothing to revoke). A store error counts as invalid:
/// the client reconnects and is checked afresh.
async fn session_still_valid(state: &Arc<AppState>, hash: Option<&str>) -> bool {
    let Some(hash) = hash.map(str::to_owned) else {
        return true;
    };
    let ttl = state.session_ttl_secs();
    let valid = blocking(state, move |s| s.session_valid(&hash, ttl))
        .await
        .unwrap_or(false);
    if !valid {
        tracing::info!("events stream closed: its session is no longer valid");
    }
    valid
}

/// One open SSE stream of a session token, counted in
/// [`AppState::sse_streams`] until dropped.
pub struct SseSlot {
    state: Arc<AppState>,
    token_hash: String,
}

impl AppState {
    /// Take a stream slot for `token_hash`, or `None` when it already has
    /// [`Self::sse_streams_per_token`] streams open.
    pub fn sse_slot(self: &Arc<Self>, token_hash: &str) -> Option<SseSlot> {
        let mut open = self.sse_streams.lock().unwrap_or_else(|e| e.into_inner());
        let n = open.entry(token_hash.to_owned()).or_default();
        if *n >= self.sse_streams_per_token() {
            return None;
        }
        *n += 1;
        Some(SseSlot {
            state: self.clone(),
            token_hash: token_hash.to_owned(),
        })
    }
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        let mut open = self
            .state
            .sse_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(n) = open.get_mut(&self.token_hash) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(&self.token_hash);
            }
        }
    }
}

async fn recv_notify(rx: &mut Option<broadcast::Receiver<NotifyEvent>>) -> Option<NotifyEvent> {
    let Some(r) = rx.as_mut() else {
        return std::future::pending().await;
    };
    match r.recv().await {
        Ok(e) => Some(e),
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(skipped = n, "notification subscriber lagged");
            None
        }
        Err(broadcast::error::RecvError::Closed) => {
            *rx = None;
            None
        }
    }
}

fn notify_event(ev: &NotifyEvent) -> Event {
    match ev {
        NotifyEvent::New(n) => Event::default()
            .event(protocol::notify::NOTIFICATION_EVENT)
            .data(serde_json::to_string(n).unwrap_or_default()),
        NotifyEvent::Read(r) => Event::default()
            .event(protocol::notify::NOTIFICATIONS_READ_EVENT)
            .data(serde_json::to_string(r).unwrap_or_default()),
    }
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

/// The version a layout event announces to a subscriber: the newest one a
/// client of `schema` can draw ([`newest_compatible`]), or the app's latest
/// when the subscriber sent no [`protocol::SCHEMA_HEADER`] (engines from before
/// D38 send none on the events channel but draw schema 2 or 3, so treating
/// them as schema 1 would hide new versions from them). Blocking.
fn announced_version(
    store: &Store,
    app_id: &str,
    schema: Option<u32>,
) -> rusqlite::Result<Option<i64>> {
    Ok(match schema {
        Some(schema) => newest_compatible(store, app_id, schema)?,
        None => store.app_latest_meta(app_id)?,
    }
    .map(|v| v.version))
}

async fn announced_async(
    state: &Arc<AppState>,
    app_id: &str,
    schema: Option<u32>,
) -> Result<Option<i64>, ApiError> {
    let id = app_id.to_owned();
    blocking(state, move |s| announced_version(s, &id, schema)).await
}

/// `GET /v1/workspaces/:id/events` (SSE). First message: the current
/// version; then one `layout` event (`{workspace_id, version}`) each time a
/// new version of this app changes what the subscriber would be served; a
/// `: keepalive` comment every [`AppState::sse_keepalive`]. With
/// [`protocol::SCHEMA_HEADER`] the version is the newest one that client can
/// draw, so a version it can't draw causes no event (and no refetch); without
/// it, the app's latest ([`announced_version`]). New versions normally arrive
/// with a deploy (a restart), so clients mostly see the first message after
/// reconnecting.
///
/// Auth mirrors `GET /v1/workspaces/:id`: no bearer is fine (events carry only
/// ids and numbers, no content); for an app with [`AuthPolicy::SignIn`] a
/// bearer that is not a valid session with that sign-in is 401 so the client
/// learns it is stale.
///
/// A stream opened with a session ends once that session stops being valid
/// (logout, `server pods rotate`, expiry): the session is checked again
/// before each private event (`notification`, `data`) and on every keepalive
/// interval, so the stream never forwards anything after the revocation and
/// closes within one interval even when idle. A re-check rather than a
/// revocation broadcast, because `server pods rotate` runs in another
/// process. One session token may hold at most
/// [`AppState::sse_streams_per_token`] streams; another one is 429
/// `too_many_streams`.
async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let app = match state.app(&id).await {
        Ok(Some(app)) => app,
        Ok(None) => return workspace_not_found(&id),
        Err(e) => return e.into_response(),
    };
    // Events carry only version numbers, so anyone may listen; only a
    // session the pod no longer knows is told so (a valid one lacking the
    // app's sign-in, e.g. a pod token's, D46, must not be dropped).
    let caller = match state.caller(&headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    if let (AuthPolicy::SignIn(_), Caller::Stale) = (app.auth(), &caller) {
        return unauthorized();
    }
    let slot = match caller.session() {
        Some(session) => match state.sse_slot(&session.token_hash) {
            Some(slot) => Some(slot),
            None => {
                tracing::warn!(workspace = %id, "events stream refused: too many open for this token");
                return json_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": "too_many_streams", "limit": state.sse_streams_per_token() }),
                );
            }
        },
        None => None,
    };
    // Notifications (D52) are private: only a valid session hears them, for
    // every app of the pod. A device with a stream open gets no Web Push.
    let notify_rx = caller.session().map(|_| state.subscribe_notifications());
    let live = match (&caller, crate::notify::device_id(&headers)) {
        (Caller::Signed(_), Some(device)) => Some(state.device_live(&device)),
        _ => None,
    };
    // The session's own `data` events (D50) need to know who listens, and
    // which sources this app draws on.
    let session_hash = caller.session().map(|s| s.token_hash.clone());
    let sources = state.sources_of(&app);
    let schema = headers
        .contains_key(protocol::SCHEMA_HEADER)
        .then(|| client_schema(&headers));
    // Subscribe before reading the current version so nothing published in
    // between is missed (a duplicate version is harmless to clients).
    let rx = state.subscribe_layouts();
    let data_rx = state.subscribe_data();
    let current = match announced_async(&state, &id, schema).await {
        Ok(Some(v)) => v,
        Ok(None) => return workspace_not_found(&id),
        Err(e) => return e.into_response(),
    };
    tracing::info!(workspace = %id, version = current, ?schema, "layout events subscriber connected");
    struct S {
        first: bool,
        /// The app was deleted: the `deleted` event went out, end.
        done: bool,
        sent: i64,
        schema: Option<u32>,
        rx: broadcast::Receiver<LayoutEvent>,
        notify_rx: Option<broadcast::Receiver<NotifyEvent>>,
        _live: Option<crate::notify::LiveDevice>,
        data_rx: broadcast::Receiver<DataEvent>,
        session_hash: Option<String>,
        sources: Vec<&'static str>,
        recheck: Option<tokio::time::Interval>,
        _slot: Option<SseSlot>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        state: Arc<AppState>,
        id: String,
    }
    let recheck = session_hash.as_ref().map(|_| {
        let every = state.sse_keepalive();
        let mut i = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        i
    });
    let stream = futures_util::stream::unfold(
        S {
            first: true,
            done: false,
            sent: current,
            schema,
            rx,
            notify_rx,
            _live: live,
            data_rx,
            session_hash,
            sources,
            recheck,
            _slot: slot,
            shutdown: state.shutdown_signal(),
            state: state.clone(),
            id,
        },
        |mut s| async move {
            let event = |s: &S| {
                layout_event(&LayoutEvent {
                    workspace_id: s.id.clone(),
                    version: s.sent,
                })
            };
            if s.done {
                return None;
            }
            if s.first {
                s.first = false;
                let e = event(&s);
                return Some((Ok::<_, std::convert::Infallible>(e), s));
            }
            loop {
                enum Got {
                    Layout(Result<LayoutEvent, broadcast::error::RecvError>),
                    Notify(Option<NotifyEvent>),
                    Data(Result<DataEvent, broadcast::error::RecvError>),
                    Recheck,
                    Stop,
                }
                let got = tokio::select! {
                    r = s.rx.recv() => Got::Layout(r),
                    n = recv_notify(&mut s.notify_rx) => Got::Notify(n),
                    d = s.data_rx.recv() => Got::Data(d),
                    _ = tick(&mut s.recheck) => Got::Recheck,
                    // Server shutting down: end the stream so graceful
                    // shutdown is not held open by subscribers.
                    _ = s.shutdown.wait_for(|stop| *stop) => Got::Stop,
                };
                let received = match got {
                    Got::Stop => return None,
                    Got::Layout(r) => r,
                    Got::Recheck => {
                        if !session_still_valid(&s.state, s.session_hash.as_deref()).await {
                            return None;
                        }
                        continue;
                    }
                    Got::Notify(Some(e)) => {
                        if !session_still_valid(&s.state, s.session_hash.as_deref()).await {
                            return None;
                        }
                        return Some((Ok(notify_event(&e)), s));
                    }
                    Got::Notify(None) => continue,
                    Got::Data(d) => {
                        let mine = match &d {
                            Ok(d) => {
                                s.sources.contains(&d.source.as_str())
                                    && s.session_hash.as_deref() == Some(d.token_hash.as_str())
                            }
                            // Lagged: a reload is never wrong.
                            Err(broadcast::error::RecvError::Lagged(_)) => s.session_hash.is_some(),
                            Err(broadcast::error::RecvError::Closed) => return None,
                        };
                        if mine {
                            if !session_still_valid(&s.state, s.session_hash.as_deref()).await {
                                return None;
                            }
                            let e = data_event(&s.id);
                            return Some((Ok(e), s));
                        }
                        continue;
                    }
                };
                let lagged = match received {
                    Ok(ev) if ev.workspace_id == s.id && ev.version == DELETED_VERSION => {
                        s.done = true;
                        let e = Event::default()
                            .event("deleted")
                            .data(json!({ "workspace_id": s.id }).to_string());
                        return Some((Ok(e), s));
                    }
                    Ok(ev) if ev.workspace_id == s.id => false,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(workspace = %s.id, skipped = n, "layout subscriber lagged; sending current");
                        true
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                };
                // Re-read what this subscriber would be served now; only a
                // change is announced (always after a lag).
                let Ok(Some(v)) = announced_async(&s.state, &s.id, s.schema).await else {
                    continue;
                };
                if v == s.sent && !lagged {
                    continue;
                }
                s.sent = v;
                let e = event(&s);
                return Some((Ok(e), s));
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
