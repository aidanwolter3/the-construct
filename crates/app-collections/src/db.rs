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

use std::cmp::Ordering;

use pod_core::store::Migration;
use protocol::{DataBundle, EntityRecord};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};

use crate::model::{self, Definition, Field, Kind};

pub static MIGRATIONS: &[Migration] = &[
    // 1: the schema.
    Migration::Sql(SCHEMA),
    // 2: computed field answers (decision `collection-computed-fields`).
    Migration::Sql(COMPUTED),
];

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

/// The live definition of `name` and its version.
pub fn latest_def(conn: &Connection, name: &str) -> rusqlite::Result<Option<(i64, Definition)>> {
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
         ORDER BY d.name",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(v, j)| Ok((v, parse_def(&j)?)))
        .collect()
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
    })
}

const ROW_COLS: &str = "id, client_id, values_json, created_at, updated_at, deleted_at";

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

/// A collection's list as a `DataBundle`: each live row at
/// `collections://<name>/<id>`, in order under `collection:<name>`.
pub fn bundle(conn: &Connection, def: &Definition) -> rusqlite::Result<DataBundle> {
    let mut data = DataBundle::default();
    let mut list = Vec::new();
    for r in rows(conn, def)? {
        let uri = row_uri(&def.name, &r.id);
        let e = model::row_entity(
            def,
            &r.id,
            &r.values,
            &pod_core::layouts::rfc3339(r.created_at),
            &pod_core::layouts::rfc3339(r.updated_at),
        );
        data.entities.insert(uri.clone(), EntityRecord(e));
        list.push(uri);
    }
    data.queries.insert(query_key(&def.name), list);
    Ok(data)
}

/// Live rows and their values' bytes across the pod's collections.
pub fn usage(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(values_json)), 0) FROM collections_rows
         WHERE deleted_at IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Insert a new row.
pub fn insert_row(tx: &Connection, collection: &str, row: &Row) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO collections_rows (collection, id, client_id, values_json, created_at,
         updated_at, deleted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            collection,
            row.id,
            row.client_id,
            Value::Object(row.values.clone()).to_string(),
            row.created_at,
            row.updated_at,
            row.deleted_at
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
}

/// Remove what has been gone longer than `window` seconds before `now`:
/// deleted rows, the data of removed fields (which then can't come back),
/// values kept aside and the undo records of edits. Blocking.
pub fn purge(tx: &Transaction<'_>, now: i64, window: i64) -> rusqlite::Result<Purged> {
    let cutoff = now - window;
    let mut out = Purged {
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
