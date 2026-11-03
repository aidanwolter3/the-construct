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
use std::time::Duration;

use protocol::{EntityRecord, Notification, UiNode};
use rusqlite::{Connection, ErrorCode, MAIN_DB, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use rusqlite::Error as StoreError;

pub type StoreResult<T> = Result<T, StoreError>;

/// Current schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: u32 = 7;

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

/// Schema v2 (M6): the bundle's resolved `queries` per workspace, and the
/// `also` field updates of a multi-field optimistic mutation per outbox row
/// (`[{"field","value_expr","prev_value"}]`, NULL for single-field rows).
const MIGRATION_V2: &str = "
ALTER TABLE active_workspace ADD COLUMN queries_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE action_outbox ADD COLUMN also_json TEXT;
";

/// Schema v3 (D34): the cached answers of data paths (`action.data.load`, a
/// screen's `data`): the entity URIs and queries each path last returned; and
/// the create info of an `action.mutation.create` outbox row
/// (`{"query_key","refresh_workspace"}`, NULL for other rows).
const MIGRATION_V3: &str = "
CREATE TABLE IF NOT EXISTS data_loads (
    path TEXT PRIMARY KEY NOT NULL,
    entity_uris_json TEXT NOT NULL,
    queries_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
ALTER TABLE action_outbox ADD COLUMN create_json TEXT;
";

/// Schema v4 (D36): whether an outbox row was queued while signed in (it
/// needs the session, so an explicit logout drops it). Rows from before v4
/// count as signed in: keeping a row that can only fail is worse than
/// dropping one at logout.
const MIGRATION_V4: &str = "
ALTER TABLE action_outbox ADD COLUMN session_bound INTEGER NOT NULL DEFAULT 1;
";

/// Schema v5 (D49): staged attachments (files under `<storage>/attachments/`).
/// `released_at` is set once the create that carried them was accepted (the
/// file is kept a little longer, while the placeholder may still show it).
const MIGRATION_V5: &str = "
CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY NOT NULL,
    path TEXT NOT NULL,
    mime TEXT NOT NULL,
    size INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    released_at INTEGER
);
";

/// Schema v6 (D52): small engine values (`engine_kv`: the device id, the
/// last fetched notification channels and unread counts, the remembered
/// push subscription) and the notification cache (the newest
/// [`NOTIFICATIONS_KEPT`], so the inbox works offline). `json` is the
/// [`Notification`] as the pod sent it; `read` mirrors its flag.
const MIGRATION_V6: &str = "
CREATE TABLE IF NOT EXISTS engine_kv (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notifications (
    id TEXT PRIMARY KEY NOT NULL,
    app_id TEXT NOT NULL,
    read INTEGER NOT NULL DEFAULT 0,
    json TEXT NOT NULL
);
";

/// Schema v7 (docs/decisions/remembered-form-values.md): form values a
/// layout asked the device to remember (`core://ChoiceChips` `remember`),
/// per app and form scope (`""` for the app's layout, else a screen id).
/// Device preferences, not private data: kept at sign-out.
const MIGRATION_V7: &str = "
CREATE TABLE IF NOT EXISTS form_values (
    app_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    field TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (app_id, scope, field)
);
";

/// `MIGRATIONS[i]` upgrades schema v`i` to v`i+1`, each in one transaction.
const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [
    MIGRATION_V1,
    MIGRATION_V2,
    MIGRATION_V3,
    MIGRATION_V4,
    MIGRATION_V5,
    MIGRATION_V6,
    MIGRATION_V7,
];

/// Notifications kept in the cache (the newest ids).
pub const NOTIFICATIONS_KEPT: usize = 200;

/// `engine_kv` keys of the notification cache (cleared at sign-out).
pub const KV_NOTIFICATION_CHANNELS: &str = "notification_channels";
pub const KV_NOTIFICATION_UNREAD: &str = "notification_unread";

/// Newest first: pod ids are sortable (numeric strings or fixed-width ids),
/// so a longer id is newer and equal lengths compare as text.
const NOTIFICATION_ORDER: &str = "LENGTH(id) DESC, id DESC";

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
    /// The bundle's resolved queries (`query_key` -> ordered entity URIs).
    pub queries: BTreeMap<String, Vec<String>>,
    pub updated_at: i64,
}

/// One `also` field update stored with an outbox row: the expression and
/// the field's value before the row was (last) applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlsoUpdate {
    pub field: String,
    pub value_expr: String,
    #[serde(default)]
    pub prev_value: Value,
}

fn also_to_json(also: &[AlsoUpdate]) -> StoreResult<Option<String>> {
    if also.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(also).map(Some).map_err(json_err)
}

/// What an `action.mutation.create` row adds: its temporary entity
/// (the row's `entity_uri`) is listed in `queries[query_key]` while pending.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CreateInfo {
    pub query_key: String,
    #[serde(default)]
    pub refresh_workspace: bool,
    /// The temporary entity is listed first (`position: start`, schema 9).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub at_start: bool,
    /// Attachments uploaded before the POST (D49), in payload order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
}

/// One attachment of a create row: the create's attachment `name`, the staged
/// attachment `id`, and its pod upload id once uploaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub name: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
}

/// A staged attachment (`attachments` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub size: i64,
    pub width: u32,
    pub height: u32,
    pub created_at: i64,
    pub released_at: Option<i64>,
}

/// One cached data path answer (`data_loads`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataLoad {
    pub entity_uris: Vec<String>,
    pub queries: BTreeMap<String, Vec<String>>,
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
    /// Extra fields updated with `field` (empty for single-field rows).
    pub also: Vec<AlsoUpdate>,
    /// Set for a create row (then `field`/`value_expr` are stored as NULL).
    pub create: Option<CreateInfo>,
    /// Queued while signed in: it needs the session (dropped at logout).
    pub session_bound: bool,
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
    /// Extra fields updated with `field`; empty if none (or if the stored
    /// JSON is corrupt, logged).
    pub also: Vec<AlsoUpdate>,
    /// Set for an `action.mutation.create` row.
    pub create: Option<CreateInfo>,
}

const OUTBOX_COLUMNS: &str = "id, entity_uri, endpoint, payload_json, created_at, \
    COALESCE(retry_count, 0), COALESCE(status, 'PENDING'), field, value_expr, prev_value_json, \
    rollback_value_json, COALESCE(next_attempt_at, 0), last_error, also_json, create_json";

fn outbox_row(r: &Row<'_>) -> rusqlite::Result<OutboxRow> {
    let id: i64 = r.get(0)?;
    let also_json: Option<String> = r.get(13)?;
    let also = match also_json.as_deref() {
        None | Some("") => Vec::new(),
        Some(raw) => serde_json::from_str(raw).unwrap_or_else(|e| {
            eprintln!("[ConstructEngine] store: outbox #{id}: ignoring corrupt also_json: {e}");
            Vec::new()
        }),
    };
    let create_json: Option<String> = r.get(14)?;
    let create = create_json
        .as_deref()
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| match serde_json::from_str(raw) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!(
                    "[ConstructEngine] store: outbox #{id}: ignoring corrupt create_json: {e}"
                );
                None
            }
        });
    Ok(OutboxRow {
        id,
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
        also,
        create,
    })
}

fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::ToSqlConversionFailure(Box::new(e))
}

fn parse_entity(uri: &str, raw: &str) -> Option<EntityRecord> {
    match serde_json::from_str(raw) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("[ConstructEngine] store: ignoring corrupt entity {uri}: {e}");
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

    /// Delete every entity that no PENDING outbox row targets (sign-out:
    /// the cache may hold the account's data, whatever app it came from).
    /// Returns the number deleted.
    fn delete_unreferenced_entities(&self) -> StoreResult<usize> {
        self.conn()
            .prepare_cached(
                "DELETE FROM entities WHERE uri NOT IN (
                     SELECT entity_uri FROM action_outbox
                     WHERE COALESCE(status, 'PENDING') = 'PENDING')",
            )?
            .execute([])
    }

    /// Delete every cached data path answer. Returns the number deleted.
    fn delete_all_data_loads(&self) -> StoreResult<usize> {
        self.conn()
            .prepare_cached("DELETE FROM data_loads")?
            .execute([])
    }

    // ---- active_workspace ----

    fn delete_workspace(&self, id: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("DELETE FROM active_workspace WHERE id = ?1")?
            .execute([id])?;
        Ok(())
    }

    /// Insert or replace a workspace's layout, entity URI list and queries.
    fn save_workspace(
        &self,
        id: &str,
        layout: &UiNode,
        entity_uris: &[String],
        queries: &BTreeMap<String, Vec<String>>,
        updated_at: i64,
    ) -> StoreResult<()> {
        let layout = serde_json::to_string(layout).map_err(json_err)?;
        let uris = serde_json::to_string(entity_uris).map_err(json_err)?;
        let queries = serde_json::to_string(queries).map_err(json_err)?;
        self.conn()
            .prepare_cached(
                "INSERT INTO active_workspace
                     (id, workspace_json, updated_at, entity_uris_json, queries_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET workspace_json = excluded.workspace_json,
                     updated_at = excluded.updated_at, entity_uris_json = excluded.entity_uris_json,
                     queries_json = excluded.queries_json",
            )?
            .execute(params![id, layout, updated_at, uris, queries])?;
        Ok(())
    }

    /// The cached workspace, or `None` if absent (or its JSON is corrupt).
    fn load_workspace(&self, id: &str) -> StoreResult<Option<CachedWorkspace>> {
        let row: Option<(String, String, i64, String)> = self
            .conn()
            .prepare_cached(
                "SELECT workspace_json, entity_uris_json, updated_at, queries_json
                 FROM active_workspace WHERE id = ?1",
            )?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        let Some((layout, uris, updated_at, queries)) = row else {
            return Ok(None);
        };
        let queries = serde_json::from_str(&queries).unwrap_or_else(|e| {
            eprintln!("[ConstructEngine] store: ignoring corrupt queries of workspace '{id}': {e}");
            BTreeMap::new()
        });
        match (serde_json::from_str(&layout), serde_json::from_str(&uris)) {
            (Ok(layout), Ok(entity_uris)) => Ok(Some(CachedWorkspace {
                layout,
                entity_uris,
                queries,
                updated_at,
            })),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("[ConstructEngine] store: ignoring corrupt workspace '{id}': {e}");
                Ok(None)
            }
        }
    }

    // ---- data_loads ----

    /// Insert or replace the cached answer of a data path.
    fn save_data_load(&self, path: &str, load: &DataLoad, updated_at: i64) -> StoreResult<()> {
        let uris = serde_json::to_string(&load.entity_uris).map_err(json_err)?;
        let queries = serde_json::to_string(&load.queries).map_err(json_err)?;
        self.conn()
            .prepare_cached(
                "INSERT INTO data_loads (path, entity_uris_json, queries_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET entity_uris_json = excluded.entity_uris_json,
                     queries_json = excluded.queries_json, updated_at = excluded.updated_at",
            )?
            .execute(params![path, uris, queries, updated_at])?;
        Ok(())
    }

    /// Every cached data path answer (corrupt rows are skipped, logged).
    fn load_data_loads(&self) -> StoreResult<BTreeMap<String, DataLoad>> {
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT path, entity_uris_json, queries_json FROM data_loads")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (path, uris, queries) = row?;
            match (serde_json::from_str(&uris), serde_json::from_str(&queries)) {
                (Ok(entity_uris), Ok(queries)) => {
                    out.insert(
                        path,
                        DataLoad {
                            entity_uris,
                            queries,
                        },
                    );
                }
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("[ConstructEngine] store: ignoring corrupt data load '{path}': {e}")
                }
            }
        }
        Ok(out)
    }

    fn delete_data_load(&self, path: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("DELETE FROM data_loads WHERE path = ?1")?
            .execute([path])?;
        Ok(())
    }

    // ---- action_outbox ----

    /// Insert a PENDING row; returns its id.
    fn insert_outbox(&self, row: &NewOutboxRow) -> StoreResult<i64> {
        self.conn()
            .prepare_cached(
                "INSERT INTO action_outbox (entity_uri, endpoint, payload_json, created_at,
                     field, value_expr, prev_value_json, rollback_value_json, also_json, create_json,
                     session_bound)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(params![
                row.entity_uri,
                row.endpoint,
                row.payload_json,
                row.created_at,
                row.create.is_none().then_some(&row.field),
                row.create.is_none().then_some(&row.value_expr),
                row.prev_value_json,
                row.rollback_value_json,
                also_to_json(&row.also)?,
                row.create
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(json_err)?,
                row.session_bound,
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

    /// Delete every PENDING row queued while signed in (explicit logout:
    /// nobody could send them any more). Returns the number deleted.
    fn delete_pending_session_rows(&self) -> StoreResult<usize> {
        self.conn()
            .prepare_cached(
                "DELETE FROM action_outbox WHERE COALESCE(status, 'PENDING') = 'PENDING'
                     AND session_bound != 0",
            )?
            .execute([])
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

    /// Replace a create row's create info (an attachment got its upload id).
    fn set_create_info(&self, id: i64, info: &CreateInfo) -> StoreResult<()> {
        let raw = serde_json::to_string(info).map_err(json_err)?;
        self.conn()
            .prepare_cached("UPDATE action_outbox SET create_json = ?2 WHERE id = ?1")?
            .execute(params![id, raw])?;
        Ok(())
    }

    // ---- attachments ----

    fn insert_attachment(&self, a: &Attachment) -> StoreResult<()> {
        self.conn()
            .prepare_cached(
                "INSERT OR REPLACE INTO attachments
                     (id, path, mime, size, width, height, created_at, released_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![
                a.id,
                a.path,
                a.mime,
                a.size,
                a.width,
                a.height,
                a.created_at,
                a.released_at
            ])?;
        Ok(())
    }

    /// Every staged attachment, oldest first.
    fn load_attachments(&self) -> StoreResult<Vec<Attachment>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, path, mime, size, width, height, created_at, released_at
             FROM attachments ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Attachment {
                id: r.get(0)?,
                path: r.get(1)?,
                mime: r.get(2)?,
                size: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                created_at: r.get(6)?,
                released_at: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    fn delete_attachment(&self, id: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("DELETE FROM attachments WHERE id = ?1")?
            .execute([id])?;
        Ok(())
    }

    fn release_attachment(&self, id: &str, now: i64) -> StoreResult<()> {
        self.conn()
            .prepare_cached("UPDATE attachments SET released_at = ?2 WHERE id = ?1")?
            .execute(params![id, now])?;
        Ok(())
    }

    /// Update a row's `prev_value_json` (rollback / refetch re-apply).
    fn set_outbox_prev_value(&self, id: i64, prev_value_json: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("UPDATE action_outbox SET prev_value_json = ?2 WHERE id = ?1")?
            .execute(params![id, prev_value_json])?;
        Ok(())
    }

    /// Update a row's primary `prev_value_json` and its `also` updates
    /// (with their new `prev_value`s) after a re-apply.
    fn set_outbox_prevs(
        &self,
        id: i64,
        prev_value_json: &str,
        also: &[AlsoUpdate],
    ) -> StoreResult<()> {
        self.conn()
            .prepare_cached(
                "UPDATE action_outbox SET prev_value_json = ?2, also_json = ?3 WHERE id = ?1",
            )?
            .execute(params![id, prev_value_json, also_to_json(also)?])?;
        Ok(())
    }

    // ---- engine_kv ----

    fn kv_get(&self, key: &str) -> StoreResult<Option<String>> {
        self.conn()
            .prepare_cached("SELECT value FROM engine_kv WHERE key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()
    }

    fn kv_set(&self, key: &str, value: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached(
                "INSERT INTO engine_kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?
            .execute(params![key, value])?;
        Ok(())
    }

    fn kv_delete(&self, key: &str) -> StoreResult<()> {
        self.conn()
            .prepare_cached("DELETE FROM engine_kv WHERE key = ?1")?
            .execute([key])?;
        Ok(())
    }

    // ---- form_values ----

    /// The remembered form values of one app's form `scope` (field -> value).
    fn form_values(&self, app_id: &str, scope: &str) -> StoreResult<BTreeMap<String, String>> {
        self.conn()
            .prepare_cached(
                "SELECT field, value FROM form_values WHERE app_id = ?1 AND scope = ?2",
            )?
            .query_map(params![app_id, scope], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    /// Remember a form value (`None` forgets it).
    fn set_form_value(
        &self,
        app_id: &str,
        scope: &str,
        field: &str,
        value: Option<&str>,
        updated_at: i64,
    ) -> StoreResult<()> {
        match value {
            Some(v) => self
                .conn()
                .prepare_cached(
                    "INSERT INTO form_values (app_id, scope, field, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(app_id, scope, field) DO UPDATE SET value = excluded.value,
                         updated_at = excluded.updated_at",
                )?
                .execute(params![app_id, scope, field, v, updated_at])?,
            None => self
                .conn()
                .prepare_cached(
                    "DELETE FROM form_values WHERE app_id = ?1 AND scope = ?2 AND field = ?3",
                )?
                .execute(params![app_id, scope, field])?,
        };
        Ok(())
    }

    // ---- notifications ----

    /// Insert or replace a notification. Returns whether it was new.
    fn upsert_notification(&self, n: &Notification) -> StoreResult<bool> {
        let existed = self
            .conn()
            .prepare_cached("SELECT 1 FROM notifications WHERE id = ?1")?
            .query_row([&n.id], |_| Ok(()))
            .optional()?
            .is_some();
        let json = serde_json::to_string(n).map_err(json_err)?;
        self.conn()
            .prepare_cached(
                "INSERT INTO notifications (id, app_id, read, json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET app_id = excluded.app_id,
                     read = excluded.read, json = excluded.json",
            )?
            .execute(params![n.id, n.app_id, n.read, json])?;
        Ok(!existed)
    }

    /// Whether a notification is cached.
    fn has_notification(&self, id: &str) -> StoreResult<bool> {
        Ok(self
            .conn()
            .prepare_cached("SELECT 1 FROM notifications WHERE id = ?1")?
            .query_row([id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// The apps with an unread cached notification.
    fn unread_notification_apps(&self) -> StoreResult<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT DISTINCT app_id FROM notifications WHERE read = 0")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    /// Keep only the newest `keep` notifications. Returns the number deleted.
    fn prune_notifications(&self, keep: usize) -> StoreResult<usize> {
        self.conn()
            .prepare_cached(&format!(
                "DELETE FROM notifications WHERE id NOT IN (
                     SELECT id FROM notifications ORDER BY {NOTIFICATION_ORDER} LIMIT ?1)"
            ))?
            .execute([keep as i64])
    }

    /// Cached notifications, newest first: older than `before` (an id) when
    /// given, of `app` when given, at most `limit` (corrupt rows skipped).
    fn load_notifications(
        &self,
        before: Option<&str>,
        limit: usize,
        app: Option<&str>,
    ) -> StoreResult<Vec<Notification>> {
        let mut stmt = self.conn().prepare_cached(&format!(
            "SELECT id, json FROM notifications
             WHERE (?1 IS NULL OR LENGTH(id) < LENGTH(?1)
                    OR (LENGTH(id) = LENGTH(?1) AND id < ?1))
               AND (?2 IS NULL OR app_id = ?2)
             ORDER BY {NOTIFICATION_ORDER} LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![before, app, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, json) = row?;
            match serde_json::from_str::<Notification>(&json) {
                Ok(n) => out.push(n),
                Err(e) => {
                    eprintln!("[ConstructEngine] store: ignoring corrupt notification {id}: {e}")
                }
            }
        }
        Ok(out)
    }

    /// Mark cached notifications read: the given `ids`, or (`ids` = None)
    /// every one of `app` (all apps when `app` is None). Returns
    /// `app_id -> number newly marked`.
    fn mark_notifications_read(
        &self,
        ids: Option<&[String]>,
        app: Option<&str>,
    ) -> StoreResult<BTreeMap<String, u32>> {
        let mut targets: Vec<(String, String, String)> = Vec::new();
        {
            let mut stmt = self.conn().prepare_cached(
                "SELECT id, app_id, json FROM notifications
                 WHERE read = 0 AND (?1 IS NULL OR app_id = ?1)",
            )?;
            let rows = stmt.query_map(params![app], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for row in rows {
                let (id, app_id, json): (String, String, String) = row?;
                if ids.is_none_or(|ids| ids.contains(&id)) {
                    targets.push((id, app_id, json));
                }
            }
        }
        let mut by_app = BTreeMap::new();
        for (id, app_id, json) in targets {
            let json = match serde_json::from_str::<Value>(&json) {
                Ok(mut v) => {
                    v["read"] = Value::Bool(true);
                    v.to_string()
                }
                Err(_) => json,
            };
            self.conn()
                .prepare_cached("UPDATE notifications SET read = 1, json = ?2 WHERE id = ?1")?
                .execute(params![id, json])?;
            *by_app.entry(app_id).or_insert(0u32) += 1;
        }
        Ok(by_app)
    }

    /// Forget every cached notification, the cached channels and unread
    /// counts (sign-out: they hold private text). Returns the number of
    /// notifications deleted.
    fn delete_notification_cache(&self) -> StoreResult<usize> {
        let n = self
            .conn()
            .prepare_cached("DELETE FROM notifications")?
            .execute([])?;
        self.kv_delete(KV_NOTIFICATION_CHANNELS)?;
        self.kv_delete(KV_NOTIFICATION_UNREAD)?;
        Ok(n)
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

/// How long a write waits for another connection's lock before failing
/// with `SQLITE_BUSY`. Kept short because `dispatch` (and so the mutate
/// transaction) runs on the UI thread on both platforms: a locked cache.db
/// costs a click at most this long, then the mutate fails atomically (no
/// change, logged) instead of freezing the UI.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// `file:<path>?mode=ro` with the URI-significant characters escaped.
fn readonly_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '%' | '?' | '#' => uri.push_str(&format!("%{:02X}", c as u32)),
            c => uri.push(c),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

/// True if `e` is SQLite's "attempt to write a readonly database" (or a
/// read-only open).
pub fn is_read_only_error(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::SqliteFailure(f, _) if f.code == ErrorCode::ReadOnly
    )
}

/// Create `dir` (and missing parents) readable only by its owner (0700 on
/// unix), and drop group/other access from it if it already exists. The
/// cache holds the private feed, the outbox and queued photos. Owner bits
/// are never added, so a deliberately read-only directory stays read-only.
pub fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        strip_group_other(dir, std::fs::metadata(dir)?.permissions().mode())
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// Create the DB file at `path` as 0600 if it is missing (SQLite gives
/// its `-wal`, `-shm` and `-journal` files the same mode), and drop
/// group/other access from those files where they already exist (caches
/// written before this was enforced).
fn make_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let p = path.with_file_name(format!("{name}{suffix}"));
            match std::fs::metadata(&p) {
                Ok(m) => strip_group_other(&p, m.permissions().mode())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(unix)]
fn strip_group_other(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if mode & 0o077 == 0 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7700))
}

fn read_only_error(msg: &str) -> StoreError {
    StoreError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
        Some(msg.to_string()),
    )
}

impl Store {
    /// Open (creating if needed) and migrate a file-backed DB in WAL mode.
    ///
    /// Fails with a read-only error ([`is_read_only_error`]) if the DB can
    /// be read but not written (file or directory permissions), checked
    /// with a write probe that is rolled back. A probe that only hits a
    /// lock held by another connection does not fail the open.
    pub fn open(path: &Path) -> StoreResult<Self> {
        if let Err(e) = make_private_file(path) {
            eprintln!(
                "[ConstructEngine] store: cannot restrict {} to its owner: {e}",
                path.display()
            );
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // WAL + NORMAL: commits append to the WAL without an fsync each
        // time; a crash never corrupts the DB, a power loss may drop the
        // last few commits.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Deleted rows are overwritten with zeros instead of lingering in
        // free pages (a purged private feed must not stay readable in the
        // file); cheap at this cache's size.
        conn.pragma_update(None, "secure_delete", "ON")?;
        if conn.is_readonly(MAIN_DB)? {
            return Err(read_only_error("database opened read-only"));
        }
        let store = Self::init(conn)?;
        store.probe_writable()?;
        Ok(store)
    }

    /// Rewrite `user_version` with its current value inside a transaction
    /// that is rolled back: fails if the DB cannot be written. `SQLITE_BUSY`
    /// (another connection holds the write lock) counts as writable.
    fn probe_writable(&self) -> StoreResult<()> {
        let version = self.schema_version()?;
        let probe = (|| {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            self.conn.pragma_update(None, "user_version", version)
        })();
        let _ = self.conn.execute_batch("ROLLBACK");
        match probe {
            Err(StoreError::SqliteFailure(f, _))
                if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) =>
            {
                eprintln!(
                    "[ConstructEngine] store: cache.db is locked by another connection at open; \
                     writes will wait up to {} ms",
                    BUSY_TIMEOUT.as_millis()
                );
                Ok(())
            }
            other => other,
        }
    }

    /// A private in-memory DB holding a read-only copy of the workspaces
    /// and entities in the DB file at `path` (opened read-only), for when
    /// that file cannot be written. Outbox rows are deliberately not copied:
    /// they could be flushed but never deleted from the file, so they would
    /// be sent again once it is writable. Returns the store and the number
    /// of (workspaces, entities) copied; if the file cannot be read the
    /// copy is skipped (logged) and the store is empty.
    pub fn open_in_memory_copy_of(path: &Path) -> StoreResult<(Self, usize, usize)> {
        let store = Self::open_in_memory()?;
        let copied = (|| {
            store
                .conn
                .execute("ATTACH DATABASE ?1 AS ro", [readonly_uri(path)])?;
            // The read-only file cannot be migrated: a v1 file has no
            // queries_json (the copy then gets the '{}' default).
            let ro_version: u32 =
                store
                    .conn
                    .pragma_query_value(Some("ro"), "user_version", |r| r.get(0))?;
            let queries = if ro_version >= 2 {
                "queries_json"
            } else {
                "'{}'"
            };
            let ws = store.conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO main.active_workspace
                         (id, workspace_json, updated_at, entity_uris_json, queries_json)
                     SELECT id, workspace_json, updated_at, entity_uris_json, {queries}
                     FROM ro.active_workspace"
                ),
                [],
            )?;
            let ents = store.conn.execute(
                "INSERT OR REPLACE INTO main.entities
                     (uri, domain, entity_type, payload_json, updated_at)
                 SELECT uri, domain, entity_type, payload_json, updated_at FROM ro.entities",
                [],
            )?;
            Ok::<_, StoreError>((ws, ents))
        })();
        let _ = store.conn.execute_batch("DETACH DATABASE ro");
        match copied {
            Ok((ws, ents)) => Ok((store, ws, ents)),
            Err(e) => {
                eprintln!(
                    "[ConstructEngine] store: cannot copy the read-only {}: {e}",
                    path.display()
                );
                Ok((store, 0, 0))
            }
        }
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
                "[ConstructEngine] store: schema version {version} is newer than {SCHEMA_VERSION}; \
                 using it as is"
            );
            return Ok(());
        }
        for (from, step) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(step)?;
            tx.pragma_update(None, "user_version", from as u32 + 1)?;
            tx.commit()?;
            if from > 0 {
                eprintln!(
                    "[ConstructEngine] store: migrated schema v{from} -> v{}",
                    from + 1
                );
            }
        }
        Ok(())
    }

    /// Copy the WAL into the main file and truncate it to zero bytes
    /// (`PRAGMA wal_checkpoint(TRUNCATE)`), so pages that held deleted rows
    /// (zeroed by `secure_delete`) no longer survive as old WAL frames.
    /// Returns whether the checkpoint completed (false if another
    /// connection's reader kept it from finishing). A no-op in memory.
    pub fn checkpoint_truncate(&self) -> StoreResult<bool> {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        Ok(busy == 0)
    }

    /// `PRAGMA user_version`.
    pub fn schema_version(&self) -> StoreResult<u32> {
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
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
            also: Vec::new(),
            create: None,
            session_bound: false,
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
        for t in [
            "action_outbox",
            "active_workspace",
            "entities",
            "idx_entities_domain",
        ] {
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
        assert_eq!(
            s.get_outbox(b).unwrap().unwrap().prev_value_json.as_deref(),
            Some("7")
        );

        assert!(s.delete_outbox(a).unwrap());
        assert!(!s.delete_outbox(a).unwrap());
        assert_eq!(s.pending_count().unwrap(), 1);
        assert_eq!(s.first_pending().unwrap().unwrap().id, b);
    }

    #[test]
    fn sign_out_purge_helpers() {
        let s = Store::open_in_memory().unwrap();
        for uri in ["x://posts/1", "x://posts/2", "y://counters/home"] {
            s.upsert_entity(uri, &entity("{}"), 1).unwrap();
        }
        // Queued while signed in, and while signed out.
        let signed_in = s
            .insert_outbox(&NewOutboxRow {
                session_bound: true,
                ..new_row("x://posts/1")
            })
            .unwrap();
        s.insert_outbox(&new_row("y://counters/home")).unwrap();
        // Every entity no pending row targets goes, whatever its domain.
        assert_eq!(s.delete_unreferenced_entities().unwrap(), 1);
        assert!(
            s.get_entity("x://posts/1").unwrap().is_some(),
            "pending row keeps it"
        );
        assert!(s.get_entity("x://posts/2").unwrap().is_none());
        assert!(s.get_entity("y://counters/home").unwrap().is_some());

        assert_eq!(s.delete_pending_session_rows().unwrap(), 1);
        assert!(s.get_outbox(signed_in).unwrap().is_none());
        assert_eq!(s.pending_count().unwrap(), 1);
        assert_eq!(s.delete_unreferenced_entities().unwrap(), 1);

        s.save_data_load("/v1/x", &DataLoad::default(), 1).unwrap();
        assert_eq!(s.delete_all_data_loads().unwrap(), 1);

        let layout: UiNode = serde_json::from_str(
            r#"{"type":"core://Workspace","id":"home","title":"Home","slots":[]}"#,
        )
        .unwrap();
        s.save_workspace("w", &layout, &[], &BTreeMap::new(), 1)
            .unwrap();
        s.delete_workspace("w").unwrap();
        assert_eq!(s.load_workspace("w").unwrap(), None);
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

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "client-core-store-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("cache.db")
    }

    /// A schema v1 file (as M5 wrote it) with one workspace and one outbox row.
    fn write_v1_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO active_workspace (id, workspace_json, updated_at, entity_uris_json)
             VALUES ('home', '{\"type\":\"core://Workspace\",\"id\":\"home\",\"title\":\"Home\",\"slots\":[]}',
                     1, '[\"core://counters/home\"]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO action_outbox (entity_uri, endpoint, payload_json, created_at, field,
                 value_expr, prev_value_json, rollback_value_json)
             VALUES ('core://counters/home', '/v1/x', '{}', 1, 'count', 'entity.count + 1', '0', '0')",
            [],
        )
        .unwrap();
    }

    /// Whether `needle` occurs in the bytes of `path` (missing file: no).
    fn file_contains(path: &Path, needle: &[u8]) -> bool {
        std::fs::read(path).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
    }

    #[test]
    fn purged_rows_do_not_survive_in_the_db_or_wal_bytes() {
        const CAPTION: &str = "PRIVATE-CAPTION-sunset-at-the-lake-7f3a";
        let path = temp_db("secure-delete");
        let wal = path.with_file_name("cache.db-wal");
        let mut s = Store::open(&path).unwrap();
        let secure: i64 = s
            .conn
            .query_row("PRAGMA secure_delete", [], |r| r.get(0))
            .unwrap();
        assert_eq!(secure, 1);
        s.transaction(|tx| {
            for i in 0..50 {
                tx.upsert_entity(
                    &format!("pixelfed://posts/{i}"),
                    &entity(&format!(r#"{{"id":"{i}","caption":"{CAPTION} {i}"}}"#)),
                    1,
                )?;
            }
            tx.upsert_entity("core://counters/home", &entity(r#"{"count":1}"#), 1)?;
            // A pending action keeps the counter.
            tx.insert_outbox(&new_row("core://counters/home"))
                .map(|_| ())
        })
        .unwrap();
        // Moved into the main file, as a long-running app would have.
        assert!(s.checkpoint_truncate().unwrap());
        assert!(file_contains(&path, CAPTION.as_bytes()));

        let n = s
            .transaction(|tx| tx.delete_unreferenced_entities())
            .unwrap();
        assert_eq!(n, 50);
        assert!(s.checkpoint_truncate().unwrap());
        assert!(
            !file_contains(&path, CAPTION.as_bytes()),
            "caption left in cache.db"
        );
        assert!(
            !file_contains(&wal, CAPTION.as_bytes()),
            "caption left in cache.db-wal"
        );
        assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);
        // Other data is intact.
        assert_eq!(
            s.load_entities(&["core://counters/home".into()])
                .unwrap()
                .len(),
            1
        );
        drop(s);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn cache_dir_and_files_are_owner_only() {
        let base = temp_db("private").parent().unwrap().to_path_buf();
        let dir = base.join("servers").join("x");
        make_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&base.join("servers")), 0o700);
        let path = dir.join("cache.db");
        let mut s = Store::open(&path).unwrap();
        s.transaction(|tx| tx.upsert_entity("core://c/a", &entity(r#"{"n":1}"#), 1))
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&path.with_file_name("cache.db-wal")), 0o600);
        assert_eq!(mode(&path.with_file_name("cache.db-shm")), 0o600);
        drop(s);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn existing_cache_files_lose_group_and_other_access() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_db("tighten");
        let dir = path.parent().unwrap().to_path_buf();
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        write_v1_db(&path);
        set(&dir, 0o755);
        set(&path, 0o644);
        make_private_dir(&dir).unwrap();
        drop(Store::open(&path).unwrap());
        assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
        // Owner bits are never added: a read-only file stays read-only.
        set(&path, 0o444);
        make_private_file(&path).unwrap();
        assert_eq!(mode(&path), 0o400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_db_migrates_to_v2_and_v3() {
        let path = temp_db("v1");
        write_v1_db(&path);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(s.load_data_loads().unwrap().is_empty());
        assert_eq!(s.first_pending().unwrap().unwrap().create, None);
        let ws = s.load_workspace("home").unwrap().unwrap();
        assert_eq!(ws.entity_uris, vec!["core://counters/home".to_string()]);
        assert!(ws.queries.is_empty());
        let old = s.first_pending().unwrap().unwrap();
        assert_eq!(old.field.as_deref(), Some("count"));
        assert!(old.also.is_empty());

        // New columns are writable.
        let also = vec![AlsoUpdate {
            field: "like_count".into(),
            value_expr: "entity.like_count + 1".into(),
            prev_value: Value::from(5),
        }];
        let id = s
            .insert_outbox(&NewOutboxRow {
                also: also.clone(),
                ..new_row("pixelfed://posts/1")
            })
            .unwrap();
        assert_eq!(s.get_outbox(id).unwrap().unwrap().also, also);
        let rebased = vec![AlsoUpdate {
            prev_value: Value::from(9),
            ..also[0].clone()
        }];
        s.set_outbox_prevs(id, "true", &rebased).unwrap();
        let row = s.get_outbox(id).unwrap().unwrap();
        assert_eq!(
            (row.prev_value_json.as_deref(), row.also),
            (Some("true"), rebased)
        );
        drop(s);
        // Re-opening a migrated DB is a no-op.
        assert_eq!(
            Store::open(&path).unwrap().schema_version().unwrap(),
            SCHEMA_VERSION
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn in_memory_copy_of_a_v1_db_has_empty_queries() {
        let path = temp_db("v1copy");
        write_v1_db(&path);
        let (s, ws, ents) = Store::open_in_memory_copy_of(&path).unwrap();
        assert_eq!((ws, ents), (1, 0));
        assert!(
            s.load_workspace("home")
                .unwrap()
                .unwrap()
                .queries
                .is_empty()
        );
        assert_eq!(s.pending_count().unwrap(), 0, "outbox rows are not copied");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
        let queries = BTreeMap::from([("q".to_string(), uris.clone())]);
        s.save_workspace("home", &layout, &uris, &queries, 3)
            .unwrap();
        let ws = s.load_workspace("home").unwrap().unwrap();
        assert_eq!(ws.layout, layout);
        assert_eq!(ws.entity_uris, uris);
        assert_eq!(ws.queries, queries);
        assert_eq!(ws.updated_at, 3);
        assert_eq!(s.load_workspace("other").unwrap(), None);
    }

    fn notification(id: &str, app: &str) -> Notification {
        Notification {
            id: id.into(),
            app_id: app.into(),
            channel: "c".into(),
            title: format!("t{id}"),
            body: String::new(),
            image_url: None,
            target_uri: None,
            group_key: None,
            created_at: 1,
            read: false,
        }
    }

    #[test]
    fn form_values_are_kept_per_app_and_scope() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.form_values("a", "").unwrap().is_empty());
        s.set_form_value("a", "", "view", Some("cards"), 1).unwrap();
        s.set_form_value("a", "post", "view", Some("grid"), 1)
            .unwrap();
        s.set_form_value("b", "", "view", Some("list"), 1).unwrap();
        s.set_form_value("a", "", "view", Some("tiles"), 2).unwrap();
        s.set_form_value("a", "", "sort", Some("new"), 2).unwrap();
        let a = s.form_values("a", "").unwrap();
        assert_eq!(a.get("view").map(String::as_str), Some("tiles"));
        assert_eq!(a.get("sort").map(String::as_str), Some("new"));
        assert_eq!(a.len(), 2);
        assert_eq!(s.form_values("a", "post").unwrap().len(), 1);
        assert_eq!(s.form_values("b", "").unwrap()["view"], "list");
        s.set_form_value("a", "", "view", None, 3).unwrap();
        assert_eq!(s.form_values("a", "").unwrap().len(), 1);
        // Forgetting what was never stored is fine.
        s.set_form_value("c", "", "x", None, 3).unwrap();
    }

    #[test]
    fn notification_cache_orders_prunes_marks_and_clears() {
        let s = Store::open_in_memory().unwrap();
        // Numeric ids: "10" is newer than "9".
        for (id, app) in [("9", "a"), ("10", "b"), ("8", "a")] {
            assert!(s.upsert_notification(&notification(id, app)).unwrap());
        }
        assert!(!s.upsert_notification(&notification("9", "a")).unwrap());
        let ids = |v: Vec<Notification>| v.into_iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(
            ids(s.load_notifications(None, 10, None).unwrap()),
            ["10", "9", "8"]
        );
        assert_eq!(
            ids(s.load_notifications(Some("10"), 1, None).unwrap()),
            ["9"]
        );
        assert_eq!(
            ids(s.load_notifications(None, 10, Some("a")).unwrap()),
            ["9", "8"]
        );
        let marked = s
            .mark_notifications_read(Some(&["9".to_owned()]), None)
            .unwrap();
        assert_eq!(marked, BTreeMap::from([("a".to_owned(), 1)]));
        assert!(s.load_notifications(None, 10, None).unwrap()[1].read);
        assert_eq!(s.unread_notification_apps().unwrap().len(), 2);
        assert_eq!(
            s.mark_notifications_read(None, Some("b")).unwrap(),
            BTreeMap::from([("b".to_owned(), 1)])
        );
        assert_eq!(s.prune_notifications(2).unwrap(), 1);
        assert_eq!(
            ids(s.load_notifications(None, 10, None).unwrap()),
            ["10", "9"]
        );
        s.kv_set(KV_NOTIFICATION_UNREAD, "{}").unwrap();
        s.kv_set("device_id", "keep-me-1").unwrap();
        assert_eq!(s.delete_notification_cache().unwrap(), 2);
        assert_eq!(s.kv_get(KV_NOTIFICATION_UNREAD).unwrap(), None);
        assert_eq!(s.kv_get("device_id").unwrap().as_deref(), Some("keep-me-1"));
    }

    #[test]
    fn data_loads_and_create_rows_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let load = DataLoad {
            entity_uris: vec!["x://c/1".into()],
            queries: BTreeMap::from([("q".to_owned(), vec!["x://c/1".to_owned()])]),
        };
        s.save_data_load("/v1/x?id=1", &load, 1).unwrap();
        s.save_data_load("/v1/x?id=1", &load, 2).unwrap();
        assert_eq!(s.load_data_loads().unwrap()["/v1/x?id=1"], load);
        s.delete_data_load("/v1/x?id=1").unwrap();
        assert!(s.load_data_loads().unwrap().is_empty());

        let create = CreateInfo {
            query_key: "q".into(),
            refresh_workspace: true,
            ..Default::default()
        };
        let id = s
            .insert_outbox(&NewOutboxRow {
                create: Some(create.clone()),
                ..new_row("x://c/tmp-1")
            })
            .unwrap();
        let row = s.get_outbox(id).unwrap().unwrap();
        assert_eq!(row.create, Some(create));
        // A create row has no field to roll back.
        assert_eq!((row.field, row.value_expr), (None, None));
    }
}
