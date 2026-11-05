//! Custom apps (decision `custom-apps-and-data-sources`): apps the pod's user
//! owns, stored in the pod, built from any layout over the pod's data
//! sources. They are served like built-in apps (`GET /v1/workspaces/:id`,
//! SSE, the versions API, feature negotiation); only their layouts come from
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

use std::collections::{HashMap, VecDeque};
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

use crate::adapter::{AppInfo, AuthPolicy, Catalog};
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
/// The pod setting of dismissed update notices: `{custom app id: the
/// built-in version dismissed}` (decision `layout-parts`).
pub const DISMISSED_SETTING: &str = "updates.dismissed";
/// `AppVersion::source` of a version appended because a referenced
/// built-in app changed.
pub const SOURCE_REF_UPDATE: &str = "ref_update";
/// `AppVersion::author` of a version the pod appended itself.
pub const AUTHOR_POD: &str = "pod";
/// Soft-deleted custom apps are purged this long after deletion.
pub const PURGE_AFTER_SECS: i64 = 30 * 86_400;
/// Most feeds one write may add.
pub const MAX_FEEDS: usize = 8;
/// Most feeds home may declare: every layout's scope (decision
/// `layouts-not-apps`).
pub const MAX_HOME_FEEDS: usize = 32;
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

/// Whether `id` has the shape of a custom app id: `c-` and 10 base32
/// characters, or the pod's own [`crate::HOME_APP`].
pub fn is_custom_id(id: &str) -> bool {
    id == crate::HOME_APP
        || id.strip_prefix("c-").is_some_and(|rest| {
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

/// Home's own feeds (`None` on a pod without a home): the feeds every
/// layout of the pod may bind (decision `layouts-not-apps`).
pub fn home_feeds_on(conn: &Connection) -> rusqlite::Result<Option<Vec<(String, String)>>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT feeds_json FROM custom_apps WHERE id = ?1 AND deleted_at IS NULL",
            [crate::HOME_APP],
            |r| r.get(0),
        )
        .optional()?;
    Ok(json.map(|j| serde_json::from_str(&j).unwrap_or_default()))
}

/// A live (not deleted) custom app. Its `feeds` are its scope: home's feeds
/// (decision `layouts-not-apps`), or its own on a pod without a home.
pub fn custom_app_on(conn: &Connection, id: &str) -> rusqlite::Result<Option<CustomApp>> {
    let app = conn
        .query_row(
            &format!("SELECT {APP_COLS} FROM custom_apps WHERE id = ?1 AND deleted_at IS NULL"),
            [id],
            app_row,
        )
        .optional()?;
    let Some(mut app) = app else {
        return Ok(None);
    };
    if app.id != crate::HOME_APP
        && let Some(home) = home_feeds_on(conn)?
    {
        app.feeds = home;
    }
    Ok(Some(app))
}

/// Every live custom app: the home app first, then oldest first (feeds as
/// in [`custom_app_on`]).
pub fn custom_apps_on(conn: &Connection) -> rusqlite::Result<Vec<CustomApp>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {APP_COLS} FROM custom_apps WHERE deleted_at IS NULL
         ORDER BY id = ?1 DESC, created_at, id"
    ))?;
    let mut apps: Vec<CustomApp> = stmt
        .query_map([crate::HOME_APP], app_row)?
        .collect::<Result<_, _>>()?;
    if let Some(home) = apps
        .iter()
        .find(|a| a.id == crate::HOME_APP)
        .map(|a| a.feeds.clone())
    {
        for a in apps.iter_mut().filter(|a| a.id != crate::HOME_APP) {
            a.feeds = home.clone();
        }
    }
    Ok(apps)
}

/// Custom apps deleted but not purged yet (their ids).
pub fn deleted_app_ids_on(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM custom_apps WHERE deleted_at IS NOT NULL ORDER BY id")?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

/// The feeds of `feeds` that `layout` (as served, parts expanded) binds:
/// those whose query keys it lists, or whose source's entities it binds by
/// URI. Only these load with it (decision `layouts-not-apps`).
pub fn bound_feeds(
    catalog: &Catalog,
    feeds: &[(String, String)],
    layout: &Value,
) -> Vec<(String, String)> {
    fn keys<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
        match v {
            Value::Object(m) => {
                for (k, c) in m {
                    match (k.as_str(), c) {
                        ("query_key", Value::String(q)) => out.push(q),
                        _ => keys(c, out),
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|c| keys(c, out)),
            _ => {}
        }
    }
    let mut used = Vec::new();
    keys(layout, &mut used);
    let text = layout.to_string();
    feeds
        .iter()
        .filter(|(source, feed)| {
            let Some((_, m)) = catalog.source(source) else {
                return false;
            };
            let Some(spec) = m.feed(feed) else {
                return false;
            };
            let by_key = spec.query_keys.iter().any(|pattern| {
                // A pattern feed's keys use its name for the placeholder.
                let filled = if spec.is_pattern() {
                    let open = pattern.find('{');
                    let close = pattern.find('}');
                    match (open, close) {
                        (Some(o), Some(c)) if o < c => {
                            format!("{}{}{}", &pattern[..o], feed, &pattern[c + 1..])
                        }
                        _ => (*pattern).to_owned(),
                    }
                } else {
                    (*pattern).to_owned()
                };
                used.iter().any(|k| crate::source::key_matches(&filled, k))
            });
            by_key
                || m.entity_schemes
                    .iter()
                    .any(|scheme| text.contains(&format!("${{{scheme}://")))
        })
        .cloned()
        .collect()
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

/// Pod setting: set by the `blueprints` migration on a pod that existed
/// before blueprints; [`seed_blueprint_apps`] converts it once and clears it.
pub const CONVERT_SETTING: &str = "blueprints.convert";
/// The `source` of a version [`seed_blueprint_apps`] wrote.
pub const SOURCE_BLUEPRINT: &str = "blueprint";

/// An app [`seed_blueprint_apps`] creates: id, title, icon, feeds, layout,
/// note.
type Seed = (String, String, String, Vec<(String, String)>, Value, String);

/// A layout that is the whole of blueprint `app`, as app `id`.
pub fn blueprint_ref(app: &str, id: &str) -> Value {
    json!({ "type": crate::parts::REF_TYPE, "app": app, "part": crate::parts::LAYOUT_PART, "id": id })
}

/// Start a pod's apps from blueprints (decision `blueprints-and-home`):
/// the [`crate::HOME_APP`] (a reference to [`crate::HOME_BLUEPRINT`]) when
/// the pod has none and the catalog has the blueprint; and once, on a pod
/// made before blueprints ([`CONVERT_SETTING`]), one app referencing each
/// built-in it showed (not hidden, not dev), so nothing disappears.
pub fn seed_blueprint_apps(store: &Store, catalog: &Catalog) -> rusqlite::Result<()> {
    let convert = store.pod_setting(CONVERT_SETTING)?.is_some();
    let has_home: bool = store.with_conn(|c| {
        c.query_row(
            "SELECT EXISTS (SELECT 1 FROM custom_apps WHERE id = ?1)",
            [crate::HOME_APP],
            |r| r.get(0),
        )
    })?;
    let mut apps: Vec<Seed> = Vec::new();
    if !has_home && catalog.is_app(crate::HOME_BLUEPRINT) {
        apps.push((
            crate::HOME_APP.to_owned(),
            "Home".to_owned(),
            "dashboard".to_owned(),
            Vec::new(),
            blueprint_ref(crate::HOME_BLUEPRINT, crate::HOME_APP),
            "Started from the Home blueprint".to_owned(),
        ));
    }
    if convert {
        let hidden = hidden_apps(store)?;
        for a in catalog.apps() {
            if a.dev || crate::SYSTEM_BLUEPRINTS.contains(&a.id) || hidden.iter().any(|h| h == a.id)
            {
                continue;
            }
            let id = new_custom_id();
            let icon = if APP_ICONS.contains(&a.icon) {
                a.icon
            } else {
                "dashboard"
            };
            let feeds = a
                .feeds
                .iter()
                .map(|(s, f)| ((*s).to_owned(), (*f).to_owned()))
                .collect();
            let layout = blueprint_ref(a.id, &id);
            apps.push((
                id,
                a.title.to_owned(),
                icon.to_owned(),
                feeds,
                layout,
                format!("Kept from before blueprints: the {} blueprint", a.title),
            ));
        }
    }
    if apps.is_empty() && !convert {
        return Ok(());
    }
    let mut rows = Vec::new();
    for (id, title, icon, feeds, layout, note) in apps {
        let expanded =
            crate::parts::expand(store, catalog, &layout, None, crate::parts::Mode::Lenient)
                .unwrap_or(Value::Null);
        let features = serde_json::from_value::<protocol::UiNode>(expanded)
            .map(|n| recorded_features(&n))
            .unwrap_or_default();
        rows.push((id, title, icon, feeds, layout.to_string(), note, features));
    }
    store.transaction(|tx| {
        let now = now_secs();
        for (id, title, icon, feeds, json, note, features) in &rows {
            tx.execute(
                "INSERT INTO custom_apps (id, title, icon, feeds_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    id,
                    title,
                    icon,
                    serde_json::to_string(feeds).expect("serializes"),
                    now
                ],
            )?;
            crate::store::append_authored_version_on(
                tx,
                id,
                json,
                SOURCE_BLUEPRINT,
                Some(note),
                Authored {
                    author: AUTHOR_POD,
                    meta_json: None,
                    features,
                },
            )?;
            tracing::info!(app = %id, "app started from a blueprint");
        }
        tx.execute("DELETE FROM pod_settings WHERE key = ?1", [CONVERT_SETTING])?;
        if convert {
            // The apps just made bring their feeds; home owns them
            // ([`convert_to_layouts`], next).
            tx.execute(
                "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, '1')",
                [LAYOUTS_CONVERT_SETTING],
            )?;
        }
        Ok(())
    })
}

/// Pod setting: set by the `layouts_not_apps` migration; cleared by
/// [`convert_to_layouts`].
pub const LAYOUTS_CONVERT_SETTING: &str = "layouts.convert";
/// The `source` of a home version [`convert_to_layouts`] wrote.
pub const SOURCE_LAYOUTS: &str = "layouts";

/// Home as a plain list of the pod's layouts, each pushing its layout, with
/// the inbox and Settings at the top (decision `layouts-not-apps`).
pub fn home_list_layout() -> Value {
    json!({
        "type": "core://Workspace", "id": crate::HOME_APP, "title": "Home",
        "slots": [{"slot_id": "home", "provider_uri": "local://home", "content": {
            "type": "core://Column", "children": [
                {"type": "core://Row", "padding_h_dp": 8, "children": [
                    {"type": "core://Spacer"},
                    {"type": "core://IconButton", "icon": "notifications", "content_description": "Notifications",
                     "on_click": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet"}},
                    {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
                     "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}
                ]},
                {"type": "core://List", "query_key": "local:layouts",
                 "empty_text": "Nothing here yet. Ask your agent to build something.",
                 "item_template": {"type": "core://Tappable",
                    "on_click": {"intent": "action.navigation.open", "layout": "$item.id", "presentation": "Push"},
                    "content": {"type": "core://Column", "padding_h_dp": 16, "padding_v_dp": 12, "spacing_dp": 2,
                        "children": [
                            {"type": "core://Text", "style": "Title", "value_expr": "$item.title"},
                            {"type": "core://Text", "style": "Secondary", "value_expr": "$item.version"}
                        ]}}}
            ]}}]
    })
}

/// Once, after the `layouts_not_apps` migration ([`LAYOUTS_CONVERT_SETTING`]):
/// home's feeds become every live layout's feeds (each once, at most
/// [`MAX_HOME_FEEDS`], in order: home's, then oldest layout first), the
/// others' are cleared; and a home whose latest version uses a retired
/// node or command gets a new version, [`home_list_layout`].
pub fn convert_to_layouts(store: &Store, catalog: &Catalog) -> rusqlite::Result<()> {
    if store.pod_setting(LAYOUTS_CONVERT_SETTING)?.is_none() {
        return Ok(());
    }
    // Raw rows: `custom_apps_on` reports home's feeds for every layout.
    let raw: Vec<(String, String)> = store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id, feeds_json FROM custom_apps WHERE deleted_at IS NULL
             ORDER BY id = ?1 DESC, created_at, id",
        )?;
        stmt.query_map([crate::HOME_APP], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut feeds: Vec<(String, String)> = Vec::new();
    for (_, json) in &raw {
        for f in serde_json::from_str::<Vec<(String, String)>>(json).unwrap_or_default() {
            if !feeds.contains(&f) && feeds.len() < MAX_HOME_FEEDS {
                feeds.push(f);
            }
        }
    }
    let has_home = raw.iter().any(|(id, _)| id == crate::HOME_APP);
    let retired = has_home
        && store.app_latest(crate::HOME_APP)?.is_some_and(|(_, json)| {
            let doc: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
            let doc = if crate::parts::may_hold_refs(&json) {
                crate::parts::expand(store, catalog, &doc, None, crate::parts::Mode::Lenient)
                    .unwrap_or(doc)
            } else {
                doc
            };
            serde_json::from_value::<protocol::UiNode>(doc).is_ok_and(|n| {
                protocol::required_features(&n)
                    .iter()
                    .any(|f| protocol::REMOVED_FEATURES.contains(f))
            })
        });
    let list = home_list_layout();
    let list_features = serde_json::from_value::<protocol::UiNode>(list.clone())
        .map(|n| recorded_features(&n))
        .unwrap_or_default();
    store.transaction(|tx| {
        if has_home {
            tx.execute(
                "UPDATE custom_apps SET feeds_json = CASE WHEN id = ?1 THEN ?2 ELSE '[]' END
                 WHERE deleted_at IS NULL",
                params![
                    crate::HOME_APP,
                    serde_json::to_string(&feeds).expect("serializes")
                ],
            )?;
        }
        if retired {
            crate::store::append_authored_version_on(
                tx,
                crate::HOME_APP,
                &list.to_string(),
                SOURCE_LAYOUTS,
                Some("A list of your layouts: the dock and drawer were retired"),
                Authored {
                    author: AUTHOR_POD,
                    meta_json: None,
                    features: &list_features,
                },
            )?;
            tracing::info!("home converted to a list of layouts");
        }
        tx.execute(
            "DELETE FROM pod_settings WHERE key = ?1",
            [LAYOUTS_CONVERT_SETTING],
        )?;
        Ok(())
    })
}

/// The features a custom layout is recorded as using: those of the layout as
/// validated, built-in parts expanded ([`protocol::required_features`]).
pub fn recorded_features(layout: &protocol::UiNode) -> String {
    protocol::required_features(layout).join(",")
}

// ---------- validation ----------

/// Why a write was refused: the HTTP status and JSON body.
pub type Refusal = (StatusCode, Value);

fn refuse(status: StatusCode, body: Value) -> Refusal {
    (status, body)
}

fn invalid(error: &str, detail: impl Into<String>) -> Refusal {
    refuse(
        StatusCode::BAD_REQUEST,
        json!({ "error": error, "detail": detail.into() }),
    )
}

/// A refused layout: 422 `invalid_layout` with `at` and `allowed`.
pub fn invalid_layout(e: &LayoutError) -> Refusal {
    let mut body = json!({ "error": "invalid_layout", "detail": e.to_string() });
    body["at"] = json!(e.pointer);
    body["allowed"] = json!(e.allowed);
    refuse(StatusCode::UNPROCESSABLE_ENTITY, body)
}

/// Check a feeds list: 1..=[`MAX_FEEDS`] distinct `[source, feed]` pairs,
/// each a declared feed of a source custom apps may use (not a dev source
/// unless dev apps are shown).
pub fn check_feeds(pod: &Pod, v: &Value) -> Result<Vec<(String, String)>, Refusal> {
    check_feeds_upto(pod, v, MAX_FEEDS)
}

/// [`check_feeds`] with at most `max` feeds; `null` or `[]` is none.
pub fn check_feeds_upto(
    pod: &Pod,
    v: &Value,
    max: usize,
) -> Result<Vec<(String, String)>, Refusal> {
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
    if v.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = v.as_array() else {
        return Err(bad(
            "\"feeds\" must be a list of [source, feed] pairs".into()
        ));
    };
    if items.len() > max {
        return Err(bad(format!("at most {max} feeds")));
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

/// Home's feeds, or `None` on a pod without a home.
async fn home_feeds(pod: &Pod) -> Result<Option<Vec<(String, String)>>, Refusal> {
    blocking(pod, |s| s.with_conn(home_feeds_on))
        .await
        .map_err(|(st, Json(b))| (st, b))
}

/// `home` with `extra` added (in order, each once): the scope of a write
/// that names feeds, and whether home grows. Refused past
/// [`MAX_HOME_FEEDS`].
fn grow(
    home: &[(String, String)],
    extra: &[(String, String)],
) -> Result<(Vec<(String, String)>, bool), Refusal> {
    let mut out = home.to_vec();
    for f in extra {
        if !out.contains(f) {
            out.push(f.clone());
        }
    }
    if out.len() > MAX_HOME_FEEDS {
        return Err(invalid(
            "invalid_feeds",
            format!("home declares at most {MAX_HOME_FEEDS} feeds; remove one it no longer needs"),
        ));
    }
    let grew = out.len() > home.len();
    Ok((out, grew))
}

/// Check that every live layout but `except` still fits `feeds` (home's
/// new feeds): refused naming the first layout and what it binds.
async fn check_scope(pod: &Pod, feeds: &[(String, String)], except: &str) -> Result<(), Refusal> {
    let rows = blocking(pod, move |s| {
        let mut out = Vec::new();
        for app in s.with_conn(custom_apps_on)? {
            if let Some((_, json)) = s.app_latest(&app.id)? {
                out.push((app, json));
            }
        }
        Ok(out)
    })
    .await
    .map_err(|(st, Json(b))| (st, b))?;
    for (app, json) in rows {
        if app.id == except {
            continue;
        }
        let layout: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
        if let Err((_, body)) = check_layout_with(pod, &app.id, feeds, &layout, false) {
            let what = body["detail"].as_str().unwrap_or("").to_owned();
            return Err(refuse(
                StatusCode::CONFLICT,
                json!({ "error": "feed_in_use", "layout": app.id, "title": app.title,
                        "at": body["at"],
                        "detail": format!("layout '{}' ({}) still binds a feed this change drops: {what}", app.title, app.id) }),
            ));
        }
    }
    Ok(())
}

/// Write home's feeds in `tx`.
fn set_home_feeds(
    tx: &rusqlite::Transaction<'_>,
    feeds: &[(String, String)],
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE custom_apps SET feeds_json = ?1 WHERE id = ?2",
        params![
            serde_json::to_string(feeds).expect("serializes"),
            crate::HOME_APP
        ],
    )?;
    Ok(())
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
/// `id`: the parsed layout and its JSON. The sources' own checks
/// ([`crate::AppAdapter::check_custom_layout`]) run too.
pub fn check_layout(
    pod: &Pod,
    id: &str,
    feeds: &[(String, String)],
    layout: &Value,
) -> Result<(protocol::UiNode, String), Refusal> {
    let (node, json, expanded) = check_layout_with(pod, id, feeds, layout, true)?;
    let _ = expanded;
    Ok((node, json))
}

/// [`check_layout`], with or without the sources' own checks (`hooks`; a
/// source checking a layout against data it is about to change runs its
/// own), also answering the layout as validated (parts expanded).
pub fn check_layout_with(
    pod: &Pod,
    id: &str,
    feeds: &[(String, String)],
    layout: &Value,
    hooks: bool,
) -> Result<(protocol::UiNode, String, Value), Refusal> {
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
    // References to built-in parts (decision `layout-parts`) are checked
    // expanded (with the built-ins' latest versions): what they bring must
    // fit this app's sources too. They are stored as written.
    let has_refs = !crate::parts::refs(&layout).is_empty();
    let expanded = if has_refs {
        crate::parts::expand(
            pod.store(),
            pod.catalog(),
            &layout,
            None,
            crate::parts::Mode::Strict,
        )
        .map_err(|e| invalid_layout(&e))?
    } else {
        layout.clone()
    };
    let node =
        crate::validate::custom_layout(id, &expanded, &sources).map_err(|e| invalid_layout(&e))?;
    if hooks {
        for (name, _) in &sources {
            let Some((adapter, _)) = pod.catalog().source(name) else {
                continue;
            };
            let names: Vec<String> = feeds
                .iter()
                .filter(|(s, _)| s == name)
                .map(|(_, f)| f.clone())
                .collect();
            adapter
                .check_custom_layout(pod, &names, &expanded)
                .map_err(|e| invalid_layout(&e))?;
        }
    }
    let json = if has_refs {
        layout.to_string()
    } else {
        serde_json::to_string(&node).expect("a UiNode serializes")
    };
    Ok((node, json, expanded))
}

/// A write's `copied_from` (`{app, version}` of a built-in app): checked.
fn check_copied_from(pod: &Pod, body: &Value) -> Result<Option<String>, Refusal> {
    let Some(c) = body.get("copied_from").filter(|c| !c.is_null()) else {
        return Ok(None);
    };
    let (Some(app), Some(version)) = (c["app"].as_str(), c["version"].as_i64()) else {
        return Err(invalid(
            "invalid_copied_from",
            "\"copied_from\" is {\"app\": <built-in app>, \"version\": n}",
        ));
    };
    let exists = pod.catalog().is_app(app)
        && pod
            .store()
            .app_version(app, version)
            .ok()
            .flatten()
            .is_some();
    if !exists {
        return Err(invalid(
            "invalid_copied_from",
            format!("'{app}' v{version} is not a built-in app's version"),
        ));
    }
    Ok(Some(
        json!({ "copied_from": { "app": app, "version": version } }).to_string(),
    ))
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
    /// The pod itself (a `ref_update`).
    Pod,
}

impl Writer {
    fn author(self) -> &'static str {
        match self {
            Writer::User => AUTHOR_USER,
            Writer::Claude => AUTHOR_CLAUDE,
            Writer::Pod => AUTHOR_POD,
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
    /// [`recorded_features`].
    features: String,
    by: Writer,
    /// Its `meta_json`; `None`: the previous version's (a copy stays one).
    meta: Option<String>,
}

/// Append a version and prune the oldest beyond the cap.
fn append(
    tx: &rusqlite::Transaction<'_>,
    caps: Caps,
    id: &str,
    v: NewVersion<'_>,
) -> rusqlite::Result<i64> {
    let meta = match v.meta {
        Some(m) => Some(m),
        None => crate::store::latest_app_version_on(tx, id)?.and_then(|l| l.meta_json),
    };
    let version = crate::store::append_authored_version_on(
        tx,
        id,
        v.json,
        v.source,
        Some(v.note),
        Authored {
            author: v.by.author(),
            meta_json: meta.as_deref(),
            features: &v.features,
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
    // Feeds named here join home's: every layout binds home's (decision
    // `layouts-not-apps`). On a pod without a home they are the app's own.
    let extra = check_feeds(pod, &body["feeds"])?;
    let note = check_note(body.get("note"), "Created")?;
    let meta = check_copied_from(pod, body)?;
    let id = new_custom_id();
    let home = home_feeds(pod).await?;
    let (scope, grew) = match &home {
        Some(h) => grow(h, &extra)?,
        None => (extra.clone(), false),
    };
    let (layout, json) = check_layout(pod, &id, &scope, &body["layout"])?;
    let features = recorded_features(&layout);
    let caps = pod.custom.caps;
    let own: &[(String, String)] = if home.is_some() { &[] } else { &extra };
    let feeds_json = serde_json::to_string(own).expect("serializes");
    let new_home = grew.then(|| scope.clone());
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
        if let Some(feeds) = &new_home {
            set_home_feeds(tx, feeds)?;
        }
        Ok(append(
            tx,
            caps,
            &app_id,
            NewVersion {
                json: &json,
                source: SOURCE_CUSTOM,
                note: &note,
                features: features.clone(),
                by,
                meta: meta.clone(),
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
    let features = recorded_features(&layout);
    let meta = check_copied_from(pod, body)?;
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
                features: features.clone(),
                by,
                meta: meta.clone(),
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
    let is_home = id == crate::HOME_APP;
    let home = home_feeds(pod).await?;
    // Home's feeds are replaced (every other layout must still fit them);
    // another layout's join home's (decision `layouts-not-apps`).
    let feeds = match body.get("feeds") {
        Some(v) if is_home || home.is_none() => {
            let max = if is_home { MAX_HOME_FEEDS } else { MAX_FEEDS };
            Some(check_feeds_upto(pod, v, max)?)
        }
        Some(v) => {
            let extra = check_feeds(pod, v)?;
            Some(grow(home.as_deref().unwrap_or_default(), &extra)?.0)
        }
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
        if is_home {
            check_scope(pod, feeds, id).await?;
        }
    }
    // The feeds are written on home (or, on a pod without a home, the app).
    let target = if home.is_some() { crate::HOME_APP } else { id }.to_owned();
    let feeds_json = feeds
        .as_ref()
        .map(|f| serde_json::to_string(f).expect("serializes"));
    let app_id = id.to_owned();
    let version = write(pod, move |tx| {
        tx.execute(
            "UPDATE custom_apps SET title = COALESCE(?1, title), icon = COALESCE(?2, icon),
             updated_at = ?3 WHERE id = ?4",
            params![title, icon, now_secs(), app_id],
        )?;
        if let Some(f) = &feeds_json {
            tx.execute(
                "UPDATE custom_apps SET feeds_json = ?1 WHERE id = ?2",
                params![f, target],
            )?;
        }
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
    let Some((old_meta, json)) = old else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            json!({ "error": "version_not_found", "app_id": id, "version": n }),
        ));
    };
    let meta = old_meta.meta_json.clone().or_else(|| Some("{}".to_owned()));
    let layout: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
    let (node, json) = check_layout(pod, id, &app.feeds, &layout)?;
    let features = recorded_features(&node);
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
                features: features.clone(),
                by,
                meta: meta.clone(),
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
    if id == crate::HOME_APP {
        return Err(refuse(
            StatusCode::CONFLICT,
            json!({ "error": "home_app", "detail": "the home app can't be deleted; change its layout instead (restore_version undoes a change)" }),
        ));
    }
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

/// A dry run of a create: `{feeds, layout}` -> `{ok, features}`.
pub fn validate(pod: &Pod, body: &Value) -> Result<Value, Refusal> {
    let extra = check_feeds(pod, &body["feeds"])?;
    let home = pod.store().with_conn(home_feeds_on).map_err(|_| {
        refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "store_error"}),
        )
    })?;
    let feeds = match &home {
        Some(h) => grow(h, &extra)?.0,
        None => extra,
    };
    let id = body["id"]
        .as_str()
        .filter(|i| is_custom_id(i))
        .unwrap_or("c-aaaaaaaaaa");
    let (node, _) = check_layout(pod, id, &feeds, &body["layout"])?;
    Ok(json!({ "ok": true, "features": protocol::required_features(&node) }))
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

// ---------- staged writes (another source's change set) ----------

/// A checked layout edit of a custom app, to append inside another write's
/// transaction ([`commit_staged`]): how a source changes its data and the
/// apps that show it at once (decision `collections`).
#[derive(Debug, Clone)]
pub struct StagedLayout {
    pub id: String,
    json: String,
    features: String,
    note: String,
    meta: Option<String>,
    base: Option<i64>,
}

/// Check an edit `{layout, note?, base_version?, copied_from?}` of custom
/// app `app` without the sources' own checks (the caller runs them against
/// the data it is about to write): the staged edit and the layout as
/// validated (parts expanded).
pub fn stage_layout(
    pod: &Pod,
    app: &CustomApp,
    body: &Value,
) -> Result<(StagedLayout, Value), Refusal> {
    let note = check_note(body.get("note"), "Edited")?;
    let base =
        match body.get("base_version") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_i64().filter(|n| *n >= 1).ok_or_else(|| {
                invalid("invalid_request", "\"base_version\" is a version number")
            })?),
        };
    let (node, json, expanded) =
        check_layout_with(pod, &app.id, &app.feeds, &body["layout"], false)?;
    let meta = check_copied_from(pod, body)?;
    Ok((
        StagedLayout {
            id: app.id.clone(),
            json,
            features: recorded_features(&node),
            note,
            meta,
            base,
        },
        expanded,
    ))
}

/// Count one custom-app write against the pod's rate cap.
pub fn allow_write(pod: &Pod) -> Result<(), Refusal> {
    if pod.custom.allow_write() {
        return Ok(());
    }
    Err(refuse(
        StatusCode::TOO_MANY_REQUESTS,
        json!({ "error": "rate_limited", "detail": format!("at most {} custom-app writes a minute", pod.custom.caps.writes_per_minute) }),
    ))
}

/// Append a staged edit in `tx` (the caller publishes the version with
/// [`crate::AppState::publish_layout`] after committing). `Ok(Err(_))`:
/// refused (the app changed since `base_version`, or was deleted); roll
/// back.
pub fn commit_staged(
    tx: &rusqlite::Transaction<'_>,
    pod: &Pod,
    staged: &StagedLayout,
    by: Writer,
) -> rusqlite::Result<Result<i64, Refusal>> {
    if custom_app_on(tx, &staged.id)?.is_none() {
        return Ok(Err(not_found(&staged.id)));
    }
    let latest = crate::store::latest_app_version_on(tx, &staged.id)?.map(|v| v.version);
    if let Some(base) = staged.base
        && Some(base) != latest
    {
        return Ok(Err(refuse(
            StatusCode::CONFLICT,
            json!({ "error": "stale_base", "app": staged.id, "latest_version": latest,
                    "detail": "the app changed since that version; read it again and reapply your edit" }),
        )));
    }
    let v = append(
        tx,
        pod.custom.caps,
        &staged.id,
        NewVersion {
            json: &staged.json,
            source: SOURCE_CUSTOM,
            note: &staged.note,
            features: staged.features.clone(),
            by,
            meta: staged.meta.clone(),
        },
    )?;
    Ok(Ok(v))
}

/// The latest layout of every live custom app with a feed of `source`:
/// `(app, latest version, layout as served to a current client, parts
/// expanded)`. Blocking.
pub fn apps_using(
    store: &Store,
    catalog: &crate::Catalog,
    source: &str,
) -> rusqlite::Result<Vec<(CustomApp, i64, Value)>> {
    let mut out = Vec::new();
    for app in store.with_conn(custom_apps_on)? {
        if !app.feeds.iter().any(|(s, _)| s == source) {
            continue;
        }
        let Some((v, json)) = store.app_latest(&app.id)? else {
            continue;
        };
        let doc: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
        let doc = if crate::parts::may_hold_refs(&json) {
            crate::parts::expand(store, catalog, &doc, None, crate::parts::Mode::Lenient)
                .unwrap_or(doc)
        } else {
            doc
        };
        out.push((app, v.version, doc));
    }
    Ok(out)
}

// ---------- built-in parts and copies (decision `layout-parts`) ----------

/// After a deploy appended versions of built-in apps (`changed`: `(app,
/// new version)`): append a `ref_update` version ("Pixelfed updated to
/// v6") to every live custom app whose latest layout references one of
/// them without a pinned version, so its history says what changed and
/// clients refetch. Returns the custom versions appended. Blocking.
pub fn ref_updates(
    store: &Store,
    catalog: &crate::Catalog,
    changed: &[(String, i64)],
) -> rusqlite::Result<Vec<(String, i64)>> {
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let apps = store.with_conn(custom_apps_on)?;
    let mut out = Vec::new();
    for app in apps {
        let Some((_, json)) = store.app_latest(&app.id)? else {
            continue;
        };
        if !crate::parts::may_hold_refs(&json) {
            continue;
        }
        let doc: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
        let refs = crate::parts::refs(&doc);
        let Some((builtin, version)) = changed
            .iter()
            .find(|(a, _)| refs.iter().any(|r| r.version.is_none() && r.app == *a))
        else {
            continue;
        };
        let title = catalog.app(builtin).map_or(builtin.as_str(), |a| a.title);
        let note = format!("{title} updated to v{version}");
        let features =
            crate::parts::expand(store, catalog, &doc, None, crate::parts::Mode::Lenient)
                .ok()
                .and_then(|v| serde_json::from_value::<protocol::UiNode>(v).ok())
                .map_or_else(
                    || protocol::feature::LEGACY_NODES.to_owned(),
                    |n| recorded_features(&n),
                );
        let id = app.id.clone();
        let v = store.transaction(|tx| {
            append(
                tx,
                Caps::default(),
                &id,
                NewVersion {
                    json: &json,
                    source: SOURCE_REF_UPDATE,
                    note: &note,
                    features: features.clone(),
                    by: Writer::Pod,
                    meta: None,
                },
            )
        })?;
        tracing::info!(app = %app.id, version = v, "{note}");
        out.push((app.id, v));
    }
    Ok(out)
}

/// The dismissed update notices ([`DISMISSED_SETTING`]).
pub fn dismissed(store: &Store) -> rusqlite::Result<HashMap<String, i64>> {
    Ok(store
        .pod_setting(DISMISSED_SETTING)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

/// The built-in a custom app's latest version was copied from, `(app,
/// version)` (`meta_json.copied_from`). Blocking.
pub fn copied_from(store: &Store, id: &str) -> rusqlite::Result<Option<(String, i64)>> {
    let Some(v) = store.app_latest_meta(id)? else {
        return Ok(None);
    };
    Ok(v.meta_json
        .and_then(|m| serde_json::from_str::<Value>(&m).ok())
        .and_then(|m| {
            let c = &m["copied_from"];
            Some((c["app"].as_str()?.to_owned(), c["version"].as_i64()?))
        }))
}

/// The pod's own layouts built on each blueprint: `blueprint id -> layout
/// id`, the first live layout (oldest first, home last) whose latest
/// version is a whole reference to it or was copied from it (decision
/// `layouts-not-apps`: notifications open these). Blocking.
pub fn layouts_for_blueprints(store: &Store) -> rusqlite::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut apps = store.with_conn(custom_apps_on)?;
    // Home is listed first; it is the last choice.
    let home_first = apps.first().is_some_and(|a| a.id == crate::HOME_APP);
    apps.rotate_left(usize::from(home_first));
    for app in apps {
        let Some((_, json)) = store.app_latest(&app.id)? else {
            continue;
        };
        let mut built_on: Vec<String> = Vec::new();
        if crate::parts::may_hold_refs(&json) {
            let doc: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
            built_on.extend(
                crate::parts::refs(&doc)
                    .into_iter()
                    .filter(|r| r.part == "layout")
                    .map(|r| r.app),
            );
        }
        if let Some((from, _)) = copied_from(store, &app.id)? {
            built_on.push(from);
        }
        for b in built_on {
            out.entry(b).or_insert_with(|| app.id.clone());
        }
    }
    Ok(out)
}

/// Set each notification's `layout` ([`layouts_for_blueprints`]).
pub fn target_layouts(map: &HashMap<String, String>, items: &mut [protocol::Notification]) {
    for n in items {
        n.layout = Some(
            map.get(&n.app_id)
                .cloned()
                .unwrap_or_else(|| n.app_id.clone()),
        );
    }
}

/// `GET /v1/apps` `updates_from` of a custom app copied from a built-in:
/// `{app, title, from_version, to_version}` when that app has a newer
/// version `client` can draw, not dismissed. Blocking.
pub fn updates_from(
    store: &Store,
    catalog: &crate::Catalog,
    id: &str,
    client: &protocol::ClientCaps,
    dismissed: &HashMap<String, i64>,
) -> rusqlite::Result<Option<Value>> {
    let Some((app, from)) = copied_from(store, id)? else {
        return Ok(None);
    };
    let Some(newest) = crate::layouts::newest_compatible(store, &app, client)? else {
        return Ok(None);
    };
    let seen = dismissed.get(id).copied().unwrap_or(0).max(from);
    if newest.version <= seen {
        return Ok(None);
    }
    let title = catalog
        .app(&app)
        .map_or(app.as_str(), |a| a.title)
        .to_owned();
    Ok(Some(
        json!({ "app": app, "title": title, "from_version": from, "to_version": newest.version }),
    ))
}

/// Dismiss app `id`'s update notice up to built-in version `version`.
pub async fn dismiss_update(pod: &Pod, id: &str, version: i64) -> Result<(), Refusal> {
    load(pod, id).await?;
    let app_id = id.to_owned();
    write(pod, move |tx| {
        let mut map: HashMap<String, i64> = crate::store::pod_setting_on(tx, DISMISSED_SETTING)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        map.insert(app_id, version);
        tx.execute(
            "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, ?2)",
            params![
                DISMISSED_SETTING,
                serde_json::to_string(&map).expect("serializes")
            ],
        )?;
        Ok(())
    })
    .await
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
        .route(
            "/v1/custom-apps/{id}/dismiss-update",
            post(dismiss_update_route),
        )
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

async fn dismiss_update_route(
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
    let Some(version) = body["version"].as_i64().filter(|v| *v >= 1) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "version": "the built-in version dismissed" } }),
        );
    };
    answer(dismiss_update(&pod, &id, version).await, |()| {
        Json(json!({ "ok": true })).into_response()
    })
}
