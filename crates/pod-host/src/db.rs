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
    // 3 (decision `mcp-connector`): a second credential kind, `mcp`: what
    // Claude holds after the OAuth flow (or `server pods mcp`), good only for
    // its pod's `/v1/mcp`; `label` says whose it is. SQLite can't change a
    // CHECK, so the table is rebuilt. `oauth_clients` are the dynamically
    // registered OAuth clients (RFC 7591) and `oauth_codes` the pending
    // authorization codes (hashed, single use, minutes long).
    "CREATE TABLE credentials_v3 (
         token_hash TEXT PRIMARY KEY NOT NULL,
         pod_id     TEXT NOT NULL REFERENCES pods(id),
         kind       TEXT NOT NULL CHECK (kind IN ('pod_token', 'mcp')),
         created_at INTEGER NOT NULL,
         label      TEXT
     );
     INSERT INTO credentials_v3 (token_hash, pod_id, kind, created_at)
         SELECT token_hash, pod_id, kind, created_at FROM credentials;
     DROP TABLE credentials;
     ALTER TABLE credentials_v3 RENAME TO credentials;
     CREATE INDEX credentials_pod ON credentials(pod_id);
     CREATE TABLE oauth_clients (
         client_id     TEXT PRIMARY KEY NOT NULL,
         name          TEXT NOT NULL,
         redirect_uris TEXT NOT NULL,
         created_at    INTEGER NOT NULL
     );
     CREATE TABLE oauth_codes (
         code_hash    TEXT PRIMARY KEY NOT NULL,
         client_id    TEXT NOT NULL,
         pod_id       TEXT NOT NULL,
         redirect_uri TEXT NOT NULL,
         challenge    TEXT NOT NULL,
         expires_at   INTEGER NOT NULL
     );",
];

/// What a credential is for (`credentials.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// The pod's own token (D46): every route of the pod.
    PodToken,
    /// Claude's (decision `mcp-connector`): only the pod's `/v1/mcp`.
    Mcp,
}

impl CredentialKind {
    fn parse(s: &str) -> Self {
        if s == "mcp" {
            CredentialKind::Mcp
        } else {
            CredentialKind::PodToken
        }
    }
}

/// A registered OAuth client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClient {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
}

/// A pending authorization code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCode {
    pub client_id: String,
    pub pod_id: String,
    pub redirect_uri: String,
    pub challenge: String,
}

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
    /// Open (creating if needed, mode 0600) `host.db`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        pod_core::util::private_db_file(path).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(1),
                Some(format!(
                    "making {} private (mode 0600): {e}",
                    path.display()
                )),
            )
        })?;
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

    /// The live pod a pod token's hash opens (not expired). An MCP
    /// credential opens nothing here: see [`Self::credential`].
    pub fn pod_for_token(&self, token_hash: &str) -> rusqlite::Result<Option<String>> {
        Ok(self
            .credential(token_hash)?
            .filter(|(_, k)| *k == CredentialKind::PodToken)
            .map(|(id, _)| id))
    }

    /// The live pod a credential's hash belongs to, and its kind.
    pub fn credential(
        &self,
        token_hash: &str,
    ) -> rusqlite::Result<Option<(String, CredentialKind)>> {
        self.conn()
            .query_row(
                "SELECT p.id, c.kind FROM credentials c JOIN pods p ON p.id = c.pod_id
                 WHERE c.token_hash = ?1 AND (p.expires_at IS NULL OR p.expires_at > ?2)",
                params![token_hash, now_secs()],
                |r| Ok((r.get(0)?, CredentialKind::parse(&r.get::<_, String>(1)?))),
            )
            .optional()
    }

    /// Add an MCP credential to a pod.
    pub fn insert_mcp_credential(
        &self,
        pod_id: &str,
        token_hash: &str,
        label: &str,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO credentials (token_hash, pod_id, kind, created_at, label)
             VALUES (?1, ?2, 'mcp', ?3, ?4)",
            params![token_hash, pod_id, now_secs(), label],
        )?;
        Ok(())
    }

    /// Revoke every MCP credential of a pod; how many went.
    pub fn revoke_mcp(&self, pod_id: &str) -> rusqlite::Result<usize> {
        self.conn().execute(
            "DELETE FROM credentials WHERE pod_id = ?1 AND kind = 'mcp'",
            [pod_id],
        )
    }

    /// `(label, created_at)` of a pod's MCP credentials.
    pub fn mcp_credentials(&self, pod_id: &str) -> rusqlite::Result<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(label, ''), created_at FROM credentials
             WHERE pod_id = ?1 AND kind = 'mcp' ORDER BY created_at",
        )?;
        stmt.query_map([pod_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    /// Register an OAuth client.
    pub fn insert_client(&self, c: &OAuthClient) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO oauth_clients (client_id, name, redirect_uris, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                c.client_id,
                c.name,
                serde_json::to_string(&c.redirect_uris).unwrap_or_default(),
                now_secs()
            ],
        )?;
        Ok(())
    }

    pub fn client(&self, client_id: &str) -> rusqlite::Result<Option<OAuthClient>> {
        self.conn()
            .query_row(
                "SELECT client_id, name, redirect_uris FROM oauth_clients WHERE client_id = ?1",
                [client_id],
                |r| {
                    let uris: String = r.get(2)?;
                    Ok(OAuthClient {
                        client_id: r.get(0)?,
                        name: r.get(1)?,
                        redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
                    })
                },
            )
            .optional()
    }

    /// Store an authorization code (by hash) valid for `ttl_secs`.
    pub fn insert_code(
        &self,
        code_hash: &str,
        c: &OAuthCode,
        ttl_secs: i64,
    ) -> rusqlite::Result<()> {
        let now = now_secs();
        let conn = self.conn();
        conn.execute("DELETE FROM oauth_codes WHERE expires_at <= ?1", [now])?;
        conn.execute(
            "INSERT INTO oauth_codes (code_hash, client_id, pod_id, redirect_uri, challenge, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                code_hash,
                c.client_id,
                c.pod_id,
                c.redirect_uri,
                c.challenge,
                now + ttl_secs
            ],
        )?;
        Ok(())
    }

    /// Take (and delete) a live authorization code: single use.
    pub fn take_code(&self, code_hash: &str) -> rusqlite::Result<Option<OAuthCode>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found = tx
            .query_row(
                "SELECT client_id, pod_id, redirect_uri, challenge FROM oauth_codes
                 WHERE code_hash = ?1 AND expires_at > ?2",
                params![code_hash, now_secs()],
                |r| {
                    Ok(OAuthCode {
                        client_id: r.get(0)?,
                        pod_id: r.get(1)?,
                        redirect_uri: r.get(2)?,
                        challenge: r.get(3)?,
                    })
                },
            )
            .optional()?;
        tx.execute("DELETE FROM oauth_codes WHERE code_hash = ?1", [code_hash])?;
        tx.commit()?;
        Ok(found)
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

    /// For monitoring (decision `server-monitoring`): live admin pods, live temporary
    /// pods, and the MCP credentials of live pods.
    pub fn monitoring_counts(&self) -> rusqlite::Result<(i64, i64, i64)> {
        let conn = self.conn();
        let live = "expires_at IS NULL OR expires_at > ?1";
        let (admin, temporary) = conn.query_row(
            &format!(
                "SELECT COALESCE(SUM(kind = 'admin'), 0), COALESCE(SUM(kind = 'temporary'), 0)
                 FROM pods WHERE {live}"
            ),
            [now_secs()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mcp = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM credentials WHERE kind = 'mcp'
                 AND pod_id IN (SELECT id FROM pods WHERE {live})"
            ),
            [now_secs()],
            |r| r.get(0),
        )?;
        Ok((admin, temporary, mcp))
    }

    /// Replace a pod's token (rotation).
    pub fn replace_token(&self, pod_id: &str, token_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // Only the pod token: Claude's MCP credentials stay (revoke them
        // with `server pods revoke-mcp`).
        tx.execute(
            "DELETE FROM credentials WHERE pod_id = ?1 AND kind = 'pod_token'",
            [pod_id],
        )?;
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
        tx.execute("DELETE FROM oauth_codes WHERE pod_id = ?1", [pod_id])?;
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
        tx.execute("DELETE FROM oauth_codes WHERE pod_id = ?1", [pod_id])?;
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

    #[test]
    fn migration_3_keeps_pod_tokens_and_adds_mcp_credentials() {
        // A host.db as staging has it: migrations 1 and 2, one pod token.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO pods (id, name, kind, created_at) VALUES ('p1', 'mine', 'admin', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO credentials (token_hash, pod_id, kind, created_at) VALUES ('h1', 'p1', 'pod_token', 1)",
            [],
        )
        .unwrap();
        let store = HostStore::init(conn).unwrap();
        assert_eq!(store.pod_for_token("h1").unwrap().as_deref(), Some("p1"));
        store.insert_mcp_credential("p1", "m1", "Claude").unwrap();
        assert_eq!(
            store.credential("m1").unwrap(),
            Some(("p1".to_owned(), CredentialKind::Mcp))
        );
        assert_eq!(
            store.pod_for_token("m1").unwrap(),
            None,
            "an MCP credential is no pod token"
        );
        // Rotation replaces only the pod token.
        store.replace_token("p1", "h2").unwrap();
        assert_eq!(store.pod_for_token("h1").unwrap(), None);
        assert!(store.credential("m1").unwrap().is_some());
        assert_eq!(store.revoke_mcp("p1").unwrap(), 1);
        assert!(store.credential("m1").unwrap().is_none());
    }
}
