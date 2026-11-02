//! SQLite entity cache and action outbox (DESIGN.md §2.3, docs/DECISIONS.md D13).
//!
//! The schema is DESIGN §2.3 plus extra columns, created by `PRAGMA
//! user_version` migrations. The engine keeps its in-memory `State` as the
//! source for snapshots and writes every change through to this store.
//!
//! All row operations live on [`StoreOps`], which is implemented both by
//! [`Store`] (autocommit) and by [`StoreTx`] (inside
//! [`Store::transaction`]), so callers can group writes atomically:
//!
//! ```no_run
//! # use client_core::store::{Store, StoreOps};
//! # let mut store = Store::open_in_memory().unwrap();
//! # let entity = Default::default();
//! store.transaction(|tx| {
//!     tx.upsert_entity("core://counters/home", &entity, 0)?;
//!     tx.delete_outbox(1)?;
//!     Ok(())
//! }).unwrap();
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use protocol::{EntityRecord, UiNode};
use rusqlite::{Connection, OptionalExtension, Row, params};

pub use rusqlite::Error as StoreError;

pub type StoreResult<T> = Result<T, StoreError>;

/// Current schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: u32 = 1;

/// Outbox row status for rows not yet accepted or rejected by the server.
pub const STATUS_PENDING: &str = "PENDING";

/// Schema v1: DESIGN §2.3 tables plus the M5 extension columns.
const MIGRATION_V1: &str = "
CREATE TABLE IF NOT EXISTS entities (
    uri TEXT PRIMARY KEY NOT NULL,
    domain TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_entities_domain ON entities(domain);
CREATE TABLE IF NOT EXISTS action_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_uri TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retry_count INTEGER DEFAULT 0,
    status TEXT DEFAULT 'PENDING',
    field TEXT,
    value_expr TEXT,
    prev_value_json TEXT,
    rollback_value_json TEXT,
    next_attempt_at INTEGER DEFAULT 0,
    last_error TEXT
);
CREATE TABLE IF NOT EXISTS active_workspace (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    entity_uris_json TEXT NOT NULL DEFAULT '[]'
);
";

/// `(domain, entity_type)` from a `<domain>://<type>/<id...>` URI, e.g.
/// `pixelfed://posts/101` -> `("pixelfed", "posts")`. Missing parts are `""`.
pub fn split_uri(uri: &str) -> (&str, &str) {
    match uri.split_once("://") {
        Some((domain, rest)) => (domain, rest.split('/').next().unwrap_or("")),
        None => ("", ""),
    }
}

/// A workspace row from `active_workspace`.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedWorkspace {
    /// The bundle's `workspace` layout node.
    pub layout: UiNode,
    /// The entity URIs the fetched bundle carried.
    pub entity_uris: Vec<String>,
    pub updated_at: i64,
}

/// A new outbox row; `retry_count`, `status`, `next_attempt_at` and
/// `last_error` take their defaults (0, PENDING, 0, NULL).
#[derive(Debug, Clone, PartialEq)]
pub struct NewOutboxRow {
    pub entity_uri: String,
    pub endpoint: String,
    pub payload_json: String,
    pub created_at: i64,
    pub field: String,
    pub value_expr: String,
    pub prev_value_json: String,
    pub rollback_value_json: String,
}

/// A full `action_outbox` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub id: i64,
    pub entity_uri: String,
    pub endpoint: String,
    pub payload_json: String,
    pub created_at: i64,
    pub retry_count: i64,
    pub status: String,
    pub field: Option<String>,
    pub value_expr: Option<String>,
    pub prev_value_json: Option<String>,
    pub rollback_value_json: Option<String>,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
}

const OUTBOX_COLUMNS: &str = "id, entity_uri, endpoint, payload_json, created_at, \
    COALESCE(retry_count, 0), COALESCE(status, 'PENDING'), field, value_expr, prev_value_json, \
    rollback_value_json, COALESCE(next_attempt_at, 0), last_error";

fn outbox_row(r: &Row<'_>) -> rusqlite::Result<OutboxRow> {
    Ok(OutboxRow {
        id: r.get(0)?,
        entity_uri: r.get(1)?,
        endpoint: r.get(2)?,
        payload_json: r.get(3)?,
        created_at: r.get(4)?,
        retry_count: r.get(5)?,
        status: r.get(6)?,
        field: r.get(7)?,
        value_expr: r.get(8)?,
        prev_value_json: r.get(9)?,
        rollback_value_json: r.get(10)?,
        next_attempt_at: r.get(11)?,
        last_error: r.get(12)?,
    })
}

fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::ToSqlConversionFailure(Box::new(e))
}

fn parse_entity(uri: &str, raw: &str) -> Option<EntityRecord> {
    match serde_json::from_str(raw) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("[CanvasEngine] store: ignoring corrupt entity {uri}: {e}");
            None
        }
    }
}

/// Row operations shared by [`Store`] and [`StoreTx`].
pub trait StoreOps {
    fn conn(&self) -> &Connection;

    // ---- entities ----

    /// Insert or replace an entity's payload.
    fn upsert_entity(&self, uri: &str, entity: &EntityRecord, updated_at: i64) -> StoreResult<()> {
        let (domain, entity_type) = split_uri(uri);
        let payload = serde_json::to_string(entity).map_err(json_err)?;
        self.conn()
            .prepare_cached(
                "INSERT INTO entities (uri, domain, entity_type, payload_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(uri) DO UPDATE SET payload_json = excluded.payload_json,
                     domain = excluded.domain, entity_type = excluded.entity_type,
                     updated_at = excluded.updated_at",
            )?
            .execute(params![uri, domain, entity_type, payload, updated_at])?;
        Ok(())
    }

    /// One entity, or `None` if absent (or its stored JSON is corrupt).
    fn get_entity(&self, uri: &str) -> StoreResult<Option<EntityRecord>> {
        let raw: Option<String> = self
            .conn()
            .prepare_cached("SELECT payload_json FROM entities WHERE uri = ?1")?
            .query_row([uri], |r| r.get(0))
            .optional()?;
        Ok(raw.and_then(|raw| parse_entity(uri, &raw)))
    }

    /// The given entities that exist (missing URIs are skipped).
    fn load_entities(&self, uris: &[String]) -> StoreResult<BTreeMap<String, EntityRecord>> {
        let mut out = BTreeMap::new();
        for uri in uris {
            if let Some(e) = self.get_entity(uri)? {
                out.insert(uri.clone(), e);
            }
        }
        Ok(out)
    }

    fn delete_entity(&self, uri: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("DELETE FROM entities WHERE uri = ?1")?
            .execute([uri])?;
        Ok(())
    }

    // ---- active_workspace ----

    /// Insert or replace a workspace's layout and entity URI list.
    fn save_workspace(
        &self,
        id: &str,
        layout: &UiNode,
        entity_uris: &[String],
        updated_at: i64,
    ) -> StoreResult<()> {
        let layout = serde_json::to_string(layout).map_err(json_err)?;
        let uris = serde_json::to_string(entity_uris).map_err(json_err)?;
        self.conn()
            .prepare_cached(
                "INSERT INTO active_workspace (id, workspace_json, updated_at, entity_uris_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET workspace_json = excluded.workspace_json,
                     updated_at = excluded.updated_at, entity_uris_json = excluded.entity_uris_json",
            )?
            .execute(params![id, layout, updated_at, uris])?;
        Ok(())
    }

    /// The cached workspace, or `None` if absent (or its JSON is corrupt).
    fn load_workspace(&self, id: &str) -> StoreResult<Option<CachedWorkspace>> {
        let row: Option<(String, String, i64)> = self
            .conn()
            .prepare_cached(
                "SELECT workspace_json, entity_uris_json, updated_at
                 FROM active_workspace WHERE id = ?1",
            )?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let Some((layout, uris, updated_at)) = row else {
            return Ok(None);
        };
        match (serde_json::from_str(&layout), serde_json::from_str(&uris)) {
            (Ok(layout), Ok(entity_uris)) => Ok(Some(CachedWorkspace {
                layout,
                entity_uris,
                updated_at,
            })),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("[CanvasEngine] store: ignoring corrupt workspace '{id}': {e}");
                Ok(None)
            }
        }
    }

    // ---- action_outbox ----

    /// Insert a PENDING row; returns its id.
    fn insert_outbox(&self, row: &NewOutboxRow) -> StoreResult<i64> {
        self.conn()
            .prepare_cached(
                "INSERT INTO action_outbox (entity_uri, endpoint, payload_json, created_at,
                     field, value_expr, prev_value_json, rollback_value_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![
                row.entity_uri,
                row.endpoint,
                row.payload_json,
                row.created_at,
                row.field,
                row.value_expr,
                row.prev_value_json,
                row.rollback_value_json,
            ])?;
        Ok(self.conn().last_insert_rowid())
    }

    /// All PENDING rows, FIFO (lowest id first).
    fn pending_outbox(&self) -> StoreResult<Vec<OutboxRow>> {
        let mut stmt = self.conn().prepare_cached(&format!(
            "SELECT {OUTBOX_COLUMNS} FROM action_outbox
             WHERE COALESCE(status, 'PENDING') = 'PENDING' ORDER BY id"
        ))?;
        let rows = stmt.query_map([], outbox_row)?;
        rows.collect()
    }

    /// The oldest PENDING row, if any. The sync worker checks its
    /// `next_attempt_at` itself: FIFO never skips past a row that is not
    /// due yet.
    fn first_pending(&self) -> StoreResult<Option<OutboxRow>> {
        self.conn()
            .prepare_cached(&format!(
                "SELECT {OUTBOX_COLUMNS} FROM action_outbox
                 WHERE COALESCE(status, 'PENDING') = 'PENDING' ORDER BY id LIMIT 1"
            ))?
            .query_row([], outbox_row)
            .optional()
    }

    fn get_outbox(&self, id: i64) -> StoreResult<Option<OutboxRow>> {
        self.conn()
            .prepare_cached(&format!(
                "SELECT {OUTBOX_COLUMNS} FROM action_outbox WHERE id = ?1"
            ))?
            .query_row([id], outbox_row)
            .optional()
    }

    /// Number of PENDING rows.
    fn pending_count(&self) -> StoreResult<u32> {
        self.conn()
            .prepare_cached(
                "SELECT COUNT(*) FROM action_outbox WHERE COALESCE(status, 'PENDING') = 'PENDING'",
            )?
            .query_row([], |r| r.get(0))
    }

    /// Delete a row (accepted or rejected by the server). Returns whether a
    /// row was deleted.
    fn delete_outbox(&self, id: i64) -> StoreResult<bool> {
        let n = self
            .conn()
            .prepare_cached("DELETE FROM action_outbox WHERE id = ?1")?
            .execute([id])?;
        Ok(n > 0)
    }

    /// Record a failed attempt: `retry_count += 1`, set `next_attempt_at`
    /// and `last_error`.
    fn mark_retry(&self, id: i64, next_attempt_at: i64, last_error: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached(
                "UPDATE action_outbox SET retry_count = COALESCE(retry_count, 0) + 1,
                     next_attempt_at = ?2, last_error = ?3 WHERE id = ?1",
            )?
            .execute(params![id, next_attempt_at, last_error])?;
        Ok(())
    }

    /// Update a row's `prev_value_json` (rollback / refetch re-apply).
    fn set_outbox_prev_value(&self, id: i64, prev_value_json: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("UPDATE action_outbox SET prev_value_json = ?2 WHERE id = ?1")?
            .execute(params![id, prev_value_json])?;
        Ok(())
    }
}

/// The SQLite connection (`<storage_path>/cache.db`, or in-memory).
pub struct Store {
    conn: Connection,
}

/// A write transaction; see [`Store::transaction`].
pub struct StoreTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl StoreOps for Store {
    fn conn(&self) -> &Connection {
        &self.conn
    }
}

impl StoreOps for StoreTx<'_> {
    fn conn(&self) -> &Connection {
        &self.tx
    }
}

impl Store {
    /// Open (creating if needed) and migrate a file-backed DB in WAL mode.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL + NORMAL: commits append to the WAL without an fsync each
        // time; a crash never corrupts the DB, a power loss may drop the
        // last few commits.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// A private in-memory DB (used when `storage_path` is empty).
    pub fn open_in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> StoreResult<Self> {
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> StoreResult<()> {
        let version = self.schema_version()?;
        if version > SCHEMA_VERSION {
            eprintln!(
                "[CanvasEngine] store: schema version {version} is newer than {SCHEMA_VERSION}; \
                 using it as is"
            );
            return Ok(());
        }
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(MIGRATION_V1)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// `PRAGMA user_version`.
    pub fn schema_version(&self) -> StoreResult<u32> {
        self.conn.pragma_query_value(None, "user_version", |r| r.get(0))
    }

    /// Run `f` in one transaction: committed if it returns `Ok`, rolled back
    /// otherwise (including when the commit itself fails).
    pub fn transaction<T>(
        &mut self,
        f: impl FnOnce(&StoreTx<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let tx = StoreTx {
            tx: self.conn.transaction()?,
        };
        let out = f(&tx)?; // dropping `tx` on error rolls back
        tx.tx.commit()?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(json: &str) -> EntityRecord {
        serde_json::from_str(json).unwrap()
    }

    fn new_row(uri: &str) -> NewOutboxRow {
        NewOutboxRow {
            entity_uri: uri.into(),
            endpoint: "/v1/x".into(),
            payload_json: "{}".into(),
            created_at: 10,
            field: "count".into(),
            value_expr: "entity.count + 1".into(),
            prev_value_json: "0".into(),
            rollback_value_json: "0".into(),
        }
    }

    #[test]
    fn split_uri_parts() {
        assert_eq!(split_uri("pixelfed://posts/101"), ("pixelfed", "posts"));
        assert_eq!(split_uri("core://counters/home"), ("core", "counters"));
        assert_eq!(split_uri("core://x"), ("core", "x"));
        assert_eq!(split_uri("nope"), ("", ""));
    }

    #[test]
    fn schema_created_with_user_version() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        let tables: Vec<String> = s
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index') ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for t in ["action_outbox", "active_workspace", "entities", "idx_entities_domain"] {
            assert!(tables.iter().any(|n| n == t), "missing {t}: {tables:?}");
        }
    }

    #[test]
    fn entity_crud_and_derived_columns() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_entity("pixelfed://posts/101", &entity(r#"{"likes":1}"#), 5)
            .unwrap();
        s.upsert_entity("pixelfed://posts/101", &entity(r#"{"likes":2}"#), 6)
            .unwrap();
        assert_eq!(
            s.get_entity("pixelfed://posts/101").unwrap(),
            Some(entity(r#"{"likes":2}"#))
        );
        let (d, t, u): (String, String, i64) = s
            .conn
            .query_row(
                "SELECT domain, entity_type, updated_at FROM entities WHERE uri = 'pixelfed://posts/101'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((d.as_str(), t.as_str(), u), ("pixelfed", "posts", 6));
        let loaded = s
            .load_entities(&["pixelfed://posts/101".into(), "missing://a/b".into()])
            .unwrap();
        assert_eq!(loaded.len(), 1);
        s.delete_entity("pixelfed://posts/101").unwrap();
        assert_eq!(s.get_entity("pixelfed://posts/101").unwrap(), None);
    }

    #[test]
    fn outbox_fifo_retry_delete() {
        let s = Store::open_in_memory().unwrap();
        let a = s.insert_outbox(&new_row("core://c/a")).unwrap();
        let b = s.insert_outbox(&new_row("core://c/b")).unwrap();
        assert!(a < b);
        assert_eq!(s.pending_count().unwrap(), 2);
        let rows = s.pending_outbox().unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(rows[0].status, STATUS_PENDING);
        assert_eq!(rows[0].retry_count, 0);
        assert_eq!(rows[0].next_attempt_at, 0);
        assert_eq!(rows[0].field.as_deref(), Some("count"));

        s.mark_retry(a, 99, "HTTP 503").unwrap();
        s.mark_retry(a, 100, "HTTP 503").unwrap();
        let first = s.first_pending().unwrap().unwrap();
        assert_eq!(first.id, a);
        assert_eq!(first.retry_count, 2);
        assert_eq!(first.next_attempt_at, 100);
        assert_eq!(first.last_error.as_deref(), Some("HTTP 503"));

        s.set_outbox_prev_value(b, "7").unwrap();
        assert_eq!(s.get_outbox(b).unwrap().unwrap().prev_value_json.as_deref(), Some("7"));

        assert!(s.delete_outbox(a).unwrap());
        assert!(!s.delete_outbox(a).unwrap());
        assert_eq!(s.pending_count().unwrap(), 1);
        assert_eq!(s.first_pending().unwrap().unwrap().id, b);
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let mut s = Store::open_in_memory().unwrap();
        let r: StoreResult<()> = s.transaction(|tx| {
            tx.upsert_entity("core://c/a", &entity(r#"{"count":1}"#), 1)?;
            tx.insert_outbox(&new_row("core://c/a"))?;
            Err(StoreError::InvalidQuery)
        });
        assert!(r.is_err());
        assert_eq!(s.get_entity("core://c/a").unwrap(), None);
        assert_eq!(s.pending_count().unwrap(), 0);

        s.transaction(|tx| {
            tx.upsert_entity("core://c/a", &entity(r#"{"count":1}"#), 1)?;
            tx.insert_outbox(&new_row("core://c/a"))
        })
        .unwrap();
        assert_eq!(s.pending_count().unwrap(), 1);
    }

    #[test]
    fn workspace_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.load_workspace("home").unwrap(), None);
        let layout: UiNode = serde_json::from_str(
            r#"{"type":"core://Workspace","id":"home","title":"Home","slots":[]}"#,
        )
        .unwrap();
        let uris = vec!["core://counters/home".to_string()];
        s.save_workspace("home", &layout, &uris, 3).unwrap();
        let ws = s.load_workspace("home").unwrap().unwrap();
        assert_eq!(ws.layout, layout);
        assert_eq!(ws.entity_uris, uris);
        assert_eq!(ws.updated_at, 3);
        assert_eq!(s.load_workspace("other").unwrap(), None);
    }
}
