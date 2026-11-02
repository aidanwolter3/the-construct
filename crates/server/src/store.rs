//! Server persistence: one SQLite database (`$CANVAS_DATA_DIR/server.db`).
//!
//! The schema is versioned with `PRAGMA user_version`. To add tables in a later
//! milestone, append a step to [`MIGRATIONS`]; each step runs once, in order,
//! inside a transaction that also bumps `user_version`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

/// Migration `i` upgrades the schema from `user_version = i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // v1 (M5): persistent counters.
    "CREATE TABLE counters (
         id    TEXT PRIMARY KEY NOT NULL,
         count INTEGER NOT NULL DEFAULT 0
     );",
    // v2 (M6): Pixelfed adapter. Single tenant: one account per instance; any
    // valid pod session may use it. Sessions store SHA-256(token), never the
    // token. pixelfed_media maps opaque proxy keys to upstream media URLs.
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
];

/// A registered OAuth app on the upstream instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedApp {
    pub client_id: String,
    pub client_secret: String,
}

/// The stored upstream account (and its access token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedAccount {
    pub access_token: String,
    pub account_id: String,
    pub username: String,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
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
                "SELECT access_token, account_id, username FROM pixelfed_account
                 WHERE instance = ?1",
                [instance],
                |r| {
                    Ok(PixelfedAccount {
                        access_token: r.get(0)?,
                        account_id: r.get(1)?,
                        username: r.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// Store (replacing any previous) the upstream account for `instance`.
    pub fn save_pixelfed_account(
        &self,
        instance: &str,
        account: &PixelfedAccount,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO pixelfed_account
                 (instance, access_token, account_id, username, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                instance,
                account.access_token,
                account.account_id,
                account.username,
                now_secs()
            ],
        )?;
        Ok(())
    }

    pub fn delete_pixelfed_account(&self, instance: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM pixelfed_account WHERE instance = ?1",
            [instance],
        )?;
        Ok(())
    }

    pub fn create_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (token_hash, created_at) VALUES (?1, ?2)",
            params![token_hash, now_secs()],
        )?;
        Ok(())
    }

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
