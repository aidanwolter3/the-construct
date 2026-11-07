//! The adapter's tables (decision `named-migrations`):
//! - `agents_runner`: the runner's URL and the pod's token for it, sealed
//!   ([`pod_core::secrets`]); one row, or none when signed out.
//! - `agents_state`: the last [`State`] (snapshot plus events), and how the
//!   connection is going, so the feeds answer while the runner is away.
//! - `agents_events`: recent events, for each session's timeline.
//! - `agents_launches`: Tasks rows a `start_task` was sent for and not yet
//!   answered by a session event, so a queued row is sent once.

use pod_core::store::{Step, Store};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::cap::{Event, Runner};
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
pub static MIGRATIONS: &[Step] = &[Step::sql("schema", SCHEMA)];

/// Events kept in all, and per session's timeline.
pub const MAX_EVENTS: i64 = 5000;
pub const TIMELINE: i64 = 200;

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
        "DELETE FROM agents_runner; DELETE FROM agents_state; DELETE FROM agents_events;
         DELETE FROM agents_launches;",
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

/// Save `state` after a good answer from the runner, with `events` (the
/// ones just applied) for the timelines. Nothing is saved once signed out.
pub fn save(store: &Store, state: &State, events: &[Event], now: i64) -> rusqlite::Result<()> {
    store.transaction(|tx| {
        let n = tx.execute(
            "UPDATE agents_state SET state_json = ?1, connected = 1, last_error = NULL,
             last_ok_at = ?2, updated_at = ?2 WHERE id = 1",
            params![serde_json::to_string(state).expect("serializes"), now],
        )?;
        if n == 0 {
            return Ok(());
        }
        for e in events {
            let mut json = serde_json::json!({"seq": e.seq, "ts": e.ts, "type": e.kind,
                "status": e.status, "sprite": e.sprite, "task_id": e.task_id, "data": e.data});
            if json.to_string().len() > 8_000 {
                json["data"] = Value::Null;
            }
            tx.execute(
                "INSERT OR IGNORE INTO agents_events (seq, session_id, ts, json) VALUES (?1, ?2, ?3, ?4)",
                params![e.seq, e.session_id, e.ts, json.to_string()],
            )?;
            if let Some(sid) = &e.session_id {
                tx.execute(
                    "DELETE FROM agents_events WHERE session_id = ?1 AND seq NOT IN
                     (SELECT seq FROM agents_events WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2)",
                    params![sid, TIMELINE],
                )?;
            }
        }
        if !events.is_empty() {
            tx.execute(
                "DELETE FROM agents_events WHERE seq NOT IN
                 (SELECT seq FROM agents_events ORDER BY seq DESC LIMIT ?1)",
                [MAX_EVENTS],
            )?;
        }
        Ok(())
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

/// Session `id`'s recent events, oldest first: `(seq, json)`.
pub fn timeline(store: &Store, id: &str) -> rusqlite::Result<Vec<Value>> {
    store.with_conn(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT json FROM (SELECT seq, json FROM agents_events WHERE session_id = ?1
             ORDER BY seq DESC LIMIT ?2) ORDER BY seq",
        )?;
        stmt.query_map(params![id, TIMELINE], |r| r.get::<_, String>(0))?
            .map(|r| r.map(|j| serde_json::from_str(&j).unwrap_or(Value::Null)))
            .collect()
    })
}

/// The task ids a launch is pending for, in `collection`.
pub fn launches(store: &Store, collection: &str) -> rusqlite::Result<Vec<String>> {
    store.with_conn(|c| {
        let mut stmt =
            c.prepare_cached("SELECT task_id FROM agents_launches WHERE collection = ?1")?;
        stmt.query_map([collection], |r| r.get(0))?.collect()
    })
}

pub fn add_launch(
    store: &Store,
    collection: &str,
    task_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR REPLACE INTO agents_launches (task_id, collection, at) VALUES (?1, ?2, ?3)",
            params![task_id, collection, now],
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
