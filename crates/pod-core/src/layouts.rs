//! The pod's layouts (decision `content-addressed-construct`): what every
//! layout of the pod shares, over the construct's names.
//!
//! - **Ids.** A pod layout's name is its id: [`crate::HOME_APP`] or `c-` and
//!   10 base32 characters ([`is_layout_id`]); a blueprint is `@core/<id>`.
//! - **Writes.** Caps and the write-rate window ([`Caps`], [`allow_write`]).
//! - **Feeds.** Home owns every feed (its element's `bind`): which feeds a
//!   layout binds ([`bound_feeds`]), which layout owns a feed for
//!   notifications ([`feed_owners`]), and checks of feed lists.
//! - **Files.** Blueprints ship as files `crates/source-*/layouts/<blueprint>/
//!   NNNN.json` ([`LayoutSource`]); [`crate::construct::core`] makes them
//!   elements named `@core/<blueprint>` when a pod opens. Files are
//!   append-only: never edit or renumber a deployed one; old ones may be
//!   deleted, oldest first (decision `retired-layout-files`).
//! - **Events.** What the construct's event stream sends besides new roots
//!   ([`DataEvent`], notifications; [`crate::construct::events`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    http::{HeaderMap, StatusCode},
    response::{Response, sse::Event},
};
use protocol::UiNode;
use rusqlite::Connection;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::adapter::Catalog;
use crate::construct::store as cstore;
use crate::source::SourceManifest;
use crate::store::Store;
use crate::{AppState, Pod, blocking, json_error};

/// Default SSE keepalive interval.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Default cap on open SSE streams per session token. A client keeps one
/// stream open; the headroom covers a pod token shared by a few devices and
/// dead connections the server has not noticed yet.
pub const DEFAULT_SSE_STREAMS_PER_TOKEN: usize = 8;
/// Capacity of the event broadcast channels (a lagging subscriber is caught
/// up with the current root, so losing intermediate events is harmless).
pub const CHANNEL_CAPACITY: usize = 64;

// ---------- layout files ----------

/// One embedded layout file: `<dir>/<app>/<name>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSource {
    /// The folder of the app folders, as the repository names it
    /// (`crates/source-events/layouts`): only for messages.
    pub dir: &'static str,
    pub app_id: &'static str,
    /// `NNNN` (4 digits).
    pub name: &'static str,
    pub text: &'static str,
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

/// A layout as JSON, through its text form: `serde_json::to_value` would
/// widen `f32` fields (`aspect_ratio` 0.8 -> 0.800000011920929).
pub fn node_value(node: &UiNode) -> Value {
    serde_json::to_string(node)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
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

// ---------- events ----------

use crate::notify::NotifyEvent;

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

pub(crate) fn data_event(
    workspace_id: &str,
    source: Option<&str>,
    scope: Option<&DataScope>,
) -> Event {
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

/// The next tick of a stream's session re-check; never without one.
pub(crate) async fn tick(i: &mut Option<tokio::time::Interval>) {
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
pub(crate) async fn session_still_valid(state: &Arc<AppState>, hash: Option<&str>) -> bool {
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

pub(crate) async fn recv_notify(
    rx: &mut Option<broadcast::Receiver<NotifyEvent>>,
) -> Option<NotifyEvent> {
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

pub(crate) fn notify_event(ev: &NotifyEvent) -> Event {
    match ev {
        NotifyEvent::New(n) => Event::default()
            .event(protocol::notify::NOTIFICATION_EVENT)
            .data(serde_json::to_string(n).unwrap_or_default()),
        NotifyEvent::Read(r) => Event::default()
            .event(protocol::notify::NOTIFICATIONS_READ_EVENT)
            .data(serde_json::to_string(r).unwrap_or_default()),
    }
}

// ---------- ids ----------

/// Whether `id` has the shape of one of the pod's own layouts: `c-` and 10
/// base32 characters, or the pod's own [`crate::HOME_APP`] (a blueprint's
/// id never holds `-`).
pub fn is_layout_id(id: &str) -> bool {
    id == crate::HOME_APP
        || id.strip_prefix("c-").is_some_and(|rest| {
            rest.len() == 10
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
        })
}

/// A new layout id.
pub fn new_layout_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("OS random number generator");
    let tail: String = buf
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % 32] as char)
        .collect();
    format!("c-{tail}")
}

// ---------- writes ----------

/// Who wrote a name's move: the user's agent.
pub const AUTHOR_CLAUDE: &str = "claude";
/// What the pod itself writes (a conversion, a deploy's `@core/` names).
pub const AUTHOR_POD: &str = "pod";
/// Most feeds home binds.
pub const MAX_HOME_FEEDS: usize = 32;
/// Most feeds one write may name.
pub const MAX_FEEDS: usize = 8;
/// Longest layout title.
pub const MAX_TITLE_CHARS: usize = 60;
/// The icons a layout may have.
pub const LAYOUT_ICONS: &[&str] = &["photo", "recipe", "dashboard"];
/// Longest note of a move.
pub const MAX_NOTE_CHARS: usize = 500;

/// Limits per pod (temporary pods get the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The pod's own layouts (home included).
    pub layouts: usize,
    /// Writes (layouts, components, collections' changes) per minute.
    pub writes_per_minute: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            layouts: 50,
            writes_per_minute: 60,
        }
    }
}

/// The per-pod write state: caps and the write-rate window.
#[derive(Debug, Default)]
pub struct WriteState {
    pub(crate) caps: Caps,
    writes: Mutex<VecDeque<Instant>>,
}

impl WriteState {
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            writes: Mutex::new(VecDeque::new()),
        }
    }

    /// Count one write; `false` when the last minute already had
    /// [`Caps::writes_per_minute`].
    fn allow(&self) -> bool {
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

/// A refused write: its status and JSON body.
pub type Refusal = (StatusCode, Value);

/// Count one write against the pod's rate cap.
pub fn allow_write(pod: &Pod) -> Result<(), Refusal> {
    if pod.writes.allow() {
        return Ok(());
    }
    Err((
        StatusCode::TOO_MANY_REQUESTS,
        json!({ "error": "rate_limited",
                "detail": format!("at most {} layout writes a minute", pod.writes.caps.writes_per_minute) }),
    ))
}

// ---------- feeds ----------

/// Home's feeds: its element's `bind` (decision `content-addressed-construct`),
/// the feeds every layout of the pod may bind.
pub fn home_feeds(conn: &Connection) -> Vec<(String, String)> {
    cstore::name(conn, crate::HOME_APP)
        .ok()
        .flatten()
        .and_then(|h| cstore::object(conn, &h).ok().flatten())
        .and_then(|(v, _)| serde_json::from_value(v["bind"]["feeds"].clone()).ok())
        .unwrap_or_default()
}

/// The sources of home's feeds, each once, in order: the data a client of
/// the construct is told changed (`event: data`).
pub fn home_sources(catalog: &Catalog, conn: &Connection) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (s, _) in home_feeds(conn) {
        if let Some((a, _)) = catalog.source(&s)
            && !out.contains(&a.name())
        {
            out.push(a.name());
        }
    }
    out
}

/// Check a feeds list: at most `max` distinct `[source, feed]` pairs (or
/// `"source/feed"`), each a declared feed of a source layouts may use (not
/// a dev source unless dev blueprints are shown); `null` or `[]` is none.
pub fn check_feeds(pod: &Pod, v: &Value, max: usize) -> Result<Vec<(String, String)>, Value> {
    let offered = || -> Vec<String> {
        pod.catalog()
            .sources()
            .filter(|(_, m)| !m.dev || pod.show_dev_apps())
            .flat_map(|(s, m)| m.feeds.iter().map(move |f| format!("{s}/{}", f.name)))
            .collect()
    };
    let bad = |detail: String| json!({ "error": "invalid_feeds", "detail": detail, "allowed": offered() });
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

/// The feeds of `feeds` that `layout` (materialized) binds: those whose
/// query keys it lists, or whose source's entities it binds by URI. Only
/// these load with it (decision `layouts-not-apps`).
pub fn bound_feeds(
    catalog: &Catalog,
    feeds: &[(String, String)],
    layout: &Value,
) -> Vec<(String, String)> {
    feed_bindings(catalog, feeds, layout)
        .into_iter()
        .map(|(f, _)| f)
        .collect()
}

/// How a layout binds a feed ([`feed_bindings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// It lists one of the feed's query keys.
    QueryKey,
    /// It only binds an entity of the feed's source by URI.
    EntityUri,
}

/// The feeds of `feeds` that `layout` binds, each with how
/// ([`bound_feeds`]).
pub fn feed_bindings(
    catalog: &Catalog,
    feeds: &[(String, String)],
    layout: &Value,
) -> Vec<((String, String), Binding)> {
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
        .filter_map(|f| {
            let (source, feed) = f;
            let (_, m) = catalog.source(source)?;
            let spec = m.feed(feed)?;
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
            if by_key {
                return Some((f.clone(), Binding::QueryKey));
            }
            m.entity_schemes
                .iter()
                .any(|scheme| text.contains(&format!("${{{scheme}://")))
                .then(|| (f.clone(), Binding::EntityUri))
        })
        .collect()
}

// ---------- the pod's layouts ----------

/// One of the pod's own layouts, as its name points now.
#[derive(Debug, Clone, PartialEq)]
pub struct PodLayout {
    /// Its id (its name).
    pub id: String,
    pub title: String,
    pub icon: String,
    /// Its element.
    pub element: String,
    /// When its name was first written (Unix seconds).
    pub created_at: i64,
}

/// Every live layout of the pod: home first, then oldest first.
pub fn pod_layouts(conn: &Connection) -> rusqlite::Result<Vec<PodLayout>> {
    let mut out = Vec::new();
    for (n, h) in cstore::names(conn)? {
        if !is_layout_id(&n) {
            continue;
        }
        let el = cstore::object(conn, &h)?.map(|(v, _)| v);
        let created_at: i64 = conn.query_row(
            "SELECT COALESCE(MIN(at), 0) FROM name_history WHERE name = ?1",
            [&n],
            |r| r.get(0),
        )?;
        let text = |k: &str| {
            el.as_ref()
                .and_then(|e| e[k].as_str())
                .unwrap_or("")
                .to_owned()
        };
        out.push(PodLayout {
            title: text("title"),
            icon: text("icon"),
            id: n,
            element: h,
            created_at,
        });
    }
    out.sort_by(|a, b| {
        (a.id != crate::HOME_APP, a.created_at, &a.id).cmp(&(
            b.id != crate::HOME_APP,
            b.created_at,
            &b.id,
        ))
    });
    Ok(out)
}

/// One live layout of the pod.
pub fn pod_layout(conn: &Connection, id: &str) -> rusqlite::Result<Option<PodLayout>> {
    if !is_layout_id(id) || cstore::name(conn, id)?.is_none() {
        return Ok(None);
    }
    Ok(pod_layouts(conn)?.into_iter().find(|l| l.id == id))
}

/// Layout `id`'s element materialized (unbound), from the store.
pub fn materialized(conn: &Connection, element: &str) -> Option<Value> {
    let objects = crate::construct::write::ConnObjects::new(conn);
    construct_elements::materialize(&objects, element)
        .ok()
        .map(|m| m.node)
}

/// The one layout each feed of `source` belongs to, for notifications
/// (decision `notification-owner`): `feed -> layout id`. Every layout binds
/// home's feeds, so home "declares" them all; the owner is the oldest live
/// layout whose element binds the feed by query key, else the oldest that
/// binds one of its entities by URI, else home (home is the last choice in
/// both). Feeds home doesn't bind are left out. Blocking.
pub fn feed_owners(
    store: &Store,
    catalog: &Catalog,
    source: &str,
) -> rusqlite::Result<HashMap<String, String>> {
    store.with_conn(|conn| {
        let mine: Vec<(String, String)> = home_feeds(conn)
            .into_iter()
            .filter(|(s, _)| s == source)
            .collect();
        if mine.is_empty() {
            return Ok(HashMap::new());
        }
        let mut layouts = pod_layouts(conn)?;
        let home_first = layouts.first().is_some_and(|a| a.id == crate::HOME_APP);
        layouts.rotate_left(usize::from(home_first));
        let mut by_key: HashMap<String, String> = HashMap::new();
        let mut by_uri: HashMap<String, String> = HashMap::new();
        for l in &layouts {
            let Some(node) = materialized(conn, &l.element) else {
                continue;
            };
            for ((_, f), how) in feed_bindings(catalog, &mine, &node) {
                let into = match how {
                    Binding::QueryKey => &mut by_key,
                    Binding::EntityUri => &mut by_uri,
                };
                into.entry(f).or_insert_with(|| l.id.clone());
            }
        }
        Ok(mine
            .into_iter()
            .map(|(_, f)| {
                let owner = by_key
                    .get(&f)
                    .or_else(|| by_uri.get(&f))
                    .cloned()
                    .unwrap_or_else(|| crate::HOME_APP.to_owned());
                (f, owner)
            })
            .collect())
    })
}

/// The pod's own layouts built on each blueprint: `blueprint id -> layout
/// id`, the first live layout (oldest first, home last) whose element uses
/// the blueprint whole (`core://Use` of `@core/<blueprint>`; decision
/// `layouts-not-apps`: notifications of a blueprint open these). Blocking.
pub fn layouts_for_blueprints(
    catalog: &Catalog,
    conn: &Connection,
) -> rusqlite::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut layouts = pod_layouts(conn)?;
    let home_first = layouts.first().is_some_and(|a| a.id == crate::HOME_APP);
    layouts.rotate_left(usize::from(home_first));
    for l in layouts {
        let Some((el, _)) = cstore::object(conn, &l.element)? else {
            continue;
        };
        construct_elements::refs::visit(&el["node"], &mut |r, site| {
            if site == construct_elements::refs::Site::Use
                && let Some(bp) = r.name.strip_prefix(crate::construct::CORE_PREFIX)
                && catalog.is_blueprint(bp)
            {
                out.entry(bp.to_owned()).or_insert_with(|| l.id.clone());
            }
        });
    }
    Ok(out)
}

/// One of the pod's layouts as a source's checks see it.
#[derive(Debug, Clone)]
pub struct SourceUse {
    pub layout: PodLayout,
    /// Its name's newest history entry.
    pub version: i64,
    /// Its element materialized as devices draw it: components expanded,
    /// its inputs filled in with what the first use site reaching it from
    /// home binds.
    pub node: Value,
    /// Home's feeds of the source (every layout may bind them).
    pub feeds: Vec<String>,
}

/// Every layout of the pod as devices draw it, when home binds a feed of
/// `source` (each may bind it): what a source checks a change of its own
/// data against (decision `collections`). A layout home reaches bound two
/// ways is listed for each. Blocking.
pub fn layouts_with_source(store: &Store, source: &str) -> rusqlite::Result<Vec<SourceUse>> {
    store.with_conn(|conn| {
        let feeds: Vec<String> = home_feeds(conn)
            .into_iter()
            .filter(|(s, _)| s == source)
            .map(|(_, f)| f)
            .collect();
        if feeds.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for l in crate::construct::write::live_layouts(conn)? {
            let element = cstore::name(conn, &l.name)?.unwrap_or_default();
            out.push(SourceUse {
                layout: PodLayout {
                    id: l.name,
                    title: l.title,
                    icon: l.icon,
                    element,
                    created_at: 0,
                },
                version: l.version,
                node: l.node,
                feeds: feeds.clone(),
            });
        }
        Ok(out)
    })
}
