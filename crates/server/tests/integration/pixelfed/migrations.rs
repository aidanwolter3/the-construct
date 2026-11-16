//! A Pixelfed session from before the pod's v11 migration, reopened by
//! this build: the old core migrations still run on a pod of that age.

use super::*;

/// Staging's upgrade (D40): a database from before sign-ins existed (v10, a
/// signed-in session) keeps its session signed in to Pixelfed after v11.
#[tokio::test]
async fn a_session_from_before_v11_stays_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let env = env_with(server::open_dir(dir.path()).unwrap()).await;
    let session = sign_in(&env).await;
    env.pod.handle.abort();
    {
        // Back to the v10 shape: no sign-in rows, no adapter versions, no
        // migration names, none of the tables adapters added later, and the
        // (empty) layout tables of the time.
        let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
        conn.execute_batch(
            "DROP TABLE session_sign_ins; DROP TABLE adapter_migrations; DROP TABLE schema_migrations;
             ALTER TABLE sessions DROP COLUMN pinned;
             ALTER TABLE pixelfed_clients RENAME TO pixelfed_apps; PRAGMA user_version = 10;
             CREATE TABLE layout_blobs (hash TEXT PRIMARY KEY NOT NULL, encoding TEXT NOT NULL,
                 raw_len INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE TABLE app_versions (app_id TEXT NOT NULL, version INTEGER NOT NULL,
                 hash TEXT NOT NULL REFERENCES layout_blobs (hash), source TEXT NOT NULL, note TEXT,
                 file_key TEXT UNIQUE, created_at INTEGER NOT NULL, file_hash TEXT, schema INTEGER,
                 PRIMARY KEY (app_id, version)) WITHOUT ROWID;",
        )
        .unwrap();
        let later: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND (name LIKE 'recipes%' OR name LIKE 'collections%' OR name LIKE 'agents%' OR name IN ('pixelfed_session_instance', 'pixelfed_pending_posts'))
                 OR (type = 'table' AND name IN ('pod_settings', 'uploads', 'notifications',
                     'notification_polls', 'devices', 'device_channel_prefs',
                     'pixelfed_notify_cursor', 'custom_apps', 'components', 'jacks', 'agent_usage',
                     'mcp_servers'))",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        for t in later {
            conn.execute_batch(&format!("DROP TABLE {t}")).unwrap();
        }
    }
    let state = server::open_dir(dir.path())
        .unwrap()
        .with_pixelfed(PixelfedConfig::new(&env.fake.url, None));
    assert_eq!(
        state
            .store()
            .session_sign_ins(&server::session::session_hash(&session))
            .unwrap(),
        [server::pixelfed::SIGN_IN]
    );
    let pod = spawn_pod(state).await;
    let (status, body) = get_home(&pod, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["queries"]["timeline:home"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
