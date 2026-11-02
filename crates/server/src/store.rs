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
];

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
