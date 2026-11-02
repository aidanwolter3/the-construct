//! Server persistence: one SQLite database (`$CONSTRUCT_DATA_DIR/server.db`).
//!
//! The schema is versioned with `PRAGMA user_version`. To add tables in a later
//! milestone, append a step to [`MIGRATIONS`]; each step runs once, in order,
//! inside a transaction that also bumps `user_version`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
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
    // v7: layouts as code (one app = one linear, append-only history; D31+).
    // The v6 catalog tables stay but are inert after this.
    Migration::Rust(migrate_v7),
    // v8: `file_hash`, the hash of a layout file's own content (canonical JSON
    // of its note and layout, see `layouts::file_hash`), independent of how
    // the current `UiNode` re-serializes it. The append-only check compares
    // it; NULL on rows recorded before v8 until the next startup sync adopts
    // it (`Store::sync_layout_files`).
    Migration::Sql("ALTER TABLE app_versions ADD COLUMN file_hash TEXT;"),
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

/// v7 schema: one linear, append-only layout history per app.
const V7_SCHEMA: &str = "
    CREATE TABLE app_versions (
        app_id     TEXT NOT NULL,
        version    INTEGER NOT NULL,
        hash       TEXT NOT NULL REFERENCES layout_blobs (hash),
        source     TEXT NOT NULL,
        note       TEXT,
        file_key   TEXT UNIQUE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (app_id, version)
    ) WITHOUT ROWID;";

/// v6 -> v7. Revision R of each workspace (`workspace_revisions`) becomes
/// version R of that app, whose layout is the one shown at that revision
/// (`layout_id` at `layout_version`) and whose note says how it got there, so
/// version numbers stay what clients saw (monotonic) and the history is what
/// was on screen. The v6 tables (`layouts`, `layout_versions`,
/// `workspace_revisions`) are kept but inert: nothing reads or writes them
/// after this. `layout_blobs` stays live (shared by hash). Refuses (the whole
/// migration rolls back and the server does not start) if a revision points
/// at a missing layout version or blob, rather than inventing history.
fn migrate_v7(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(V7_SCHEMA)?;
    type Rev = (String, i64, String, i64, String, i64);
    let revisions: Vec<Rev> = {
        let mut stmt = tx.prepare(
            "SELECT workspace_id, revision, layout_id, layout_version, reason, created_at
             FROM workspace_revisions ORDER BY workspace_id, revision",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (ws, revision, layout_id, layout_version, reason, created_at) in revisions {
        let found: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT hash, source, prompt FROM layout_versions
                 WHERE layout_id = ?1 AND version = ?2",
                params![layout_id, layout_version],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((hash, layout_source, prompt)) = found else {
            return Err(refuse(format!(
                "migration v7: revision {revision} of '{ws}' shows {layout_id} v{layout_version}, \
                 which is not in layout_versions"
            )));
        };
        let has_blob = tx
            .query_row(
                "SELECT 1 FROM layout_blobs WHERE hash = ?1",
                [&hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !has_blob {
            return Err(refuse(format!(
                "migration v7: the layout of {layout_id} v{layout_version} (revision {revision} \
                 of '{ws}') is missing from layout_blobs"
            )));
        }
        let title: String = tx
            .query_row(
                "SELECT title FROM layouts WHERE id = ?1",
                [&layout_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| layout_id.clone());
        let (source, note) = v7_source_and_note(&reason, &layout_source, prompt.as_deref(), &title);
        tx.execute(
            "INSERT INTO app_versions (app_id, version, hash, source, note, file_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![ws, revision, hash, source, note, created_at],
        )?;
    }
    Ok(())
}

/// `(source, note)` of a migrated revision: a switch of the active layout
/// (`reason` `activate`) -> `switch`, "Switched to <title>"; otherwise the
/// source of the layout version it showed, with a note from its prompt.
fn v7_source_and_note(
    reason: &str,
    layout_source: &str,
    prompt: Option<&str>,
    title: &str,
) -> (String, String) {
    if reason == REASON_ACTIVATE {
        return ("switch".to_owned(), format!("Switched to {title}"));
    }
    let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
    let note = match layout_source {
        SOURCE_SEED => format!("Built-in layout: {title}"),
        SOURCE_RESTORE => match prompt.and_then(|p| p.strip_prefix("restore v")) {
            Some(n) => format!("Restored {title} v{n}"),
            None => prompt.map_or_else(|| format!("Restored {title}"), str::to_owned),
        },
        SOURCE_ROLLBACK => match prompt.and_then(|p| p.strip_prefix("rollback to v")) {
            Some(n) => format!("Rolled back to v{n}"),
            None => prompt.map_or_else(|| "Rolled back".to_owned(), str::to_owned),
        },
        SOURCE_AGENT => prompt.map_or_else(|| "AI edit".to_owned(), str::to_owned),
        SOURCE_MANUAL => prompt.map_or_else(|| "Manual edit".to_owned(), str::to_owned),
        other => prompt.map_or_else(|| other.to_owned(), str::to_owned),
    };
    (layout_source.to_owned(), note)
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
/// A version created from a layout file (`layouts/<app>/NNNN.json`).
pub const SOURCE_FILE: &str = "file";
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

/// One version of an app's layout history (metadata; the layout itself is
/// the blob `hash`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppVersion {
    pub app_id: String,
    /// 1, 2, ... per app, gapless and append-only.
    pub version: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// [`SOURCE_FILE`] for layouts-as-code versions; migrated rows keep
    /// `seed` | `agent` | `rollback` | `restore` | `manual` | `switch`.
    pub source: String,
    pub note: Option<String>,
    /// `<app_id>/NNNN` for a version created from `layouts/<app_id>/NNNN.json`.
    pub file_key: Option<String>,
    pub hash: String,
}

/// A layout file to sync (see [`Store::sync_layout_files`]). `json` must be
/// canonical (`serde_json::to_string(&UiNode)`), so equal layouts hash equal;
/// it is what the pod serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion {
    /// `<app_id>/NNNN`.
    pub key: String,
    /// NNNN.
    pub number: u32,
    /// `layouts/<app_id>/NNNN.json`, for error messages.
    pub path: String,
    pub note: String,
    pub json: String,
    /// Hash of the file's own content (note and layout), independent of the
    /// Rust types: what the append-only check compares.
    pub file_hash: String,
}

/// What [`Store::sync_layout_files`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// `(path, version)` of each file appended as a new version.
    pub appended: Vec<(String, i64)>,
    /// Paths of recorded files whose `file_hash` was recorded now (rows from
    /// before migration v8).
    pub adopted: Vec<String>,
    /// One warning per recorded file this build does not have (a rollback
    /// deploy, an older branch): the recorded history is served unchanged.
    pub missing: Vec<String>,
}

/// One recorded layout file of an app.
struct RecordedFile {
    key: String,
    version: i64,
    hash: String,
    note: Option<String>,
    file_hash: Option<String>,
}

/// NNNN of a file key `<app>/NNNN`.
fn key_number(key: &str) -> Option<u32> {
    key.rsplit_once('/').and_then(|(_, n)| n.parse().ok())
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

const APP_VERSION_COLS: &str = "app_id, version, created_at, source, note, file_key, hash";

fn app_version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AppVersion> {
    Ok(AppVersion {
        app_id: r.get(0)?,
        version: r.get(1)?,
        created_at: r.get(2)?,
        source: r.get(3)?,
        note: r.get(4)?,
        file_key: r.get(5)?,
        hash: r.get(6)?,
    })
}

fn latest_app_version_on(conn: &Connection, app_id: &str) -> rusqlite::Result<Option<AppVersion>> {
    conn.query_row(
        &format!(
            "SELECT {APP_VERSION_COLS} FROM app_versions WHERE app_id = ?1
             ORDER BY version DESC LIMIT 1"
        ),
        [app_id],
        app_version_row,
    )
    .optional()
}

/// A layout file's key and content hash (versions created from files).
type FileRef<'a> = Option<(&'a str, &'a str)>;

/// Append `json` as version `MAX + 1` of the app (blob stored once by hash);
/// `file` = `(file_key, file_hash)`.
fn append_app_version_on(
    conn: &Connection,
    app_id: &str,
    json: &str,
    source: &str,
    note: Option<&str>,
    file: FileRef<'_>,
) -> rusqlite::Result<i64> {
    let hash = put_blob(conn, json)?;
    conn.query_row(
        "INSERT INTO app_versions
             (app_id, version, hash, source, note, file_key, file_hash, created_at)
         SELECT ?1, COALESCE(MAX(version), 0) + 1, ?2, ?3, ?4, ?5, ?6, ?7
         FROM app_versions WHERE app_id = ?1
         RETURNING version",
        params![
            app_id,
            hash,
            source,
            note,
            file.map(|f| f.0),
            file.map(|f| f.1),
            now_secs()
        ],
        |r| r.get(0),
    )
}

/// Sync one app's files inside `tx`; `Ok(Err(message))` refuses.
fn sync_app_files_on(
    tx: &Transaction<'_>,
    app_id: &str,
    files: &[FileVersion],
    report: &mut SyncReport,
) -> rusqlite::Result<Result<(), String>> {
    let recorded: Vec<RecordedFile> = {
        let mut stmt = tx.prepare(
            "SELECT file_key, version, hash, note, file_hash FROM app_versions
             WHERE app_id = ?1 AND file_key IS NOT NULL ORDER BY version",
        )?;
        stmt.query_map([app_id], |r| {
            Ok(RecordedFile {
                key: r.get(0)?,
                version: r.get(1)?,
                hash: r.get(2)?,
                note: r.get(3)?,
                file_hash: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    // New files must be numbered after every recorded one (present or not).
    let max_recorded = recorded.iter().filter_map(|r| key_number(&r.key)).max();
    let max_file = files.iter().map(|f| f.number).max();
    let next_name = format!("{:04}.json", max_recorded.max(max_file).unwrap_or(0) + 1);
    for r in &recorded {
        if !files.iter().any(|f| f.key == r.key) {
            report.missing.push(format!(
                "layouts/{}.json was deployed as version {} but is missing from this build; \
                 serving the recorded history unchanged",
                r.key, r.version
            ));
        }
    }
    for file in files {
        if let Some(r) = recorded.iter().find(|r| r.key == file.key) {
            let unchanged = match &r.file_hash {
                Some(h) => *h == file.file_hash,
                // Recorded before v8: adopt the file's hash if its layout
                // (as this build serializes it) and note are what was
                // recorded.
                None => {
                    r.hash == content_hash(&file.json)
                        && r.note.as_deref() == Some(file.note.as_str())
                }
            };
            if !unchanged {
                return Ok(Err(format!(
                    "{path} changed after it was deployed as version {version}; layout files are \
                     append-only. To recover, restore the deployed content of {path} (git log -p \
                     -- {path}) and put the change in a new file, {next_name}",
                    path = file.path,
                    version = r.version,
                )));
            }
            if r.file_hash.is_none() {
                tx.execute(
                    "UPDATE app_versions SET file_hash = ?1 WHERE app_id = ?2 AND version = ?3",
                    params![file.file_hash, app_id, r.version],
                )?;
                report.adopted.push(file.path.clone());
            }
            continue;
        }
        if let Some(max) = max_recorded.filter(|m| *m > file.number) {
            return Ok(Err(format!(
                "{} is new but numbered below layouts/{app_id}/{max:04}.json, which is already \
                 deployed; layout files are append-only. To recover, move its content to a new \
                 file numbered after the newest deployed one, {next_name}",
                file.path
            )));
        }
        let version = append_app_version_on(
            tx,
            app_id,
            &file.json,
            SOURCE_FILE,
            Some(&file.note),
            Some((&file.key, &file.file_hash)),
        )?;
        report.appended.push((file.path.clone(), version));
    }
    Ok(Ok(()))
}

/// A plain SQLite error carrying `msg` (migrations that refuse to continue).
fn refuse(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(msg))
}

/// Run migrations up to `user_version = target`. Each step runs in an
/// IMMEDIATE transaction (the write lock is taken up front) that re-reads
/// `user_version`, so two processes starting on the same database never both
/// run a step.
fn migrate(conn: &mut Connection, target: usize) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, step) in MIGRATIONS
        .iter()
        .enumerate()
        .take(target)
        .skip(version.max(0) as usize)
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if now > i as i64 {
            // Another process ran this step meanwhile.
            continue;
        }
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
}

const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Switch the database to WAL. SQLite answers the journal-mode change with
/// SQLITE_BUSY immediately (the busy handler is not invoked) when another
/// connection holds a lock, so skip it when already in WAL and otherwise
/// retry with a short backoff up to the busy timeout.
fn enable_wal(conn: &Connection) -> rusqlite::Result<()> {
    let start = std::time::Instant::now();
    let mut delay = std::time::Duration::from_millis(10);
    loop {
        let attempt = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .and_then(|mode| {
                if mode.eq_ignore_ascii_case("wal") {
                    Ok(())
                } else {
                    conn.pragma_update(None, "journal_mode", "WAL")
                }
            });
        match attempt {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && start.elapsed() < BUSY_TIMEOUT =>
            {
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

impl Store {
    /// Open (creating if needed) the database file and run migrations.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // Before WAL: switching the journal mode can meet a concurrent starter.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn)?;
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
        })
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

    // ---------- App versions (v7, layouts as code) ----------

    /// The app's newest version and its layout JSON.
    pub fn app_latest(&self, app_id: &str) -> rusqlite::Result<Option<(AppVersion, String)>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(v) = latest_app_version_on(&tx, app_id)? else {
            return Ok(None);
        };
        let json = get_blob(&tx, &v.hash)?;
        Ok(Some((v, json)))
    }

    /// The app's newest version (metadata only).
    pub fn app_latest_meta(&self, app_id: &str) -> rusqlite::Result<Option<AppVersion>> {
        latest_app_version_on(&self.conn(), app_id)
    }

    /// Versions older than `before` (all if `None`), newest first, at most `limit`.
    pub fn app_versions(
        &self,
        app_id: &str,
        before: Option<i64>,
        limit: i64,
    ) -> rusqlite::Result<Vec<AppVersion>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {APP_VERSION_COLS} FROM app_versions
             WHERE app_id = ?1 AND (?2 IS NULL OR version < ?2)
             ORDER BY version DESC LIMIT ?3"
        ))?;
        stmt.query_map(params![app_id, before, limit], app_version_row)?
            .collect()
    }

    /// One version's metadata and layout JSON.
    pub fn app_version(
        &self,
        app_id: &str,
        version: i64,
    ) -> rusqlite::Result<Option<(AppVersion, String)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                &format!(
                    "SELECT {APP_VERSION_COLS} FROM app_versions WHERE app_id = ?1 AND version = ?2"
                ),
                params![app_id, version],
                app_version_row,
            )
            .optional()?;
        match row {
            Some(v) => {
                let json = get_blob(&conn, &v.hash)?;
                Ok(Some((v, json)))
            }
            None => Ok(None),
        }
    }

    /// Append a version outside the file sync (tests and tooling only; no
    /// HTTP route reaches this). Returns the new version number.
    pub fn append_app_version(
        &self,
        app_id: &str,
        json: &str,
        source: &str,
        note: Option<&str>,
    ) -> rusqlite::Result<i64> {
        append_app_version_on(&self.conn(), app_id, json, source, note, None)
    }

    /// Sync every app's layout files (`(app_id, files in number order)`)
    /// into the histories, in ONE transaction. A file already recorded (by
    /// key) must be unchanged: same `file_hash` (rows from before v8, whose
    /// `file_hash` is NULL, adopt it if the layout blob hash and note match);
    /// a new file is appended as version `MAX + 1` (source [`SOURCE_FILE`]).
    /// A recorded file missing from `files` is only reported
    /// ([`SyncReport::missing`]). Refuses (`Ok(Err(message))`, nothing
    /// written, for any app) when a recorded file changed or a new file is
    /// numbered below a recorded one.
    pub fn sync_layout_files(
        &self,
        apps: &[(&str, Vec<FileVersion>)],
    ) -> rusqlite::Result<Result<SyncReport, String>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = SyncReport::default();
        for (app_id, files) in apps {
            if let Err(msg) = sync_app_files_on(&tx, app_id, files, &mut report)? {
                return Ok(Err(msg)); // dropping `tx` rolls back
            }
        }
        tx.commit()?;
        Ok(Ok(report))
    }

    /// Storage accounting: `(blobs, blob bytes stored, app versions)`.
    pub fn layout_storage(&self) -> rusqlite::Result<(i64, i64, i64)> {
        self.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM layout_blobs),
                    (SELECT COALESCE(SUM(LENGTH(data)), 0) FROM layout_blobs),
                    (SELECT COUNT(*) FROM app_versions)",
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

    fn file(app: &str, n: u32, json: &str, note: &str) -> FileVersion {
        FileVersion {
            key: format!("{app}/{n:04}"),
            number: n,
            path: format!("layouts/{app}/{n:04}.json"),
            note: note.into(),
            json: json.into(),
            // Stands in for `layouts::file_hash` (the file's own content).
            file_hash: content_hash(&format!("{note}\n{json}")),
        }
    }

    /// Sync one app; the appended `(path, version)`s or the refusal.
    fn sync1(s: &Store, app: &str, files: &[FileVersion]) -> Result<Vec<(String, i64)>, String> {
        s.sync_layout_files(&[(app, files.to_vec())])
            .unwrap()
            .map(|r| r.appended)
    }

    fn file_hash_of(s: &Store, app: &str, version: i64) -> Option<String> {
        s.conn()
            .query_row(
                "SELECT file_hash FROM app_versions WHERE app_id = ?1 AND version = ?2",
                params![app, version],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn history(s: &Store, app: &str) -> Vec<(i64, String, Option<String>)> {
        s.app_versions(app, None, 100)
            .unwrap()
            .into_iter()
            .map(|v| (v.version, v.source, v.note))
            .collect()
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn layout_file_sync_is_append_only_and_idempotent() {
        let s = Store::open_in_memory().unwrap();
        let one = file("a", 1, "{\"v\":1}", "First");
        assert_eq!(
            sync1(&s, "a", std::slice::from_ref(&one)).unwrap(),
            [("layouts/a/0001.json".to_owned(), 1)]
        );
        assert_eq!(file_hash_of(&s, "a", 1), Some(one.file_hash.clone()));
        // Idempotent.
        assert!(
            sync1(&s, "a", std::slice::from_ref(&one))
                .unwrap()
                .is_empty()
        );
        // A changed layout or note is refused, naming the file and how to recover.
        let changed = file("a", 1, "{\"v\":2}", "First");
        assert_eq!(
            sync1(&s, "a", &[changed]).unwrap_err(),
            "layouts/a/0001.json changed after it was deployed as version 1; layout files are \
             append-only. To recover, restore the deployed content of layouts/a/0001.json \
             (git log -p -- layouts/a/0001.json) and put the change in a new file, 0002.json"
        );
        let renoted = file("a", 1, "{\"v\":1}", "Other");
        assert!(
            sync1(&s, "a", &[renoted])
                .unwrap_err()
                .contains("changed after it was deployed")
        );
        // A new file appends; the history is per app.
        let two = file("a", 2, "{\"v\":1}", "Same layout again");
        assert_eq!(
            sync1(&s, "a", &[one.clone(), two.clone()]).unwrap(),
            [("layouts/a/0002.json".to_owned(), 2)]
        );
        assert_eq!(
            history(&s, "a"),
            [
                (2, "file".into(), Some("Same layout again".into())),
                (1, "file".into(), Some("First".into())),
            ]
        );
        let (v, json) = s.app_latest("a").unwrap().unwrap();
        assert_eq!(
            (v.version, v.file_key.as_deref(), json.as_str()),
            (2, Some("a/0002"), "{\"v\":1}")
        );
        assert_eq!(s.app_version("a", 1).unwrap().unwrap().1, "{\"v\":1}");
        assert_eq!(s.app_version("a", 3).unwrap(), None);
        assert_eq!(s.layout_storage().unwrap().0, 1, "one blob for both");
        // A new file numbered below a deployed one is refused, and nothing is written.
        let b = Store::open_in_memory().unwrap();
        sync1(&b, "b", &[file("b", 2, "{}", "two")]).unwrap();
        let err = sync1(
            &b,
            "b",
            &[file("b", 1, "{}", "one"), file("b", 2, "{}", "two")],
        )
        .unwrap_err();
        assert_eq!(
            err,
            "layouts/b/0001.json is new but numbered below layouts/b/0002.json, which is \
             already deployed; layout files are append-only. To recover, move its content to a \
             new file numbered after the newest deployed one, 0003.json"
        );
        assert_eq!(history(&b, "b").len(), 1);
        // Paging.
        let page: Vec<i64> = s
            .app_versions("a", Some(2), 1)
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(page, [1]);
    }

    #[test]
    fn a_missing_recorded_file_is_a_warning_and_later_files_still_append() {
        let s = Store::open_in_memory().unwrap();
        let (one, two, three) = (
            file("a", 1, "{\"v\":1}", "One"),
            file("a", 2, "{\"v\":2}", "Two"),
            file("a", 3, "{\"v\":3}", "Three"),
        );
        sync1(&s, "a", &[one.clone(), two.clone()]).unwrap();
        // A rollback deploy without 0002.json: warned, history unchanged.
        let report = s
            .sync_layout_files(&[("a", vec![one.clone()])])
            .unwrap()
            .unwrap();
        assert_eq!(
            report.missing,
            [
                "layouts/a/0002.json was deployed as version 2 but is missing from this build; \
              serving the recorded history unchanged"
            ]
        );
        assert!(report.appended.is_empty());
        assert_eq!(s.app_latest("a").unwrap().unwrap().0.version, 2);
        // The same number with other content is still a change.
        let other_two = file("a", 2, "{\"v\":22}", "Two'");
        assert!(
            sync1(&s, "a", &[one.clone(), other_two])
                .unwrap_err()
                .contains("0002.json changed after it was deployed as version 2")
        );
        // A build whose numbering continues after the newest recorded file appends.
        assert_eq!(
            sync1(&s, "a", &[one.clone(), two.clone(), three.clone()]).unwrap(),
            [("layouts/a/0003.json".to_owned(), 3)]
        );
        // Missing 0002 and 0003, with a new file numbered below them: refused.
        let s2 = Store::open_in_memory().unwrap();
        sync1(&s2, "a", &[one.clone(), two.clone(), three.clone()]).unwrap();
        s2.conn()
            .execute("DELETE FROM app_versions WHERE version = 2", [])
            .unwrap();
        let err = sync1(&s2, "a", &[one, two]).unwrap_err();
        assert!(
            err.starts_with("layouts/a/0002.json is new but numbered below layouts/a/0003.json"),
            "{err}"
        );
        assert!(err.ends_with("0004.json"), "{err}");
    }

    #[test]
    fn file_hash_decides_changes_and_is_adopted_for_pre_v8_rows() {
        let s = Store::open_in_memory().unwrap();
        let one = file("a", 1, "{\"v\":1}", "One");
        sync1(&s, "a", std::slice::from_ref(&one)).unwrap();
        // This build serializes the same file differently (a protocol
        // refactor: field order, a new defaulted field): not a change, and
        // the recorded blob is still what is served.
        let reserialized = FileVersion {
            json: "{\"v\":1,\"new_default\":null}".into(),
            ..one.clone()
        };
        assert!(
            sync1(&s, "a", std::slice::from_ref(&reserialized))
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.app_latest("a").unwrap().unwrap().1, "{\"v\":1}");
        // A real content change (another file hash) is refused even with the
        // same serialized layout.
        let edited = FileVersion {
            file_hash: content_hash("edited"),
            ..one.clone()
        };
        assert!(
            sync1(&s, "a", &[edited])
                .unwrap_err()
                .contains("changed after")
        );

        // Rows recorded before v8 (file_hash NULL) adopt the hash when the
        // layout and note still match, then compare it.
        s.conn()
            .execute("UPDATE app_versions SET file_hash = NULL", [])
            .unwrap();
        let mismatched = FileVersion {
            json: "{\"v\":2}".into(),
            ..one.clone()
        };
        assert!(
            sync1(&s, "a", &[mismatched])
                .unwrap_err()
                .contains("changed after")
        );
        assert_eq!(file_hash_of(&s, "a", 1), None, "nothing written");
        let report = s
            .sync_layout_files(&[("a", vec![one.clone()])])
            .unwrap()
            .unwrap();
        assert_eq!(report.adopted, ["layouts/a/0001.json"]);
        assert_eq!(file_hash_of(&s, "a", 1), Some(one.file_hash.clone()));
        let again = s
            .sync_layout_files(&[("a", vec![one.clone()])])
            .unwrap()
            .unwrap();
        assert_eq!(again, SyncReport::default(), "idempotent");
        assert!(
            sync1(&s, "a", std::slice::from_ref(&reserialized))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_refusal_in_one_app_writes_nothing_for_any_app() {
        let s = Store::open_in_memory().unwrap();
        sync1(&s, "b", &[file("b", 1, "{}", "one")]).unwrap();
        let err = s
            .sync_layout_files(&[
                ("a", vec![file("a", 1, "{}", "a1")]),
                ("b", vec![file("b", 1, "{}", "changed")]),
            ])
            .unwrap()
            .unwrap_err();
        assert!(err.starts_with("layouts/b/0001.json changed"), "{err}");
        assert!(history(&s, "a").is_empty(), "app a rolled back too");
    }

    #[test]
    fn concurrent_starters_run_each_migration_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || Store::open(&path).map(|_| ()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), MIGRATIONS.len() as i64);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn v6_and_v7_carry_v5_history_forward() {
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
            migrate(&mut conn, 6).unwrap();
            let revs: i64 = conn
                .query_row("SELECT COUNT(*) FROM workspace_revisions", [], |r| r.get(0))
                .unwrap();
            assert_eq!(revs, 5);
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(user_version(&s.conn()), MIGRATIONS.len() as i64);
        assert_eq!(
            history(&s, "home"),
            [
                (3, "rollback".into(), Some("Rolled back to v1".into())),
                (2, "agent".into(), Some("hide captions".into())),
                (
                    1,
                    "seed".into(),
                    Some("Built-in layout: home.counter".into())
                ),
            ]
        );
        let rows = s.app_versions("home", None, 10).unwrap();
        assert_eq!(
            rows[0].hash, rows[2].hash,
            "identical layouts share one blob"
        );
        assert_eq!(
            rows.iter().map(|r| r.created_at).collect::<Vec<_>>(),
            [1003, 1002, 1001]
        );
        assert_eq!(
            s.app_version("home", 2).unwrap().unwrap().1,
            "{\"edited\":true}"
        );
        assert_eq!(
            s.app_latest("pixelfed_home").unwrap().unwrap().1,
            "{\"b\":1}"
        );
        assert_eq!(s.app_latest("pixelfed_demo").unwrap(), None);
        assert_eq!(s.layout_storage().unwrap().0, 3, "3 distinct blobs");
    }

    /// Writes v6 catalog rows the way the v6 code did.
    struct V6<'a>(&'a Connection);

    impl V6<'_> {
        fn layout(&self, id: &str, ws: &str, title: &str) {
            self.0
                .execute(
                    "INSERT INTO layouts (id, workspace_id, title, builtin, created_at)
                     VALUES (?1, ?2, ?3, 1, 1)",
                    params![id, ws, title],
                )
                .unwrap();
        }

        fn version(&self, layout: &str, v: i64, json: &str, source: &str, prompt: Option<&str>) {
            let hash = put_blob(self.0, json).unwrap();
            let provider = match source {
                SOURCE_AGENT => Some("rules"),
                SOURCE_ROLLBACK => Some(ROLLBACK),
                _ => None,
            };
            self.0
                .execute(
                    "INSERT INTO layout_versions
                         (layout_id, version, hash, source, prompt, provider, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![layout, v, hash, source, prompt, provider, 100 + v],
                )
                .unwrap();
        }

        fn revision(&self, ws: &str, rev: i64, layout: &str, v: i64, reason: &str) {
            self.0
                .execute(
                    "INSERT INTO workspace_revisions
                         (workspace_id, revision, layout_id, layout_version, reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![ws, rev, layout, v, reason, 2000 + rev],
                )
                .unwrap();
        }
    }

    const C1: &str = "{\"counter\":1}";
    const C2: &str = "{\"counter\":2}";
    const B1: &str = "{\"big\":1}";

    /// A v6 database: `home` went seed -> agent edit -> switch to Big counter
    /// -> switch back restoring v1 -> restore v2 (active) -> rollback to v1;
    /// `pixelfed_demo` has catalog rows but no revisions.
    fn v6_db(path: &Path) {
        let mut conn = Connection::open(path).unwrap();
        migrate(&mut conn, 6).unwrap();
        let db = V6(&conn);
        db.layout("home.counter", "home", "Counter");
        db.layout("home.big_counter", "home", "Big counter");
        db.layout("pixelfed_demo.classic", "pixelfed_demo", "Classic feed");
        db.version("home.counter", 1, C1, SOURCE_SEED, None);
        db.version("home.big_counter", 1, B1, SOURCE_SEED, None);
        db.version("pixelfed_demo.classic", 1, "{\"d\":1}", SOURCE_SEED, None);
        db.revision("home", 1, "home.counter", 1, SOURCE_SEED);
        db.version("home.counter", 2, C2, SOURCE_AGENT, Some("make it bigger"));
        db.revision("home", 2, "home.counter", 2, SOURCE_AGENT);
        db.revision("home", 3, "home.big_counter", 1, REASON_ACTIVATE);
        db.version("home.counter", 3, C1, SOURCE_RESTORE, Some("restore v1"));
        db.revision("home", 4, "home.counter", 3, REASON_ACTIVATE);
        db.version("home.counter", 4, C2, SOURCE_RESTORE, Some("restore v2"));
        db.revision("home", 5, "home.counter", 4, SOURCE_RESTORE);
        db.version(
            "home.counter",
            5,
            C1,
            SOURCE_ROLLBACK,
            Some("rollback to v1"),
        );
        db.revision("home", 6, "home.counter", 5, SOURCE_ROLLBACK);
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn v7_turns_workspace_revisions_into_app_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        v6_db(&path);
        let s = Store::open(&path).unwrap();
        let note = |n: &str| Some(n.to_owned());
        assert_eq!(
            history(&s, "home"),
            [
                (6, "rollback".into(), note("Rolled back to v1")),
                (5, "restore".into(), note("Restored Counter v2")),
                (4, "switch".into(), note("Switched to Counter")),
                (3, "switch".into(), note("Switched to Big counter")),
                (2, "agent".into(), note("make it bigger")),
                (1, "seed".into(), note("Built-in layout: Counter")),
            ]
        );
        // Each version is what was on screen at that revision.
        let shown: Vec<String> = (1..=6)
            .map(|v| s.app_version("home", v).unwrap().unwrap().1)
            .collect();
        assert_eq!(shown, [C1, C2, B1, C1, C2, C1]);
        assert_eq!(
            s.app_version("home", 3).unwrap().unwrap().0.created_at,
            2003
        );
        assert!(history(&s, "pixelfed_demo").is_empty());
        {
            // No new blobs; the v6 tables are left as they were.
            let conn = s.conn();
            assert_eq!(count(&conn, "layout_blobs"), 4);
            assert_eq!(count(&conn, "layout_versions"), 7);
            assert_eq!(count(&conn, "workspace_revisions"), 6);
            assert_eq!(count(&conn, "layouts"), 3);
        }
        // Layout files continue after the migrated history.
        assert_eq!(
            sync1(&s, "home", &[file("home", 1, C1, "Counter")]).unwrap(),
            [("layouts/home/0001.json".to_owned(), 7)]
        );
        assert_eq!(
            sync1(&s, "pixelfed_demo", &[file("pixelfed_demo", 1, "{}", "x")]).unwrap(),
            [("layouts/pixelfed_demo/0001.json".to_owned(), 1)]
        );
        drop(s);
        // Reopening runs nothing again.
        let s = Store::open(&path).unwrap();
        assert_eq!(history(&s, "home").len(), 7);
    }

    #[test]
    fn v7_refuses_a_revision_without_its_layout_version_or_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        v6_db(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "DELETE FROM layout_versions WHERE layout_id = 'home.counter' AND version = 4",
                [],
            )
            .unwrap();
        }
        let err = Store::open(&path).err().unwrap().to_string();
        assert!(
            err.contains(
                "revision 5 of 'home' shows home.counter v4, which is not in layout_versions"
            ),
            "{err}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 6, "nothing was migrated");
        drop(conn);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        v6_db(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "DELETE FROM layout_blobs WHERE hash = ?1",
                [content_hash(B1)],
            )
            .unwrap();
        }
        let err = Store::open(&path).err().unwrap().to_string();
        assert!(err.contains("is missing from layout_blobs"), "{err}");
    }

    #[test]
    fn v7_on_an_empty_v6_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, 6).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.app_latest("home").unwrap(), None);
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
