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
    // v3 (M6, real Pixelfed): the refresh token Passport issues with the access
    // token, its expiry, and the redirect_uri whose app (client credentials)
    // issued it, so the pod can refresh on an upstream 401.
    "ALTER TABLE pixelfed_account ADD COLUMN refresh_token TEXT;
     ALTER TABLE pixelfed_account ADD COLUMN expires_at INTEGER;
     ALTER TABLE pixelfed_account ADD COLUMN redirect_uri TEXT;",
    // v4 (M6 review): the pinned owner, the upstream account id that may sign
    // in (D17). An existing stored account becomes the owner.
    "CREATE TABLE pixelfed_owner (
         instance   TEXT PRIMARY KEY NOT NULL,
         account_id TEXT NOT NULL,
         created_at INTEGER NOT NULL
     );
     INSERT INTO pixelfed_owner (instance, account_id, created_at)
         SELECT instance, account_id, created_at FROM pixelfed_account;",
    // v5 (M7): versioned workspace layouts (D22). Append-only history; `json`
    // is the workspace root UiNode. Version 1 (the built-in layout) is
    // inserted lazily by `layouts::latest`.
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
];

/// One row of [`Store::layout_versions`] (metadata only, no layout JSON).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayoutVersionInfo {
    pub version: i64,
    pub created_at: i64,
    pub prompt: Option<String>,
    pub provider: Option<String>,
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

fn append_layout_on(
    conn: &Connection,
    workspace_id: &str,
    json: &str,
    meta: &LayoutMeta,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "INSERT INTO layout_versions
             (workspace_id, version, json, prompt, provider, patch_json, created_at)
         SELECT ?1, COALESCE(MAX(version), 0) + 1, ?2, ?3, ?4, ?5, ?6
         FROM layout_versions WHERE workspace_id = ?1
         RETURNING version",
        params![
            workspace_id,
            json,
            meta.prompt,
            meta.provider,
            meta.patch_json,
            now_secs()
        ],
        |r| r.get(0),
    )
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

    // ---------- Layout versions (v5) ----------

    /// Insert version 1 for `workspace_id` unless any version exists.
    /// Returns whether a row was inserted.
    pub fn seed_layout(&self, workspace_id: &str, json: &str) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "INSERT INTO layout_versions (workspace_id, version, json, created_at)
             SELECT ?1, 1, ?2, ?3
             WHERE NOT EXISTS (SELECT 1 FROM layout_versions WHERE workspace_id = ?1)",
            params![workspace_id, json, now_secs()],
        )? > 0)
    }

    /// The newest `(version, json)` for `workspace_id`.
    pub fn latest_layout(&self, workspace_id: &str) -> rusqlite::Result<Option<(i64, String)>> {
        self.conn()
            .query_row(
                "SELECT version, json FROM layout_versions WHERE workspace_id = ?1
                 ORDER BY version DESC LIMIT 1",
                [workspace_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// The layout JSON of one version.
    pub fn layout_at(&self, workspace_id: &str, version: i64) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT json FROM layout_versions WHERE workspace_id = ?1 AND version = ?2",
                params![workspace_id, version],
                |r| r.get(0),
            )
            .optional()
    }

    /// Append a version (max + 1, atomically) and return its number.
    pub fn append_layout(
        &self,
        workspace_id: &str,
        json: &str,
        meta: &LayoutMeta,
    ) -> rusqlite::Result<i64> {
        append_layout_on(&self.conn(), workspace_id, json, meta)
    }

    /// Append a version only if `expected` is still the newest one (one
    /// atomic statement); `None` if another version was appended meanwhile.
    pub fn append_layout_if_latest(
        &self,
        workspace_id: &str,
        expected: i64,
        json: &str,
        meta: &LayoutMeta,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "INSERT INTO layout_versions
                     (workspace_id, version, json, prompt, provider, patch_json, created_at)
                 SELECT ?1, MAX(version) + 1, ?2, ?3, ?4, ?5, ?6
                 FROM layout_versions WHERE workspace_id = ?1
                 HAVING MAX(version) = ?7
                 RETURNING version",
                params![
                    workspace_id,
                    json,
                    meta.prompt,
                    meta.provider,
                    meta.patch_json,
                    now_secs(),
                    expected
                ],
                |r| r.get(0),
            )
            .optional()
    }

    /// Copy version `from` into a new version; `None` if `from` is unknown.
    pub fn copy_layout(
        &self,
        workspace_id: &str,
        from: i64,
        meta: &LayoutMeta,
    ) -> rusqlite::Result<Option<(i64, String)>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let json: Option<String> = tx
            .query_row(
                "SELECT json FROM layout_versions WHERE workspace_id = ?1 AND version = ?2",
                params![workspace_id, from],
                |r| r.get(0),
            )
            .optional()?;
        let Some(json) = json else {
            return Ok(None);
        };
        let version = append_layout_on(&tx, workspace_id, &json, meta)?;
        tx.commit()?;
        Ok(Some((version, json)))
    }

    /// All versions of `workspace_id`, newest first.
    pub fn layout_versions(&self, workspace_id: &str) -> rusqlite::Result<Vec<LayoutVersionInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT version, created_at, prompt, provider FROM layout_versions
             WHERE workspace_id = ?1 ORDER BY version DESC",
        )?;
        stmt.query_map([workspace_id], |r| {
            Ok(LayoutVersionInfo {
                version: r.get(0)?,
                created_at: r.get(1)?,
                prompt: r.get(2)?,
                provider: r.get(3)?,
            })
        })?
        .collect()
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
            for (i, sql) in MIGRATIONS[..3].iter().enumerate() {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64)
                    .unwrap();
                tx.commit().unwrap();
            }
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

    #[test]
    fn layout_versions_are_append_only_and_per_workspace() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.seed_layout("a", "{\"v\":1}").unwrap());
        assert!(!s.seed_layout("a", "{\"other\":1}").unwrap(), "seeded once");
        let meta = LayoutMeta {
            prompt: Some("p".into()),
            provider: Some("rules".into()),
            patch_json: Some("[]".into()),
        };
        assert_eq!(s.append_layout("a", "{\"v\":2}", &meta).unwrap(), 2);
        assert_eq!(s.append_layout("b", "{\"b\":1}", &meta).unwrap(), 1);
        assert_eq!(s.latest_layout("a").unwrap(), Some((2, "{\"v\":2}".into())));
        assert_eq!(
            s.copy_layout("a", 1, &LayoutMeta::default()).unwrap(),
            Some((3, "{\"v\":1}".into()))
        );
        assert_eq!(s.copy_layout("a", 9, &LayoutMeta::default()).unwrap(), None);
        // Compare-and-append: only on top of the newest version.
        assert_eq!(s.append_layout_if_latest("a", 2, "{}", &meta).unwrap(), None);
        assert_eq!(s.append_layout_if_latest("a", 3, "{\"v\":4}", &meta).unwrap(), Some(4));
        assert_eq!(s.append_layout_if_latest("a", 3, "{}", &meta).unwrap(), None);
        assert_eq!(s.append_layout_if_latest("none", 1, "{}", &meta).unwrap(), None);
        assert_eq!(s.layout_at("a", 2).unwrap().as_deref(), Some("{\"v\":2}"));
        let versions: Vec<i64> = s
            .layout_versions("a")
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(versions, [4, 3, 2, 1]);
        assert_eq!(
            s.layout_versions("a").unwrap()[2].provider.as_deref(),
            Some("rules")
        );
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
