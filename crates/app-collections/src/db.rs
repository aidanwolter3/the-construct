//! The Collections tables (`collections_*`) and what reads and writes them.
//!
//! - `collections_defs`: every version of every definition (the latest
//!   is the live one); a collection exists once it has a version.
//! - `collections_rows`: rows, values as JSON keyed by field id, soft
//!   deleted (`deleted_at`), and the client's create id for idempotent
//!   retries from the outbox.
//! - `collections_aside`: values a type change couldn't convert, kept so
//!   nothing is lost (and put back when a later change can read them).
//! - `collections_edits` / `collections_edit_rows`: Claude's row edits,
//!   each row's state before and after, so an edit undoes as one step.
//! - `collections_computed`: answers for `model`-computed fields (decision
//!   `collection-computed-fields`) as JSON, by collection, field id and the
//!   normalized source values, so each input is asked about once; a
//!   person's edit of the field replaces the answer.
//! - `collections_fills`: each filler's fill state (decision
//!   `collection-fillers`): validators and body hash of the last fetch,
//!   health, the next scheduled fill and an agent's fills today. Rows a
//!   filler wrote carry its id (`collections_rows.filler`).
//! - `collections_alerts_sent`: which row an alert already notified about.
//! - `collections_secrets`: values fetch recipes send in headers
//!   (`{secret:<name>}`), sealed with the server key; never shown back,
//!   and sent only to the `host` each was stored for (none: rows from
//!   before hosts, sent nowhere until set again).
//! - `collections_reminders_sent`: which row a reminder (decision
//!   `collection-reminders`) already notified about, for which date, so
//!   each fires once even after the core's notification retention.
//! - `collections_deleted`: deleted collections (decision
//!   `collections`, issue #107). Their definitions, rows and fill state
//!   stay, hidden from everything that reads live definitions, until
//!   restored or purged after [`crate::KEEP_SECS`].

use std::cmp::Ordering;

use pod_core::store::Step;
use protocol::{DataBundle, EntityRecord};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};

use crate::model::{self, Definition, Field, Kind};

/// The adapter's migrations, by name (decision `named-migrations`): add
/// steps at the end; never rename, edit or remove one that shipped.
pub static MIGRATIONS: &[Step] = &[
    // 1: the schema.
    Step::sql("schema", SCHEMA),
    // 2: computed field answers (decision `collection-computed-fields`).
    Step::sql("computed_fields", COMPUTED),
    // 3: reminders already sent (decision `collection-reminders`).
    Step::sql("reminders", REMINDERS),
    // 4: fill state and secrets (decision `collection-fillers`).
    Step::sql("fills", FILLS),
    // 5: the host each secret may be sent to (security audit: a secret went
    // to any URL a recipe named).
    Step::sql("secret_hosts", SECRET_HOSTS),
    // 6: items a fill left out as past (`upcoming`), so a page listing only
    // past events doesn't read as a broken recipe (issue #131).
    Step::sql("fill_past", FILL_PAST),
    // 7: deleted collections, restorable for 30 days (issue #107).
    Step::sql("deleted_collections", DELETED),
];

const DELETED: &str = "
CREATE TABLE collections_deleted (
    name       TEXT PRIMARY KEY NOT NULL,
    deleted_at INTEGER NOT NULL
);
";

/// Leaves out deleted collections (`collections_deleted`).
const LIVE: &str = "NOT IN (SELECT name FROM collections_deleted)";

const FILL_PAST: &str = "ALTER TABLE collections_fills ADD COLUMN last_past INTEGER;";

const SECRET_HOSTS: &str = "ALTER TABLE collections_secrets ADD COLUMN host TEXT;";

const FILLS: &str = "
ALTER TABLE collections_rows ADD COLUMN filler TEXT;
CREATE TABLE collections_fills (
    collection   TEXT NOT NULL,
    filler       TEXT NOT NULL,
    fingerprint  TEXT NOT NULL DEFAULT '',
    next_at      INTEGER,
    etag         TEXT,
    last_modified TEXT,
    content_hash TEXT,
    last_fill_at INTEGER,
    last_ok_at   INTEGER,
    last_error   TEXT,
    failures     INTEGER NOT NULL DEFAULT 0,
    empty_runs   INTEGER NOT NULL DEFAULT 0,
    last_rows    INTEGER,
    last_skipped INTEGER,
    day          TEXT,
    day_fills    INTEGER NOT NULL DEFAULT 0,
    quiet_next   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (collection, filler)
);
CREATE TABLE collections_alerts_sent (
    collection TEXT NOT NULL,
    alert      TEXT NOT NULL,
    row_id     TEXT NOT NULL,
    sent_at    INTEGER NOT NULL,
    PRIMARY KEY (collection, alert, row_id)
);
CREATE TABLE collections_secrets (
    name       TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
";

const REMINDERS: &str = "
CREATE TABLE collections_reminders_sent (
    collection TEXT NOT NULL,
    reminder   TEXT NOT NULL,
    row_id     TEXT NOT NULL,
    date       TEXT NOT NULL,
    sent_at    INTEGER NOT NULL,
    PRIMARY KEY (collection, reminder, row_id, date)
);
";

const COMPUTED: &str = "
CREATE TABLE collections_computed (
    collection TEXT NOT NULL,
    field_id   TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    source     TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (collection, field_id, key)
);
";

const SCHEMA: &str = "
CREATE TABLE collections_defs (
    name       TEXT NOT NULL,
    version    INTEGER NOT NULL,
    def_json   TEXT NOT NULL,
    note       TEXT NOT NULL,
    author     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (name, version)
);
CREATE TABLE collections_rows (
    collection  TEXT NOT NULL,
    id          TEXT NOT NULL,
    client_id   TEXT,
    values_json TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    deleted_at  INTEGER,
    PRIMARY KEY (collection, id)
);
CREATE UNIQUE INDEX collections_rows_client
    ON collections_rows (collection, client_id) WHERE client_id IS NOT NULL;
CREATE TABLE collections_aside (
    collection  TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    field_id    TEXT NOT NULL,
    value_json  TEXT NOT NULL,
    def_version INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (collection, row_id, field_id)
);
CREATE TABLE collections_edits (
    id         TEXT PRIMARY KEY NOT NULL,
    collection TEXT NOT NULL,
    note       TEXT NOT NULL,
    count      INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    undone_at  INTEGER
);
CREATE TABLE collections_edit_rows (
    edit_id     TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    before_json TEXT,
    after_json  TEXT NOT NULL,
    PRIMARY KEY (edit_id, row_id)
);
";

/// Definition versions kept per collection (the oldest are pruned).
pub const MAX_DEF_VERSIONS: i64 = 500;

/// A random id: `prefix` and 10 base32 characters.
pub fn new_id(prefix: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("OS random number generator");
    let tail: String = buf
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % 32] as char)
        .collect();
    format!("{prefix}{tail}")
}

// ---------- definitions ----------

fn parse_def(json: &str) -> rusqlite::Result<Definition> {
    serde_json::from_str(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// The live definition of `name` and its version (`None` when it doesn't
/// exist or is deleted).
pub fn latest_def(conn: &Connection, name: &str) -> rusqlite::Result<Option<(i64, Definition)>> {
    if deleted_at(conn, name)?.is_some() {
        return Ok(None);
    }
    newest_def(conn, name)
}

/// The newest definition of `name`, deleted or not.
fn newest_def(conn: &Connection, name: &str) -> rusqlite::Result<Option<(i64, Definition)>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT version, def_json FROM collections_defs WHERE name = ?1
             ORDER BY version DESC LIMIT 1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(v, j)| Ok((v, parse_def(&j)?))).transpose()
}

/// Every collection's live definition, keyed by name.
pub fn defs_by_name(conn: &Connection) -> rusqlite::Result<model::Defs> {
    Ok(all_defs(conn)?
        .into_iter()
        .map(|(_, d)| (d.name.clone(), d))
        .collect())
}

/// Every collection's live definition, by name.
pub fn all_defs(conn: &Connection) -> rusqlite::Result<Vec<(i64, Definition)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.version, d.def_json FROM collections_defs d
         WHERE d.version = (SELECT MAX(version) FROM collections_defs WHERE name = d.name)
           AND d.name NOT IN (SELECT name FROM collections_deleted)
         ORDER BY d.name",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(v, j)| Ok((v, parse_def(&j)?)))
        .collect()
}

// ---------- deleted collections ----------

/// When `name` was deleted, if it is.
pub fn deleted_at(conn: &Connection, name: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT deleted_at FROM collections_deleted WHERE name = ?1",
        [name],
        |r| r.get(0),
    )
    .optional()
}

/// A deleted collection: when it was deleted and its last definition.
pub fn deleted_def(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<Option<(i64, i64, Definition)>> {
    let Some(at) = deleted_at(conn, name)? else {
        return Ok(None);
    };
    Ok(newest_def(conn, name)?.map(|(v, d)| (at, v, d)))
}

/// Every deleted collection, newest first: `(deleted_at, version, def)`.
pub fn deleted_defs(conn: &Connection) -> rusqlite::Result<Vec<(i64, i64, Definition)>> {
    let names: Vec<String> = conn
        .prepare_cached("SELECT name FROM collections_deleted ORDER BY deleted_at DESC, name")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for n in names {
        out.extend(deleted_def(conn, &n)?);
    }
    Ok(out)
}

/// Delete collection `name` (restorable with [`undelete`]).
pub fn mark_deleted(tx: &Connection, name: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO collections_deleted (name, deleted_at) VALUES (?1, ?2)",
        params![name, now],
    )?;
    Ok(())
}

/// Bring deleted collection `name` back as it was.
pub fn undelete(tx: &Connection, name: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM collections_deleted WHERE name = ?1", [name])?;
    Ok(())
}

/// Remove everything of collection `name`; its name is free again.
fn drop_collection(tx: &Connection, name: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM collections_edit_rows WHERE edit_id IN
         (SELECT id FROM collections_edits WHERE collection = ?1)",
        [name],
    )?;
    for table in [
        "collections_rows",
        "collections_aside",
        "collections_edits",
        "collections_computed",
        "collections_fills",
        "collections_alerts_sent",
        "collections_reminders_sent",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE collection = ?1"),
            [name],
        )?;
    }
    tx.execute("DELETE FROM collections_defs WHERE name = ?1", [name])?;
    tx.execute("DELETE FROM collections_deleted WHERE name = ?1", [name])?;
    Ok(())
}

/// Version `version` of `name`'s definition.
pub fn def_version(
    conn: &Connection,
    name: &str,
    version: i64,
) -> rusqlite::Result<Option<Definition>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT def_json FROM collections_defs WHERE name = ?1 AND version = ?2",
            params![name, version],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|j| parse_def(&j)).transpose()
}

/// `{version, note, author, created_at}` of the newest `limit` versions.
pub fn def_history(conn: &Connection, name: &str, limit: i64) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT version, note, author, created_at FROM collections_defs WHERE name = ?1
         ORDER BY version DESC LIMIT ?2",
    )?;
    stmt.query_map(params![name, limit], |r| {
        Ok(json!({
            "version": r.get::<_, i64>(0)?,
            "note": r.get::<_, String>(1)?,
            "author": r.get::<_, String>(2)?,
            "created_at": pod_core::layouts::rfc3339(r.get(3)?),
        }))
    })?
    .collect()
}

/// Append a definition version; the new version number.
pub fn append_def(
    tx: &Transaction<'_>,
    def: &Definition,
    note: &str,
    author: &str,
    now: i64,
) -> rusqlite::Result<i64> {
    let latest: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM collections_defs WHERE name = ?1",
        [&def.name],
        |r| r.get(0),
    )?;
    let version = latest + 1;
    tx.execute(
        "INSERT INTO collections_defs (name, version, def_json, note, author, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            def.name,
            version,
            serde_json::to_string(def).expect("serializes"),
            note,
            author,
            now
        ],
    )?;
    tx.execute(
        "DELETE FROM collections_defs WHERE name = ?1 AND version <= ?2",
        params![def.name, version - MAX_DEF_VERSIONS],
    )?;
    Ok(version)
}

// ---------- rows ----------

/// A stored row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub client_id: Option<String>,
    pub values: Map<String, Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    /// The filler that wrote it (`None`: a person or Claude).
    pub filler: Option<String>,
}

impl Row {
    /// `{values, deleted_at}`: what an edit records before and after.
    pub fn state(&self) -> Value {
        json!({ "values": self.values, "deleted_at": self.deleted_at })
    }
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let values_json: String = r.get(2)?;
    Ok(Row {
        id: r.get(0)?,
        client_id: r.get(1)?,
        values: serde_json::from_str(&values_json).unwrap_or_default(),
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        deleted_at: r.get(5)?,
        filler: r.get(6)?,
    })
}

const ROW_COLS: &str = "id, client_id, values_json, created_at, updated_at, deleted_at, filler";

/// Row `id` of `collection` (deleted or not).
pub fn row(conn: &Connection, collection: &str, id: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!("SELECT {ROW_COLS} FROM collections_rows WHERE collection = ?1 AND id = ?2"),
        params![collection, id],
        row_from,
    )
    .optional()
}

/// The row a create with client id `client_id` made.
pub fn row_by_client(
    conn: &Connection,
    collection: &str,
    client_id: &str,
) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!(
            "SELECT {ROW_COLS} FROM collections_rows WHERE collection = ?1 AND client_id = ?2"
        ),
        params![collection, client_id],
        row_from,
    )
    .optional()
}

/// Every row of `collection` (`deleted`: also the deleted ones), unordered.
pub fn all_rows(conn: &Connection, collection: &str, deleted: bool) -> rusqlite::Result<Vec<Row>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ROW_COLS} FROM collections_rows WHERE collection = ?1
         AND (?2 OR deleted_at IS NULL) ORDER BY created_at, rowid"
    ))?;
    stmt.query_map(params![collection, deleted], row_from)?
        .collect()
}

/// The live rows of `def`'s collection in its order: by each order field
/// in turn (empty last), then oldest first.
pub fn rows(conn: &Connection, def: &Definition) -> rusqlite::Result<Vec<Row>> {
    let mut rows = all_rows(conn, &def.name, false)?;
    let keys: Vec<&Field> = def.order_fields().collect();
    if !keys.is_empty() {
        // Stable: equal rows stay oldest first.
        rows.sort_by(|a, b| {
            keys.iter()
                .map(|f| compare_rows(f, a, b, def.descending))
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        });
    }
    Ok(rows)
}

fn compare_rows(field: &Field, a: &Row, b: &Row, descending: bool) -> Ordering {
    let x = a
        .values
        .get(&field.id)
        .cloned()
        .or_else(|| field.default.clone());
    let y = b
        .values
        .get(&field.id)
        .cloned()
        .or_else(|| field.default.clone());
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let o = compare(field, &x, &y);
            if descending { o.reverse() } else { o }
        }
    }
}

/// Two values of `field`: a choice by the order of its options (a value
/// that is no longer an option after them).
fn compare(field: &Field, x: &Value, y: &Value) -> Ordering {
    match field.kind {
        Kind::Number => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(Ordering::Equal),
        Kind::Bool => x.as_bool().cmp(&y.as_bool()),
        Kind::Choice => {
            let pos = |v: &Value| {
                field
                    .options
                    .iter()
                    .position(|o| Some(o.as_str()) == v.as_str())
                    .unwrap_or(field.options.len())
            };
            pos(x).cmp(&pos(y))
        }
        Kind::Text => x
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .cmp(&y.as_str().unwrap_or("").to_lowercase()),
        Kind::Date => x.as_str().cmp(&y.as_str()),
        Kind::Datetime => match (model::instant(x), model::instant(y)) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => x.as_str().cmp(&y.as_str()),
        },
    }
}

/// The entity URI of row `id` of `collection`.
pub fn row_uri(collection: &str, id: &str) -> String {
    format!("collections://{collection}/{id}")
}

/// The query key of a collection's list.
pub fn query_key(collection: &str) -> String {
    format!("collection:{collection}")
}

/// A row as an entity: its fields ([`model::row_entity`]) and, with
/// fillers, `filler_id` and `filler_title`; with alerts, `alert_matched` and
/// `alert_names` (decision `collection-fillers`).
pub fn row_record(def: &Definition, r: &Row, now: i64) -> EntityRecord {
    let mut e = model::row_entity(
        def,
        &r.id,
        &r.values,
        &pod_core::layouts::rfc3339(r.created_at),
        &pod_core::layouts::rfc3339(r.updated_at),
    );
    let derived = def.derived_names();
    let mut put = |k: &str, v: Value| {
        if derived.iter().any(|d| d == k) {
            e.insert(k.to_owned(), v);
        }
    };
    if !def.fillers.is_empty() {
        let f = r
            .filler
            .as_deref()
            .and_then(|id| crate::filler::find(def, id));
        put("filler_id", json!(r.filler.clone().unwrap_or_default()));
        put("filler_title", json!(f.map_or("", |f| f.title.as_str())));
    }
    if !def.alerts.is_empty() {
        let names = crate::alerts::matching(def, r, now);
        put("alert_matched", json!(!names.is_empty()));
        put("alert_names", json!(names.join(", ")));
    }
    EntityRecord(e)
}

/// The query key of a collection's fillers.
pub fn fillers_key(collection: &str) -> String {
    format!("collection:{collection}:fillers")
}

/// The query key of a collection's rows some alert matches.
pub fn alerted_key(collection: &str) -> String {
    format!("collection:{collection}:alerted")
}

/// The entity URI of filler `id` of `collection`.
pub fn filler_uri(collection: &str, id: &str) -> String {
    format!("collections://{collection}/fillers/{id}")
}

/// The entity URI of alert `id` of `collection`.
pub fn alert_uri(collection: &str, id: &str) -> String {
    format!("collections://{collection}/alerts/{id}")
}

/// A filler as an entity, for an app's list of sites: its title, kind,
/// URL, health (`state`, `detail`, `failing`) and its alerts' names.
pub fn filler_record(
    def: &Definition,
    f: &crate::filler::Filler,
    st: &crate::fill::State,
    now: i64,
    off: bool,
) -> EntityRecord {
    let (state, detail) = crate::fill::health_state(f, st, now, off, true);
    let names: Vec<String> = crate::alerts::of_filler(def, &f.id)
        .map(|a| a.name.clone())
        .collect();
    let alerts_text = if names.is_empty() {
        "No alerts yet".to_owned()
    } else {
        format!("Watching for {}", names.join(", "))
    };
    let mut e = Map::new();
    for (k, v) in [
        ("id", json!(f.id)),
        ("uri", json!(filler_uri(&def.name, &f.id))),
        ("title", json!(f.title)),
        ("kind", json!(f.kind())),
        ("url", json!(f.url().unwrap_or(""))),
        ("state", json!(state)),
        ("detail", json!(detail)),
        ("failing", json!(matches!(state, "failing" | "empty"))),
        ("alert_count", json!(names.len())),
        ("alerts_text", json!(alerts_text)),
    ] {
        e.insert(k.to_owned(), v);
    }
    EntityRecord(e)
}

/// An alert as an entity: its name, summary and filler.
pub fn alert_record(def: &Definition, a: &crate::alerts::Alert) -> EntityRecord {
    let mut e = Map::new();
    for (k, v) in [
        ("id", json!(a.id)),
        ("uri", json!(alert_uri(&def.name, &a.id))),
        ("name", json!(a.name)),
        ("summary", json!(a.summary())),
        ("filler_id", json!(a.filler.clone().unwrap_or_default())),
    ] {
        e.insert(k.to_owned(), v);
    }
    EntityRecord(e)
}

/// Whether fillers are off on this pod (`collections.fill = 0`).
fn fills_off(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(pod_core::store::pod_setting_on(conn, crate::fill::FILL_SETTING)?.is_some_and(|v| v == "0"))
}

/// A collection's list as a `DataBundle`: each live row at
/// `collections://<name>/<id>`, in order under `collection:<name>`; with
/// fillers also `collection:<name>:fillers`, with alerts
/// `collection:<name>:alerted` (the rows an alert matches).
pub fn bundle(conn: &Connection, def: &Definition) -> rusqlite::Result<DataBundle> {
    let now = pod_core::util::now_secs();
    let mut data = DataBundle::default();
    let mut list = Vec::new();
    let mut alerted = Vec::new();
    for r in rows(conn, def)? {
        let uri = row_uri(&def.name, &r.id);
        let rec = row_record(def, &r, now);
        if rec.0.get("alert_matched") == Some(&json!(true)) {
            alerted.push(uri.clone());
        }
        data.entities.insert(uri.clone(), rec);
        list.push(uri);
    }
    data.queries.insert(query_key(&def.name), list);
    if !def.alerts.is_empty() {
        data.queries.insert(alerted_key(&def.name), alerted);
    }
    if !def.fillers.is_empty() {
        let off = fills_off(conn)?;
        let states = crate::fill::states(conn, def)?;
        let mut keys = Vec::new();
        for f in &def.fillers {
            let uri = filler_uri(&def.name, &f.id);
            let st = states.get(&f.id).cloned().unwrap_or_default();
            data.entities
                .insert(uri.clone(), filler_record(def, f, &st, now, off));
            keys.push(uri);
        }
        data.queries.insert(fillers_key(&def.name), keys);
    }
    Ok(data)
}

/// One filler's screen: the filler (a one-item list,
/// `collection:<c>:filler:<f>`), its rows (`…:rows`, in order) and its
/// alerts (`…:alerts`). `None`: no such filler.
pub fn filler_bundle(
    conn: &Connection,
    def: &Definition,
    filler: &str,
) -> rusqlite::Result<Option<DataBundle>> {
    let Some(f) = crate::filler::find(def, filler) else {
        return Ok(None);
    };
    let now = pod_core::util::now_secs();
    let off = fills_off(conn)?;
    let st = crate::fill::state(conn, &def.name, &f.id)?;
    let base = format!("collection:{}:filler:{}", def.name, f.id);
    let mut data = DataBundle::default();
    let uri = filler_uri(&def.name, &f.id);
    data.entities
        .insert(uri.clone(), filler_record(def, f, &st, now, off));
    data.queries.insert(base.clone(), vec![uri]);
    let mut list = Vec::new();
    for r in rows(conn, def)?
        .into_iter()
        .filter(|r| r.filler.as_deref() == Some(filler))
    {
        let uri = row_uri(&def.name, &r.id);
        data.entities.insert(uri.clone(), row_record(def, &r, now));
        list.push(uri);
    }
    data.queries.insert(format!("{base}:rows"), list);
    let mut alerts = Vec::new();
    for a in crate::alerts::of_filler(def, filler) {
        let uri = alert_uri(&def.name, &a.id);
        data.entities.insert(uri.clone(), alert_record(def, a));
        alerts.push(uri);
    }
    data.queries.insert(format!("{base}:alerts"), alerts);
    Ok(Some(data))
}

/// One row's screen: the row as a one-item list `collection:<c>:row:<id>`.
pub fn row_bundle(
    conn: &Connection,
    def: &Definition,
    id: &str,
) -> rusqlite::Result<Option<DataBundle>> {
    let Some(r) = row(conn, &def.name, id)?.filter(|r| r.deleted_at.is_none()) else {
        return Ok(None);
    };
    let mut data = DataBundle::default();
    let uri = row_uri(&def.name, &r.id);
    data.entities
        .insert(uri.clone(), row_record(def, &r, pod_core::util::now_secs()));
    data.queries
        .insert(format!("collection:{}:row:{}", def.name, r.id), vec![uri]);
    Ok(Some(data))
}

/// Live rows and their values' bytes across the pod's collections.
pub fn usage(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(values_json)), 0) FROM collections_rows
             WHERE deleted_at IS NULL AND collection {LIVE}"
        ),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Insert a new row.
pub fn insert_row(tx: &Connection, collection: &str, row: &Row) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO collections_rows (collection, id, client_id, values_json, created_at,
         updated_at, deleted_at, filler) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            collection,
            row.id,
            row.client_id,
            Value::Object(row.values.clone()).to_string(),
            row.created_at,
            row.updated_at,
            row.deleted_at,
            row.filler
        ],
    )?;
    Ok(())
}

/// Store a row's values and deleted state.
pub fn save_row(tx: &Connection, collection: &str, row: &Row) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE collections_rows SET values_json = ?1, updated_at = ?2, deleted_at = ?3
         WHERE collection = ?4 AND id = ?5",
        params![
            Value::Object(row.values.clone()).to_string(),
            row.updated_at,
            row.deleted_at,
            collection,
            row.id
        ],
    )?;
    Ok(())
}

// ---------- computed answers ----------

/// A model gave the answer.
pub const SOURCE_MODEL: &str = "model";
/// A person (or Claude's row edit) set the field by hand.
pub const SOURCE_EDIT: &str = "edit";

/// The cached answer for field `field_id` of `collection` and inputs key
/// `key` (normalized source values, see [`model::cache_key`]).
pub fn computed(
    conn: &Connection,
    collection: &str,
    field_id: &str,
    key: &str,
) -> rusqlite::Result<Option<Value>> {
    let text: Option<String> = conn
        .query_row(
            "SELECT value FROM collections_computed WHERE collection = ?1 AND field_id = ?2 AND key = ?3",
            params![collection, field_id, key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

/// Remember `value` as the answer for `key` (`source`: [`SOURCE_MODEL`] or
/// [`SOURCE_EDIT`]). A model's answer never replaces a person's.
pub fn remember_computed(
    conn: &Connection,
    collection: &str,
    field_id: &str,
    key: &str,
    value: &Value,
    source: &str,
    now: i64,
) -> rusqlite::Result<()> {
    if key.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO collections_computed (collection, field_id, key, value, source, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (collection, field_id, key) DO UPDATE SET
             value = excluded.value, source = excluded.source, updated_at = excluded.updated_at
         WHERE excluded.source = 'edit' OR collections_computed.source = 'model'",
        params![collection, field_id, key, value.to_string(), source, now],
    )?;
    Ok(())
}

/// Every cached answer of field `field_id` of `collection`: `(key, value,
/// source)`, oldest first.
pub fn computed_answers(
    conn: &Connection,
    collection: &str,
    field_id: &str,
) -> rusqlite::Result<Vec<(String, Value, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT key, value, source FROM collections_computed WHERE collection = ?1 AND field_id = ?2
         ORDER BY updated_at, key",
    )?;
    stmt.query_map(params![collection, field_id], |r| {
        let v: String = r.get(1)?;
        Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str(&v).unwrap_or(Value::Null),
            r.get::<_, String>(2)?,
        ))
    })?
    .collect()
}

/// Cached answers per field id of `collection`.
pub fn computed_counts(
    conn: &Connection,
    collection: &str,
) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT field_id, COUNT(*) FROM collections_computed WHERE collection = ?1 GROUP BY field_id",
    )?;
    stmt.query_map([collection], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

// ---------- type changes ----------

/// What converting a field's rows did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversion {
    pub field: String,
    pub converted: usize,
    /// Values that don't fit the new type, kept aside (not lost).
    pub set_aside: usize,
    /// Values kept aside earlier that fit again and were put back.
    pub restored: usize,
    /// A few values set aside, `{id, value}`.
    pub samples: Vec<Value>,
}

impl Conversion {
    pub fn to_json(&self, collection_title: &str) -> Value {
        let mut v = json!({
            "field": self.field, "converted": self.converted,
            "set_aside": self.set_aside, "restored": self.restored,
        });
        if self.set_aside > 0 {
            v["detail"] = json!(format!(
                "{} {} had {} values it couldn't read as the new type; they are kept aside (a later change back puts them back)",
                self.set_aside,
                collection_title.to_lowercase(),
                self.field
            ));
            v["samples"] = json!(self.samples);
        }
        v
    }
}

/// Convert field `new`'s values (every row, deleted ones too) to its type
/// and options; what doesn't fit is moved to `collections_aside`. Values
/// kept aside earlier that fit now come back to rows that are empty there.
pub fn convert_field(
    tx: &Transaction<'_>,
    collection: &str,
    new: &Field,
    def_version: i64,
    now: i64,
    max_chars: usize,
) -> rusqlite::Result<Conversion> {
    let mut report = Conversion {
        field: new.name.clone(),
        ..Conversion::default()
    };
    for mut r in all_rows(tx, collection, true)? {
        let Some(v) = r.values.get(&new.id).cloned() else {
            continue;
        };
        match model::convert(new, &v, max_chars) {
            Ok(Some(c)) => {
                if c != v {
                    report.converted += 1;
                }
                r.values.insert(new.id.clone(), c);
            }
            Ok(None) => {
                r.values.remove(&new.id);
            }
            Err(_) => {
                tx.execute(
                    "INSERT OR REPLACE INTO collections_aside (collection, row_id, field_id,
                     value_json, def_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![collection, r.id, new.id, v.to_string(), def_version, now],
                )?;
                r.values.remove(&new.id);
                report.set_aside += 1;
                if report.samples.len() < 5 {
                    report.samples.push(json!({ "id": r.id, "value": v }));
                }
            }
        }
        save_row_values(tx, collection, &r)?;
    }
    // Put back what fits again.
    let aside: Vec<(String, String)> = {
        let mut stmt = tx.prepare_cached(
            "SELECT row_id, value_json FROM collections_aside WHERE collection = ?1 AND field_id = ?2",
        )?;
        stmt.query_map(params![collection, new.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (row_id, value_json) in aside {
        let v: Value = serde_json::from_str(&value_json).unwrap_or(Value::Null);
        let Ok(Some(c)) = model::convert(new, &v, max_chars) else {
            continue;
        };
        let Some(mut r) = row(tx, collection, &row_id)? else {
            continue;
        };
        if r.values.contains_key(&new.id) {
            continue;
        }
        r.values.insert(new.id.clone(), c);
        save_row_values(tx, collection, &r)?;
        tx.execute(
            "DELETE FROM collections_aside WHERE collection = ?1 AND row_id = ?2 AND field_id = ?3",
            params![collection, row_id, new.id],
        )?;
        report.restored += 1;
    }
    Ok(report)
}

/// Store a row's values without touching `updated_at` (a definition change
/// is not an edit of the row).
pub fn save_row_values(tx: &Connection, collection: &str, row: &Row) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE collections_rows SET values_json = ?1 WHERE collection = ?2 AND id = ?3",
        params![
            Value::Object(row.values.clone()).to_string(),
            collection,
            row.id
        ],
    )?;
    Ok(())
}

/// How many values are kept aside per field id of `collection`.
pub fn aside_counts(conn: &Connection, collection: &str) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT field_id, COUNT(*) FROM collections_aside WHERE collection = ?1 GROUP BY field_id",
    )?;
    stmt.query_map([collection], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

// ---------- edits ----------

/// Record an edit of rows: `(row id, state before or None for a new row,
/// state after)`. Returns its id.
pub fn record_edit(
    tx: &Transaction<'_>,
    collection: &str,
    note: &str,
    changes: &[(String, Option<Value>, Value)],
    now: i64,
) -> rusqlite::Result<String> {
    let id = new_id("e");
    tx.execute(
        "INSERT INTO collections_edits (id, collection, note, count, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, collection, note, changes.len() as i64, now],
    )?;
    for (row_id, before, after) in changes {
        tx.execute(
            "INSERT OR REPLACE INTO collections_edit_rows (edit_id, row_id, before_json, after_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                row_id,
                before.as_ref().map(Value::to_string),
                after.to_string()
            ],
        )?;
    }
    Ok(id)
}

/// An edit's `(collection, undone_at, rows: (row id, before, after))`.
#[allow(clippy::type_complexity)]
pub fn edit(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<(String, Option<i64>, Vec<(String, Option<Value>, Value)>)>> {
    let head: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT collection, undone_at FROM collections_edits WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((collection, undone)) = head else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT row_id, before_json, after_json FROM collections_edit_rows WHERE edit_id = ?1",
    )?;
    let rows = stmt
        .query_map([id], |r| {
            let before: Option<String> = r.get(1)?;
            let after: String = r.get(2)?;
            Ok((
                r.get::<_, String>(0)?,
                before.and_then(|b| serde_json::from_str(&b).ok()),
                serde_json::from_str(&after).unwrap_or(Value::Null),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some((collection, undone, rows)))
}

/// `{id, note, count, created_at, undone}` of `collection`'s recent edits.
pub fn recent_edits(conn: &Connection, collection: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, note, count, created_at, undone_at FROM collections_edits WHERE collection = ?1
         ORDER BY created_at DESC, rowid DESC LIMIT 10",
    )?;
    stmt.query_map([collection], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?, "note": r.get::<_, String>(1)?,
            "count": r.get::<_, i64>(2)?,
            "created_at": pod_core::layouts::rfc3339(r.get(3)?),
            "undone": r.get::<_, Option<i64>>(4)?.is_some(),
        }))
    })?
    .collect()
}

// ---------- purge ----------

/// What a purge removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Purged {
    pub rows: usize,
    pub fields: usize,
    pub aside: usize,
    pub edits: usize,
    pub collections: usize,
}

/// Bytes counted for each kept item on top of its values (its keys and
/// timestamps), so many empty items add up too.
const ITEM_OVERHEAD: i64 = 64;

/// What deleted collections, deleted rows, values kept aside and the undo
/// records of edits take: `(kind, key, at, bytes)` per item, oldest first
/// (by time, then by when it was written).
/// `kind` is `c` (a deleted collection, `key` its name, with all its data),
/// `r` (a deleted row's rowid), `a` (an aside value's rowid) or `e` (an
/// edit's id).
fn retained(conn: &Connection) -> rusqlite::Result<Vec<(String, String, i64, i64)>> {
    let o = ITEM_OVERHEAD;
    let sql = format!(
        "SELECT kind, key, at, bytes FROM (
           SELECT 'c' AS kind, d.name AS key, d.deleted_at AS at, d.rowid AS seq, {o}
             + (SELECT COALESCE(SUM(LENGTH(values_json) + {o}), 0) FROM collections_rows
                WHERE collection = d.name)
             + (SELECT COALESCE(SUM(LENGTH(value_json) + {o}), 0) FROM collections_aside
                WHERE collection = d.name)
             + (SELECT COALESCE(SUM(COALESCE(LENGTH(er.before_json), 0) + LENGTH(er.after_json) + {o}), 0)
                FROM collections_edit_rows er JOIN collections_edits e ON e.id = er.edit_id
                WHERE e.collection = d.name) AS bytes
           FROM collections_deleted d
           UNION ALL
           SELECT 'r', CAST(rowid AS TEXT), deleted_at, rowid, LENGTH(values_json) + {o}
           FROM collections_rows WHERE deleted_at IS NOT NULL AND collection {LIVE}
           UNION ALL
           SELECT 'a', CAST(rowid AS TEXT), created_at, rowid, LENGTH(value_json) + {o}
           FROM collections_aside WHERE collection {LIVE}
           UNION ALL
           SELECT 'e', e.id, e.created_at, e.rowid, {o}
             + (SELECT COALESCE(SUM(COALESCE(LENGTH(before_json), 0) + LENGTH(after_json) + {o}), 0)
                FROM collections_edit_rows WHERE edit_id = e.id)
           FROM collections_edits e WHERE e.collection {LIVE}
         ) ORDER BY at, seq"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Keep what [`retained`] lists under `max_bytes`, removing the oldest
/// first, ahead of [`purge`]'s window: deleted collections and rows, values
/// kept aside and undo records are for undoing, and must not grow a pod's
/// storage past its caps (a pod that deletes and re-adds rows or
/// collections, or edits them over and over). Returns how many items went.
/// Blocking.
pub fn trim_retained(conn: &Connection, max_bytes: i64) -> rusqlite::Result<usize> {
    let items = retained(conn)?;
    let mut total: i64 = items.iter().map(|i| i.3).sum();
    let mut removed = 0;
    for (kind, key, _, bytes) in items {
        if total <= max_bytes {
            break;
        }
        match kind.as_str() {
            "c" => drop_collection(conn, &key)?,
            "r" => {
                conn.execute("DELETE FROM collections_rows WHERE rowid = ?1", [&key])?;
            }
            "a" => {
                conn.execute("DELETE FROM collections_aside WHERE rowid = ?1", [&key])?;
            }
            _ => {
                conn.execute(
                    "DELETE FROM collections_edit_rows WHERE edit_id = ?1",
                    [&key],
                )?;
                conn.execute("DELETE FROM collections_edits WHERE id = ?1", [&key])?;
            }
        }
        total -= bytes;
        removed += 1;
    }
    if removed > 0 {
        tracing::info!(removed, "kept collection data trimmed to the pod's cap");
    }
    Ok(removed)
}

/// Remove what has been gone longer than `window` seconds before `now`:
/// deleted rows, the data of removed fields (which then can't come back),
/// values kept aside, the undo records of edits and deleted collections.
/// Blocking.
pub fn purge(tx: &Transaction<'_>, now: i64, window: i64) -> rusqlite::Result<Purged> {
    let cutoff = now - window;
    let expired: Vec<String> = tx
        .prepare_cached("SELECT name FROM collections_deleted WHERE deleted_at <= ?1")?
        .query_map([cutoff], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for name in &expired {
        drop_collection(tx, name)?;
    }
    let mut out = Purged {
        collections: expired.len(),
        rows: tx.execute(
            "DELETE FROM collections_rows WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
            [cutoff],
        )?,
        aside: tx.execute(
            "DELETE FROM collections_aside WHERE created_at <= ?1",
            [cutoff],
        )?,
        ..Purged::default()
    };
    out.edits = tx.execute(
        "DELETE FROM collections_edit_rows WHERE edit_id IN
         (SELECT id FROM collections_edits WHERE created_at <= ?1)",
        [cutoff],
    )?;
    tx.execute(
        "DELETE FROM collections_edits WHERE created_at <= ?1",
        [cutoff],
    )?;
    // Alerts sent about rows that are gone.
    tx.execute(
        "DELETE FROM collections_alerts_sent WHERE NOT EXISTS (SELECT 1 FROM collections_rows r
         WHERE r.collection = collections_alerts_sent.collection
           AND r.id = collections_alerts_sent.row_id)",
        [],
    )?;
    // Older than any reminder still fires (`remind::LATE_SECS`).
    tx.execute(
        "DELETE FROM collections_reminders_sent WHERE sent_at <= ?1",
        [cutoff],
    )?;
    for (version, mut def) in all_defs(tx)? {
        let expired: Vec<String> = def
            .fields
            .iter()
            .filter(|f| !f.purged && f.removed_at.is_some_and(|t| t <= cutoff))
            .map(|f| f.id.clone())
            .collect();
        if expired.is_empty() {
            continue;
        }
        for mut r in all_rows(tx, &def.name, true)? {
            if expired.iter().any(|id| r.values.remove(id).is_some()) {
                save_row_values(tx, &def.name, &r)?;
            }
        }
        for f in def.fields.iter_mut().filter(|f| expired.contains(&f.id)) {
            f.purged = true;
        }
        // Rewrite the live version in place: purging isn't a change anyone
        // made, and older versions still restore without the data.
        tx.execute(
            "UPDATE collections_defs SET def_json = ?1 WHERE name = ?2 AND version = ?3",
            params![
                serde_json::to_string(&def).expect("serializes"),
                def.name,
                version
            ],
        )?;
        out.fields += expired.len();
    }
    Ok(out)
}
