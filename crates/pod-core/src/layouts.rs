//! Layouts as code, the read-only apps / versions API and the layout push
//! channel.
//!
//! The pod serves the apps its adapters register (one workspace each; dev
//! apps are listed only with `CONSTRUCT_SHOW_DEV_APPS=1`). Each app has ONE
//! linear, append-only layout history (`app_versions`, store migration v7).
//! New versions come only from the developer: files
//! `crates/<adapter>/layouts/<app_id>/NNNN.json` in the repo, embedded in the
//! binary by the app's adapter ([`crate::AppAdapter::layout_files`]) and appended at startup
//! by [`sync`] before the server listens. No HTTP route changes a built-in
//! app's layout; custom apps (the pod user's own, [`crate::custom`]) get
//! their versions from validated writes instead of files.
//!
//! # Adding a layout version
//!
//! 1. Copy the newest file, e.g. `crates/app-pixelfed/layouts/pixelfed/0011.json`
//!    to `.../pixelfed/0012.json` (4 digits, the next number: numbering
//!    is contiguous from the app's oldest file), and edit it. The format is
//!    `{"note": "short changelog line", "layout": <UiNode>}`; the layout root
//!    is the app's `core://Workspace` (id = the app id). Clients are served
//!    the newest version whose features they all draw
//!    ([`protocol::ClientCaps::can_draw`], decision `named-vocabulary`).
//!    Deployed files may still carry the `"schema"` of D36; it is ignored.
//! 2. Nothing to list: the adapter's `build.rs` embeds every
//!    `layouts/<app_id>/*.json` of its crate (`crates/pod-core/build_support/`). The
//!    layout features a file uses are worked out, not declared
//!    ([`protocol::required_features`], recorded per version).
//! 3. `cargo test --workspace` (validates every file), deploy. On start the pod
//!    appends it as the app's next version and pushes it to connected clients
//!    on their next fetch.
//!
//! Files are append-only: once deployed, a file must never change (the pod
//! refuses to start, naming the file and how to recover, if its content
//! differs from what it recorded: [`file_hash`], the hash of the file's own
//! note and layout JSON, independent of how the Rust types serialize it); add
//! a new file instead. To stop clients drawing old versions, remove the
//! feature they use (move its file to `crates/protocol/features/removed/`)
//! rather than editing files. Old files may be deleted from the repo, oldest
//! first (decision `retired-layout-files`): an app's files then start above
//! `0001`, pods that recorded the deleted ones keep them in their history
//! (no warning), and new pods start from the oldest file present. A gap in
//! the numbering, a file that is not a valid
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

/// One embedded layout file: `<dir>/<app>/<name>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSource {
    /// The folder of the app folders, as the repository names it
    /// (`crates/app-events/layouts`): only for messages.
    pub dir: &'static str,
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
    /// `<dir>/<app>/NNNN.json` ([`LayoutSource::dir`]).
    pub path: String,
    pub note: String,
    pub layout: UiNode,
    /// [`file_hash`] of the file's note and layout.
    pub file_hash: String,
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

/// SHA-256 of `v`'s canonical JSON (sorted keys, no whitespace).
pub fn canonical_hash(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
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
    parse_file_with(catalog, src, catalog.component_defs())
}

/// [`parse_file`] with these server components (`defs`) instead of this
/// build's: what a file expanded to when its components were older.
pub fn parse_file_with(
    catalog: &Catalog,
    src: &LayoutSource,
    defs: &crate::parts::Pending,
) -> Result<LayoutFile, String> {
    let path = format!("{}/{}/{}.json", src.dir, src.app_id, src.name);
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
        return Err(fail(r#"expected {"note": "...", "layout": {...}}"#.into()));
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "note" | "schema" | "layout" | "parts"))
    {
        return Err(fail(format!(
            "unknown key '{k}' (only \"note\", \"layout\" and \"parts\")"
        )));
    }
    let note = obj
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| fail("\"note\" must be a non-empty string".into()))?
        .to_owned();
    let written = obj
        .get("layout")
        .ok_or_else(|| fail("\"layout\" is missing".into()))?;
    // Server components (decision `plumbing-and-porcelain`) are expanded
    // here: the pod records the plumbing the file stands for.
    let expanded;
    let layout = if src.text.contains(crate::components::USE_TYPE) {
        expanded = crate::parts::expand_file_with(catalog, written, defs)
            .map_err(|e| fail(format!("invalid layout: {e}")))?;
        &expanded
    } else {
        written
    };
    let parsed = crate::validate::validate_structure(src.app_id, layout)
        .map_err(|e| fail(format!("invalid layout: {e}")))?;
    // An unknown intent tag would deserialize as `ActionIntent::Unknown` and
    // ship as a dead action (lenient only for legacy stored versions).
    if crate::validate::intents(&parsed)
        .iter()
        .any(|i| matches!(i, protocol::ActionIntent::Unknown))
    {
        let mut found = Vec::new();
        // Pointers into the file as written.
        unknown_intent_tags(written, "/layout", &mut found);
        let what = match found.first() {
            Some((at, tag)) => format!("unknown action intent '{tag}' at {at}"),
            None => "unknown action intent".to_owned(),
        };
        return Err(fail(format!(
            "invalid layout: {what} (a typo? this build does not know it)"
        )));
    }
    // Node types this build doesn't know are refused (D37). Versions deployed
    // before a node type was removed stay in pods' history, served as they are.
    let unknown = crate::validate::unknown_node_types(&parsed);
    if let Some(t) = unknown.first() {
        return Err(fail(format!(
            "invalid layout: unknown node type '{t}' (a typo? this build does not know it)"
        )));
    }
    // Layouts reach only what their app's data sources declare (decision
    // `custom-apps-and-data-sources`).
    crate::source::check_routes(&parsed, &catalog.app_sources(src.app_id))
        .map_err(|e| fail(format!("invalid layout: {e}")))?;
    // `"schema"` (D36): deployed files keep it, so it is allowed and not
    // read; features replaced it (decision `named-vocabulary`).
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
            let v = if v.to_string().contains(crate::components::USE_TYPE) {
                crate::parts::expand_file_with(catalog, v, defs)
                    .map_err(|e| fail(format!("part '{name}': {e}")))?
            } else {
                v.clone()
            };
            let node: UiNode = serde_json::from_value(v)
                .map_err(|e| fail(format!("part '{name}' is not a node: {e}")))?;
            if let Some(t) = crate::validate::unknown_node_types(&node).first() {
                return Err(fail(format!("part '{name}': unknown node type '{t}'")));
            }
            crate::source::check_routes(&node, &catalog.app_sources(src.app_id))
                .map_err(|e| fail(format!("part '{name}': {e}")))?;
            parts.insert(name.clone(), node);
        }
    }
    let file_hash = file_hash_with_parts(&note, written, raw_parts);
    Ok(LayoutFile {
        parts,
        app_id: src.app_id.to_owned(),
        number,
        path,
        note,
        layout: parsed,
        file_hash,
    })
}

/// Parse every source; per app the numbers must run without gaps from the
/// app's lowest file (`0001`, or a later one once older files are retired:
/// decision `retired-layout-files`).
pub fn parse_files(catalog: &Catalog, sources: &[LayoutSource]) -> Result<Vec<LayoutFile>, String> {
    let mut files = sources
        .iter()
        .map(|s| parse_file(catalog, s))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|a, b| (a.app_id.as_str(), a.number).cmp(&(b.app_id.as_str(), b.number)));
    for a in catalog.apps() {
        let mut app_files = files.iter().filter(|f| f.app_id == a.id).peekable();
        let first = app_files.peek().map_or(1, |f| f.number);
        for (expected, f) in (first..).zip(app_files) {
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
/// the store; see the module docs. Blocking; idempotent. `Err` is the message
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
        .map_err(|e| format!("syncing the layout files: {e}"))?
}

/// An app's oldest layout file in this build (its lowest-numbered one; see
/// [`parse_files`]). Panics if the embedded files are invalid (the pod would
/// not start either).
pub fn builtin(catalog: &Catalog, workspace_id: &str) -> Option<UiNode> {
    let src = catalog
        .files()
        .iter()
        .filter(|f| f.app_id == workspace_id)
        .min_by(|a, b| a.name.cmp(b.name))?;
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

/// What the client draws: the features in [`protocol::FEATURES_HEADER`].
/// Without the header the caller is a tool, not a client
/// ([`protocol::ClientCaps::unfiltered`]): it gets the latest version.
pub fn client_caps(headers: &HeaderMap) -> protocol::ClientCaps {
    match headers
        .get(protocol::FEATURES_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(features) => protocol::ClientCaps::from_header(features),
        None => protocol::ClientCaps::unfiltered(),
    }
}

/// The newest version of an app `client` can draw (by the features it uses,
/// [`AppVersion::compatibility`]): metadata
/// only. `None` if the app has no version that fits. Blocking.
pub fn newest_compatible(
    store: &Store,
    app_id: &str,
    client: &protocol::ClientCaps,
) -> rusqlite::Result<Option<AppVersion>> {
    let mut before = None;
    loop {
        let page = store.app_versions(app_id, before, DEFAULT_VERSIONS_LIMIT)?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        before = Some(last.version);
        if let Some(v) = page
            .into_iter()
            .find(|v| v.compatibility(client) == protocol::Compatibility::Compatible)
        {
            return Ok(Some(v));
        }
    }
}

/// The newest version of an app `client` can draw ([`newest_compatible`])
/// with its layout; `None` if the workspace is not an app or no version fits.
/// Blocking.
pub fn active_for(
    store: &Store,
    catalog: &Catalog,
    workspace_id: &str,
    client: &protocol::ClientCaps,
) -> rusqlite::Result<Option<Served>> {
    if !catalog.is_app(workspace_id) && !crate::custom::is_custom_id(workspace_id) {
        return Ok(None);
    }
    let Some(latest) = store.app_latest_meta(workspace_id)? else {
        return Ok(None);
    };
    // The newest version whose recorded features fit and, when it uses
    // components or parts, whose expansion today fits too (a use that
    // follows a component can expand to newer vocabulary than was recorded;
    // decision `plumbing-and-porcelain`).
    let mut before = None;
    let (v, served) = 'found: loop {
        let page = store.app_versions(workspace_id, before, DEFAULT_VERSIONS_LIMIT)?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        before = Some(last.version);
        for v in page {
            if v.compatibility(client) != protocol::Compatibility::Compatible {
                continue;
            }
            let Some((_, json)) = store.app_version(workspace_id, v.version)? else {
                continue;
            };
            let served = crate::parts::serve(store, catalog, &json, Some(client));
            if (crate::parts::may_hold_refs(&json) || v.is_workspace())
                && let Ok(node) = serde_json::from_str::<UiNode>(&served)
                && client.can_draw(&protocol::required_features(&node))
                    != protocol::Compatibility::Compatible
            {
                tracing::info!(
                    app = workspace_id,
                    version = v.version,
                    "skipped a version whose expansion the client can't draw"
                );
                continue;
            }
            break 'found (v, served);
        }
    };
    if v.version != latest.version {
        tracing::info!(
            app = workspace_id,
            latest = latest.version,
            served = v.version,
            missing = ?latest
                .features
                .iter()
                .filter(|f| !client.features.contains(*f))
                .collect::<Vec<_>>(),
            "served an older layout version the client can draw"
        );
    }
    Ok(Some(Served {
        version: v.version,
        layout: from_json(&served)?,
    }))
}

/// Async [`active_for`] with the client read from the request headers.
pub async fn active_for_client(
    state: &Arc<AppState>,
    workspace_id: &str,
    headers: &HeaderMap,
) -> Result<Option<Served>, ApiError> {
    let ws = workspace_id.to_owned();
    let client = client_caps(headers);
    let st = state.clone();
    blocking(state, move |s| active_for(s, st.catalog(), &ws, &client)).await
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

/// One version's metadata for `client`: `features` (the layout features it
/// uses), `compatible`, and
/// `incompatible` (`update_required` | `retired`) when the client cannot draw
/// it.
fn version_json(v: &AppVersion, client: &protocol::ClientCaps) -> Value {
    let compat = v.compatibility(client);
    let mut out = json!({
        "version": v.version,
        "created_at": rfc3339(v.created_at),
        "source": v.source,
        "compatible": compat.reason().is_none(),
    });
    if let Some(n) = &v.note {
        out["note"] = json!(n);
    }
    if let Some(a) = &v.author {
        out["author"] = json!(a);
    }
    if !v.features.is_empty() {
        out["features"] = json!(v.features);
    }
    if let Some(r) = compat.reason() {
        out["incompatible"] = json!(r);
    }
    out
}

/// `GET /v1/apps[?all=1][&blueprints=1]` -> `{"apps": [{id, title, icon,
/// kind, hidden, latest_version, latest_compatible_version, updated_at,
/// updates_from?}]}` (public): the pod's own apps (`kind` `custom`, the
/// home app among them) oldest first; with `blueprints=1` the built-in
/// blueprints first (`kind` `builtin`, decision `blueprints-and-home`). Hidden apps (`PUT /v1/apps/:id/hidden`) are listed only
/// with `?all=1`. Dev apps only with `CONSTRUCT_SHOW_DEV_APPS=1`.
/// `latest_compatible_version` is the newest version the client
/// ([`protocol::FEATURES_HEADER`]) can draw,
/// i.e. what opening the app serves it (null if none fits).
async fn list_apps(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let client = client_caps(&headers);
    let flag = |k: &str| query.get(k).is_some_and(|v| v == "1" || v == "true");
    let all = flag("all");
    // Built-in apps are blueprints (decision `blueprints-and-home`): a pod
    // lists only its own apps, and the blueprints with `?blueprints=1`.
    let builtin: Vec<AppInfo> = if flag("blueprints") {
        state
            .catalog()
            .apps()
            .filter(|a| !a.dev || state.show_dev_apps())
            .copied()
            .collect()
    } else {
        Vec::new()
    };
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
                let compatible = newest_compatible(s, a.id(), &client)?.map(|v| v.version);
                let is_hidden = hidden.iter().any(|h| h == a.id());
                let updates = match &a {
                    crate::custom::AppRef::Custom(c) => {
                        crate::custom::updates_from(s, st.catalog(), &c.id, &client, &dismissed)?
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
/// created_at, source, note?, features?, compatible, incompatible?}],
/// next_before?}` newest first; `next_before` iff older versions exist (pass
/// it as `before` for the next page). `compatible` and
/// `latest_compatible_version` are for the client's
/// [`protocol::FEATURES_HEADER`].
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
    let client = client_caps(&headers);
    let lookup = app_id.clone();
    let result = blocking(&state, {
        let client = client.clone();
        move |s| {
            let latest = s.app_latest_meta(&lookup)?.map(|v| v.version);
            let compatible = newest_compatible(s, &lookup, &client)?.map(|v| v.version);
            let rows = s.app_versions(&lookup, before, limit + 1)?;
            Ok((latest, compatible, rows))
        }
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
            .map(|v| version_json(v, &client))
            .collect::<Vec<_>>(),
    });
    if more && let Some(last) = page.last() {
        body["next_before"] = json!(last.version);
    }
    Json(body).into_response()
}

/// `GET /v1/apps/:id/versions/:n` -> `{app_id, version, created_at, source,
/// note?, features?, compatible, incompatible?, layout}` (the layout is served
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
    let client = client_caps(&headers);
    let st = state.clone();
    let found = blocking(&state, move |s| {
        // A custom version's built-in parts, as this client would be served them.
        Ok(s.app_version(&lookup, n)?.map(|(v, json)| {
            let served = crate::parts::serve(s, st.catalog(), &json, Some(&client));
            (v, served)
        }))
    })
    .await;
    match found {
        Ok(Some((v, stored))) => {
            let mut body = version_json(&v, &client_caps(&headers));
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
    /// The session whose data changed; `None`: the pod's own data (every
    /// session's subscribers, decision `collections`).
    pub token_hash: Option<String>,
    /// What changed, when the publisher knows (decision
    /// `scoped-data-events`); `None`: anything may have.
    pub scope: Option<DataScope>,
    /// The device whose own write this is, which already shows it: its
    /// streams don't get the event.
    pub except_device: Option<String>,
}

/// What a scoped `event: data` says changed (decision
/// `scoped-data-events`). Clients reload only the data whose entity URIs
/// start with one of `uri_prefixes`, or whose query keys are one of
/// `query_prefixes` or start with one followed by `:`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataScope {
    pub uri_prefixes: Vec<String>,
    pub query_prefixes: Vec<String>,
    /// The source's own account of the change, sent as is (Collections:
    /// `changes: [{collection, rows}]`).
    pub detail: serde_json::Map<String, Value>,
}

fn data_event(workspace_id: &str, source: Option<&str>, scope: Option<&DataScope>) -> Event {
    let mut body = json!({ "workspace_id": workspace_id });
    if let (Some(source), Some(scope)) = (source, scope) {
        body["source"] = json!(source);
        for (k, v) in &scope.detail {
            body[k] = v.clone();
        }
        body["uri_prefixes"] = json!(scope.uri_prefixes);
        body["query_prefixes"] = json!(scope.query_prefixes);
    }
    Event::default().event("data").data(body.to_string())
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

/// The version a layout event announces to a subscriber: the newest one
/// `client` can draw ([`newest_compatible`]). Blocking.
fn announced_version(
    store: &Store,
    app_id: &str,
    client: &protocol::ClientCaps,
) -> rusqlite::Result<Option<i64>> {
    Ok(newest_compatible(store, app_id, client)?.map(|v| v.version))
}

async fn announced_async(
    state: &Arc<AppState>,
    app_id: &str,
    client: protocol::ClientCaps,
) -> Result<Option<i64>, ApiError> {
    let id = app_id.to_owned();
    blocking(state, move |s| announced_version(s, &id, &client)).await
}

/// Other layouts' `(id, announced version)`s and deleted layouts' ids.
type Others = (Vec<(String, i64)>, Vec<String>);

/// Home's stream (decision `layouts-not-apps`) on connecting and after a
/// lag: every other layout of the pod with the version it announces, and
/// the custom layouts deleted but not purged yet. A client that was not
/// listening while one changed (in the background, offline, the server
/// restarting) learns of it here; it ignores layouts it hasn't opened and
/// versions it already has. Blocking.
fn others_now(
    store: &Store,
    catalog: &Catalog,
    client: &protocol::ClientCaps,
) -> rusqlite::Result<Others> {
    let custom = store.with_conn(crate::custom::custom_apps_on)?;
    let ids = catalog
        .apps()
        .map(|a| a.id.to_owned())
        .chain(custom.into_iter().map(|a| a.id))
        .filter(|id| id != crate::HOME_APP);
    let mut live = Vec::new();
    for id in ids {
        if let Some(v) = announced_version(store, &id, client)? {
            live.push((id, v));
        }
    }
    let deleted = store.with_conn(crate::custom::deleted_app_ids_on)?;
    Ok((live, deleted))
}

async fn others_async(
    state: &Arc<AppState>,
    client: protocol::ClientCaps,
) -> Result<Others, ApiError> {
    let st = state.clone();
    blocking(state, move |s| others_now(s, st.catalog(), &client)).await
}

/// Queue on home's stream what [`others_now`] finds changed since `others`
/// (all of it on a fresh stream) and every deleted layout, and remember it.
fn queue_others(
    (live, deleted): Others,
    others: &mut HashMap<String, i64>,
    queue: &mut std::collections::VecDeque<Event>,
) {
    for (id, v) in live {
        if others.get(&id) == Some(&v) {
            continue;
        }
        others.insert(id.clone(), v);
        queue.push_back(layout_event(&LayoutEvent {
            workspace_id: id,
            version: v,
        }));
    }
    for id in deleted {
        others.remove(&id);
        queue.push_back(deleted_event(&id));
    }
}

fn deleted_event(id: &str) -> Event {
    Event::default()
        .event("deleted")
        .data(json!({ "workspace_id": id }).to_string())
}

/// `GET /v1/workspaces/:id/events` (SSE). First message: the current
/// version; then one `layout` event (`{workspace_id, version}`) each time a
/// new version of this app changes what the subscriber would be served (on
/// home's stream, after the first message, every other layout's version and
/// deletion: [`others_now`]); a
/// `: keepalive` comment every [`AppState::sse_keepalive`]. The version is
/// the newest one the client ([`protocol::FEATURES_HEADER`]) can draw, so a
/// version it can't draw causes no event (and no refetch). New versions normally arrive
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
    let device = crate::notify::device_id(&headers);
    let sources = state.sources_of(&app);
    let client = client_caps(&headers);
    // Subscribe before reading the current version so nothing published in
    // between is missed (a duplicate version is harmless to clients).
    let rx = state.subscribe_layouts();
    let data_rx = state.subscribe_data();
    let current = match announced_async(&state, &id, client.clone()).await {
        Ok(Some(v)) => v,
        Ok(None) => return workspace_not_found(&id),
        Err(e) => return e.into_response(),
    };
    tracing::info!(workspace = %id, version = current, ?client, "layout events subscriber connected");
    struct S {
        first: bool,
        /// The app was deleted: the `deleted` event went out, end.
        done: bool,
        sent: i64,
        client: protocol::ClientCaps,
        rx: broadcast::Receiver<LayoutEvent>,
        notify_rx: Option<broadcast::Receiver<NotifyEvent>>,
        _live: Option<crate::notify::LiveDevice>,
        data_rx: broadcast::Receiver<DataEvent>,
        session_hash: Option<String>,
        /// This stream's device (`x-construct-device`), whose own writes
        /// it isn't told of.
        device: Option<String>,
        sources: Vec<&'static str>,
        recheck: Option<tokio::time::Interval>,
        _slot: Option<SseSlot>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        state: Arc<AppState>,
        id: String,
        /// Home's stream: the version last announced of each other layout.
        others: HashMap<String, i64>,
        /// Events to send before waiting for more ([`queue_others`]).
        queue: std::collections::VecDeque<Event>,
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
            client,
            rx,
            notify_rx,
            _live: live,
            data_rx,
            session_hash,
            device,
            sources,
            recheck,
            _slot: slot,
            shutdown: state.shutdown_signal(),
            state: state.clone(),
            id,
            others: Default::default(),
            queue: Default::default(),
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
                if s.id == crate::HOME_APP {
                    match others_async(&s.state, s.client.clone()).await {
                        Ok(o) => queue_others(o, &mut s.others, &mut s.queue),
                        Err(e) => tracing::warn!(error = ?e, "events: other layouts not announced"),
                    }
                }
                let e = event(&s);
                return Some((Ok::<_, std::convert::Infallible>(e), s));
            }
            loop {
                if let Some(e) = s.queue.pop_front() {
                    return Some((Ok(e), s));
                }
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
                                    && d.token_hash.as_ref().is_none_or(|h| {
                                        s.session_hash.as_deref() == Some(h.as_str())
                                    })
                                    && (d.except_device.is_none() || d.except_device != s.device)
                            }
                            // Lagged: a reload is never wrong.
                            Err(broadcast::error::RecvError::Lagged(_)) => s.session_hash.is_some(),
                            Err(broadcast::error::RecvError::Closed) => return None,
                        };
                        if mine {
                            if !session_still_valid(&s.state, s.session_hash.as_deref()).await {
                                return None;
                            }
                            // A lag lost what changed: the unscoped event
                            // (reload everything).
                            let e = match &d {
                                Ok(d) => data_event(&s.id, Some(&d.source), d.scope.as_ref()),
                                Err(_) => data_event(&s.id, None, None),
                            };
                            return Some((Ok(e), s));
                        }
                        continue;
                    }
                };
                // Home's stream carries every layout's changes (decision
                // `layouts-not-apps`): one engine shows them all.
                if s.id == crate::HOME_APP
                    && let Ok(ev) = &received
                    && ev.workspace_id != s.id
                {
                    if ev.version == DELETED_VERSION {
                        s.others.remove(&ev.workspace_id);
                        return Some((Ok(deleted_event(&ev.workspace_id)), s));
                    }
                    let other = ev.workspace_id.clone();
                    let Ok(Some(v)) = announced_async(&s.state, &other, s.client.clone()).await
                    else {
                        continue;
                    };
                    if s.others.get(&other) == Some(&v) {
                        continue;
                    }
                    s.others.insert(other.clone(), v);
                    let e = layout_event(&LayoutEvent {
                        workspace_id: other,
                        version: v,
                    });
                    return Some((Ok(e), s));
                }
                let lagged = match received {
                    Ok(ev) if ev.workspace_id == s.id && ev.version == DELETED_VERSION => {
                        s.done = true;
                        return Some((Ok(deleted_event(&s.id)), s));
                    }
                    Ok(ev) if ev.workspace_id == s.id => false,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(workspace = %s.id, skipped = n, "layout subscriber lagged; sending current");
                        if s.id == crate::HOME_APP
                            && let Ok(o) = others_async(&s.state, s.client.clone()).await
                        {
                            queue_others(o, &mut s.others, &mut s.queue);
                        }
                        true
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                };
                // Re-read what this subscriber would be served now; only a
                // change is announced (always after a lag).
                let Ok(Some(v)) = announced_async(&s.state, &s.id, s.client.clone()).await else {
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
