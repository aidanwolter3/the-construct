//! The home app's tables: `home_pins` (apps pinned to the home screen) and
//! `home_requests` (apps asked for with +, and how their build went).

use pod_core::store::Migration;
use protocol::{DataBundle, EntityRecord};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// Query key of the pod's apps (drawer).
pub const APPS_QUERY: &str = "home:apps";
/// Query key of the pinned apps, in pin order.
pub const PINS_QUERY: &str = "home:pins";
/// Query key of the app requests still waiting, building or failed.
pub const REQUESTS_QUERY: &str = "home:requests";

/// How long a request stays on the home screen.
pub const REQUEST_DAYS: i64 = 7;

pub static MIGRATIONS: &[Migration] = &[Migration::Sql(SCHEMA)];

const SCHEMA: &str = "
CREATE TABLE home_pins (
    id         TEXT PRIMARY KEY NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('construct', 'device')),
    target     TEXT NOT NULL,
    title      TEXT NOT NULL,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (kind, target)
);
CREATE TABLE home_requests (
    id          TEXT PRIMARY KEY NOT NULL,
    prompt      TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('queued', 'building', 'ready', 'failed')),
    status_text TEXT NOT NULL,
    app_id      TEXT NOT NULL DEFAULT '',
    app_title   TEXT NOT NULL DEFAULT '',
    error       TEXT NOT NULL DEFAULT '',
    run_id      TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX home_requests_created ON home_requests (created_at);
";

/// An app the home screen lists (`home://apps/<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedApp {
    pub id: String,
    pub title: String,
}

/// A pinned app (`home://pins/<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub title: String,
}

/// An app request (`home://requests/<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub prompt: String,
    pub status: String,
    pub status_text: String,
    pub app_id: String,
    pub app_title: String,
    pub error: String,
    pub created_at: i64,
}

pub fn app_uri(id: &str) -> String {
    format!("home://apps/{id}")
}

pub fn pin_uri(id: &str) -> String {
    format!("home://pins/{id}")
}

pub fn request_uri(id: &str) -> String {
    format!("home://requests/{id}")
}

/// The screen query of one request.
pub fn request_query(id: &str) -> String {
    format!("home:request:{id}")
}

/// The first letter of `title`, upper case ("#" when it isn't a letter).
pub fn letter(title: &str) -> String {
    match title.trim().chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_owned(),
    }
}

fn entity(v: Value) -> EntityRecord {
    match v {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord::default(),
    }
}

// ---------- pins ----------

pub fn pins(conn: &Connection) -> rusqlite::Result<Vec<Pin>> {
    let mut stmt = conn
        .prepare("SELECT id, kind, target, title FROM home_pins ORDER BY position, created_at")?;
    stmt.query_map([], |r| {
        Ok(Pin {
            id: r.get(0)?,
            kind: r.get(1)?,
            target: r.get(2)?,
            title: r.get(3)?,
        })
    })?
    .collect()
}

/// Pin `pin` (last). Pinning the same app again keeps the first pin: its id.
pub fn add_pin(conn: &Connection, pin: &Pin, now: i64) -> rusqlite::Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM home_pins WHERE kind = ?1 AND target = ?2",
            params![pin.kind, pin.target],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let taken: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM home_pins WHERE id = ?1)",
        [&pin.id],
        |r| r.get(0),
    )?;
    if taken {
        // A retried request whose pin is already there (or a clash): keep it.
        return Ok(pin.id.clone());
    }
    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM home_pins",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO home_pins (id, kind, target, title, position, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![pin.id, pin.kind, pin.target, pin.title, position, now],
    )?;
    Ok(pin.id.clone())
}

pub fn pin_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM home_pins", [], |r| r.get(0))
}

/// Unpin `id`; true when it was pinned.
pub fn remove_pin(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM home_pins WHERE id = ?1", [id])? > 0)
}

// ---------- requests ----------

const REQUEST_COLUMNS: &str =
    "id, prompt, status, status_text, app_id, app_title, error, created_at";

fn request_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Request> {
    Ok(Request {
        id: r.get(0)?,
        prompt: r.get(1)?,
        status: r.get(2)?,
        status_text: r.get(3)?,
        app_id: r.get(4)?,
        app_title: r.get(5)?,
        error: r.get(6)?,
        created_at: r.get(7)?,
    })
}

pub fn request(conn: &Connection, id: &str) -> rusqlite::Result<Option<Request>> {
    conn.query_row(
        &format!("SELECT {REQUEST_COLUMNS} FROM home_requests WHERE id = ?1"),
        [id],
        request_row,
    )
    .optional()
}

/// The requests of the last [`REQUEST_DAYS`], newest first.
pub fn recent_requests(conn: &Connection, now: i64) -> rusqlite::Result<Vec<Request>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REQUEST_COLUMNS} FROM home_requests WHERE created_at >= ?1
         ORDER BY created_at DESC, id DESC"
    ))?;
    stmt.query_map([now - REQUEST_DAYS * 86_400], request_row)?
        .collect()
}

/// Requests made since `since` (a daily cap).
pub fn requests_since(conn: &Connection, since: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM home_requests WHERE created_at >= ?1",
        [since],
        |r| r.get(0),
    )
}

/// This process's id: requests record the run that builds them, so a
/// restart can tell its own from the ones it interrupted.
pub fn run_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("{}-{nanos}", std::process::id())
    })
}

/// Record a new request (queued); false when `id` already exists.
pub fn add_request(conn: &Connection, id: &str, prompt: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO home_requests
           (id, prompt, status, status_text, run_id, created_at, updated_at)
         VALUES (?1, ?2, 'queued', 'Waiting to start', ?3, ?4, ?4)",
        params![id, prompt, run_id(), now],
    )? > 0)
}

/// Set a request's status and its line of progress.
pub fn set_status(
    conn: &Connection,
    id: &str,
    status: &str,
    status_text: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE home_requests SET status = ?2, status_text = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, status, status_text, now],
    )?;
    Ok(())
}

/// The build finished: `app_id` is ready.
pub fn set_ready(
    conn: &Connection,
    id: &str,
    app_id: &str,
    app_title: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE home_requests SET status = 'ready', status_text = ?4, app_id = ?2,
           app_title = ?3, error = '', updated_at = ?5 WHERE id = ?1",
        params![id, app_id, app_title, format!("{app_title} is ready"), now],
    )?;
    Ok(())
}

/// The build failed with `error` (a sentence for people).
pub fn set_failed(conn: &Connection, id: &str, error: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE home_requests SET status = 'failed', status_text = 'Couldn''t build it',
           error = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, error, now],
    )?;
    Ok(())
}

/// Requests a restart interrupted (unfinished, from another run): failed.
/// Returns how many.
pub fn fail_interrupted(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE home_requests SET status = 'failed', status_text = 'Couldn''t build it',
           error = 'Interrupted — try again.', updated_at = ?1
         WHERE status IN ('queued', 'building') AND run_id != ?2",
        params![now, run_id()],
    )
}

/// A request's title: its app's once built, else the prompt's first line,
/// shortened.
pub fn request_title(r: &Request) -> String {
    if !r.app_title.is_empty() {
        return r.app_title.clone();
    }
    let line = r.prompt.lines().next().unwrap_or("").trim();
    if line.chars().count() > 60 {
        let cut: String = line.chars().take(57).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_owned()
    }
}

pub fn request_entity(r: &Request) -> EntityRecord {
    entity(json!({
        "id": r.id,
        "prompt": r.prompt,
        "title": request_title(r),
        "status": r.status,
        "status_text": r.status_text,
        "app_id": r.app_id,
        "app_title": r.app_title,
        "error": r.error,
    }))
}

// ---------- bundles ----------

/// The home feed: `apps` (the app list's, already filtered and without the
/// home app), the pins (construct pins of apps in `existing` only), and the
/// recent requests.
pub fn home_bundle(
    conn: &Connection,
    apps: &[ListedApp],
    existing: &[String],
    now: i64,
) -> rusqlite::Result<DataBundle> {
    let mut bundle = DataBundle::default();
    let mut sorted: Vec<&ListedApp> = apps.iter().collect();
    sorted.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut order = Vec::new();
    for a in sorted {
        let uri = app_uri(&a.id);
        bundle.entities.insert(
            uri.clone(),
            entity(json!({ "id": a.id, "title": a.title, "letter": letter(&a.title), "kind": "construct" })),
        );
        order.push(uri);
    }
    bundle.queries.insert(APPS_QUERY.into(), order);

    let mut order = Vec::new();
    for p in pins(conn)? {
        if p.kind == "construct" && !existing.contains(&p.target) {
            continue;
        }
        let uri = pin_uri(&p.id);
        bundle.entities.insert(
            uri.clone(),
            entity(json!({ "id": p.id, "kind": p.kind, "target": p.target, "title": p.title, "unpinned": false })),
        );
        order.push(uri);
    }
    bundle.queries.insert(PINS_QUERY.into(), order);

    let mut order = Vec::new();
    for r in recent_requests(conn, now)? {
        let uri = request_uri(&r.id);
        if r.status != "ready" {
            order.push(uri.clone());
        }
        bundle.entities.insert(uri, request_entity(&r));
    }
    bundle.queries.insert(REQUESTS_QUERY.into(), order);
    Ok(bundle)
}

/// One request's screen data: the request under `home:request:<id>` (an
/// empty list while it isn't known yet).
pub fn request_bundle(conn: &Connection, id: &str) -> rusqlite::Result<DataBundle> {
    let mut bundle = DataBundle::default();
    let mut order = Vec::new();
    if let Some(r) = request(conn, id)? {
        let uri = request_uri(&r.id);
        bundle.entities.insert(uri.clone(), request_entity(&r));
        order.push(uri);
    }
    bundle.queries.insert(request_query(id), order);
    Ok(bundle)
}
