//! The Pixelfed adapter's tables: registered OAuth apps, the stored upstream
//! account and its pinned owner per instance, and the media proxy's keys
//! (core migrations v2-v4, which predate per-adapter migrations; D40); and
//! which instance each session signed in to ([`MIGRATIONS`], D45) and the
//! notification poll's cursor per instance (D52).
//!
//! [`PixelfedStore`] adds these queries to the core's [`Store`].
//!
//! Upstream credentials (`access_token`, `refresh_token`, `client_secret`)
//! are sealed with the store's key ([`pod_core::secrets`]); a row that does
//! not open (the key is missing or wrong) reads as absent, so the user signs
//! in again. [`reseal`] seals rows written before the key was set.

use pod_core::secrets::Secrets;
use pod_core::store::{Migration, Store, open_session_on};
use pod_core::util::now_secs;
use rusqlite::{Connection, OptionalExtension, params};

use crate::adapter::SIGN_IN;

/// The adapter's own migrations (append-only; staging has run them).
pub static MIGRATIONS: &[Migration] = &[
    // 1 (D45): the instance each session signed in to. A session without a
    // row (one from before D45) uses the default instance. Rows of sessions
    // that are gone are pruned at the next sign-in.
    Migration::Sql(
        "CREATE TABLE pixelfed_session_instance (
             token_hash TEXT PRIMARY KEY NOT NULL,
             instance   TEXT NOT NULL
         );",
    ),
    // 2 (D50): posts on their way to Pixelfed (crate::posting).
    Migration::Sql(crate::posting::MIGRATION),
    // 3 (D52): the notification poll's position per instance: the newest
    // upstream notification id seen (`last_id`, None while there were none)
    // and when the first poll ran (its backlog was stored quietly).
    Migration::Sql(
        "CREATE TABLE IF NOT EXISTS pixelfed_notify_cursor (
             instance       TEXT PRIMARY KEY NOT NULL,
             last_id        TEXT,
             initialized_at INTEGER NOT NULL
         );",
    ),
    // 4 (decision `rename-apps-and-folders`): the apps' new ids, `pixelfed`
    // (was `pixelfed_home`) and `pixelfed_sample_no_login` (was
    // `pixelfed_demo`, also its data source).
    Migration::Rust(rename_apps),
];

fn rename_apps(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    use pod_core::store::{rename_app_on, rename_source_on};
    rename_app_on(tx, "pixelfed_home", crate::apps::PIXELFED.id)?;
    rename_app_on(tx, "pixelfed_demo", crate::apps::SAMPLE_NAME)?;
    rename_source_on(tx, "pixelfed_demo", crate::apps::SAMPLE_NAME)?;
    // The sample's adapter had no migrations; nothing else is stored by it.
    tx.execute(
        "DELETE FROM adapter_migrations WHERE adapter = 'pixelfed_demo'",
        [],
    )?;
    Ok(())
}

/// Outcome of [`PixelfedStore::sign_in_pixelfed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// Account stored and the session's `pixelfed` sign-in recorded.
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

fn access_ctx(instance: &str) -> String {
    format!("pixelfed_account.access_token:{instance}")
}

fn refresh_ctx(instance: &str) -> String {
    format!("pixelfed_account.refresh_token:{instance}")
}

fn client_secret_ctx(instance: &str, redirect_uri: &str) -> String {
    format!("pixelfed_apps.client_secret:{instance} {redirect_uri}")
}

/// Open a stored secret; `None` (logged) when it doesn't open.
fn open_or_warn(
    secrets: &Secrets,
    what: &str,
    instance: &str,
    ctx: &str,
    stored: &str,
) -> Option<String> {
    match secrets.open(ctx, stored) {
        Ok(plain) => Some(plain),
        Err(e) => {
            tracing::error!(
                instance,
                "stored Pixelfed {what} can't be read ({e}); treating it as signed out"
            );
            None
        }
    }
}

fn save_account_on(
    conn: &Connection,
    secrets: &Secrets,
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
            secrets.seal(&access_ctx(instance), &account.access_token),
            account.account_id,
            account.username,
            now_secs(),
            account
                .refresh_token
                .as_deref()
                .map(|t| secrets.seal(&refresh_ctx(instance), t)),
            account.expires_at,
            account.redirect_uri,
        ],
    )?;
    Ok(())
}

/// The Pixelfed adapter's queries on the pod's store.
pub trait PixelfedStore {
    fn pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
    ) -> rusqlite::Result<Option<PixelfedApp>>;
    fn save_pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
        app: &PixelfedApp,
    ) -> rusqlite::Result<()>;
    /// Forget a cached app (the upstream no longer knows it).
    fn delete_pixelfed_app(&self, instance: &str, redirect_uri: &str) -> rusqlite::Result<()>;
    fn pixelfed_account(&self, instance: &str) -> rusqlite::Result<Option<PixelfedAccount>>;
    /// Store (replacing any previous) the upstream account for `instance`.
    /// Does not touch the pinned owner (token refreshes use this).
    fn save_pixelfed_account(
        &self,
        instance: &str,
        account: &PixelfedAccount,
    ) -> rusqlite::Result<()>;
    /// The pinned owner's upstream account id, if anyone has signed in.
    fn pixelfed_owner(&self, instance: &str) -> rusqlite::Result<Option<String>>;
    /// Sign-in, atomically: if no owner is pinned on `instance` (or
    /// `replace_owner`), `account` becomes its owner; if another account is
    /// pinned, nothing changes and [`SignIn::NotOwner`] is returned.
    /// Otherwise the account is stored, the session `token_hash` gets the
    /// `pixelfed` sign-in (it is created if new; sessions older than
    /// `session_ttl_secs` are pruned) and now uses `instance`.
    fn sign_in_pixelfed(
        &self,
        instance: &str,
        account: &PixelfedAccount,
        token_hash: &str,
        replace_owner: bool,
        session_ttl_secs: i64,
    ) -> rusqlite::Result<SignIn>;
    /// The instance session `token_hash` signed in to (None: from before D45).
    fn session_instance(&self, token_hash: &str) -> rusqlite::Result<Option<String>>;
    fn delete_pixelfed_account(&self, instance: &str) -> rusqlite::Result<()>;
    /// Compare-and-delete: forget the stored account only if it still holds
    /// `access_token` (a concurrent sign-in or refresh may have replaced it).
    /// Returns whether a row was deleted.
    fn delete_pixelfed_account_if_token(
        &self,
        instance: &str,
        access_token: &str,
    ) -> rusqlite::Result<bool>;
    /// Remember proxy keys (idempotent).
    fn save_media_keys<'a>(
        &self,
        keys: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> rusqlite::Result<()>;
    fn media_url(&self, key: &str) -> rusqlite::Result<Option<String>>;
    /// Every stored upstream account, by instance (D52 polls each).
    fn pixelfed_accounts(&self) -> rusqlite::Result<Vec<(String, PixelfedAccount)>>;
    /// The notification cursor of `instance`: `None` before its first poll,
    /// else the newest upstream notification id seen (None: none yet).
    fn notify_cursor(&self, instance: &str) -> rusqlite::Result<Option<Option<String>>>;
    /// Move the cursor of `instance` (created on the first poll).
    fn save_notify_cursor(&self, instance: &str, last_id: Option<&str>) -> rusqlite::Result<()>;
    /// Forget the cursor of `instance` (its account is gone; the next
    /// sign-in starts with a quiet first poll again).
    fn delete_notify_cursor(&self, instance: &str) -> rusqlite::Result<()>;
}

const ACCOUNT_COLS: &str =
    "access_token, account_id, username, refresh_token, expires_at, redirect_uri";

/// A stored account row, its secrets still sealed.
fn row_to_account(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<PixelfedAccount> {
    Ok(PixelfedAccount {
        access_token: r.get(at)?,
        account_id: r.get(at + 1)?,
        username: r.get(at + 2)?,
        refresh_token: r.get(at + 3)?,
        expires_at: r.get(at + 4)?,
        redirect_uri: r.get(at + 5)?,
    })
}

/// `sealed` with its tokens opened; `None` (logged) when one doesn't open.
fn open_account(
    secrets: &Secrets,
    instance: &str,
    sealed: PixelfedAccount,
) -> Option<PixelfedAccount> {
    let access_token = open_or_warn(
        secrets,
        "access token",
        instance,
        &access_ctx(instance),
        &sealed.access_token,
    )?;
    let refresh_token = match &sealed.refresh_token {
        None => None,
        Some(t) => Some(open_or_warn(
            secrets,
            "refresh token",
            instance,
            &refresh_ctx(instance),
            t,
        )?),
    };
    Some(PixelfedAccount {
        access_token,
        refresh_token,
        ..sealed
    })
}

/// Seal (again) every stored credential that is plaintext or sealed with an
/// older key, under the store's current key: rows from before sealing, or
/// written while no key was set. Runs when a pod opens; values that don't
/// open are left as they are. Returns how many values were sealed.
pub fn reseal(store: &Store) -> rusqlite::Result<usize> {
    let secrets = store.secrets().clone();
    if !secrets.enabled() {
        return Ok(0);
    }
    store.transaction(|tx| {
        let mut done = 0;
        let mut reseal_one = |stored: &str, ctx: &str| -> Option<String> {
            if !secrets.needs_reseal(stored) {
                return None;
            }
            let plain = secrets.open(ctx, stored).ok()?;
            done += 1;
            Some(secrets.seal(ctx, &plain))
        };
        let accounts: Vec<(String, String, Option<String>)> = {
            let mut stmt =
                tx.prepare("SELECT instance, access_token, refresh_token FROM pixelfed_account")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (instance, access, refresh) in accounts {
            let new_access = reseal_one(&access, &access_ctx(&instance));
            let new_refresh = refresh
                .as_deref()
                .and_then(|t| reseal_one(t, &refresh_ctx(&instance)));
            if new_access.is_some() || new_refresh.is_some() {
                tx.execute(
                    "UPDATE pixelfed_account SET access_token = ?2, refresh_token = ?3
                     WHERE instance = ?1",
                    params![
                        instance,
                        new_access.unwrap_or(access),
                        new_refresh.or(refresh)
                    ],
                )?;
            }
        }
        let apps: Vec<(String, String, String)> = {
            let mut stmt =
                tx.prepare("SELECT instance, redirect_uri, client_secret FROM pixelfed_apps")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (instance, redirect_uri, secret) in apps {
            if let Some(new) = reseal_one(&secret, &client_secret_ctx(&instance, &redirect_uri)) {
                tx.execute(
                    "UPDATE pixelfed_apps SET client_secret = ?3
                     WHERE instance = ?1 AND redirect_uri = ?2",
                    params![instance, redirect_uri, new],
                )?;
            }
        }
        Ok(done)
    })
}

impl PixelfedStore for Store {
    fn pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
    ) -> rusqlite::Result<Option<PixelfedApp>> {
        let row: Option<(String, String)> = self.with_conn(|c| {
            c.query_row(
                "SELECT client_id, client_secret FROM pixelfed_apps
                 WHERE instance = ?1 AND redirect_uri = ?2",
                [instance, redirect_uri],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })?;
        // A secret that doesn't open reads as no app: the next sign-in
        // registers a new one, replacing the row.
        Ok(row.and_then(|(client_id, sealed)| {
            let client_secret = open_or_warn(
                self.secrets(),
                "client secret",
                instance,
                &client_secret_ctx(instance, redirect_uri),
                &sealed,
            )?;
            Some(PixelfedApp {
                client_id,
                client_secret,
            })
        }))
    }

    fn save_pixelfed_app(
        &self,
        instance: &str,
        redirect_uri: &str,
        app: &PixelfedApp,
    ) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO pixelfed_apps (instance, redirect_uri, client_id, client_secret)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    instance,
                    redirect_uri,
                    app.client_id,
                    self.secrets().seal(
                        &client_secret_ctx(instance, redirect_uri),
                        &app.client_secret
                    )
                ],
            )
        })?;
        Ok(())
    }

    fn delete_pixelfed_app(&self, instance: &str, redirect_uri: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM pixelfed_apps WHERE instance = ?1 AND redirect_uri = ?2",
                [instance, redirect_uri],
            )
        })?;
        Ok(())
    }

    fn pixelfed_account(&self, instance: &str) -> rusqlite::Result<Option<PixelfedAccount>> {
        let sealed = self.with_conn(|c| {
            c.query_row(
                &format!("SELECT {ACCOUNT_COLS} FROM pixelfed_account WHERE instance = ?1"),
                [instance],
                |r| row_to_account(r, 0),
            )
            .optional()
        })?;
        Ok(sealed.and_then(|a| open_account(self.secrets(), instance, a)))
    }

    fn save_pixelfed_account(
        &self,
        instance: &str,
        account: &PixelfedAccount,
    ) -> rusqlite::Result<()> {
        self.with_conn(|c| save_account_on(c, self.secrets(), instance, account))
    }

    fn pixelfed_owner(&self, instance: &str) -> rusqlite::Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT account_id FROM pixelfed_owner WHERE instance = ?1",
                [instance],
                |r| r.get(0),
            )
            .optional()
        })
    }

    fn sign_in_pixelfed(
        &self,
        instance: &str,
        account: &PixelfedAccount,
        token_hash: &str,
        replace_owner: bool,
        session_ttl_secs: i64,
    ) -> rusqlite::Result<SignIn> {
        self.transaction(|tx| {
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
            tx.execute(
                "INSERT INTO pixelfed_owner (instance, account_id, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(instance) DO UPDATE SET account_id = excluded.account_id,
                     created_at = CASE WHEN account_id = excluded.account_id THEN created_at
                                       ELSE excluded.created_at END",
                params![instance, account.account_id, now_secs()],
            )?;
            save_account_on(tx, self.secrets(), instance, account)?;
            open_session_on(tx, token_hash, SIGN_IN, session_ttl_secs)?;
            tx.execute(
                "DELETE FROM pixelfed_session_instance
                 WHERE token_hash NOT IN (SELECT token_hash FROM sessions)",
                [],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO pixelfed_session_instance (token_hash, instance)
                 VALUES (?1, ?2)",
                params![token_hash, instance],
            )?;
            Ok(SignIn::Ok)
        })
    }

    fn session_instance(&self, token_hash: &str) -> rusqlite::Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT instance FROM pixelfed_session_instance WHERE token_hash = ?1",
                [token_hash],
                |r| r.get(0),
            )
            .optional()
        })
    }

    fn delete_pixelfed_account(&self, instance: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM pixelfed_account WHERE instance = ?1",
                [instance],
            )
        })?;
        Ok(())
    }

    fn delete_pixelfed_account_if_token(
        &self,
        instance: &str,
        access_token: &str,
    ) -> rusqlite::Result<bool> {
        // Sealed values differ on every write, so compare the plaintext.
        self.transaction(|tx| {
            let stored: Option<String> = tx
                .query_row(
                    "SELECT access_token FROM pixelfed_account WHERE instance = ?1",
                    [instance],
                    |r| r.get(0),
                )
                .optional()?;
            let matches = stored.is_some_and(|s| {
                self.secrets()
                    .open(&access_ctx(instance), &s)
                    .is_ok_and(|t| t == access_token)
            });
            if matches {
                tx.execute(
                    "DELETE FROM pixelfed_account WHERE instance = ?1",
                    [instance],
                )?;
            }
            Ok(matches)
        })
    }

    fn save_media_keys<'a>(
        &self,
        keys: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> rusqlite::Result<()> {
        self.transaction(|tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO pixelfed_media (key, url) VALUES (?1, ?2)",
            )?;
            for (key, url) in keys {
                stmt.execute([key, url])?;
            }
            Ok(())
        })
    }

    fn media_url(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT url FROM pixelfed_media WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()
        })
    }

    fn pixelfed_accounts(&self) -> rusqlite::Result<Vec<(String, PixelfedAccount)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(&format!(
                "SELECT instance, {ACCOUNT_COLS} FROM pixelfed_account ORDER BY instance"
            ))?;
            let rows: Vec<(String, PixelfedAccount)> = stmt
                .query_map([], |r| Ok((r.get(0)?, row_to_account(r, 1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            // Accounts that don't open are skipped (logged), like signed out.
            Ok(rows
                .into_iter()
                .filter_map(|(i, a)| open_account(self.secrets(), &i, a).map(|a| (i, a)))
                .collect())
        })
    }

    fn notify_cursor(&self, instance: &str) -> rusqlite::Result<Option<Option<String>>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT last_id FROM pixelfed_notify_cursor WHERE instance = ?1",
                [instance],
                |r| r.get(0),
            )
            .optional()
        })
    }

    fn save_notify_cursor(&self, instance: &str, last_id: Option<&str>) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO pixelfed_notify_cursor (instance, last_id, initialized_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(instance) DO UPDATE SET last_id = excluded.last_id",
                params![instance, last_id, now_secs()],
            )
        })?;
        Ok(())
    }

    fn delete_notify_cursor(&self, instance: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM pixelfed_notify_cursor WHERE instance = ?1",
                [instance],
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.migrate_adapter("pixelfed", MIGRATIONS).unwrap();
        s
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
            pod_core::store::migrate_to(&mut conn, 3).unwrap();
            save_account_on(&conn, &Secrets::none(), "i", &account("7", "t")).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.pixelfed_owner("i").unwrap().as_deref(), Some("7"));
    }

    #[test]
    fn sign_in_pins_the_first_owner() {
        let s = store();
        let ttl = 86_400;
        assert_eq!(
            s.sign_in_pixelfed("i", &account("1", "a"), "h1", false, ttl)
                .unwrap(),
            SignIn::Ok
        );
        assert_eq!(s.session_sign_ins("h1").unwrap(), [SIGN_IN]);
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
    fn sign_in_records_the_session_instance() {
        let s = store();
        let ttl = 86_400;
        s.sign_in_pixelfed("a", &account("1", "x"), "h", false, ttl)
            .unwrap();
        assert_eq!(s.session_instance("h").unwrap().as_deref(), Some("a"));
        // Another instance: its own owner, and the session moves there.
        s.sign_in_pixelfed("b", &account("9", "y"), "h", false, ttl)
            .unwrap();
        assert_eq!(s.session_instance("h").unwrap().as_deref(), Some("b"));
        assert_eq!(s.pixelfed_owner("a").unwrap().as_deref(), Some("1"));
        assert_eq!(s.pixelfed_owner("b").unwrap().as_deref(), Some("9"));
        // A session from before D45 has no row; rows of deleted sessions go at the next sign-in.
        assert_eq!(s.session_instance("never").unwrap(), None);
        assert!(s.delete_session("h").unwrap());
        s.sign_in_pixelfed("a", &account("1", "z"), "h2", false, ttl)
            .unwrap();
        assert_eq!(s.session_instance("h").unwrap(), None);
    }

    #[test]
    fn sign_in_adds_pixelfed_to_an_existing_session() {
        let s = store();
        s.create_session("h").unwrap();
        s.attach_sign_in("h", "other").unwrap();
        s.sign_in_pixelfed("i", &account("1", "a"), "h", false, 86_400)
            .unwrap();
        assert_eq!(s.session_sign_ins("h").unwrap(), ["other", SIGN_IN]);
    }

    #[test]
    fn sign_in_prunes_expired_sessions() {
        let s = store();
        s.create_session("old").unwrap();
        s.with_conn(|c| c.execute("UPDATE sessions SET created_at = created_at - 1000", []))
            .unwrap();
        s.sign_in_pixelfed("i", &account("1", "a"), "new", false, 999)
            .unwrap();
        assert!(!s.session_exists("old").unwrap());
        assert!(s.session_valid("new", 999).unwrap());
    }

    #[test]
    fn account_compare_and_delete() {
        let s = store();
        s.save_pixelfed_account("i", &account("1", "new")).unwrap();
        assert!(!s.delete_pixelfed_account_if_token("i", "old").unwrap());
        assert!(s.pixelfed_account("i").unwrap().is_some());
        assert!(s.delete_pixelfed_account_if_token("i", "new").unwrap());
        assert!(s.pixelfed_account("i").unwrap().is_none());
    }

    #[test]
    fn accounts_and_notify_cursors() {
        let s = store();
        s.save_pixelfed_account("b", &account("2", "y")).unwrap();
        s.save_pixelfed_account("a", &account("1", "x")).unwrap();
        let all = s.pixelfed_accounts().unwrap();
        assert_eq!(
            all.iter()
                .map(|(i, a)| (i.as_str(), a.access_token.as_str()))
                .collect::<Vec<_>>(),
            [("a", "x"), ("b", "y")]
        );
        assert_eq!(s.notify_cursor("a").unwrap(), None);
        s.save_notify_cursor("a", None).unwrap();
        assert_eq!(s.notify_cursor("a").unwrap(), Some(None));
        s.save_notify_cursor("a", Some("42")).unwrap();
        assert_eq!(s.notify_cursor("a").unwrap(), Some(Some("42".into())));
        assert_eq!(s.notify_cursor("b").unwrap(), None);
        s.delete_notify_cursor("a").unwrap();
        assert_eq!(s.notify_cursor("a").unwrap(), None);
    }

    fn sealed_store(keys: &[[u8; 32]]) -> Store {
        let s = Store::open_in_memory()
            .unwrap()
            .with_secrets(Secrets::from_keys(keys));
        s.migrate_adapter("pixelfed", MIGRATIONS).unwrap();
        s
    }

    fn raw_tokens(s: &Store) -> (String, Option<String>, String) {
        s.with_conn(|c| -> rusqlite::Result<_> {
            let (a, r) = c.query_row(
                "SELECT access_token, refresh_token FROM pixelfed_account WHERE instance = 'i'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let secret = c.query_row(
                "SELECT client_secret FROM pixelfed_apps WHERE instance = 'i'",
                [],
                |r| r.get(0),
            )?;
            Ok((a, r, secret))
        })
        .unwrap()
    }

    fn seed(s: &Store) {
        let a = PixelfedAccount {
            refresh_token: Some("refresh".into()),
            ..account("1", "access")
        };
        s.save_pixelfed_account("i", &a).unwrap();
        let app = PixelfedApp {
            client_id: "cid".into(),
            client_secret: "csecret".into(),
        };
        s.save_pixelfed_app("i", "r", &app).unwrap();
    }

    #[test]
    fn credentials_are_sealed_at_rest() {
        let s = sealed_store(&[[7; 32]]);
        seed(&s);
        let (access, refresh, secret) = raw_tokens(&s);
        for raw in [&access, refresh.as_ref().unwrap(), &secret] {
            assert!(Secrets::is_sealed(raw), "{raw}");
        }
        assert!(!access.contains("access") && !secret.contains("csecret"));
        let a = s.pixelfed_account("i").unwrap().unwrap();
        assert_eq!(
            (a.access_token.as_str(), a.refresh_token.as_deref()),
            ("access", Some("refresh"))
        );
        assert_eq!(
            s.pixelfed_app("i", "r").unwrap().unwrap().client_secret,
            "csecret"
        );
        assert_eq!(s.pixelfed_accounts().unwrap()[0].1.access_token, "access");
        // A sealed value moved to another row doesn't open there.
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO pixelfed_account (instance, access_token, account_id, username, created_at)
                 VALUES ('j', ?1, '2', 'u2', 0)",
                [&access],
            )
        })
        .unwrap();
        assert_eq!(s.pixelfed_account("j").unwrap(), None);
        // Compare-and-delete compares the plaintext.
        assert!(!s.delete_pixelfed_account_if_token("i", &access).unwrap());
        assert!(s.delete_pixelfed_account_if_token("i", "access").unwrap());
        assert_eq!(s.pixelfed_account("i").unwrap(), None);
    }

    #[test]
    fn reseal_seals_plaintext_rows_and_rotates() {
        // Rows written without a key (staging before sealing) ...
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let s = Store::open(&path).unwrap().with_secrets(Secrets::none());
            s.migrate_adapter("pixelfed", MIGRATIONS).unwrap();
            seed(&s);
            assert_eq!(raw_tokens(&s).0, "access");
            assert_eq!(reseal(&s).unwrap(), 0, "no key: nothing to do");
        }
        // ... are sealed once a key is set, and the sign-in keeps working.
        let old = Store::open(&path)
            .unwrap()
            .with_secrets(Secrets::from_keys(&[[1; 32]]));
        assert_eq!(
            old.pixelfed_account("i").unwrap().unwrap().access_token,
            "access"
        );
        assert_eq!(reseal(&old).unwrap(), 3);
        assert_eq!(reseal(&old).unwrap(), 0, "idempotent");
        let sealed_old = raw_tokens(&old);
        assert!(Secrets::is_sealed(&sealed_old.0));
        drop(old);
        // Rotation: a new first key; the old one still opens until resealed.
        let rotated = Store::open(&path)
            .unwrap()
            .with_secrets(Secrets::from_keys(&[[2; 32], [1; 32]]));
        assert_eq!(
            rotated.pixelfed_account("i").unwrap().unwrap().access_token,
            "access"
        );
        assert_eq!(reseal(&rotated).unwrap(), 3);
        assert_ne!(raw_tokens(&rotated).0, sealed_old.0);
        drop(rotated);
        let new_only = Store::open(&path)
            .unwrap()
            .with_secrets(Secrets::from_keys(&[[2; 32]]));
        assert_eq!(
            new_only
                .pixelfed_app("i", "r")
                .unwrap()
                .unwrap()
                .client_secret,
            "csecret"
        );
    }

    #[test]
    fn missing_or_wrong_key_reads_as_signed_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.db");
        {
            let s = Store::open(&path)
                .unwrap()
                .with_secrets(Secrets::from_keys(&[[1; 32]]));
            s.migrate_adapter("pixelfed", MIGRATIONS).unwrap();
            seed(&s);
        }
        for secrets in [Secrets::none(), Secrets::from_keys(&[[9; 32]])] {
            let s = Store::open(&path).unwrap().with_secrets(secrets);
            assert_eq!(s.pixelfed_account("i").unwrap(), None);
            assert_eq!(s.pixelfed_accounts().unwrap(), []);
            assert_eq!(s.pixelfed_app("i", "r").unwrap(), None);
            assert!(!s.delete_pixelfed_account_if_token("i", "access").unwrap());
            assert_eq!(
                reseal(&s).unwrap(),
                0,
                "values that don't open stay as they are"
            );
            // The rows are kept: setting the right key again restores them.
            assert!(Secrets::is_sealed(&raw_tokens(&s).0));
        }
        let s = Store::open(&path)
            .unwrap()
            .with_secrets(Secrets::from_keys(&[[1; 32]]));
        assert_eq!(
            s.pixelfed_account("i").unwrap().unwrap().access_token,
            "access"
        );
    }

    #[test]
    fn media_keys_round_trip() {
        let s = store();
        let (k, u) = ("k".to_owned(), "https://x/a.jpg".to_owned());
        s.save_media_keys([(&k, &u)]).unwrap();
        s.save_media_keys([(&k, &u)]).unwrap();
        assert_eq!(
            s.media_url("k").unwrap().as_deref(),
            Some("https://x/a.jpg")
        );
        assert_eq!(s.media_url("missing").unwrap(), None);
    }
}
