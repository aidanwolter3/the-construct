//! `host.db`: the pods this server hosts and the tokens that open them (D46).

use std::path::Path;
use std::sync::Mutex;

use pod_core::util::now_secs;
use rusqlite::{Connection, OptionalExtension, params};

/// Migration `i` upgrades `host.db` from `user_version = i` to `i + 1`.
/// Append-only: staging has run them.
const MIGRATIONS: &[&str] = &[
    // 1 (D46): pods and their credentials. `kind` leaves room for
    // self-service pods; a credential `kind` other than `pod_token` (a
    // passkey, an OAuth login) can come later.
    "CREATE TABLE pods (
         id         TEXT PRIMARY KEY NOT NULL,
         name       TEXT NOT NULL UNIQUE,
         kind       TEXT NOT NULL CHECK (kind IN ('admin', 'temporary')),
         created_at INTEGER NOT NULL,
         expires_at INTEGER
     );
     CREATE TABLE credentials (
         token_hash TEXT PRIMARY KEY NOT NULL,
         pod_id     TEXT NOT NULL REFERENCES pods(id),
         kind       TEXT NOT NULL CHECK (kind IN ('pod_token')),
         created_at INTEGER NOT NULL
     );
     CREATE INDEX credentials_pod ON credentials(pod_id);",
    // 2 (D52): when each pod next wants its notification polls run (NULL:
    // nothing to poll), so the scheduler opens only pods that need it; and
    // server-wide secrets (the Web Push VAPID key when none is configured).
    "ALTER TABLE pods ADD COLUMN next_wake_at INTEGER;
     CREATE TABLE host_secrets (
         key   TEXT PRIMARY KEY NOT NULL,
         value TEXT NOT NULL
     );",
];

/// Who a pod is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodKind {
    /// Made by an admin (CLI); lives until deleted.
    Admin,
    /// Made by anyone on a server that allows it; deleted when it expires.
    Temporary,
}

impl PodKind {
    fn as_str(self) -> &'static str {
        match self {
            PodKind::Admin => "admin",
            PodKind::Temporary => "temporary",
        }
    }

    fn parse(s: &str) -> PodKind {
        if s == "temporary" {
            PodKind::Temporary
        } else {
            PodKind::Admin
        }
    }
}

/// A row of `pods`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodRow {
    pub id: String,
    pub name: String,
    pub kind: PodKind,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// The host's own database (one connection; every call is short).
pub struct HostStore {
    conn: Mutex<Connection>,
}

impl HostStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        loop {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let Some(step) = MIGRATIONS.get(version as usize) else {
                break;
            };
            tx.execute_batch(step)?;
            tx.pragma_update(None, "user_version", version + 1)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a pod and its token in one transaction.
    pub fn insert_pod(&self, pod: &PodRow, token_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO pods (id, name, kind, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                pod.id,
                pod.name,
                pod.kind.as_str(),
                pod.created_at,
                pod.expires_at
            ],
        )?;
        tx.execute(
            "INSERT INTO credentials (token_hash, pod_id, kind, created_at)
             VALUES (?1, ?2, 'pod_token', ?3)",
            params![token_hash, pod.id, now_secs()],
        )?;
        tx.commit()
    }

    /// The live pod a token hash opens (not expired).
    pub fn pod_for_token(&self, token_hash: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT p.id FROM credentials c JOIN pods p ON p.id = c.pod_id
                 WHERE c.token_hash = ?1 AND (p.expires_at IS NULL OR p.expires_at > ?2)",
                params![token_hash, now_secs()],
                |r| r.get(0),
            )
            .optional()
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PodRow> {
        Ok(PodRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: PodKind::parse(&r.get::<_, String>(2)?),
            created_at: r.get(3)?,
            expires_at: r.get(4)?,
        })
    }

    /// A live pod by id.
    pub fn live_pod(&self, id: &str) -> rusqlite::Result<Option<PodRow>> {
        self.conn()
            .query_row(
                "SELECT id, name, kind, created_at, expires_at FROM pods
                 WHERE id = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
                params![id, now_secs()],
                Self::row,
            )
            .optional()
    }

    pub fn pod_by_name(&self, name: &str) -> rusqlite::Result<Option<PodRow>> {
        self.conn()
            .query_row(
                "SELECT id, name, kind, created_at, expires_at FROM pods WHERE name = ?1",
                [name],
                Self::row,
            )
            .optional()
    }

    pub fn pods(&self) -> rusqlite::Result<Vec<PodRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, created_at, expires_at FROM pods ORDER BY created_at, name",
        )?;
        stmt.query_map([], Self::row)?.collect()
    }

    /// Live temporary pods.
    pub fn live_temporary_count(&self) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM pods WHERE kind = 'temporary'
             AND (expires_at IS NULL OR expires_at > ?1)",
            [now_secs()],
            |r| r.get(0),
        )
    }

    /// Replace a pod's token (rotation).
    pub fn replace_token(&self, pod_id: &str, token_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM credentials WHERE pod_id = ?1", [pod_id])?;
        tx.execute(
            "INSERT INTO credentials (token_hash, pod_id, kind, created_at)
             VALUES (?1, ?2, 'pod_token', ?3)",
            params![token_hash, pod_id, now_secs()],
        )?;
        tx.commit()
    }

    /// Stop a pod now: its tokens go, and the sweeper removes it.
    pub fn expire_pod(&self, pod_id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM credentials WHERE pod_id = ?1", [pod_id])?;
        tx.execute(
            "UPDATE pods SET expires_at = ?2 WHERE id = ?1",
            params![pod_id, now_secs()],
        )?;
        tx.commit()
    }

    /// Pods past their expiry.
    pub fn expired_pods(&self) -> rusqlite::Result<Vec<PodRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, created_at, expires_at FROM pods
             WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        )?;
        stmt.query_map([now_secs()], Self::row)?.collect()
    }

    /// Record when pod `pod_id` next wants its polls run (`None`: never).
    pub fn set_next_wake(&self, pod_id: &str, at: Option<i64>) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE pods SET next_wake_at = ?2 WHERE id = ?1",
            params![pod_id, at],
        )?;
        Ok(())
    }

    /// Live pods whose polls are due at `now`.
    pub fn due_pods(&self, now: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM pods WHERE next_wake_at IS NOT NULL AND next_wake_at <= ?1
             AND (expires_at IS NULL OR expires_at > ?1) ORDER BY next_wake_at",
        )?;
        stmt.query_map([now], |r| r.get(0))?.collect()
    }

    /// The server-wide secret `key`, made with `make` (and kept) on first use.
    pub fn secret_or(&self, key: &str, make: impl FnOnce() -> String) -> rusqlite::Result<String> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let found: Option<String> = tx
            .query_row(
                "SELECT value FROM host_secrets WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        let value = match found {
            Some(v) => v,
            None => {
                let v = make();
                tx.execute(
                    "INSERT INTO host_secrets (key, value) VALUES (?1, ?2)",
                    params![key, v],
                )?;
                v
            }
        };
        tx.commit()?;
        Ok(value)
    }

    /// Forget a pod (after its files are gone).
    pub fn delete_pod(&self, pod_id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM credentials WHERE pod_id = ?1", [pod_id])?;
        tx.execute("DELETE FROM pods WHERE id = ?1", [pod_id])?;
        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(id: &str, kind: PodKind, expires_at: Option<i64>) -> PodRow {
        PodRow {
            id: id.into(),
            name: format!("n-{id}"),
            kind,
            created_at: now_secs(),
            expires_at,
        }
    }

    #[test]
    fn tokens_open_live_pods_only() {
        let s = HostStore::open_in_memory().unwrap();
        s.insert_pod(&pod("a", PodKind::Admin, None), "ha").unwrap();
        s.insert_pod(&pod("t", PodKind::Temporary, Some(now_secs() + 60)), "ht")
            .unwrap();
        s.insert_pod(&pod("old", PodKind::Temporary, Some(now_secs() - 1)), "ho")
            .unwrap();
        assert_eq!(s.pod_for_token("ha").unwrap().as_deref(), Some("a"));
        assert_eq!(s.pod_for_token("ht").unwrap().as_deref(), Some("t"));
        assert_eq!(s.pod_for_token("ho").unwrap(), None);
        assert_eq!(s.pod_for_token("nope").unwrap(), None);
        assert_eq!(s.live_temporary_count().unwrap(), 1);
        assert_eq!(
            s.expired_pods()
                .unwrap()
                .into_iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            ["old"]
        );
        s.replace_token("a", "ha2").unwrap();
        assert_eq!(s.pod_for_token("ha").unwrap(), None);
        assert_eq!(s.pod_for_token("ha2").unwrap().as_deref(), Some("a"));
        s.expire_pod("a").unwrap();
        assert_eq!(s.pod_for_token("ha2").unwrap(), None);
        assert!(s.live_pod("a").unwrap().is_none());
        s.delete_pod("a").unwrap();
        assert!(s.pod_by_name("n-a").unwrap().is_none());
    }
}
