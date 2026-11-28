//! Server persistence: one SQLite database (`$CONSTRUCT_DATA_DIR/server.db`).
//!
//! The schema is a list of named steps ([`crate::migrations::core::STEPS`], decision
//! `named-migrations`): to change it, append a [`Step`] with a new name. Each
//! step runs once, in order, recorded in `schema_migrations` under the scope
//! `core`; adapters keep their own lists ([`Store::migrate_adapter`]).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub use crate::migrations::core::{rename_app_on, rename_source_on};
use crate::migrations::{self, Counter};
pub use crate::migrations::{Migration, Step};

/// A pod setting inside a transaction (an adapter migration reads its
/// choices here, D46). `None`: not set (a pod made before D46, tests).
pub fn pod_setting_on(tx: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT value FROM pod_settings WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
}

use crate::util::now_secs;

pub(crate) fn attach_sign_in_on(
    conn: &Connection,
    token_hash: &str,
    sign_in: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO session_sign_ins (token_hash, provider, created_at)
         VALUES (?1, ?2, ?3)",
        params![token_hash, sign_in, now_secs()],
    )?;
    // A polling adapter that lost this sign-in polls again (D52).
    conn.execute(
        "UPDATE notification_polls SET stopped_for = NULL, next_at = 0, failures = 0
         WHERE stopped_for = ?1",
        [sign_in],
    )?;
    Ok(())
}

/// A completed sign-in, inside the adapter's transaction ([`Store::transaction`]):
/// prunes sessions older than `session_ttl_secs` (and their sign-ins), creates
/// the session `token_hash` if it does not exist, and records `sign_in` on it.
/// Calling it again for the same session adds the sign-in (one session
/// collects the sign-ins of every app, D40).
pub fn open_session_on(
    tx: &Connection,
    token_hash: &str,
    sign_in: &str,
    session_ttl_secs: i64,
) -> rusqlite::Result<()> {
    let now = now_secs();
    let cutoff = now.saturating_sub(session_ttl_secs);
    tx.execute(
        "DELETE FROM session_sign_ins WHERE token_hash IN
             (SELECT token_hash FROM sessions WHERE created_at <= ?1 AND pinned = 0)",
        [cutoff],
    )?;
    tx.execute(
        "DELETE FROM sessions WHERE created_at <= ?1 AND pinned = 0",
        [cutoff],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO sessions (token_hash, created_at) VALUES (?1, ?2)",
        params![token_hash, now],
    )?;
    attach_sign_in_on(tx, token_hash, sign_in)
}

/// A plain SQLite error carrying `msg` (migrations that refuse to continue).
fn refuse(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(msg))
}

/// Run the core's steps up to and including `name` (tests of later
/// migrations: an adapter's own test builds a database as an older build left
/// it, then opens it).
#[doc(hidden)]
pub fn migrate_to(conn: &mut Connection, name: &str) -> rusqlite::Result<()> {
    let upto = migrations::position(migrations::core::STEPS, name)
        .ok_or_else(|| refuse(format!("no core migration named '{name}'")))?;
    migrate(conn, upto)
}

/// Run the first `upto` core steps that have not run ([`migrations::run`]).
fn migrate(conn: &mut Connection, upto: usize) -> rusqlite::Result<()> {
    migrations::run(
        conn,
        "core",
        migrations::core::STEPS,
        Counter::UserVersion,
        upto,
    )
}

/// The store hit its size limit ([`Store::set_max_bytes`]) or the disk is
/// full.
pub fn is_full(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::DiskFull)
}

/// The store's connection, held: dereferences to it, and records the wait
/// and the hold when dropped (issue #576).
pub(crate) struct Held<'a> {
    guard: MutexGuard<'a, Connection>,
    waited: Duration,
    taken: Instant,
    at: &'static std::panic::Location<'static>,
}

impl std::ops::Deref for Held<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.guard
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.guard
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        crate::timing::record(self.waited, self.taken.elapsed(), self.at);
    }
}

pub struct Store {
    conn: Mutex<Connection>,
    /// Seals adapters' upstream credentials at rest ([`crate::secrets`]).
    secrets: crate::secrets::Secrets,
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
    /// Open (creating if needed, mode 0600) the database file and run
    /// migrations. Secrets are sealed with [`crate::secrets::Secrets::process`].
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Err(e) = crate::util::private_db_file(path) {
            return Err(refuse(format!(
                "making {} private (mode 0600): {e}",
                path.display()
            )));
        }
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
        migrate(&mut conn, migrations::core::STEPS.len())?;
        Ok(Self {
            conn: Mutex::new(conn),
            secrets: crate::secrets::Secrets::process(),
        })
    }

    /// Hold the database file to `max_bytes` (SQLite's `max_page_count`,
    /// decision `pod-storage-limits`): everything the pod stores, uploads
    /// included. A write that would grow it further fails with
    /// `SQLITE_FULL` ([`is_full`]), which [`crate::blocking`] answers with
    /// 507 `pod_storage_full`; deletes free pages for later writes. A file
    /// already larger keeps its size as the limit. Per connection, so set
    /// on each open.
    pub fn set_max_bytes(&self, max_bytes: u64) -> rusqlite::Result<()> {
        let conn = self.conn();
        let page: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages = max_bytes.div_ceil(page.max(512) as u64).max(1);
        conn.query_row(&format!("PRAGMA max_page_count = {pages}"), [], |_| Ok(()))?;
        Ok(())
    }

    /// Seal secrets with `secrets` instead of the process-wide keys (tests,
    /// or a host that reads its keys elsewhere).
    pub fn with_secrets(mut self, secrets: crate::secrets::Secrets) -> Self {
        self.secrets = secrets;
        self
    }

    /// The keys adapters seal their stored upstream credentials with.
    pub fn secrets(&self) -> &crate::secrets::Secrets {
        &self.secrets
    }

    /// The connection, timed: how long this waited for it and, when the
    /// guard drops, how long it was held ([`crate::timing::record`]).
    #[track_caller]
    fn conn(&self) -> Held<'_> {
        let at = std::panic::Location::caller();
        let asked = Instant::now();
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (statements are atomic), so recover from poisoning.
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Held {
            guard,
            waited: asked.elapsed(),
            taken: Instant::now(),
            at,
        }
    }

    /// Run `f` on the connection (an adapter's own queries). Keep it short:
    /// the store has one connection, behind a lock.
    #[track_caller]
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        f(&self.conn())
    }

    /// Run `f` in one transaction, committed when `f` returns `Ok` (also
    /// when it decided to write nothing) and rolled back on `Err`. Adapters
    /// use it to make their own writes and a sign-in ([`open_session_on`])
    /// atomic.
    #[track_caller]
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run an adapter's steps that have not run yet, in order, recorded in
    /// `schema_migrations` under the adapter's name ([`migrations::run`]).
    /// Its row in `adapter_migrations` keeps counting the leading steps that
    /// ran, for older builds.
    pub fn migrate_adapter(&self, adapter: &str, steps: &[Step]) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        migrations::run(
            &mut conn,
            adapter,
            steps,
            Counter::Adapter(adapter),
            steps.len(),
        )
    }

    /// The names of an adapter's steps that ran.
    pub fn adapter_migrations(
        &self,
        adapter: &str,
    ) -> rusqlite::Result<std::collections::HashSet<String>> {
        migrations::applied(&self.conn(), adapter)
    }

    /// How many of an adapter's leading steps ran (0 before its first).
    pub fn adapter_version(&self, adapter: &str) -> rusqlite::Result<i64> {
        Ok(self
            .conn()
            .query_row(
                "SELECT version FROM adapter_migrations WHERE adapter = ?1",
                [adapter],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    // ---------- Sessions and sign-ins (v2, v11) ----------

    /// Create a session with no sign-ins (tests and tooling; real sessions
    /// come from a sign-in, [`open_session_on`]).
    pub fn create_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (token_hash, created_at) VALUES (?1, ?2)",
            params![token_hash, now_secs()],
        )?;
        Ok(())
    }

    /// Create (or keep) the pinned session of a pod token (D46): it never
    /// expires and a logout keeps it.
    pub fn create_pinned_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (token_hash, created_at, pinned) VALUES (?1, ?2, 1)
             ON CONFLICT(token_hash) DO UPDATE SET pinned = 1",
            params![token_hash, now_secs()],
        )?;
        Ok(())
    }

    /// Replace the pinned session(s) with one for `token_hash` (a pod token
    /// rotation): the old token's session and its sign-ins go.
    pub fn replace_pinned_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_sign_ins WHERE token_hash IN
                 (SELECT token_hash FROM sessions WHERE pinned = 1)",
            [],
        )?;
        tx.execute("DELETE FROM sessions WHERE pinned = 1", [])?;
        tx.execute(
            "INSERT INTO sessions (token_hash, created_at, pinned) VALUES (?1, ?2, 1)",
            params![token_hash, now_secs()],
        )?;
        tx.commit()
    }

    /// The pod owner's session: the newest pinned one (a pod token's,
    /// D46), if any.
    pub fn owner_session(&self) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT token_hash FROM sessions WHERE pinned = 1 ORDER BY created_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
    }

    /// Whether the session is pinned (a pod token's).
    pub fn session_pinned(&self, token_hash: &str) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT pinned FROM sessions WHERE token_hash = ?1",
                [token_hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|p| p != 0))
    }

    /// Remove every sign-in of a session, keeping the session (logout of a
    /// pinned session).
    pub fn clear_sign_ins(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM session_sign_ins WHERE token_hash = ?1",
            [token_hash],
        )?;
        Ok(())
    }

    /// Remove one sign-in from a session (an adapter found it unusable).
    pub fn detach_sign_in(&self, token_hash: &str, sign_in: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM session_sign_ins WHERE token_hash = ?1 AND provider = ?2",
            [token_hash, sign_in],
        )?;
        Ok(())
    }

    /// A pod setting (D46); `None` when not set.
    pub fn pod_setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        pod_setting_on(&self.conn(), key)
    }

    pub fn set_pod_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    /// Record that `sign_in` completed on an existing session (tests and
    /// tooling; see [`open_session_on`]).
    pub fn attach_sign_in(&self, token_hash: &str, sign_in: &str) -> rusqlite::Result<()> {
        attach_sign_in_on(&self.conn(), token_hash, sign_in)
    }

    /// The sign-ins completed on a session, sorted.
    pub fn session_sign_ins(&self, token_hash: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT provider FROM session_sign_ins WHERE token_hash = ?1 ORDER BY provider",
        )?;
        stmt.query_map([token_hash], |r| r.get(0))?.collect()
    }

    /// Whether the session exists and is younger than `ttl_secs` (or pinned).
    pub fn session_valid(&self, token_hash: &str, ttl_secs: i64) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM sessions WHERE token_hash = ?1 AND (created_at > ?2 OR pinned = 1)",
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

    /// Delete a session and its sign-ins (logout). Returns whether a session
    /// was deleted.
    pub fn delete_session(&self, token_hash: &str) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_sign_ins WHERE token_hash = ?1",
            [token_hash],
        )?;
        let deleted = tx.execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])? > 0;
        tx.commit()?;
        Ok(deleted)
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
            s.create_session("h").unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = s
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, crate::migrations::core::STEPS.len() as i64);
        assert!(s.session_exists("h").unwrap(), "data survives reopening");
    }

    /// `notification_layouts` keeps a pod's notifications and devices'
    /// channel choices: only the column's name changes.
    #[test]
    fn notifications_and_channel_choices_survive_the_column_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate_to(&mut conn, "mcp_servers").unwrap();
            conn.execute_batch(
                "INSERT INTO notifications (app_id, channel, dedupe_key, title, created_at, inserted_at)
                     VALUES ('pixelfed', 'likes', 'k', 'Liked', 1, 1);
                 INSERT INTO device_channel_prefs (device_id, app_id, channel, enabled)
                     VALUES ('d1', 'pixelfed', 'likes', 0);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let got: (String, String, i64) = s
            .conn()
            .query_row(
                "SELECT n.layout, p.layout, p.enabled FROM notifications n, device_channel_prefs p",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(got, ("pixelfed".into(), "pixelfed".into(), 0));
        // Still idempotent on the renamed unique key.
        let inserted = s
            .conn()
            .execute(
                "INSERT OR IGNORE INTO notifications (layout, channel, dedupe_key, title, created_at, inserted_at)
                 VALUES ('pixelfed', 'likes', 'k', 'Again', 2, 2)",
                [],
            )
            .unwrap();
        assert_eq!(inserted, 0);
    }

    #[test]
    fn v10_drops_the_counters_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, 9).unwrap();
            conn.execute("INSERT INTO counters (id, count) VALUES ('home', 3)", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let tables: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'counters'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn v11_gives_every_existing_session_the_pixelfed_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, 10).unwrap();
            conn.execute(
                "INSERT INTO sessions (token_hash, created_at) VALUES ('h', 5)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.session_sign_ins("h").unwrap(), ["pixelfed"]);
        assert!(s.session_sign_ins("other").unwrap().is_empty());
    }

    #[test]
    fn sessions_collect_sign_ins_and_logout_removes_them() {
        let s = Store::open_in_memory().unwrap();
        let ttl = 86_400;
        s.transaction(|tx| open_session_on(tx, "h", "b", ttl))
            .unwrap();
        s.transaction(|tx| open_session_on(tx, "h", "a", ttl))
            .unwrap();
        // Again: idempotent.
        s.transaction(|tx| open_session_on(tx, "h", "a", ttl))
            .unwrap();
        assert!(s.session_valid("h", ttl).unwrap());
        assert_eq!(s.session_sign_ins("h").unwrap(), ["a", "b"]);
        assert!(s.delete_session("h").unwrap());
        assert!(s.session_sign_ins("h").unwrap().is_empty());
        assert!(!s.delete_session("h").unwrap());
    }

    #[test]
    fn session_ttl_and_pruning() {
        let s = Store::open_in_memory().unwrap();
        s.create_session("old").unwrap();
        s.attach_sign_in("old", "a").unwrap();
        s.conn()
            .execute("UPDATE sessions SET created_at = created_at - 1000", [])
            .unwrap();
        assert!(s.session_valid("old", 2000).unwrap());
        assert!(!s.session_valid("old", 999).unwrap());
        assert!(!s.session_valid("missing", 2000).unwrap());
        // A sign-in prunes sessions past the TTL, with their sign-ins.
        s.transaction(|tx| open_session_on(tx, "new", "a", 999))
            .unwrap();
        assert!(!s.session_exists("old").unwrap());
        assert!(s.session_sign_ins("old").unwrap().is_empty());
        assert!(s.session_valid("new", 999).unwrap());
    }

    #[test]
    fn a_failed_transaction_writes_nothing() {
        let s = Store::open_in_memory().unwrap();
        let r: rusqlite::Result<()> = s.transaction(|tx| {
            open_session_on(tx, "h", "a", 100)?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(r.is_err());
        assert!(!s.session_exists("h").unwrap());
    }

    #[test]
    fn adapter_migrations_run_once_in_order() {
        fn add_row(tx: &Transaction<'_>) -> rusqlite::Result<()> {
            tx.execute("INSERT INTO demo_t (n) VALUES (2)", [])?;
            Ok(())
        }
        static STEPS: &[Step] = &[
            Step::sql("demo_t", "CREATE TABLE demo_t (n INTEGER NOT NULL);"),
            Step::rust("demo_row", add_row),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        for _ in 0..2 {
            let s = Store::open(&path).unwrap();
            s.migrate_adapter("demo", STEPS).unwrap();
            assert_eq!(s.adapter_version("demo").unwrap(), 2);
            let rows: i64 = s
                .with_conn(|c| c.query_row("SELECT COUNT(*) FROM demo_t", [], |r| r.get(0)))
                .unwrap();
            assert_eq!(rows, 1, "the Rust step ran once");
        }
        // A later build adds a step: only it runs.
        static MORE: &[Step] = &[
            Step::sql("demo_t", "CREATE TABLE demo_t (n INTEGER NOT NULL);"),
            Step::rust("demo_row", add_row),
            Step::sql("demo_m", "ALTER TABLE demo_t ADD COLUMN m INTEGER;"),
        ];
        let s = Store::open(&path).unwrap();
        s.migrate_adapter("demo", MORE).unwrap();
        assert_eq!(s.adapter_version("demo").unwrap(), 3);
        assert_eq!(s.adapter_version("other").unwrap(), 0);
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
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
        assert_eq!(
            user_version(&conn),
            crate::migrations::core::STEPS.len() as i64
        );
    }
}
