//! Custom apps (decision `custom-apps-and-data-sources`): apps the pod's user
//! owns, stored in the pod, built from any layout over the pod's data
//! sources. They are served like built-in apps (`GET /v1/workspaces/:id`,
//! SSE, the versions API, schema negotiation); only their layouts come from
//! writes instead of files, each validated by
//! [`crate::validate::custom_layout`] and appended to the app's history.
//!
//! | Call | Effect |
//! |---|---|
//! | `GET /v1/custom-apps/{id}` | the app, its feeds and its latest layout |
//! | `POST /v1/custom-apps {title, icon, feeds, layout, note?}` | create v1 |
//! | `PUT /v1/custom-apps/{id}/layout {layout, note?, base_version?}` | append a version (409 `stale_base`) |
//! | `PATCH /v1/custom-apps/{id} {title?, icon?, feeds?}` | rename, re-icon, re-feed (the latest layout is checked again) |
//! | `POST /v1/custom-apps/{id}/restore {version}` | append a copy of an old version |
//! | `DELETE /v1/custom-apps/{id}` | soft delete (purged after 30 days) |
//! | `POST /v1/custom-apps/validate {feeds, layout}` | dry run |
//! | `PUT /v1/apps/{id}/hidden {hidden}` | hide or show any app (built-ins too) |
//!
//! Writes need the pod's own credential: its pinned session (a pod token,
//! D46). Clients also hold that token, but only delete and hide; any valid
//! session may do those two. The root `id` of a written layout is set by
//! the pod to the app's id.

// Helpers return a ready error `Response` for the handler to send as is.
#![allow(clippy::result_large_err)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::adapter::{AppInfo, AuthPolicy};
use crate::session::{Caller, unauthorized};
use crate::source::SourceManifest;
use crate::store::{Authored, Store};
use crate::util::now_secs;
use crate::validate::LayoutError;
use crate::{Pod, blocking, json_error, parse_json_body};

/// `AppVersion::source` of a version written through this API.
pub const SOURCE_CUSTOM: &str = "custom";
/// `AppVersion::source` of a version that restores an older one.
pub const SOURCE_RESTORE: &str = "restore";
/// `AppVersion::author` of a version written with the pod token.
pub const AUTHOR_USER: &str = "user";
/// `AppVersion::author` of a version written through the MCP endpoint.
pub const AUTHOR_CLAUDE: &str = "claude";
/// The pod setting listing hidden app ids (a JSON array).
pub const HIDDEN_SETTING: &str = "apps.hidden";
/// Soft-deleted custom apps are purged this long after deletion.
pub const PURGE_AFTER_SECS: i64 = 30 * 86_400;
/// Most feeds one custom app may list.
pub const MAX_FEEDS: usize = 8;
/// Longest title.
pub const MAX_TITLE_CHARS: usize = 60;
/// The app icons a custom app may have (clients draw any other name as a
/// generic app icon).
pub const APP_ICONS: &[&str] = &["photo", "recipe", "dashboard"];
/// Longest version note.
pub const MAX_NOTE_CHARS: usize = 500;

/// Limits per pod (temporary pods get the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Custom apps (not counting deleted ones).
    pub apps: usize,
    /// Versions kept per app; the oldest are pruned beyond it.
    pub versions: usize,
    /// Writes (create, edit, rename, restore, delete) per minute.
    pub writes_per_minute: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            apps: 50,
            versions: 500,
            writes_per_minute: 60,
        }
    }
}

/// The per-pod state of this module: caps and the write-rate window.
#[derive(Debug, Default)]
pub struct CustomState {
    pub(crate) caps: Caps,
    writes: Mutex<VecDeque<Instant>>,
}

impl CustomState {
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            writes: Mutex::new(VecDeque::new()),
        }
    }

    /// Count one write; `false` when the last minute already had
    /// [`Caps::writes_per_minute`].
    fn allow_write(&self) -> bool {
        let mut w = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        while w
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            w.pop_front();
        }
        if w.len() >= self.caps.writes_per_minute {
            return false;
        }
        w.push_back(now);
        true
    }
}

// ---------- apps ----------

/// A custom app as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomApp {
    /// `c-` and 10 base32 characters (built-in ids never contain `-`).
    pub id: String,
    pub title: String,
    pub icon: String,
    /// Its root feeds, `(source, feed)`.
    pub feeds: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An app the pod serves: built in (from an adapter) or custom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppRef {
    Builtin(AppInfo),
    Custom(CustomApp),
}

impl AppRef {
    pub fn id(&self) -> &str {
        match self {
            AppRef::Builtin(a) => a.id,
            AppRef::Custom(a) => &a.id,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            AppRef::Builtin(a) => a.title,
            AppRef::Custom(a) => &a.title,
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            AppRef::Builtin(a) => a.icon,
            AppRef::Custom(a) => &a.icon,
        }
    }

    /// `builtin` | `custom` (`GET /v1/apps` `kind`).
    pub fn kind(&self) -> &'static str {
        match self {
            AppRef::Builtin(_) => "builtin",
            AppRef::Custom(_) => "custom",
        }
    }

    /// Custom apps are public at the app level; each source keeps its own
    /// sign-in.
    pub fn auth(&self) -> AuthPolicy {
        match self {
            AppRef::Builtin(a) => a.auth,
            AppRef::Custom(_) => AuthPolicy::Public,
        }
    }

    pub fn feeds(&self) -> Vec<(String, String)> {
        match self {
            AppRef::Builtin(a) => a
                .feeds
                .iter()
                .map(|(s, f)| ((*s).to_owned(), (*f).to_owned()))
                .collect(),
            AppRef::Custom(a) => a.feeds.clone(),
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, AppRef::Custom(_))
    }
}

/// Whether `id` has the shape of a custom app id.
pub fn is_custom_id(id: &str) -> bool {
    id.strip_prefix("c-").is_some_and(|rest| {
        rest.len() == 10
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

/// A new custom app id.
pub fn new_custom_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("OS random number generator");
    let tail: String = buf
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % 32] as char)
        .collect();
    format!("c-{tail}")
}

fn app_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CustomApp> {
    let feeds_json: String = r.get(3)?;
    let feeds: Vec<(String, String)> = serde_json::from_str(&feeds_json).unwrap_or_default();
    Ok(CustomApp {
        id: r.get(0)?,
        title: r.get(1)?,
        icon: r.get(2)?,
        feeds,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

const APP_COLS: &str = "id, title, icon, feeds_json, created_at, updated_at";

/// A live (not deleted) custom app.
pub fn custom_app_on(conn: &Connection, id: &str) -> rusqlite::Result<Option<CustomApp>> {
    conn.query_row(
        &format!("SELECT {APP_COLS} FROM custom_apps WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        app_row,
    )
    .optional()
}

/// Every live custom app, oldest first.
pub fn custom_apps_on(conn: &Connection) -> rusqlite::Result<Vec<CustomApp>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {APP_COLS} FROM custom_apps WHERE deleted_at IS NULL ORDER BY created_at, id"
    ))?;
    stmt.query_map([], app_row)?.collect()
}

/// The hidden app ids ([`HIDDEN_SETTING`]).
pub fn hidden_apps(store: &Store) -> rusqlite::Result<Vec<String>> {
    Ok(store
        .pod_setting(HIDDEN_SETTING)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

/// Delete custom apps (and their versions) deleted more than
/// [`PURGE_AFTER_SECS`] before `now`. Returns how many went.
pub fn purge(store: &Store, now: i64) -> rusqlite::Result<usize> {
    store.transaction(|tx| {
        let cutoff = now - PURGE_AFTER_SECS;
        tx.execute(
            "DELETE FROM app_versions WHERE app_id IN
             (SELECT id FROM custom_apps WHERE deleted_at IS NOT NULL AND deleted_at <= ?1)",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM custom_apps WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
            [cutoff],
        )
    })
}

/// The client schema a custom layout is recorded as written for: what its
/// vocabulary needs, raised to the oldest schema current clients still draw
/// ([`protocol::RETIRED`]).
pub fn recorded_schema(layout: &protocol::UiNode) -> u32 {
    protocol::required_schema(layout).max(protocol::oldest_layout_schema(protocol::SCHEMA_VERSION))
}

// ---------- validation ----------

/// Why a write was refused: the HTTP status and JSON body.
type Refusal = (StatusCode, Value);

fn refuse(status: StatusCode, body: Value) -> Refusal {
    (status, body)
}

fn invalid(error: &str, detail: impl Into<String>) -> Refusal {
    refuse(
        StatusCode::BAD_REQUEST,
        json!({ "error": error, "detail": detail.into() }),
    )
}

fn invalid_layout(e: &LayoutError) -> Refusal {
    let mut body = json!({ "error": "invalid_layout", "detail": e.to_string() });
    body["at"] = json!(e.pointer);
    body["allowed"] = json!(e.allowed);
    refuse(StatusCode::UNPROCESSABLE_ENTITY, body)
}

/// Check a feeds list: 1..=[`MAX_FEEDS`] distinct `[source, feed]` pairs,
/// each a declared feed of a source custom apps may use (not a dev source
/// unless dev apps are shown).
pub fn check_feeds(pod: &Pod, v: &Value) -> Result<Vec<(String, String)>, Refusal> {
    let offered = || -> Vec<String> {
        pod.catalog()
            .sources()
            .filter(|(_, m)| !m.dev || pod.show_dev_apps())
            .flat_map(|(s, m)| m.feeds.iter().map(move |f| format!("{s}/{}", f.name)))
            .collect()
    };
    let bad = |detail: String| {
        let mut r = invalid("invalid_feeds", detail);
        r.1["allowed"] = json!(offered());
        r
    };
    let Some(items) = v.as_array() else {
        return Err(bad(
            "\"feeds\" must be a list of [source, feed] pairs".into()
        ));
    };
    if items.is_empty() || items.len() > MAX_FEEDS {
        return Err(bad(format!("an app lists 1..={MAX_FEEDS} feeds")));
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for item in items {
        let pair = item.as_array().and_then(|p| match p.as_slice() {
            [Value::String(s), Value::String(f)] => Some((s.clone(), f.clone())),
            _ => None,
        });
        // "source/feed" is accepted too.
        let pair = pair.or_else(|| {
            item.as_str()
                .and_then(|t| t.split_once('/'))
                .map(|(s, f)| (s.to_owned(), f.to_owned()))
        });
        let Some((source, feed)) = pair else {
            return Err(bad(format!("{item} is not a [source, feed] pair")));
        };
        let ok = pod
            .catalog()
            .source(&source)
            .is_some_and(|(_, m)| (!m.dev || pod.show_dev_apps()) && m.feed(&feed).is_some());
        if !ok {
            return Err(bad(format!("'{source}/{feed}' is not a feed of this pod")));
        }
        if out.contains(&(source.clone(), feed.clone())) {
            return Err(bad(format!("'{source}/{feed}' is listed twice")));
        }
        out.push((source, feed));
    }
    Ok(out)
}

/// The manifests of `feeds`' sources, each once.
pub fn feed_sources<'a>(
    pod: &'a Pod,
    feeds: &[(String, String)],
) -> Vec<(&'static str, &'a SourceManifest)> {
    let mut out: Vec<(&'static str, &SourceManifest)> = Vec::new();
    for (s, _) in feeds {
        if let Some((a, m)) = pod.catalog().source(s)
            && !out.iter().any(|(n, _)| *n == a.name())
        {
            out.push((a.name(), m));
        }
    }
    out
}

/// Validate `layout` for app `id` over `feeds`, with the root id set to
/// `id`: the parsed layout and its JSON.
pub fn check_layout(
    pod: &Pod,
    id: &str,
    feeds: &[(String, String)],
    layout: &Value,
) -> Result<(protocol::UiNode, String), Refusal> {
    let mut layout = layout.clone();
    let Some(obj) = layout.as_object_mut() else {
        return Err(invalid_layout(&LayoutError {
            pointer: String::new(),
            message: "the layout must be a core://Workspace object".into(),
            allowed: Vec::new(),
        }));
    };
    obj.insert("id".into(), json!(id));
    let sources = feed_sources(pod, feeds);
    let node =
        crate::validate::custom_layout(id, &layout, &sources).map_err(|e| invalid_layout(&e))?;
    let json = serde_json::to_string(&node).expect("a UiNode serializes");
    Ok((node, json))
}

fn check_title(v: Option<&Value>) -> Result<Option<String>, Refusal> {
    let Some(v) = v else { return Ok(None) };
    let t = v.as_str().map(str::trim).unwrap_or("");
    if t.is_empty() || t.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid(
            "invalid_title",
            format!("\"title\" is 1..={MAX_TITLE_CHARS} characters"),
        ));
    }
    Ok(Some(t.to_owned()))
}

fn check_icon(v: Option<&Value>) -> Result<Option<String>, Refusal> {
    let Some(v) = v else { return Ok(None) };
    match v.as_str() {
        Some(i) if APP_ICONS.contains(&i) => Ok(Some(i.to_owned())),
        _ => {
            let mut r = invalid(
                "invalid_icon",
                "\"icon\" must be an app icon the clients know",
            );
            r.1["allowed"] = json!(APP_ICONS);
            Err(r)
        }
    }
}

fn check_note(v: Option<&Value>, default: &str) -> Result<String, Refusal> {
    match v {
        None | Some(Value::Null) => Ok(default.to_owned()),
        Some(Value::String(n)) if !n.trim().is_empty() => {
            Ok(n.trim().chars().take(MAX_NOTE_CHARS).collect())
        }
        _ => Err(invalid("invalid_note", "\"note\" is a non-empty string")),
    }
}

// ---------- writes ----------

/// Who writes: decides [`crate::store::AppVersion::author`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// The pod token (curl, tooling).
    User,
    /// The MCP endpoint (decision `mcp-connector`).
    Claude,
}

impl Writer {
    fn author(self) -> &'static str {
        match self {
            Writer::User => AUTHOR_USER,
            Writer::Claude => AUTHOR_CLAUDE,
        }
    }
}

/// What a write did: the app's id and new latest version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub id: String,
    pub version: i64,
}

/// A store error or a refusal, inside a blocking write.
enum WriteErr {
    Db(rusqlite::Error),
    Refused(Refusal),
}

impl From<rusqlite::Error> for WriteErr {
    fn from(e: rusqlite::Error) -> Self {
        WriteErr::Db(e)
    }
}

fn not_found(id: &str) -> Refusal {
    refuse(
        StatusCode::NOT_FOUND,
        json!({ "error": "app_not_found", "id": id }),
    )
}

/// Run a write transaction off the async workers, after the write-rate cap.
async fn write<T: Send + 'static>(
    pod: &Pod,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, WriteErr> + Send + 'static,
) -> Result<T, Refusal> {
    if !pod.custom.allow_write() {
        return Err(refuse(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "rate_limited", "detail": format!("at most {} custom-app writes a minute", pod.custom.caps.writes_per_minute) }),
        ));
    }
    let refused: std::sync::Arc<Mutex<Option<Refusal>>> = Default::default();
    let slot = refused.clone();
    let result = blocking(pod, move |s| {
        s.transaction(|tx| match f(tx) {
            Ok(v) => Ok(Some(v)),
            Err(WriteErr::Db(e)) => Err(e),
            Err(WriteErr::Refused(r)) => {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                // Roll back anything written before the refusal.
                Err(rusqlite::Error::QueryReturnedNoRows)
            }
        })
    })
    .await;
    if let Some(r) = refused.lock().unwrap_or_else(|e| e.into_inner()).take() {
        return Err(r);
    }
    match result {
        Ok(Some(v)) => Ok(v),
        Ok(None) => unreachable!("a write either returns or refuses"),
        Err((status, Json(body))) => Err((status, body)),
    }
}

/// A version to append ([`append`]).
struct NewVersion<'a> {
    json: &'a str,
    source: &'a str,
    note: &'a str,
    schema: u32,
    by: Writer,
}

/// Append a version and prune the oldest beyond the cap.
fn append(
    tx: &rusqlite::Transaction<'_>,
    caps: Caps,
    id: &str,
    v: NewVersion<'_>,
) -> rusqlite::Result<i64> {
    let version = crate::store::append_authored_version_on(
        tx,
        id,
        v.json,
        v.source,
        Some(v.note),
        v.schema,
        Authored {
            author: v.by.author(),
            meta_json: None,
        },
    )?;
    tx.execute(
        "DELETE FROM app_versions WHERE app_id = ?1 AND version <= ?2",
        params![id, version - caps.versions as i64],
    )?;
    tx.execute(
        "UPDATE custom_apps SET updated_at = ?1 WHERE id = ?2",
        params![now_secs(), id],
    )?;
    Ok(version)
}

/// Create a custom app from a request body `{title, icon, feeds, layout,
/// note?}`.
pub async fn create(pod: &Pod, body: &Value, by: Writer) -> Result<Written, Refusal> {
    let title = check_title(Some(&body["title"]))?.expect("checked");
    let icon = check_icon(Some(body.get("icon").unwrap_or(&json!("photo"))))?.expect("checked");
    let feeds = check_feeds(pod, &body["feeds"])?;
    let note = check_note(body.get("note"), "Created")?;
    let id = new_custom_id();
    let (layout, json) = check_layout(pod, &id, &feeds, &body["layout"])?;
    let schema = recorded_schema(&layout);
    let caps = pod.custom.caps;
    let feeds_json = serde_json::to_string(&feeds).expect("serializes");
    let app_id = id.clone();
    let version = write(pod, move |tx| {
        let live: i64 = tx.query_row(
            "SELECT COUNT(*) FROM custom_apps WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        if live as usize >= caps.apps {
            return Err(WriteErr::Refused(refuse(
                StatusCode::CONFLICT,
                json!({ "error": "too_many_apps", "limit": caps.apps,
                        "detail": "delete a custom app first" }),
            )));
        }
        let now = now_secs();
        tx.execute(
            "INSERT INTO custom_apps (id, title, icon, feeds_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![app_id, title, icon, feeds_json, now],
        )?;
        Ok(append(
            tx,
            caps,
            &app_id,
            NewVersion {
                json: &json,
                source: SOURCE_CUSTOM,
                note: &note,
                schema,
                by,
            },
        )?)
    })
    .await?;
    pod.publish_layout(&id, version);
    tracing::info!(app = %id, version, "custom app created");
    Ok(Written { id, version })
}

/// The live custom app `id`, or 404.
async fn load(pod: &Pod, id: &str) -> Result<CustomApp, Refusal> {
    if !is_custom_id(id) {
        return Err(not_found(id));
    }
    let lookup = id.to_owned();
    match blocking(pod, move |s| s.with_conn(|c| custom_app_on(c, &lookup))).await {
        Ok(Some(app)) => Ok(app),
        Ok(None) => Err(not_found(id)),
        Err((status, Json(body))) => Err((status, body)),
    }
}

/// Append a new layout `{layout, note?, base_version?}` to app `id`.
pub async fn update_layout(
    pod: &Pod,
    id: &str,
    body: &Value,
    by: Writer,
) -> Result<Written, Refusal> {
    let app = load(pod, id).await?;
    let note = check_note(body.get("note"), "Edited")?;
    let base =
        match body.get("base_version") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_i64().filter(|n| *n >= 1).ok_or_else(|| {
                invalid("invalid_request", "\"base_version\" is a version number")
            })?),
        };
    let (layout, json) = check_layout(pod, id, &app.feeds, &body["layout"])?;
    let schema = recorded_schema(&layout);
    let caps = pod.custom.caps;
    let app_id = id.to_owned();
    let version = write(pod, move |tx| {
        let latest = crate::store::latest_app_version_on(tx, &app_id)?.map(|v| v.version);
        if let Some(base) = base
            && Some(base) != latest
        {
            return Err(WriteErr::Refused(refuse(
                StatusCode::CONFLICT,
                json!({ "error": "stale_base", "latest_version": latest,
                        "detail": "the app changed since that version; read it again and reapply your edit" }),
            )));
        }
        Ok(append(
            tx,
            caps,
            &app_id,
            NewVersion {
                json: &json,
                source: SOURCE_CUSTOM,
                note: &note,
                schema,
                by,
            },
        )?)
    })
    .await?;
    pod.publish_layout(id, version);
    Ok(Written {
        id: id.to_owned(),
        version,
    })
}

/// Rename, re-icon or re-feed app `id` (`{title?, icon?, feeds?}`). New
/// feeds must still fit its latest layout.
pub async fn patch(pod: &Pod, id: &str, body: &Value) -> Result<Written, Refusal> {
    let app = load(pod, id).await?;
    let title = check_title(body.get("title"))?;
    let icon = check_icon(body.get("icon"))?;
    let feeds = match body.get("feeds") {
        Some(v) => Some(check_feeds(pod, v)?),
        None => None,
    };
    if let Some(feeds) = &feeds {
        let lookup = id.to_owned();
        let latest = blocking(pod, move |s| s.app_latest(&lookup))
            .await
            .map_err(|(st, Json(b))| (st, b))?;
        if let Some((_, json)) = latest {
            let layout: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
            check_layout(pod, id, feeds, &layout)?;
        }
    }
    let feeds_json = feeds
        .as_ref()
        .map(|f| serde_json::to_string(f).expect("serializes"));
    let app_id = id.to_owned();
    let version = write(pod, move |tx| {
        tx.execute(
            "UPDATE custom_apps SET title = COALESCE(?1, title), icon = COALESCE(?2, icon),
             feeds_json = COALESCE(?3, feeds_json), updated_at = ?4 WHERE id = ?5",
            params![title, icon, feeds_json, now_secs(), app_id],
        )?;
        Ok(crate::store::latest_app_version_on(tx, &app_id)?.map_or(0, |v| v.version))
    })
    .await?;
    let _ = app;
    // New feeds change the data: clients reload.
    pod.publish_layout(id, version);
    Ok(Written {
        id: id.to_owned(),
        version,
    })
}

/// Append a copy of version `{version}` of app `id`, checked against its
/// current feeds.
pub async fn restore(pod: &Pod, id: &str, body: &Value, by: Writer) -> Result<Written, Refusal> {
    let app = load(pod, id).await?;
    let Some(n) = body["version"].as_i64().filter(|n| *n >= 1) else {
        return Err(invalid(
            "invalid_request",
            "\"version\" is the version number to restore",
        ));
    };
    let lookup = id.to_owned();
    let old = blocking(pod, move |s| s.app_version(&lookup, n))
        .await
        .map_err(|(st, Json(b))| (st, b))?;
    let Some((_, json)) = old else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "app_id": id, "version": n }),
        ));
    };
    let layout: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
    let (node, json) = check_layout(pod, id, &app.feeds, &layout)?;
    let schema = recorded_schema(&node);
    let caps = pod.custom.caps;
    let note = check_note(body.get("note"), &format!("Restored version {n}"))?;
    let app_id = id.to_owned();
    let version = write(pod, move |tx| {
        Ok(append(
            tx,
            caps,
            &app_id,
            NewVersion {
                json: &json,
                source: SOURCE_RESTORE,
                note: &note,
                schema,
                by,
            },
        )?)
    })
    .await?;
    pod.publish_layout(id, version);
    Ok(Written {
        id: id.to_owned(),
        version,
    })
}

/// Soft-delete app `id`; its open event streams end with `event: deleted`.
pub async fn delete(pod: &Pod, id: &str) -> Result<(), Refusal> {
    load(pod, id).await?;
    let app_id = id.to_owned();
    write(pod, move |tx| {
        tx.execute(
            "UPDATE custom_apps SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now_secs(), app_id],
        )?;
        Ok(())
    })
    .await?;
    pod.publish_layout(id, crate::layouts::DELETED_VERSION);
    tracing::info!(app = %id, "custom app deleted");
    Ok(())
}

/// Hide or show app `id` (any app the pod serves).
pub async fn set_hidden(pod: &Pod, id: &str, hidden: bool) -> Result<(), Refusal> {
    if pod.app(id).await.map_err(|(s, Json(b))| (s, b))?.is_none() {
        return Err(not_found(id));
    }
    let app_id = id.to_owned();
    write(pod, move |tx| {
        let mut list: Vec<String> = crate::store::pod_setting_on(tx, HIDDEN_SETTING)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        list.retain(|a| *a != app_id);
        if hidden {
            list.push(app_id);
        }
        tx.execute(
            "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, ?2)",
            params![
                HIDDEN_SETTING,
                serde_json::to_string(&list).expect("serializes")
            ],
        )?;
        Ok(())
    })
    .await
}

/// A dry run of a create: `{feeds, layout}` -> `{ok, schema}`.
pub fn validate(pod: &Pod, body: &Value) -> Result<Value, Refusal> {
    let feeds = check_feeds(pod, &body["feeds"])?;
    let id = body["id"]
        .as_str()
        .filter(|i| is_custom_id(i))
        .unwrap_or("c-aaaaaaaaaa");
    let (node, _) = check_layout(pod, id, &feeds, &body["layout"])?;
    Ok(json!({ "ok": true, "schema": recorded_schema(&node) }))
}

/// One custom app with its feeds and latest layout (`GET
/// /v1/custom-apps/{id}`).
pub async fn describe(pod: &Pod, id: &str) -> Result<Value, Refusal> {
    let app = load(pod, id).await?;
    let lookup = id.to_owned();
    let latest = blocking(pod, move |s| s.app_latest(&lookup))
        .await
        .map_err(|(st, Json(b))| (st, b))?;
    let (version, layout) = match latest {
        Some((v, json)) => (
            Some(v.version),
            serde_json::from_str::<Value>(&json).unwrap_or(Value::Null),
        ),
        None => (None, Value::Null),
    };
    Ok(json!({
        "id": app.id,
        "title": app.title,
        "icon": app.icon,
        "feeds": app.feeds,
        "created_at": crate::layouts::rfc3339(app.created_at),
        "updated_at": crate::layouts::rfc3339(app.updated_at),
        "latest_version": version,
        "layout": layout,
    }))
}

// ---------- routes ----------

pub(crate) fn router() -> Router<Pod> {
    Router::new()
        .route("/v1/custom-apps", post(create_route))
        .route("/v1/custom-apps/validate", post(validate_route))
        .route(
            "/v1/custom-apps/{id}",
            get(describe_route).patch(patch_route).delete(delete_route),
        )
        .route("/v1/custom-apps/{id}/layout", put(layout_route))
        .route("/v1/custom-apps/{id}/restore", post(restore_route))
        .route("/v1/apps/{id}/hidden", put(hidden_route))
}

/// The caller of a write: the pod token's (pinned) session, or 401/403.
async fn require_writer(pod: &Pod, headers: &HeaderMap) -> Result<Writer, Response> {
    match pod.caller(headers).await {
        Ok(Caller::Signed(s)) if s.pinned => Ok(Writer::User),
        Ok(Caller::Signed(_)) => Err(json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "pod_token_required",
                    "detail": "custom apps are written with the pod's token (or through Claude); clients may only delete and hide" }),
        )),
        Ok(_) => Err(unauthorized()),
        Err(e) => Err(e.into_response()),
    }
}

/// The caller of a delete or hide: any valid session.
async fn require_session(pod: &Pod, headers: &HeaderMap) -> Result<(), Response> {
    match pod.caller(headers).await {
        Ok(Caller::Signed(_)) => Ok(()),
        Ok(_) => Err(unauthorized()),
        Err(e) => Err(e.into_response()),
    }
}

fn answer<T>(r: Result<T, Refusal>, ok: impl FnOnce(T) -> Response) -> Response {
    match r {
        Ok(v) => ok(v),
        Err((status, body)) => json_error(status, body),
    }
}

fn written(w: Written, status: StatusCode) -> Response {
    (
        status,
        Json(json!({ "ok": true, "id": w.id, "version": w.version })),
    )
        .into_response()
}

async fn create_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let by = match require_writer(&pod, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    answer(create(&pod, &body, by).await, |w| {
        written(w, StatusCode::CREATED)
    })
}

async fn validate_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = require_writer(&pod, &headers).await {
        return r;
    }
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    answer(validate(&pod, &body), |v| Json(v).into_response())
}

async fn describe_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = require_session(&pod, &headers).await {
        return r;
    }
    answer(describe(&pod, &id).await, |v| Json(v).into_response())
}

async fn layout_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let by = match require_writer(&pod, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    answer(update_layout(&pod, &id, &body, by).await, |w| {
        written(w, StatusCode::OK)
    })
}

async fn patch_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = require_writer(&pod, &headers).await {
        return r;
    }
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    answer(patch(&pod, &id, &body).await, |w| {
        written(w, StatusCode::OK)
    })
}

async fn restore_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let by = match require_writer(&pod, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    answer(restore(&pod, &id, &body, by).await, |w| {
        written(w, StatusCode::OK)
    })
}

async fn delete_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = require_session(&pod, &headers).await {
        return r;
    }
    answer(delete(&pod, &id).await, |()| {
        Json(json!({ "ok": true })).into_response()
    })
}

async fn hidden_route(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = require_session(&pod, &headers).await {
        return r;
    }
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let Some(hidden) = body["hidden"].as_bool() else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "hidden": "boolean" } }),
        );
    };
    answer(set_hidden(&pod, &id, hidden).await, |()| {
        Json(json!({ "ok": true, "id": id, "hidden": hidden })).into_response()
    })
}
