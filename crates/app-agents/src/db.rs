//! The adapter's tables (decision `named-migrations`):
//! - `agents_runner`: the runner's URL and the pod's token for it, sealed
//!   ([`pod_core::secrets`]); one row, or none when signed out.
//! - `agents_state`: the mirror of the runner's records ([`State`]), and how
//!   the connection is going, so the feeds answer while the runner is away.
//! - `agents_launches`: Tasks rows a `start_run` was sent for, with the run
//!   it answered, so a queued row is sent once.
//! - `agents_marks`: what the Tasks wiring did once per run (a `stop_run`
//!   sent, an ended run noted in the row's diagnosis).
//!
//! Step `cap_v2` (decision `agents-runs`) drops v1's event timeline (a run's
//! log is read from the runner on demand) and empties the stored state, so
//! the follower takes a v2 snapshot.

use pod_core::store::{Step, Store};
use rusqlite::{OptionalExtension, params};

use crate::cap::Runner;
use crate::state::State;

const SCHEMA: &str = "
CREATE TABLE agents_runner (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    url TEXT NOT NULL,
    token TEXT NOT NULL,
    signed_in_at INTEGER NOT NULL
);
CREATE TABLE agents_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    state_json TEXT NOT NULL,
    connected INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_ok_at INTEGER,
    updated_at INTEGER NOT NULL
);
CREATE TABLE agents_events (
    seq INTEGER PRIMARY KEY,
    session_id TEXT,
    ts INTEGER NOT NULL,
    json TEXT NOT NULL
);
CREATE INDEX agents_events_session ON agents_events (session_id, seq);
CREATE TABLE agents_launches (
    task_id TEXT PRIMARY KEY,
    collection TEXT NOT NULL,
    at INTEGER NOT NULL
);
";

/// By name: add steps at the end; never rename, edit or remove one that
/// shipped.
pub static MIGRATIONS: &[Step] = &[Step::sql("schema", SCHEMA), Step::sql("cap_v2", CAP_V2)];

const CAP_V2: &str = "
DROP TABLE agents_events;
DELETE FROM agents_launches;
ALTER TABLE agents_launches ADD COLUMN run_id TEXT NOT NULL DEFAULT '';
CREATE TABLE agents_marks (
    key TEXT PRIMARY KEY,
    at INTEGER NOT NULL
);
UPDATE agents_state SET state_json = '{}', connected = 0, last_ok_at = NULL;
";

/// Marks kept (the newest).
pub const MAX_MARKS: i64 = 2000;

const TOKEN_CONTEXT: &str = "agents_runner.token:1";

/// The runner the pod is signed in to.
pub fn runner(store: &Store) -> rusqlite::Result<Option<Runner>> {
    let row: Option<(String, String)> = store.with_conn(|c| {
        c.query_row(
            "SELECT url, token FROM agents_runner WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    })?;
    Ok(row.and_then(
        |(url, sealed)| match store.secrets().open(TOKEN_CONTEXT, &sealed) {
            Ok(token) => Some(Runner { url, token }),
            Err(e) => {
                tracing::error!(error = %e, "the agent runner's token can't be opened");
                None
            }
        },
    ))
}

/// Store `runner` (sealing its token) and the state it answered, replacing
/// whatever was there.
pub fn sign_in(store: &Store, runner: &Runner, state: &State, now: i64) -> rusqlite::Result<()> {
    let sealed = store.secrets().seal(TOKEN_CONTEXT, &runner.token);
    store.transaction(|tx| {
        clear_on(tx)?;
        tx.execute(
            "INSERT INTO agents_runner (id, url, token, signed_in_at) VALUES (1, ?1, ?2, ?3)",
            params![runner.url, sealed, now],
        )?;
        tx.execute(
            "INSERT INTO agents_state (id, state_json, connected, last_ok_at, updated_at)
             VALUES (1, ?1, 1, ?2, ?2)",
            params![serde_json::to_string(state).expect("serializes"), now],
        )?;
        Ok(())
    })
}

fn clear_on(tx: &rusqlite::Connection) -> rusqlite::Result<()> {
    tx.execute_batch(
        "DELETE FROM agents_runner; DELETE FROM agents_state; DELETE FROM agents_launches;
         DELETE FROM agents_marks;",
    )
}

/// Forget the runner and everything it sent.
pub fn sign_out(store: &Store) -> rusqlite::Result<()> {
    store.transaction(|tx| clear_on(tx))
}

/// Re-seal a token stored as plaintext or under an old key.
pub fn reseal(store: &Store) -> rusqlite::Result<()> {
    let sealed: Option<String> = store.with_conn(|c| {
        c.query_row("SELECT token FROM agents_runner WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()
    })?;
    let Some(sealed) = sealed else { return Ok(()) };
    if !store.secrets().needs_reseal(&sealed) {
        return Ok(());
    }
    let Ok(plain) = store.secrets().open(TOKEN_CONTEXT, &sealed) else {
        return Ok(());
    };
    let again = store.secrets().seal(TOKEN_CONTEXT, &plain);
    store.with_conn(|c| {
        c.execute("UPDATE agents_runner SET token = ?1 WHERE id = 1", [again])
            .map(|_| ())
    })
}

/// How the connection to the runner is going.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub connected: bool,
    pub last_error: Option<String>,
    /// Unix milliseconds of the last good answer.
    pub last_ok_at: Option<i64>,
}

/// The stored state and health (`None` when signed out).
pub fn state(store: &Store) -> rusqlite::Result<Option<(State, Health)>> {
    store.with_conn(|c| {
        c.query_row(
            "SELECT state_json, connected, last_error, last_ok_at FROM agents_state WHERE id = 1",
            [],
            |r| {
                let json: String = r.get(0)?;
                Ok((
                    serde_json::from_str(&json).unwrap_or_default(),
                    Health {
                        connected: r.get::<_, i64>(1)? != 0,
                        last_error: r.get(2)?,
                        last_ok_at: r.get(3)?,
                    },
                ))
            },
        )
        .optional()
    })
}

/// Save `state` after a good answer from the runner. Nothing is saved once
/// signed out.
pub fn save(store: &Store, state: &State, now: i64) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE agents_state SET state_json = ?1, connected = 1, last_error = NULL,
             last_ok_at = ?2, updated_at = ?2 WHERE id = 1",
            params![serde_json::to_string(state).expect("serializes"), now],
        )
        .map(|_| ())
    })
}

/// Whether the pod is signed in to a runner (without opening the token).
pub fn signed_in(store: &Store) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        c.query_row("SELECT 1 FROM agents_runner WHERE id = 1", [], |_| Ok(()))
            .optional()
            .map(|r| r.is_some())
    })
}

/// Record that the runner can't be reached (the state stays as it was).
pub fn mark_unreachable(store: &Store, error: &str, now: i64) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE agents_state SET connected = 0, last_error = ?1, updated_at = ?2 WHERE id = 1",
            params![error, now],
        )
        .map(|_| ())
    })
}

/// The launches sent for `collection`'s rows: `(task_id, run_id, at)`.
pub fn launches(store: &Store, collection: &str) -> rusqlite::Result<Vec<(String, String, i64)>> {
    store.with_conn(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT task_id, run_id, at FROM agents_launches WHERE collection = ?1",
        )?;
        stmt.query_map([collection], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
    })
}

/// Remember that `start_run` for row `task_id` answered run `run_id`.
pub fn add_launch(
    store: &Store,
    collection: &str,
    task_id: &str,
    run_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR REPLACE INTO agents_launches (task_id, collection, at, run_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, collection, now, run_id],
        )
        .map(|_| ())
    })
}

pub fn remove_launch(store: &Store, task_id: &str) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute("DELETE FROM agents_launches WHERE task_id = ?1", [task_id])
            .map(|_| ())
    })
}

/// Set mark `key` unless it was set; whether it is new.
pub fn mark(store: &Store, key: &str, now: i64) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        let n = c.execute(
            "INSERT OR IGNORE INTO agents_marks (key, at) VALUES (?1, ?2)",
            params![key, now],
        )?;
        if n > 0 {
            c.execute(
                "DELETE FROM agents_marks WHERE key NOT IN
                 (SELECT key FROM agents_marks ORDER BY at DESC, rowid DESC LIMIT ?1)",
                [MAX_MARKS],
            )?;
        }
        Ok(n > 0)
    })
}

/// Whether mark `key` is set.
pub fn marked(store: &Store, key: &str) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        c.query_row("SELECT 1 FROM agents_marks WHERE key = ?1", [key], |_| {
            Ok(())
        })
        .optional()
        .map(|r| r.is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_v2_step_upgrades_a_v1_database() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(SCHEMA)?;
                c.execute_batch(
                    "INSERT INTO agents_state (id, state_json, connected, last_ok_at, updated_at)
                     VALUES (1, '{\"cursor\": 42, \"sessions\": {}}', 1, 5, 5);
                     INSERT INTO agents_launches (task_id, collection, at) VALUES ('r1', 'tasks', 1);",
                )?;
                c.execute_batch(CAP_V2)
            })
            .unwrap();
        let (st, h) = state(&store).unwrap().unwrap();
        assert_eq!(st.cursor, 0);
        assert!(!h.connected && h.last_ok_at.is_none());
        assert!(launches(&store, "tasks").unwrap().is_empty());
        assert!(mark(&store, "stop:run_1", 1).unwrap());
        assert!(!mark(&store, "stop:run_1", 2).unwrap());
        assert!(marked(&store, "stop:run_1").unwrap());
    }
}
