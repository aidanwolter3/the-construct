//! Server persistence: one SQLite database (`$CONSTRUCT_DATA_DIR/server.db`).
//!
//! The schema is versioned with `PRAGMA user_version`. To add tables in a later
//! milestone, append a step to [`MIGRATIONS`]; each step runs once, in order,
//! inside a transaction that also bumps `user_version`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

/// One schema upgrade: plain SQL, or Rust code for data conversions SQL
/// cannot do (hashing, compression).
enum Migration {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> rusqlite::Result<()>),
}

/// Migration `i` upgrades the schema from `user_version = i` to `i + 1`.
const MIGRATIONS: &[Migration] = &[
    // v1 (M5): persistent counters.
    Migration::Sql(
        "CREATE TABLE counters (
         id    TEXT PRIMARY KEY NOT NULL,
         count INTEGER NOT NULL DEFAULT 0
     );",
    ),
    // v2 (M6): Pixelfed adapter. Single tenant: one account per instance; any
    // valid pod session may use it. Sessions store SHA-256(token), never the
    // token. pixelfed_media maps opaque proxy keys to upstream media URLs.
    Migration::Sql(
        "CREATE TABLE pixelfed_apps (
         instance      TEXT NOT NULL,
         redirect_uri  TEXT NOT NULL,
         client_id     TEXT NOT NULL,
         client_secret TEXT NOT NULL,
         PRIMARY KEY (instance, redirect_uri)
     );
     CREATE TABLE pixelfed_account (
         instance     TEXT PRIMARY KEY NOT NULL,
         access_token TEXT NOT NULL,
         account_id   TEXT NOT NULL,
         username     TEXT NOT NULL,
         created_at   INTEGER NOT NULL
     );
     CREATE TABLE sessions (
         token_hash TEXT PRIMARY KEY NOT NULL,
         created_at INTEGER NOT NULL
     );
     CREATE TABLE pixelfed_media (
         key TEXT PRIMARY KEY NOT NULL,
         url TEXT NOT NULL
     );",
    ),
    // v3 (M6, real Pixelfed): the refresh token Passport issues with the access
    // token, its expiry, and the redirect_uri whose app (client credentials)
    // issued it, so the pod can refresh on an upstream 401.
    Migration::Sql(
        "ALTER TABLE pixelfed_account ADD COLUMN refresh_token TEXT;
     ALTER TABLE pixelfed_account ADD COLUMN expires_at INTEGER;
     ALTER TABLE pixelfed_account ADD COLUMN redirect_uri TEXT;",
    ),
    // v4 (M6 review): the pinned owner, the upstream account id that may sign
    // in (D17). An existing stored account becomes the owner.
    Migration::Sql(
        "CREATE TABLE pixelfed_owner (
         instance   TEXT PRIMARY KEY NOT NULL,
         account_id TEXT NOT NULL,
         created_at INTEGER NOT NULL
     );
     INSERT INTO pixelfed_owner (instance, account_id, created_at)
         SELECT instance, account_id, created_at FROM pixelfed_account;",
    ),
    // v5 (M7): versioned workspace layouts (D22). Append-only history; `json`
    // is the workspace root UiNode. Version 1 (the built-in layout) is
    // inserted lazily by `layouts::latest`.
    Migration::Sql(
        "CREATE TABLE layout_versions (
         id           INTEGER PRIMARY KEY,
         workspace_id TEXT NOT NULL,
         version      INTEGER NOT NULL,
         json         TEXT NOT NULL,
         prompt       TEXT,
         provider     TEXT,
         patch_json   TEXT,
         created_at   INTEGER NOT NULL,
         UNIQUE (workspace_id, version)
     );",
    ),
    // v6: layout catalog (D26). Several layouts per workspace, each with its
    // own append-only history; specs are content-addressed (zstd) blobs; one
    // workspace revision log whose latest row is the active layout. The v5
    // rows become the history of each workspace's default built-in layout.
    Migration::Rust(migrate_v6),
];

/// v6 schema. `layout_versions` (v5) is renamed, converted and dropped.
const V6_SCHEMA: &str = "
    CREATE TABLE layout_blobs (
        hash     TEXT PRIMARY KEY NOT NULL,
        encoding TEXT NOT NULL,
        raw_len  INTEGER NOT NULL,
        data     BLOB NOT NULL
    );
    CREATE TABLE layouts (
        id           TEXT PRIMARY KEY NOT NULL,
        workspace_id TEXT NOT NULL,
        title        TEXT NOT NULL,
        description  TEXT NOT NULL DEFAULT '',
        preview_hint TEXT NOT NULL DEFAULT '',
        builtin      INTEGER NOT NULL DEFAULT 0,
        created_at   INTEGER NOT NULL
    );
    CREATE INDEX layouts_by_workspace ON layouts (workspace_id);
    ALTER TABLE layout_versions RENAME TO layout_versions_v5;
    CREATE TABLE layout_versions (
        layout_id  TEXT NOT NULL,
        version    INTEGER NOT NULL,
        hash       TEXT NOT NULL,
        source     TEXT NOT NULL,
        prompt     TEXT,
        provider   TEXT,
        patch_json TEXT,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (layout_id, version)
    ) WITHOUT ROWID;
    CREATE TABLE workspace_revisions (
        workspace_id   TEXT NOT NULL,
        revision       INTEGER NOT NULL,
        layout_id      TEXT NOT NULL,
        layout_version INTEGER NOT NULL,
        reason         TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        PRIMARY KEY (workspace_id, revision)
    ) WITHOUT ROWID;";

/// The default built-in layout of a workspace (where v5 history goes).
pub fn default_layout_id(workspace_id: &str) -> String {
    match workspace_id {
        "home" => "home.counter".to_owned(),
        ws => format!("{ws}.classic"),
    }
}

/// v5 -> v6. Each v5 row (workspace W, version N) becomes version N of
/// `default_layout_id(W)` with the same prompt, provider, patch and time, and
/// revision N of W, so the workspace revision continues at the old version
/// number (a connected client's version never goes backwards). The layout
/// rows get their titles when the built-ins are seeded.
fn migrate_v6(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(V6_SCHEMA)?;
    type V5Row = (
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let rows: Vec<V5Row> = {
        let mut stmt = tx.prepare(
            "SELECT workspace_id, version, json, prompt, provider, patch_json, created_at
             FROM layout_versions_v5 ORDER BY workspace_id, version",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (ws, version, json, prompt, provider, patch_json, created_at) in rows {
        let layout_id = default_layout_id(&ws);
        tx.execute(
            "INSERT OR IGNORE INTO layouts (id, workspace_id, title, builtin, created_at)
             VALUES (?1, ?2, ?1, 1, ?3)",
            params![layout_id, ws, created_at],
        )?;
        // Canonical form (what new versions hash), so identical layouts dedupe.
        let json = canonical_json(&json);
        let hash = put_blob(tx, &json)?;
        let source = match (provider.as_deref(), prompt.as_deref()) {
            (Some(ROLLBACK), _) => SOURCE_ROLLBACK,
            (None, None) if version == 1 => SOURCE_SEED,
            (None, None) => SOURCE_MANUAL,
            _ => SOURCE_AGENT,
        };
        tx.execute(
            "INSERT INTO layout_versions
                 (layout_id, version, hash, source, prompt, provider, patch_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                layout_id, version, hash, source, prompt, provider, patch_json, created_at
            ],
        )?;
        tx.execute(
            "INSERT INTO workspace_revisions
                 (workspace_id, revision, layout_id, layout_version, reason, created_at)
             VALUES (?1, ?2, ?3, ?2, ?4, ?5)",
            params![ws, version, layout_id, source, created_at],
        )?;
    }
    tx.execute_batch("DROP TABLE layout_versions_v5;")
}

/// Re-serialize a stored layout through [`protocol::UiNode`] (the canonical
/// form every new version uses); text that does not parse is kept as is.
fn canonical_json(json: &str) -> String {
    serde_json::from_str::<protocol::UiNode>(json)
        .ok()
        .and_then(|n| serde_json::to_string(&n).ok())
        .unwrap_or_else(|| json.to_owned())
}

/// `source` of a version.
pub const SOURCE_SEED: &str = "seed";
pub const SOURCE_AGENT: &str = "agent";
pub const SOURCE_ROLLBACK: &str = "rollback";
pub const SOURCE_RESTORE: &str = "restore";
pub const SOURCE_MANUAL: &str = "manual";
/// `reason` of a revision that switched the active layout.
pub const REASON_ACTIVATE: &str = "activate";
/// The provider recorded on rollbacks (v5 and `POST .../rollback`).
const ROLLBACK: &str = "rollback";

/// zstd level for layout blobs (see D26 for the measurements).
pub const BLOB_ZSTD_LEVEL: i32 = 19;

/// SHA-256 hex of a layout's canonical JSON (its blob key).
pub fn content_hash(json: &str) -> String {
    crate::pixelfed::hex(&Sha256::digest(json.as_bytes()))
}

/// `(encoding, data)`: zstd when it is smaller, else the raw bytes.
pub fn encode_blob(json: &str) -> (&'static str, Vec<u8>) {
    match zstd::bulk::compress(json.as_bytes(), BLOB_ZSTD_LEVEL) {
        Ok(z) if z.len() < json.len() => ("zstd", z),
        _ => ("raw", json.as_bytes().to_vec()),
    }
}

fn decode_blob(encoding: &str, data: Vec<u8>, raw_len: i64) -> rusqlite::Result<String> {
    let bytes = match encoding {
        "raw" => data,
        "zstd" => zstd::bulk::decompress(&data, raw_len.max(0) as usize)
            .map_err(|e| blob_error(Box::new(e)))?,
        other => return Err(blob_error(format!("unknown blob encoding {other}").into())),
    };
    String::from_utf8(bytes).map_err(|e| blob_error(Box::new(e)))
}

fn blob_error(e: Box<dyn std::error::Error + Send + Sync>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, e)
}

/// Store `json` once (by content hash) and return the hash. A blob that
/// already exists (a restore, a re-seed) costs nothing.
fn put_blob(conn: &Connection, json: &str) -> rusqlite::Result<String> {
    let hash = content_hash(json);
    let exists = conn
        .query_row(
            "SELECT 1 FROM layout_blobs WHERE hash = ?1",
            [&hash],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        let (encoding, data) = encode_blob(json);
        conn.execute(
            "INSERT INTO layout_blobs (hash, encoding, raw_len, data) VALUES (?1, ?2, ?3, ?4)",
            params![hash, encoding, json.len() as i64, data],
        )?;
    }
    Ok(hash)
}

fn get_blob(conn: &Connection, hash: &str) -> rusqlite::Result<String> {
    let (encoding, data, raw_len): (String, Vec<u8>, i64) = conn.query_row(
        "SELECT encoding, data, raw_len FROM layout_blobs WHERE hash = ?1",
        [hash],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    decode_blob(&encoding, data, raw_len)
}

/// One row of [`Store::layout_versions`] (metadata only, no layout JSON):
/// the history of a workspace's active layout (the pre-catalog shape).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayoutVersionInfo {
    pub version: i64,
    pub created_at: i64,
    pub prompt: Option<String>,
    pub provider: Option<String>,
}

/// The workspace's latest revision: which layout (and which of its
/// versions) is on display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveLayout {
    pub workspace_id: String,
    /// Monotonic per workspace; bumps on every visible change.
    pub revision: i64,
    pub layout_id: String,
    pub layout_version: i64,
}

/// A catalog entry with its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutRow {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub description: String,
    pub preview_hint: String,
    pub builtin: bool,
    pub created_at: i64,
    /// Newest version (0 for a layout without versions, which seeding never leaves).
    pub current_version: i64,
    /// `created_at` of the newest version.
    pub updated_at: i64,
}

/// One version of a layout (metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRow {
    pub version: i64,
    pub created_at: i64,
    pub source: String,
    pub prompt: Option<String>,
    pub provider: Option<String>,
    pub hash: String,
    /// Uncompressed JSON size.
    pub bytes: i64,
}

/// A built-in layout to seed (`json` must be canonical: `to_json(UiNode)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedLayout {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub description: String,
    pub preview_hint: String,
    pub json: String,
}

/// Outcome of [`Store::restore_version`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    LayoutNotFound,
    VersionNotFound,
    /// The version's content equals the latest one; nothing appended.
    Unchanged {
        version: i64,
    },
    Appended {
        version: i64,
        json: String,
        /// The new revision, when the layout is the active one.
        active: Option<ActiveLayout>,
    },
}

/// Outcome of [`Store::activate_layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activated {
    LayoutNotFound,
    VersionNotFound,
    Done {
        active: ActiveLayout,
        /// The version a `version` argument was restored into, if it appended one.
        restored: Option<i64>,
        /// Already active at that content: no new revision.
        unchanged: bool,
    },
}

/// Metadata stored with a new layout version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutMeta {
    pub prompt: Option<String>,
    pub provider: Option<String>,
    /// The RFC 6902 patch that produced the layout, as JSON text.
    pub patch_json: Option<String>,
}

/// Outcome of [`Store::sign_in_pixelfed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// Account stored and session created.
    Ok,
    /// Another account owns this pod; nothing was changed.
    NotOwner { owner: String },
}

/// A registered OAuth app on the upstream instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedApp {
    pub client_id: String,
    pub client_secret: String,
}

/// The stored upstream account (and its access token).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PixelfedAccount {
    pub access_token: String,
    pub account_id: String,
    pub username: String,
    /// Upstream refresh token (Pixelfed issues one; the fake too).
    pub refresh_token: Option<String>,
    /// Unix seconds when the access token expires, if the upstream said.
    pub expires_at: Option<i64>,
    /// The redirect_uri of the app (pixelfed_apps row) that issued the token.
    pub redirect_uri: Option<String>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn save_account_on(
    conn: &Connection,
    instance: &str,
    account: &PixelfedAccount,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pixelfed_account
             (instance, access_token, account_id, username, created_at,
              refresh_token, expires_at, redirect_uri)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            instance,
            account.access_token,
            account.account_id,
            account.username,
            now_secs(),
            account.refresh_token,
            account.expires_at,
            account.redirect_uri,
        ],
    )?;
    Ok(())
}

/// Append `max + 1` to a layout's history (blob stored once by hash).
fn append_version_on(
    conn: &Connection,
    layout_id: &str,
    json: &str,
    source: &str,
    meta: &LayoutMeta,
) -> rusqlite::Result<i64> {
    let hash = put_blob(conn, json)?;
    append_hash_on(conn, layout_id, &hash, source, meta)
}

fn append_hash_on(
    conn: &Connection,
    layout_id: &str,
    hash: &str,
    source: &str,
    meta: &LayoutMeta,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "INSERT INTO layout_versions
             (layout_id, version, hash, source, prompt, provider, patch_json, created_at)
         SELECT ?1, COALESCE(MAX(version), 0) + 1, ?2, ?3, ?4, ?5, ?6, ?7
         FROM layout_versions WHERE layout_id = ?1
         RETURNING version",
        params![
            layout_id,
            hash,
            source,
            meta.prompt,
            meta.provider,
            meta.patch_json,
            now_secs()
        ],
        |r| r.get(0),
    )
}

/// Append the workspace's next revision.
fn append_revision_on(
    conn: &Connection,
    workspace_id: &str,
    layout_id: &str,
    layout_version: i64,
    reason: &str,
) -> rusqlite::Result<ActiveLayout> {
    let revision = conn.query_row(
        "INSERT INTO workspace_revisions
             (workspace_id, revision, layout_id, layout_version, reason, created_at)
         SELECT ?1, COALESCE(MAX(revision), 0) + 1, ?2, ?3, ?4, ?5
         FROM workspace_revisions WHERE workspace_id = ?1
         RETURNING revision",
        params![workspace_id, layout_id, layout_version, reason, now_secs()],
        |r| r.get(0),
    )?;
    Ok(ActiveLayout {
        workspace_id: workspace_id.to_owned(),
        revision,
        layout_id: layout_id.to_owned(),
        layout_version,
    })
}

fn active_on(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Option<ActiveLayout>> {
    conn.query_row(
        "SELECT revision, layout_id, layout_version FROM workspace_revisions
         WHERE workspace_id = ?1 ORDER BY revision DESC LIMIT 1",
        [workspace_id],
        |r| {
            Ok(ActiveLayout {
                workspace_id: workspace_id.to_owned(),
                revision: r.get(0)?,
                layout_id: r.get(1)?,
                layout_version: r.get(2)?,
            })
        },
    )
    .optional()
}

fn layout_workspace_on(conn: &Connection, layout_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT workspace_id FROM layouts WHERE id = ?1",
        [layout_id],
        |r| r.get(0),
    )
    .optional()
}

/// `(version, hash)` of a layout's newest version.
fn latest_version_on(
    conn: &Connection,
    layout_id: &str,
) -> rusqlite::Result<Option<(i64, String)>> {
    conn.query_row(
        "SELECT version, hash FROM layout_versions WHERE layout_id = ?1
         ORDER BY version DESC LIMIT 1",
        [layout_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

fn version_hash_on(
    conn: &Connection,
    layout_id: &str,
    version: i64,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT hash FROM layout_versions WHERE layout_id = ?1 AND version = ?2",
        params![layout_id, version],
        |r| r.get(0),
    )
    .optional()
}

/// Copy version `from` of `layout_id` into a new version (and a new
/// revision when the layout is active). With `skip_if_same`, a copy whose
/// content equals the newest version is not appended.
fn restore_on(
    conn: &Connection,
    layout_id: &str,
    from: i64,
    source: &str,
    meta: &LayoutMeta,
    skip_if_same: bool,
) -> rusqlite::Result<Restored> {
    let Some(ws) = layout_workspace_on(conn, layout_id)? else {
        return Ok(Restored::LayoutNotFound);
    };
    let Some(hash) = version_hash_on(conn, layout_id, from)? else {
        return Ok(Restored::VersionNotFound);
    };
    if skip_if_same
        && let Some((latest, latest_hash)) = latest_version_on(conn, layout_id)?
        && latest_hash == hash
    {
        return Ok(Restored::Unchanged { version: latest });
    }
    let version = append_hash_on(conn, layout_id, &hash, source, meta)?;
    let active = match active_on(conn, &ws)? {
        Some(a) if a.layout_id == layout_id => {
            Some(append_revision_on(conn, &ws, layout_id, version, source)?)
        }
        _ => None,
    };
    Ok(Restored::Appended {
        version,
        json: get_blob(conn, &hash)?,
        active,
    })
}

fn version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VersionRow> {
    Ok(VersionRow {
        version: r.get(0)?,
        created_at: r.get(1)?,
        source: r.get(2)?,
        prompt: r.get(3)?,
        provider: r.get(4)?,
        hash: r.get(5)?,
        bytes: r.get(6)?,
    })
}

const LAYOUT_ROW_SQL: &str = "
    SELECT l.id, l.workspace_id, l.title, l.description, l.preview_hint, l.builtin, l.created_at,
           COALESCE(v.version, 0), COALESCE(v.created_at, l.created_at)
    FROM layouts l
    LEFT JOIN layout_versions v ON v.layout_id = l.id
        AND v.version = (SELECT MAX(version) FROM layout_versions WHERE layout_id = l.id)";

fn layout_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LayoutRow> {
    Ok(LayoutRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        preview_hint: r.get(4)?,
        builtin: r.get(5)?,
        created_at: r.get(6)?,
        current_version: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// Run migrations up to `user_version = target`.
fn migrate(conn: &mut Connection, target: usize) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, step) in MIGRATIONS
        .iter()
        .enumerate()
        .take(target)
        .skip(version.max(0) as usize)
    {
        let tx = conn.transaction()?;
        match step {
            Migration::Sql(sql) => tx.execute_batch(sql)?,
            Migration::Rust(f) => f(&tx)?,
        }
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// Result of a decrement that would take the count below zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BelowZero {
    /// The unchanged current count.
    pub count: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    /// Set once the built-in layouts were seeded on this connection.
    catalog_seeded: AtomicBool,
}

impl Store {
    /// Open (creating if needed) the database file and run migrations.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    /// A private in-memory database (tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> rusqlite::Result<Self> {
        migrate(&mut conn, MIGRATIONS.len())?;
        Ok(Self {
            conn: Mutex::new(conn),
            catalog_seeded: AtomicBool::new(false),
        })
    }

    /// Whether [`Store::seed_catalog`] already ran on this store.
    pub fn catalog_seeded(&self) -> bool {
        self.catalog_seeded.load(Ordering::Acquire)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (statements are atomic), so recover from poisoning.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current count (0 if the counter has never been written).
    pub fn counter(&self, id: &str) -> rusqlite::Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT count FROM counters WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0))
    }

    pub fn increment(&self, id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "INSERT INTO counters (id, count) VALUES (?1, 1)
             ON CONFLICT(id) DO UPDATE SET count = count + 1
             RETURNING count",
            [id],
            |r| r.get(0),
        )
    }

    /// Decrement, refusing (and changing nothing) if the result would be < 0.
    pub fn decrement(&self, id: &str) -> rusqlite::Result<Result<i64, BelowZero>> {
        let conn = self.conn();
        let updated: Option<i64> = conn
            .query_row(
                "UPDATE counters SET count = count - 1 WHERE id = ?1 AND count > 0
                 RETURNING count",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        match updated {
            Some(n) => Ok(Ok(n)),
            None => {
                let count = conn
                    .query_row("SELECT count FROM counters WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .unwrap_or(0);
                Ok(Err(BelowZero { count }))
            }
        }
    }

    pub fn reset(&self, id: &str) -> rusqlite::Result<i64> {
        self.conn().execute(
            "INSERT INTO counters (id, count) VALUES (?1, 0)
             ON CONFLICT(id) DO UPDATE SET count = 0",
            params![id],
        )?;
        Ok(0)
    }

    // ---------- Pixelfed (v2) ----------

    pub fn pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
    ) -> rusqlite::Result<Option<PixelfedApp>> {
        self.conn()
            .query_row(
                "SELECT client_id, client_secret FROM pixelfed_apps
                 WHERE instance = ?1 AND redirect_uri = ?2",
                [instance, redirect_uri],
                |r| {
                    Ok(PixelfedApp {
                        client_id: r.get(0)?,
                        client_secret: r.get(1)?,
                    })
                },
            )
            .optional()
    }

    pub fn save_pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
        app: &PixelfedApp,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO pixelfed_apps (instance, redirect_uri, client_id, client_secret)
             VALUES (?1, ?2, ?3, ?4)",
            params![instance, redirect_uri, app.client_id, app.client_secret],
        )?;
        Ok(())
    }

    /// Forget a cached app (the upstream no longer knows it).
    pub fn delete_pixelfed_app(&self, instance: &str, redirect_uri: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM pixelfed_apps WHERE instance = ?1 AND redirect_uri = ?2",
            [instance, redirect_uri],
        )?;
        Ok(())
    }

    pub fn pixelfed_account(&self, instance: &str) -> rusqlite::Result<Option<PixelfedAccount>> {
        self.conn()
            .query_row(
                "SELECT access_token, account_id, username, refresh_token, expires_at, redirect_uri
                 FROM pixelfed_account WHERE instance = ?1",
                [instance],
                |r| {
                    Ok(PixelfedAccount {
                        access_token: r.get(0)?,
                        account_id: r.get(1)?,
                        username: r.get(2)?,
                        refresh_token: r.get(3)?,
                        expires_at: r.get(4)?,
                        redirect_uri: r.get(5)?,
                    })
                },
            )
            .optional()
    }

    /// Store (replacing any previous) the upstream account for `instance`.
    /// Does not touch the pinned owner (token refreshes use this).
    pub fn save_pixelfed_account(
        &self,
        instance: &str,
        account: &PixelfedAccount,
    ) -> rusqlite::Result<()> {
        save_account_on(&self.conn(), instance, account)
    }

    /// The pinned owner's upstream account id, if anyone has signed in.
    pub fn pixelfed_owner(&self, instance: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT account_id FROM pixelfed_owner WHERE instance = ?1",
                [instance],
                |r| r.get(0),
            )
            .optional()
    }

    /// Sign-in, atomically: if no owner is pinned (or `replace_owner`),
    /// `account` becomes the owner; if another account is pinned, nothing
    /// changes and [`SignIn::NotOwner`] is returned. Otherwise the account is
    /// stored, the session created, and sessions older than `session_ttl_secs`
    /// are pruned.
    pub fn sign_in_pixelfed(
        &self,
        instance: &str,
        account: &PixelfedAccount,
        token_hash: &str,
        replace_owner: bool,
        session_ttl_secs: i64,
    ) -> rusqlite::Result<SignIn> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT account_id FROM pixelfed_owner WHERE instance = ?1",
                [instance],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(owner) = owner
            && owner != account.account_id
            && !replace_owner
        {
            return Ok(SignIn::NotOwner { owner });
        }
        let now = now_secs();
        tx.execute(
            "INSERT INTO pixelfed_owner (instance, account_id, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(instance) DO UPDATE SET account_id = excluded.account_id,
                 created_at = CASE WHEN account_id = excluded.account_id THEN created_at
                                   ELSE excluded.created_at END",
            params![instance, account.account_id, now],
        )?;
        save_account_on(&tx, instance, account)?;
        tx.execute(
            "DELETE FROM sessions WHERE created_at <= ?1",
            [now.saturating_sub(session_ttl_secs)],
        )?;
        tx.execute(
            "INSERT INTO sessions (token_hash, created_at) VALUES (?1, ?2)",
            params![token_hash, now],
        )?;
        tx.commit()?;
        Ok(SignIn::Ok)
    }

    pub fn delete_pixelfed_account(&self, instance: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM pixelfed_account WHERE instance = ?1",
            [instance],
        )?;
        Ok(())
    }

    /// Compare-and-delete: forget the stored account only if it still holds
    /// `access_token` (a concurrent sign-in or refresh may have replaced it).
    /// Returns whether a row was deleted.
    pub fn delete_pixelfed_account_if_token(
        &self,
        instance: &str,
        access_token: &str,
    ) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "DELETE FROM pixelfed_account WHERE instance = ?1 AND access_token = ?2",
            [instance, access_token],
        )? > 0)
    }

    pub fn create_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (token_hash, created_at) VALUES (?1, ?2)",
            params![token_hash, now_secs()],
        )?;
        Ok(())
    }

    /// Whether the session exists and is younger than `ttl_secs`.
    pub fn session_valid(&self, token_hash: &str, ttl_secs: i64) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM sessions WHERE token_hash = ?1 AND created_at > ?2",
                params![token_hash, now_secs().saturating_sub(ttl_secs)],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Whether the session row exists (regardless of age).
    pub fn session_exists(&self, token_hash: &str) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM sessions WHERE token_hash = ?1",
                [token_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Returns whether a session was deleted.
    pub fn delete_session(&self, token_hash: &str) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?
            > 0)
    }

    /// Remember proxy keys (idempotent).
    pub fn save_media_keys<'a>(
        &self,
        keys: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO pixelfed_media (key, url) VALUES (?1, ?2)",
            )?;
            for (key, url) in keys {
                stmt.execute([key, url])?;
            }
        }
        tx.commit()
    }

    // ---------- Layout catalog (v6, D26) ----------

    /// Seed built-in layouts, idempotently, in one transaction: each seed's
    /// metadata is upserted and its version 1 (source `seed`) inserted only
    /// if it has no versions (history is never overwritten); a workspace
    /// without revisions gets revision 1 on its default layout (`defaults`:
    /// `(workspace_id, layout_id)`). Returns how many versions were inserted.
    pub fn seed_catalog(
        &self,
        seeds: &[SeedLayout],
        defaults: &[(&str, &str)],
    ) -> rusqlite::Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = now_secs();
        let mut inserted = 0;
        for seed in seeds {
            tx.execute(
                "INSERT INTO layouts (id, workspace_id, title, description, preview_hint, builtin, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title,
                     description = excluded.description, preview_hint = excluded.preview_hint,
                     builtin = 1",
                params![seed.id, seed.workspace_id, seed.title, seed.description, seed.preview_hint, now],
            )?;
            if latest_version_on(&tx, &seed.id)?.is_none() {
                append_version_on(
                    &tx,
                    &seed.id,
                    &seed.json,
                    SOURCE_SEED,
                    &LayoutMeta::default(),
                )?;
                inserted += 1;
            }
        }
        for (ws, layout_id) in defaults {
            if active_on(&tx, ws)?.is_none()
                && let Some((version, _)) = latest_version_on(&tx, layout_id)?
            {
                append_revision_on(&tx, ws, layout_id, version, SOURCE_SEED)?;
            }
        }
        tx.commit()?;
        self.catalog_seeded.store(true, Ordering::Release);
        Ok(inserted)
    }

    /// The workspace's latest revision (`None` before seeding).
    pub fn active(&self, workspace_id: &str) -> rusqlite::Result<Option<ActiveLayout>> {
        active_on(&self.conn(), workspace_id)
    }

    /// The active layout and its JSON (one consistent read).
    pub fn active_layout(
        &self,
        workspace_id: &str,
    ) -> rusqlite::Result<Option<(ActiveLayout, String)>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(active) = active_on(&tx, workspace_id)? else {
            return Ok(None);
        };
        let hash = version_hash_on(&tx, &active.layout_id, active.layout_version)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let json = get_blob(&tx, &hash)?;
        Ok(Some((active, json)))
    }

    /// Append `json` as the next version of the workspace's active layout and
    /// the next revision, atomically. With `expected = (layout_id,
    /// layout_version)`, only if that is still what is active (compare-and-
    /// append: `None` if another change or a switch happened meanwhile).
    /// `None` too if the workspace has no active layout.
    pub fn append_active(
        &self,
        workspace_id: &str,
        expected: Option<(&str, i64)>,
        json: &str,
        source: &str,
        meta: &LayoutMeta,
    ) -> rusqlite::Result<Option<ActiveLayout>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(active) = active_on(&tx, workspace_id)? else {
            return Ok(None);
        };
        if let Some((id, version)) = expected
            && (id != active.layout_id || version != active.layout_version)
        {
            return Ok(None);
        }
        let version = append_version_on(&tx, &active.layout_id, json, source, meta)?;
        let new = append_revision_on(&tx, workspace_id, &active.layout_id, version, source)?;
        tx.commit()?;
        Ok(Some(new))
    }

    /// Copy version `from` of a layout into a new version (append-only), plus
    /// a revision if the layout is active. With `skip_if_same`, restoring
    /// content equal to the newest version appends nothing.
    pub fn restore_version(
        &self,
        layout_id: &str,
        from: i64,
        source: &str,
        meta: &LayoutMeta,
        skip_if_same: bool,
    ) -> rusqlite::Result<Restored> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let out = restore_on(&tx, layout_id, from, source, meta, skip_if_same)?;
        tx.commit()?;
        Ok(out)
    }

    /// [`Store::restore_version`] on the workspace's active layout (always
    /// appends, like the pre-catalog rollback). `None` without an active layout.
    pub fn rollback_active(
        &self,
        workspace_id: &str,
        from: i64,
        meta: &LayoutMeta,
    ) -> rusqlite::Result<Option<Restored>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(active) = active_on(&tx, workspace_id)? else {
            return Ok(None);
        };
        let out = restore_on(&tx, &active.layout_id, from, SOURCE_ROLLBACK, meta, false)?;
        tx.commit()?;
        Ok(Some(out))
    }

    /// Make a layout its workspace's active one. With `version`, that version
    /// is first restored into the layout (append-only, skipped when equal to
    /// the newest). Already active at the same version -> `unchanged`.
    pub fn activate_layout(
        &self,
        layout_id: &str,
        version: Option<i64>,
    ) -> rusqlite::Result<Activated> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(ws) = layout_workspace_on(&tx, layout_id)? else {
            return Ok(Activated::LayoutNotFound);
        };
        let mut restored = None;
        if let Some(from) = version {
            let meta = LayoutMeta {
                prompt: Some(format!("restore v{from}")),
                ..Default::default()
            };
            match restore_on(&tx, layout_id, from, SOURCE_RESTORE, &meta, true)? {
                Restored::LayoutNotFound => return Ok(Activated::LayoutNotFound),
                Restored::VersionNotFound => return Ok(Activated::VersionNotFound),
                Restored::Unchanged { .. } => {}
                Restored::Appended {
                    version, active, ..
                } => {
                    restored = Some(version);
                    if let Some(active) = active {
                        // It was already active: the restore's revision is the switch.
                        tx.commit()?;
                        return Ok(Activated::Done {
                            active,
                            restored,
                            unchanged: false,
                        });
                    }
                }
            }
        }
        let Some((latest, _)) = latest_version_on(&tx, layout_id)? else {
            return Ok(Activated::VersionNotFound);
        };
        let (active, unchanged) = match active_on(&tx, &ws)? {
            Some(a) if a.layout_id == layout_id && a.layout_version == latest => (a, true),
            _ => (
                append_revision_on(&tx, &ws, layout_id, latest, REASON_ACTIVATE)?,
                false,
            ),
        };
        tx.commit()?;
        Ok(Activated::Done {
            active,
            restored,
            unchanged,
        })
    }

    /// Catalog entries (of one workspace, or all), by workspace then id.
    pub fn layouts(&self, workspace_id: Option<&str>) -> rusqlite::Result<Vec<LayoutRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "{LAYOUT_ROW_SQL} WHERE ?1 IS NULL OR l.workspace_id = ?1 ORDER BY l.workspace_id, l.id"
        ))?;
        stmt.query_map([workspace_id], layout_row)?.collect()
    }

    pub fn layout(&self, layout_id: &str) -> rusqlite::Result<Option<LayoutRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!("{LAYOUT_ROW_SQL} WHERE l.id = ?1"))?;
        stmt.query_row([layout_id], layout_row).optional()
    }

    /// Versions of a layout older than `before` (all if `None`), newest
    /// first, at most `limit`.
    pub fn layout_version_rows(
        &self,
        layout_id: &str,
        before: Option<i64>,
        limit: i64,
    ) -> rusqlite::Result<Vec<VersionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT v.version, v.created_at, v.source, v.prompt, v.provider, v.hash, b.raw_len
             FROM layout_versions v JOIN layout_blobs b ON b.hash = v.hash
             WHERE v.layout_id = ?1 AND (?2 IS NULL OR v.version < ?2)
             ORDER BY v.version DESC LIMIT ?3",
        )?;
        stmt.query_map(params![layout_id, before, limit], version_row)?
            .collect()
    }

    /// One version's metadata and JSON.
    pub fn layout_version(
        &self,
        layout_id: &str,
        version: i64,
    ) -> rusqlite::Result<Option<(VersionRow, String)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT v.version, v.created_at, v.source, v.prompt, v.provider, v.hash, b.raw_len
                 FROM layout_versions v JOIN layout_blobs b ON b.hash = v.hash
                 WHERE v.layout_id = ?1 AND v.version = ?2",
                params![layout_id, version],
                version_row,
            )
            .optional()?;
        match row {
            Some(row) => {
                let json = get_blob(&conn, &row.hash)?;
                Ok(Some((row, json)))
            }
            None => Ok(None),
        }
    }

    /// All versions of the workspace's active layout, newest first (the
    /// pre-catalog `GET /v1/workspaces/:id/versions` shape).
    pub fn layout_versions(&self, workspace_id: &str) -> rusqlite::Result<Vec<LayoutVersionInfo>> {
        let conn = self.conn();
        let Some(active) = active_on(&conn, workspace_id)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare_cached(
            "SELECT version, created_at, prompt, provider FROM layout_versions
             WHERE layout_id = ?1 ORDER BY version DESC",
        )?;
        stmt.query_map([&active.layout_id], |r| {
            Ok(LayoutVersionInfo {
                version: r.get(0)?,
                created_at: r.get(1)?,
                prompt: r.get(2)?,
                provider: r.get(3)?,
            })
        })?
        .collect()
    }

    /// Storage accounting: `(blobs, blob bytes stored, versions)`.
    pub fn layout_storage(&self) -> rusqlite::Result<(i64, i64, i64)> {
        self.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM layout_blobs),
                    (SELECT COALESCE(SUM(LENGTH(data)), 0) FROM layout_blobs),
                    (SELECT COUNT(*) FROM layout_versions)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    pub fn media_url(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT url FROM pixelfed_media WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent_and_versioned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.increment("home").unwrap(), 1);
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = s
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        assert_eq!(s.counter("home").unwrap(), 1);
    }

    fn account(id: &str, token: &str) -> PixelfedAccount {
        PixelfedAccount {
            access_token: token.into(),
            account_id: id.into(),
            username: format!("u{id}"),
            ..Default::default()
        }
    }

    #[test]
    fn v4_pins_an_existing_account_as_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            // A v3 database with a stored account.
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, 3).unwrap();
            save_account_on(&conn, "i", &account("7", "t")).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.pixelfed_owner("i").unwrap().as_deref(), Some("7"));
    }

    #[test]
    fn sign_in_pins_the_first_owner() {
        let s = Store::open_in_memory().unwrap();
        let ttl = 86_400;
        assert_eq!(
            s.sign_in_pixelfed("i", &account("1", "a"), "h1", false, ttl)
                .unwrap(),
            SignIn::Ok
        );
        assert_eq!(
            s.sign_in_pixelfed("i", &account("2", "b"), "h2", false, ttl)
                .unwrap(),
            SignIn::NotOwner { owner: "1".into() }
        );
        // Nothing changed: account, owner, no session for the rejected sign-in.
        assert_eq!(s.pixelfed_account("i").unwrap().unwrap().access_token, "a");
        assert!(!s.session_exists("h2").unwrap());
        // Same account again; then a forced replacement.
        assert_eq!(
            s.sign_in_pixelfed("i", &account("1", "c"), "h3", false, ttl)
                .unwrap(),
            SignIn::Ok
        );
        assert_eq!(
            s.sign_in_pixelfed("i", &account("2", "d"), "h4", true, ttl)
                .unwrap(),
            SignIn::Ok
        );
        assert_eq!(s.pixelfed_owner("i").unwrap().as_deref(), Some("2"));
        // Owners are per instance.
        assert_eq!(s.pixelfed_owner("other").unwrap(), None);
    }

    #[test]
    fn session_ttl_and_pruning() {
        let s = Store::open_in_memory().unwrap();
        s.create_session("old").unwrap();
        s.conn()
            .execute("UPDATE sessions SET created_at = created_at - 1000", [])
            .unwrap();
        assert!(s.session_valid("old", 2000).unwrap());
        assert!(!s.session_valid("old", 999).unwrap());
        assert!(!s.session_valid("missing", 2000).unwrap());
        // A sign-in prunes sessions past the TTL.
        s.sign_in_pixelfed("i", &account("1", "a"), "new", false, 999)
            .unwrap();
        assert!(!s.session_exists("old").unwrap());
        assert!(s.session_valid("new", 999).unwrap());
    }

    #[test]
    fn account_compare_and_delete() {
        let s = Store::open_in_memory().unwrap();
        s.save_pixelfed_account("i", &account("1", "new")).unwrap();
        assert!(!s.delete_pixelfed_account_if_token("i", "old").unwrap());
        assert!(s.pixelfed_account("i").unwrap().is_some());
        assert!(s.delete_pixelfed_account_if_token("i", "new").unwrap());
        assert!(s.pixelfed_account("i").unwrap().is_none());
    }

    fn seed(id: &str, ws: &str, json: &str) -> SeedLayout {
        SeedLayout {
            id: id.into(),
            workspace_id: ws.into(),
            title: format!("T {id}"),
            description: String::new(),
            preview_hint: "hint".into(),
            json: json.into(),
        }
    }

    fn meta(prompt: &str) -> LayoutMeta {
        LayoutMeta {
            prompt: Some(prompt.into()),
            provider: Some("rules".into()),
            patch_json: Some("[]".into()),
        }
    }

    fn catalog() -> Store {
        let s = Store::open_in_memory().unwrap();
        let seeds = [
            seed("a.one", "a", "{\"v\":1}"),
            seed("a.two", "a", "{\"w\":1}"),
        ];
        assert_eq!(s.seed_catalog(&seeds, &[("a", "a.one")]).unwrap(), 2);
        s
    }

    fn blobs(s: &Store) -> i64 {
        s.layout_storage().unwrap().0
    }

    #[test]
    fn seeding_is_idempotent_and_keeps_history_and_the_active_layout() {
        let s = catalog();
        let a = s.active("a").unwrap().unwrap();
        assert_eq!(
            (a.revision, a.layout_id.as_str(), a.layout_version),
            (1, "a.one", 1)
        );
        s.append_active("a", None, "{\"v\":2}", SOURCE_AGENT, &meta("p"))
            .unwrap()
            .unwrap();
        // Re-seed with new metadata, changed seed content and a new built-in.
        let mut one = seed("a.one", "a", "{\"v\":\"changed\"}");
        one.title = "Renamed".into();
        let seeds = [
            one,
            seed("a.two", "a", "{}"),
            seed("a.three", "a", "{\"x\":1}"),
        ];
        assert_eq!(s.seed_catalog(&seeds, &[("a", "a.three")]).unwrap(), 1);
        let a = s.active("a").unwrap().unwrap();
        assert_eq!(
            (a.revision, a.layout_id.as_str(), a.layout_version),
            (2, "a.one", 2)
        );
        let row = s.layout("a.one").unwrap().unwrap();
        assert_eq!((row.title.as_str(), row.current_version), ("Renamed", 2));
        assert_eq!(s.active_layout("a").unwrap().unwrap().1, "{\"v\":2}");
        let ids: Vec<String> = s
            .layouts(Some("a"))
            .unwrap()
            .into_iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(ids, ["a.one", "a.three", "a.two"]);
        assert!(s.layouts(Some("b")).unwrap().is_empty());
        assert_eq!(s.layouts(None).unwrap().len(), 3);
    }

    #[test]
    fn compare_and_append_detects_edits_and_switches() {
        let s = catalog();
        assert_eq!(
            s.append_active(
                "a",
                Some(("a.one", 1)),
                "{\"v\":2}",
                SOURCE_AGENT,
                &meta("p")
            )
            .unwrap()
            .map(|a| (a.revision, a.layout_version)),
            Some((2, 2))
        );
        // Stale base version.
        assert_eq!(
            s.append_active("a", Some(("a.one", 1)), "{}", SOURCE_AGENT, &meta("p"))
                .unwrap(),
            None
        );
        // Switch the active layout: an edit made for a.one no longer applies.
        let Activated::Done {
            active, unchanged, ..
        } = s.activate_layout("a.two", None).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (active.revision, active.layout_id.as_str(), unchanged),
            (3, "a.two", false)
        );
        assert_eq!(
            s.append_active("a", Some(("a.one", 2)), "{}", SOURCE_AGENT, &meta("p"))
                .unwrap(),
            None
        );
        let a = s
            .append_active(
                "a",
                Some(("a.two", 1)),
                "{\"w\":2}",
                SOURCE_AGENT,
                &meta("p"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (a.revision, a.layout_id.as_str(), a.layout_version),
            (4, "a.two", 2)
        );
        // a.one is untouched by edits to a.two.
        assert_eq!(s.layout("a.one").unwrap().unwrap().current_version, 2);
        assert_eq!(
            s.append_active("none", None, "{}", SOURCE_AGENT, &meta("p"))
                .unwrap(),
            None
        );
    }

    #[test]
    fn restore_dedupes_blobs_and_bumps_the_revision_only_when_active() {
        let s = catalog();
        s.append_active("a", None, "{\"v\":2}", SOURCE_AGENT, &meta("p"))
            .unwrap();
        let before = blobs(&s);
        let m = LayoutMeta {
            prompt: Some("restore v1".into()),
            ..Default::default()
        };
        let Restored::Appended {
            version,
            json,
            active,
        } = s
            .restore_version("a.one", 1, SOURCE_RESTORE, &m, true)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!((version, json.as_str()), (3, "{\"v\":1}"));
        assert_eq!(active.unwrap().revision, 3);
        assert_eq!(blobs(&s), before, "a restore stores no new blob");
        // Restoring content equal to the latest is a no-op.
        assert_eq!(
            s.restore_version("a.one", 1, SOURCE_RESTORE, &m, true)
                .unwrap(),
            Restored::Unchanged { version: 3 }
        );
        // An inactive layout: new version, no revision.
        s.restore_version("a.two", 1, SOURCE_RESTORE, &m, false)
            .unwrap();
        assert_eq!(s.active("a").unwrap().unwrap().revision, 3);
        assert_eq!(s.layout("a.two").unwrap().unwrap().current_version, 2);
        assert_eq!(
            s.restore_version("a.one", 9, SOURCE_RESTORE, &m, true)
                .unwrap(),
            Restored::VersionNotFound
        );
        assert_eq!(
            s.restore_version("x", 1, SOURCE_RESTORE, &m, true).unwrap(),
            Restored::LayoutNotFound
        );
        let rows = s.layout_version_rows("a.one", None, 10).unwrap();
        let v: Vec<(i64, &str)> = rows
            .iter()
            .map(|r| (r.version, r.source.as_str()))
            .collect();
        assert_eq!(v, [(3, "restore"), (2, "agent"), (1, "seed")]);
        assert_eq!(rows[0].hash, rows[2].hash);
        assert_eq!(rows[0].bytes, 7);
        let page: Vec<i64> = s
            .layout_version_rows("a.one", Some(3), 1)
            .unwrap()
            .iter()
            .map(|r| r.version)
            .collect();
        assert_eq!(page, [2]);
        // Legacy per-workspace list = the active layout's history.
        assert_eq!(s.layout_versions("a").unwrap().len(), 3);
    }

    #[test]
    fn activate_with_a_version_restores_first() {
        let s = catalog();
        s.restore_version("a.two", 1, SOURCE_RESTORE, &LayoutMeta::default(), false)
            .unwrap();
        // Same content as the latest: no restore, just the switch.
        let Activated::Done {
            active,
            restored,
            unchanged,
        } = s.activate_layout("a.two", Some(1)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (active.revision, active.layout_version, restored, unchanged),
            (2, 2, None, false)
        );
        // Again: already active.
        let Activated::Done {
            active, unchanged, ..
        } = s.activate_layout("a.two", None).unwrap()
        else {
            panic!()
        };
        assert_eq!((active.revision, unchanged), (2, true));
        // Back to a.one at an older version: restore (no revision: inactive), then switch.
        s.append_active("a", None, "{\"w\":9}", SOURCE_AGENT, &meta("p"))
            .unwrap();
        s.activate_layout("a.one", None).unwrap();
        s.append_active("a", None, "{\"v\":2}", SOURCE_AGENT, &meta("p"))
            .unwrap();
        let Activated::Done {
            active,
            restored,
            unchanged,
        } = s.activate_layout("a.two", Some(1)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (
                active.layout_id.as_str(),
                active.layout_version,
                restored,
                unchanged
            ),
            ("a.two", 4, Some(4), false)
        );
        // Active layout restored to an older version: the restore's revision is the change.
        let rev = active.revision;
        let Activated::Done {
            active, restored, ..
        } = s.activate_layout("a.two", Some(3)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (active.revision, active.layout_version, restored),
            (rev + 1, 5, Some(5))
        );
        assert_eq!(
            s.activate_layout("a.two", Some(99)).unwrap(),
            Activated::VersionNotFound
        );
        assert_eq!(
            s.activate_layout("zz", None).unwrap(),
            Activated::LayoutNotFound
        );
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn v6_migrates_v5_history_into_the_default_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        let classic = serde_json::to_string(&protocol::UiNode::Text {
            value_expr: "x".into(),
            style: protocol::TextStyle::Body,
        })
        .unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, 5).unwrap();
            let rows: [(&str, i64, &str, Option<&str>, Option<&str>, Option<&str>); 5] = [
                ("home", 1, classic.as_str(), None, None, None),
                (
                    "home",
                    2,
                    "{\"edited\":true}",
                    Some("hide captions"),
                    Some("rules"),
                    Some("[{\"op\":\"x\"}]"),
                ),
                (
                    "home",
                    3,
                    classic.as_str(),
                    Some("rollback to v1"),
                    Some("rollback"),
                    None,
                ),
                ("pixelfed_home", 1, classic.as_str(), None, None, None),
                (
                    "pixelfed_home",
                    2,
                    "{\"b\":1}",
                    Some("p"),
                    Some("anthropic"),
                    Some("[]"),
                ),
            ];
            for (ws, v, json, prompt, provider, patch) in rows {
                conn.execute(
                    "INSERT INTO layout_versions
                         (workspace_id, version, json, prompt, provider, patch_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![ws, v, json, prompt, provider, patch, 1000 + v],
                )
                .unwrap();
            }
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = s
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 6);
        let a = s.active("home").unwrap().unwrap();
        assert_eq!(
            (a.revision, a.layout_id.as_str(), a.layout_version),
            (3, "home.counter", 3)
        );
        let rows = s.layout_version_rows("home.counter", None, 10).unwrap();
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.version,
                    r.source.as_str(),
                    r.prompt.as_deref(),
                    r.provider.as_deref(),
                    r.created_at,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    3,
                    "rollback",
                    Some("rollback to v1"),
                    Some("rollback"),
                    1003
                ),
                (2, "agent", Some("hide captions"), Some("rules"), 1002),
                (1, "seed", None, None, 1001),
            ]
        );
        assert_eq!(
            rows[0].hash, rows[2].hash,
            "identical layouts share one blob"
        );
        assert_eq!(
            s.layout_version("home.counter", 2).unwrap().unwrap().1,
            "{\"edited\":true}"
        );
        let patch: Option<String> = s
            .conn()
            .query_row(
                "SELECT patch_json FROM layout_versions WHERE layout_id = 'home.counter' AND version = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(patch.as_deref(), Some("[{\"op\":\"x\"}]"));
        assert_eq!(
            s.active_layout("pixelfed_home").unwrap().unwrap().1,
            "{\"b\":1}"
        );
        assert_eq!(s.active("pixelfed_home").unwrap().unwrap().revision, 2);
        assert_eq!(s.active("pixelfed_demo").unwrap(), None);
        // 3 distinct blobs for 5 rows.
        assert_eq!(
            s.layout_storage().unwrap(),
            (3, s.layout_storage().unwrap().1, 5)
        );
        // Seeding afterwards adds new built-ins without touching history or the active layout.
        let seeds = [
            seed("home.counter", "home", "{\"new\":1}"),
            seed("home.big_counter", "home", "{\"big\":1}"),
            seed("pixelfed_demo.classic", "pixelfed_demo", "{\"d\":1}"),
        ];
        let defaults = [
            ("home", "home.counter"),
            ("pixelfed_demo", "pixelfed_demo.classic"),
        ];
        assert_eq!(s.seed_catalog(&seeds, &defaults).unwrap(), 2);
        let a = s.active("home").unwrap().unwrap();
        assert_eq!(
            (a.revision, a.layout_id.as_str(), a.layout_version),
            (3, "home.counter", 3)
        );
        assert_eq!(
            s.layout("home.counter").unwrap().unwrap().title,
            "T home.counter"
        );
        assert_eq!(s.active("pixelfed_demo").unwrap().unwrap().revision, 1);
    }

    #[test]
    fn blobs_compress_and_round_trip() {
        let big = format!("{{\"k\":\"{}\"}}", "abc".repeat(500));
        let (enc, data) = encode_blob(&big);
        assert_eq!(enc, "zstd");
        assert!(data.len() < big.len() / 10);
        assert_eq!(decode_blob(enc, data, big.len() as i64).unwrap(), big);
        // Tiny inputs that zstd cannot shrink are stored raw.
        assert_eq!(encode_blob("{}"), ("raw", b"{}".to_vec()));
        assert_eq!(content_hash("{}").len(), 64);
    }

    #[test]
    fn decrement_floor() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.decrement("home").unwrap(), Err(BelowZero { count: 0 }));
        s.increment("home").unwrap();
        assert_eq!(s.decrement("home").unwrap(), Ok(0));
        assert_eq!(s.decrement("home").unwrap(), Err(BelowZero { count: 0 }));
        assert_eq!(s.counter("home").unwrap(), 0);
    }
}
