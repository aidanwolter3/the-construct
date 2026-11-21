//! The pod's own migration list (scope `core`, decision `named-migrations`)
//! and the code of its older steps. Steps that shipped never change: the
//! old layout system's tables are created here and dropped by
//! [`super::layout_system::STEPS`] once a pod is on the construct.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::Step;
use crate::store::pod_setting_on;

/// The core's steps, in order. Never rename or remove a deployed step; add
/// new ones at the end. (The `vN` in older comments is the step's position
/// from when steps were counted.)
pub const STEPS: &[Step] = &[
    // v1 (M5): persistent counters (the counter app; dropped in v10).
    Step::sql(
        "counters",
        "CREATE TABLE counters (
         id    TEXT PRIMARY KEY NOT NULL,
         count INTEGER NOT NULL DEFAULT 0
     );",
    ),
    // v2 (M6): Pixelfed adapter. Single tenant: one account per instance; any
    // valid pod session may use it. Sessions store SHA-256(token), never the
    // token. pixelfed_media maps opaque proxy keys to upstream media URLs.
    Step::sql(
        "pixelfed_tables",
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
    Step::sql(
        "pixelfed_refresh_tokens",
        "ALTER TABLE pixelfed_account ADD COLUMN refresh_token TEXT;
     ALTER TABLE pixelfed_account ADD COLUMN expires_at INTEGER;
     ALTER TABLE pixelfed_account ADD COLUMN redirect_uri TEXT;",
    ),
    // v4 (M6 review): the pinned owner, the upstream account id that may sign
    // in (D17). An existing stored account becomes the owner.
    Step::sql(
        "pixelfed_owner",
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
    Step::sql(
        "layout_versions",
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
    Step::rust("layout_catalog", migrate_v6),
    // v7: layouts as code (one app = one linear, append-only history; D31+).
    // The v6 catalog tables stay but are inert after this.
    Step::rust("layouts_as_code", migrate_v7),
    // v8: `file_hash`, the hash of a layout file's own content (canonical JSON
    // of its note and layout, see `layouts::file_hash`), independent of how
    // the current `UiNode` re-serializes it. The append-only check compares
    // it; NULL on rows recorded before v8 until the next startup sync adopts
    // it (`Store::sync_layout_files`).
    Step::sql(
        "layout_file_hash",
        "ALTER TABLE app_versions ADD COLUMN file_hash TEXT;",
    ),
    // v9 (D36): `schema`, the client schema a version was written for. Rows
    // from layout files stay NULL until the next startup sync adopts the
    // file's declared `"schema"`; other rows get the schema their layout's
    // vocabulary needs.
    Step::rust("layout_schema_column", migrate_v9),
    // v10 (D39): the counter app is gone. Its old `home` rows in
    // `app_versions` stay, inert (no app reads them).
    Step::sql("drop_counters", "DROP TABLE IF EXISTS counters;"),
    // v11 (D40): adapters and sign-ins. `adapter_migrations` tracks each
    // adapter's own migration list (`Store::migrate_adapter`).
    // `session_sign_ins` records which sign-ins a session completed; every
    // session that exists was made by the Pixelfed sign-in (the only one there
    // was), so each gets a `pixelfed` row and stays signed in. The v2-v4
    // `pixelfed_*` tables above are frozen history here: only the Pixelfed
    // adapter reads them.
    Step::sql(
        "adapters_and_sign_ins",
        "CREATE TABLE adapter_migrations (
         adapter TEXT PRIMARY KEY NOT NULL,
         version INTEGER NOT NULL
     );
     CREATE TABLE session_sign_ins (
         token_hash TEXT NOT NULL,
         provider   TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         PRIMARY KEY (token_hash, provider)
     );
     INSERT INTO session_sign_ins (token_hash, provider, created_at)
         SELECT token_hash, 'pixelfed', created_at FROM sessions;",
    ),
    // v12 (D46): a pinned session is a pod token's session: it never expires
    // and a logout keeps it (only its sign-ins go). `pod_settings` are the
    // host's per-pod choices (e.g. whether an adapter seeds data), written
    // before the adapters' migrations run.
    Step::sql(
        "pinned_sessions_and_pod_settings",
        "ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
     CREATE TABLE pod_settings (
         key   TEXT PRIMARY KEY NOT NULL,
         value TEXT NOT NULL
     );",
    ),
    // v13 (D49): uploads. Files a session sends before an action that needs
    // them; `owner` is the adapter's claim (kept until it releases them).
    Step::sql(
        "uploads",
        "CREATE TABLE uploads (
         id           TEXT PRIMARY KEY NOT NULL,
         token_hash   TEXT NOT NULL,
         idem_key     TEXT NOT NULL,
         content_type TEXT NOT NULL,
         size         INTEGER NOT NULL,
         sha256       TEXT NOT NULL,
         data         BLOB NOT NULL,
         owner        TEXT,
         created_at   INTEGER NOT NULL,
         released_at  INTEGER,
         UNIQUE (token_hash, idem_key)
     );
     CREATE INDEX uploads_owner ON uploads(owner);",
    ),
    // v14 (D52): notifications from apps. `notifications` holds what adapters
    // reported (`UNIQUE(app_id, dedupe_key)` makes reporting idempotent);
    // `notification_polls` is each polling adapter's schedule and backoff
    // (`stopped_for` = the sign-in it lost; a new sign-in resumes it);
    // `devices` are the clients that registered (bound to a session, with an
    // optional Web Push subscription) and `device_channel_prefs` their
    // channel choices (no row = the channel's default).
    Step::sql(
        "notifications",
        "CREATE TABLE notifications (
         id          INTEGER PRIMARY KEY AUTOINCREMENT,
         app_id      TEXT NOT NULL,
         channel     TEXT NOT NULL,
         dedupe_key  TEXT NOT NULL,
         title       TEXT NOT NULL,
         body        TEXT NOT NULL DEFAULT '',
         image_url   TEXT,
         target_uri  TEXT,
         group_key   TEXT,
         created_at  INTEGER NOT NULL,
         inserted_at INTEGER NOT NULL,
         read_at     INTEGER,
         UNIQUE (app_id, dedupe_key)
     );
     CREATE INDEX notifications_unread ON notifications (read_at, app_id);
     CREATE TABLE notification_polls (
         adapter     TEXT PRIMARY KEY NOT NULL,
         next_at     INTEGER NOT NULL,
         failures    INTEGER NOT NULL DEFAULT 0,
         last_error  TEXT,
         stopped_for TEXT
     );
     CREATE TABLE devices (
         id            TEXT PRIMARY KEY NOT NULL,
         token_hash    TEXT NOT NULL,
         platform      TEXT NOT NULL,
         name          TEXT NOT NULL DEFAULT '',
         created_at    INTEGER NOT NULL,
         last_seen     INTEGER NOT NULL,
         push_endpoint TEXT,
         push_p256dh   TEXT,
         push_auth     TEXT,
         push_failures INTEGER NOT NULL DEFAULT 0
     );
     CREATE TABLE device_channel_prefs (
         device_id TEXT NOT NULL,
         app_id    TEXT NOT NULL,
         channel   TEXT NOT NULL,
         enabled   INTEGER NOT NULL,
         PRIMARY KEY (device_id, app_id, channel)
     );",
    ),
    // v15 (docs/decisions/layout-features.md): `features`, the layout
    // features a version uses (comma-separated, sorted; worked out from the
    // layout when it is appended). Every earlier version uses none.
    Step::sql(
        "layout_features",
        "ALTER TABLE app_versions ADD COLUMN features TEXT NOT NULL DEFAULT '';",
    ),
    // v16 (decision `custom-apps-and-data-sources`): custom apps, owned by
    // the pod's user. `feeds_json` is the app's root feeds, `[[source,
    // feed], ...]`; `deleted_at` marks a soft delete (purged after 30 days).
    // Their versions are `app_versions` rows like a built-in app's;
    // `author` says who wrote a version (`user` | `claude`; NULL for files)
    // and `meta_json` holds extra facts about it (`copied_from`, ...).
    Step::sql(
        "custom_apps",
        "CREATE TABLE custom_apps (
         id         TEXT PRIMARY KEY NOT NULL,
         title      TEXT NOT NULL,
         icon       TEXT NOT NULL,
         feeds_json TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL,
         deleted_at INTEGER
     );
     ALTER TABLE app_versions ADD COLUMN author TEXT;
     ALTER TABLE app_versions ADD COLUMN meta_json TEXT;",
    ),
    // v17 (decision `named-vocabulary`): `features` names the base
    // vocabulary too, so a version's features are everything it uses.
    // Worked out again for every stored version.
    Step::rust("named_vocabulary_features", migrate_v17),
    // Decision `blueprints-and-home`: built-in apps became blueprints, which
    // pods don't list. A pod that existed before (it has layout history)
    // gets an app for each built-in it showed, on its next open
    // (`custom::seed_blueprint_apps`); a new pod starts with none.
    Step::sql(
        "blueprints",
        "INSERT INTO pod_settings (key, value)
         SELECT 'core_layouts.convert', '1' WHERE EXISTS (SELECT 1 FROM app_versions);",
    ),
    // Decision `layouts-not-apps`: home owns every feed, and a home that
    // draws a retired navigation node becomes a plain list, on the pod's
    // next open (`custom::convert_to_layouts`).
    Step::sql(
        "layouts_not_apps",
        "INSERT INTO pod_settings (key, value)
         SELECT 'layouts.convert', '1'
         WHERE EXISTS (SELECT 1 FROM custom_apps) OR EXISTS (SELECT 1 FROM app_versions);",
    ),
    // Decision `plumbing-and-porcelain`: component definitions, the
    // server's (`core:`, recorded from files) and the pod's own (`pod:`).
    Step::sql(
        "components",
        "CREATE TABLE components (
            name       TEXT NOT NULL,
            version    INTEGER NOT NULL,
            json       TEXT NOT NULL,
            note       TEXT,
            author     TEXT NOT NULL,
            file_hash  TEXT,
            meta_json  TEXT,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (name, version)
        ) WITHOUT ROWID;",
    ),
    // Decision `jacks`: each jack's state on this pod (no row: its
    // default), and the model calls the agent module made per day and
    // feature (its daily budgets).
    Step::sql(
        "jacks",
        "CREATE TABLE jacks (
            id         TEXT PRIMARY KEY NOT NULL,
            plugged    INTEGER NOT NULL,
            config     TEXT NOT NULL DEFAULT '{}',
            secrets    TEXT NOT NULL DEFAULT '{}',
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE agent_usage (
            day     TEXT NOT NULL,
            feature TEXT NOT NULL,
            calls   INTEGER NOT NULL,
            PRIMARY KEY (day, feature)
        ) WITHOUT ROWID;",
    ),
    // The MCP servers the pod is connected to (decision `mcp-jack`):
    // `config` holds OAuth endpoints, the client id and expiry; `secrets`
    // sealed tokens by field.
    Step::sql(
        "mcp_servers",
        "CREATE TABLE mcp_servers (
            name       TEXT PRIMARY KEY NOT NULL,
            url        TEXT NOT NULL,
            auth       TEXT NOT NULL,
            config     TEXT NOT NULL DEFAULT '{}',
            secrets    TEXT NOT NULL DEFAULT '{}',
            state      TEXT NOT NULL,
            last_error TEXT,
            tools      TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    ),
    // Decision `source-adapters`: a notification and a device's channel
    // choice name the layout (or blueprint) the channel belongs to, so the
    // column is `layout`. Values stay; the wire still sends `app_id` too.
    Step::sql(
        "notification_layouts",
        "ALTER TABLE notifications RENAME COLUMN app_id TO layout;
        ALTER TABLE device_channel_prefs RENAME COLUMN app_id TO layout;",
    ),
    // Decision `content-addressed-construct` (issue #300): every pod is on
    // the construct. Its tables (`crate::construct::store`), which the first
    // switch used to create; a pod switched before has them.
    Step::sql(
        "construct_tables",
        "CREATE TABLE IF NOT EXISTS objects (
             hash TEXT PRIMARY KEY,
             kind TEXT NOT NULL,
             bytes BLOB NOT NULL,
             features TEXT NOT NULL DEFAULT '',
             size INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS names (
             name TEXT PRIMARY KEY,
             hash TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS name_history (
             name TEXT NOT NULL,
             seq INTEGER NOT NULL,
             hash TEXT NOT NULL,
             at INTEGER NOT NULL,
             author TEXT NOT NULL,
             note TEXT,
             PRIMARY KEY (name, seq)
         );
         CREATE TABLE IF NOT EXISTS follows (
             name TEXT NOT NULL,
             follower TEXT NOT NULL,
             PRIMARY KEY (name, follower)
         );
         CREATE INDEX IF NOT EXISTS follows_by_follower ON follows (follower);",
    ),
    // Decision `notifications-in-a-collection`: channel switches are the
    // pod's, not each device's (a channel any device turned off stays off),
    // and a notification may name what it is about (`subject`), so it can
    // go when that does. Where a source stores the pod's notifications
    // (Collections), these tables only hand over what they held.
    Step::sql(
        "pod_channel_switches",
        "CREATE TABLE notification_switches (
             layout  TEXT NOT NULL,
             channel TEXT NOT NULL,
             enabled INTEGER NOT NULL,
             PRIMARY KEY (layout, channel)
         ) WITHOUT ROWID;
         INSERT INTO notification_switches (layout, channel, enabled)
             SELECT layout, channel, MIN(enabled) FROM device_channel_prefs GROUP BY layout, channel;
         ALTER TABLE notifications ADD COLUMN subject TEXT;
         CREATE INDEX notifications_subject ON notifications (subject);",
    ),
    // Decision `device-versions`: the devices that reach the pod (one row
    // per device id, with the build it runs), which build each ran each day,
    // the server builds this pod has run (to count how far behind a device
    // is), and agents' watches for a device to update.
    Step::sql(
        "device_versions",
        "CREATE TABLE client_devices (
             id          TEXT PRIMARY KEY NOT NULL,
             platform    TEXT NOT NULL,
             name        TEXT NOT NULL DEFAULT '',
             nickname    TEXT,
             build       TEXT NOT NULL DEFAULT '',
             features    TEXT NOT NULL DEFAULT '',
             first_seen  INTEGER NOT NULL,
             last_seen   INTEGER NOT NULL,
             build_since INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE client_days (
             day    TEXT NOT NULL,
             device TEXT NOT NULL,
             build  TEXT NOT NULL,
             pings  INTEGER NOT NULL,
             PRIMARY KEY (day, device)
         ) WITHOUT ROWID;
         CREATE TABLE server_builds (
             build      TEXT PRIMARY KEY NOT NULL,
             first_seen INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE device_watches (
             id         TEXT PRIMARY KEY NOT NULL,
             device     TEXT,
             platform   TEXT,
             until_do   TEXT NOT NULL,
             then_do    TEXT NOT NULL,
             note       TEXT,
             created_at INTEGER NOT NULL,
             expires_at INTEGER NOT NULL,
             fired_at   INTEGER,
             outcome    TEXT
         ) WITHOUT ROWID;",
    ),
    // The model tokens each day's calls of a feature spent (the agent
    // module's daily token budget).
    Step::sql(
        "agent_usage_tokens",
        "ALTER TABLE agent_usage ADD COLUMN tokens INTEGER NOT NULL DEFAULT 0;",
    ),
    // Issue #435: a device watch runs its action with the scope of the
    // credential that made it; watches made before this are taken as an MCP
    // credential's, the narrower scope.
    Step::sql(
        "device_watch_scope",
        "ALTER TABLE device_watches ADD COLUMN scope TEXT NOT NULL DEFAULT 'mcp';",
    ),
];

/// Whether the old layout system's tables are still there (dropped by
/// [`super::layout_system::STEPS`]).
fn old_tables(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'custom_apps'")?
        .exists([])
}

/// A plain SQLite error carrying `msg` (migrations that refuse to continue).
fn refuse(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(msg))
}

/// Rename a built-in app's stored data from `from` to `to`, for the
/// adapter migration of an app whose id changes (decision
/// `rename-apps-and-folders`): its layout history (every version's root id
/// and file key, so the history and pinned versions stay), its
/// notifications, devices' channel choices for it, the hidden list, and
/// custom apps' references to it (`core://Ref` parts, `copied_from`). A pod
/// with nothing under `from` is left as it is; one with a history under
/// both ids refuses.
pub fn rename_app_on(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    for table in ["notifications", "device_channel_prefs"] {
        tx.execute(
            &format!("UPDATE {table} SET layout = ?1 WHERE layout = ?2"),
            params![to, from],
        )?;
    }
    // A pod on the construct has no old layout tables any more (decision
    // `content-addressed-construct`): only its notifications move.
    if !old_tables(tx)? {
        return Ok(());
    }
    let has = |id: &str| {
        tx.prepare("SELECT 1 FROM app_versions WHERE app_id = ?1")?
            .exists([id])
    };
    if has(from)? && has(to)? {
        return Err(refuse(format!(
            "renaming app '{from}' to '{to}': '{to}' already has layout versions"
        )));
    }
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt =
            tx.prepare("SELECT version, hash, file_key FROM app_versions WHERE app_id = ?1")?;
        stmt.query_map([from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let key_prefix = format!("{from}/");
    for (version, hash, key) in rows {
        let hash = match renamed_layout(&get_blob(tx, &hash)?, from, to, false) {
            Some(json) => put_blob(tx, &json)?,
            None => hash,
        };
        let key = key.map(|k| match k.strip_prefix(&key_prefix) {
            Some(rest) => format!("{to}/{rest}"),
            None => k,
        });
        tx.execute(
            "UPDATE app_versions SET app_id = ?1, hash = ?2, file_key = ?3
             WHERE app_id = ?4 AND version = ?5",
            params![to, hash, key, from, version],
        )?;
    }
    let hidden = super::layout_system::tables::HIDDEN_SETTING;
    if let Some(list) = pod_setting_on(tx, hidden)?
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .filter(|l| l.iter().any(|id| id == from))
    {
        let list: Vec<String> = list
            .into_iter()
            .map(|id| if id == from { to.to_owned() } else { id })
            .collect();
        tx.execute(
            "UPDATE pod_settings SET value = ?1 WHERE key = ?2",
            params![serde_json::to_string(&list).expect("serializes"), hidden],
        )?;
    }
    // Custom apps' versions that reference the app.
    let custom: Vec<(String, i64, String, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT app_id, version, hash, meta_json FROM app_versions
             WHERE app_id IN (SELECT id FROM custom_apps)",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (app, version, hash, meta) in custom {
        let new_hash = match renamed_layout(&get_blob(tx, &hash)?, from, to, true) {
            Some(json) => Some(put_blob(tx, &json)?),
            None => None,
        };
        let new_meta = meta
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .filter(|m| m["copied_from"]["app"] == from)
            .map(|mut m| {
                m["copied_from"]["app"] = serde_json::Value::from(to);
                m.to_string()
            });
        if new_hash.is_some() || new_meta.is_some() {
            tx.execute(
                "UPDATE app_versions SET hash = ?1, meta_json = ?2 WHERE app_id = ?3 AND version = ?4",
                params![
                    new_hash.unwrap_or(hash),
                    new_meta.or(meta),
                    app,
                    version
                ],
            )?;
        }
    }
    Ok(())
}

/// Rename a data source in custom apps' feeds (`custom_apps.feeds_json`,
/// `[[source, feed], ...]`), for the adapter migration of a source whose
/// id changes (decision `rename-apps-and-folders`).
pub fn rename_source_on(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    if !old_tables(tx)? {
        return Ok(());
    }
    let apps: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, feeds_json FROM custom_apps")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, feeds) in apps {
        let Ok(list) = serde_json::from_str::<Vec<(String, String)>>(&feeds) else {
            continue;
        };
        if !list.iter().any(|(s, _)| s == from) {
            continue;
        }
        let list: Vec<(String, String)> = list
            .into_iter()
            .map(|(s, f)| (if s == from { to.to_owned() } else { s }, f))
            .collect();
        tx.execute(
            "UPDATE custom_apps SET feeds_json = ?1 WHERE id = ?2",
            params![serde_json::to_string(&list).expect("serializes"), id],
        )?;
    }
    Ok(())
}

/// A stored layout with app `from` renamed to `to`: the root
/// `core://Workspace` id of the app's own layouts, or (`refs`) the `app` of
/// every `core://Ref` in a custom app's. `None` when nothing changes.
fn renamed_layout(json: &str, from: &str, to: &str, refs: bool) -> Option<String> {
    fn walk(v: &mut serde_json::Value, from: &str, to: &str) -> bool {
        match v {
            serde_json::Value::Object(map) => {
                let mut changed = false;
                if map.get("type").and_then(|t| t.as_str())
                    == Some(super::layout_system::expand::REF_TYPE)
                    && map.get("app").and_then(|a| a.as_str()) == Some(from)
                {
                    map.insert("app".into(), to.into());
                    changed = true;
                }
                for child in map.values_mut() {
                    changed |= walk(child, from, to);
                }
                changed
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().fold(false, |c, i| walk(i, from, to) | c)
            }
            _ => false,
        }
    }
    let mut v: serde_json::Value = serde_json::from_str(json).ok()?;
    let changed = if refs {
        walk(&mut v, from, to)
    } else if v["type"] == "core://Workspace" && v["id"] == from {
        v["id"] = to.into();
        true
    } else {
        false
    };
    changed.then(|| canonical_json(&v.to_string()))
}

fn migrate_v9(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // A version's schema (D36), dropped again by v17 (decision
    // `named-vocabulary`): v17 works out every version's features instead.
    tx.execute_batch("ALTER TABLE app_versions ADD COLUMN schema INTEGER;")
}

fn migrate_v17(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // A database made by a build that never ran v9's column (tests that
    // build one by hand) has nothing to drop.
    let has_schema: bool = tx
        .prepare("SELECT 1 FROM pragma_table_info('app_versions') WHERE name = 'schema'")?
        .exists([])?;
    if has_schema {
        tx.execute_batch("ALTER TABLE app_versions DROP COLUMN schema;")?;
    }
    let rows: Vec<(String, i64, String)> = {
        let mut stmt = tx.prepare("SELECT app_id, version, hash FROM app_versions")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (app_id, version, hash) in rows {
        let features = features_of_json(&get_blob(tx, &hash)?);
        tx.execute(
            "UPDATE app_versions SET features = ?1 WHERE app_id = ?2 AND version = ?3",
            params![features, app_id, version],
        )?;
    }
    Ok(())
}

/// The layout features a stored layout uses ([`protocol::required_features`]),
/// as stored: comma-separated, sorted; `legacy_nodes` (removed) for a legacy
/// layout this build cannot read.
fn features_of_json(json: &str) -> String {
    serde_json::from_str::<protocol::UiNode>(json)
        .map(|n| protocol::required_features(&n).join(","))
        .unwrap_or_else(|_| protocol::feature::LEGACY_NODES.to_owned())
}

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
/// A version created from a layout file (`crates/<adapter>/layouts/<app>/NNNN.json`).
pub const SOURCE_FILE: &str = "file";
/// `reason` of a revision that switched the active layout.
pub const REASON_ACTIVATE: &str = "activate";
/// The provider recorded on rollbacks (v5 and `POST .../rollback`).
const ROLLBACK: &str = "rollback";

/// zstd level for layout blobs (see D26 for the measurements).
pub const BLOB_ZSTD_LEVEL: i32 = 19;

/// SHA-256 hex of a layout's canonical JSON (its blob key).
pub fn content_hash(json: &str) -> String {
    crate::util::hex(&Sha256::digest(json.as_bytes()))
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

pub(crate) fn get_blob(conn: &Connection, hash: &str) -> rusqlite::Result<String> {
    let (encoding, data, raw_len): (String, Vec<u8>, i64) = conn.query_row(
        "SELECT encoding, data, raw_len FROM layout_blobs WHERE hash = ?1",
        [hash],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    decode_blob(&encoding, data, raw_len)
}
