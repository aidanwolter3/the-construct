//! The adapter's tables (decision `named-migrations`):
//! - `agents_runner`: the runner's URL and the pod's token for it, sealed
//!   ([`pod_core::secrets`]); one row, or none when signed out.
//! - `agents_state`: the mirror of the runner's records ([`State`]), the
//!   event log's cursor (its own column, so the follower resumes from it
//!   after any gap) and how the connection is going, so the feeds answer
//!   while the runner is away.
//! - `agents_launches`: Tasks rows a `start_run` was sent (or is being
//!   sent) for, with its request id and the run it answered, so a queued row
//!   is sent once and a resend after a crash reuses the request id.
//! - `agents_marks`: what the Tasks wiring did once per run (a `stop_run`
//!   sent, an ended run noted in the row's diagnosis).
//! - `agents_schedules`: the pod's schedules (decision `agents-cap-v3`), and
//!   `agents_occurrences`: each occurrence the timer took up (sent, waiting
//!   to be sent, or skipped as missed).
//!
//! Step `cap_v2` (decision `agents-runs`) drops v1's event timeline and
//! empties the stored state. Step `cap_v3` (decision `agents-cap-v3`) keeps
//! only the notices already sent, adds the cursor column (0: a v3 snapshot
//! is taken), gives launches a request id, drops the pod-only reviewed
//! marks (`agents_reviewed`: the runner owns a run's outcome now) and adds
//! the schedule tables.

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
pub static MIGRATIONS: &[Step] = &[
    Step::sql("schema", SCHEMA),
    Step::sql("cap_v2", CAP_V2),
    Step::sql("reviewed", REVIEWED),
    Step::rust("cap_v3", cap_v3),
    Step::sql("chained", CHAINED),
];

/// A schedule can start when another one's run ends (decision
/// `chained-schedules`): the name of the schedule it follows.
const CHAINED: &str = "
ALTER TABLE agents_schedules ADD COLUMN after_schedule TEXT NOT NULL DEFAULT '';
";

const CAP_V3: &str = "
ALTER TABLE agents_state ADD COLUMN cursor INTEGER NOT NULL DEFAULT 0;
DROP TABLE agents_reviewed;
DELETE FROM agents_launches;
ALTER TABLE agents_launches ADD COLUMN request_id TEXT NOT NULL DEFAULT '';
CREATE TABLE agents_schedules (
    id              TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL UNIQUE,
    cron            TEXT NOT NULL,
    timezone        TEXT NOT NULL,
    workspace       TEXT NOT NULL,
    model           TEXT NOT NULL DEFAULT '',
    max_runtime_min INTEGER NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 1,
    prompt          TEXT NOT NULL,
    looked_until    INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE TABLE agents_occurrences (
    schedule_id TEXT NOT NULL,
    occurrence  TEXT NOT NULL,
    due_at      INTEGER NOT NULL,
    state       TEXT NOT NULL,
    request_id  TEXT NOT NULL,
    run_id      TEXT NOT NULL DEFAULT '',
    manual      INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (schedule_id, occurrence, manual)
) WITHOUT ROWID;
";

/// v2's records mean nothing to v3: keep only the notices already sent (so
/// nothing is notified twice), and start from a v3 snapshot.
fn cap_v3(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(CAP_V3)?;
    let old: Option<String> = tx
        .query_row(
            "SELECT state_json FROM agents_state WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        let notified = serde_json::from_str::<serde_json::Value>(&old)
            .ok()
            .and_then(|v| v.get("notified").cloned())
            .filter(serde_json::Value::is_array)
            .unwrap_or_else(|| serde_json::json!([]));
        let kept = serde_json::json!({"notified": notified}).to_string();
        tx.execute(
            "UPDATE agents_state SET state_json = ?1, cursor = 0, connected = 0, last_ok_at = NULL",
            [kept],
        )?;
    }
    Ok(())
}

const REVIEWED: &str = "
CREATE TABLE agents_reviewed (
    run_id TEXT PRIMARY KEY,
    at INTEGER NOT NULL
);
";

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
            "INSERT INTO agents_state (id, state_json, cursor, connected, last_ok_at, updated_at)
             VALUES (1, ?1, ?2, 1, ?3, ?3)",
            params![
                serde_json::to_string(state).expect("serializes"),
                state.cursor,
                now
            ],
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

/// The stored state and health (`None` when signed out), its cursor read
/// from its column.
pub fn state(store: &Store) -> rusqlite::Result<Option<(State, Health)>> {
    store.with_conn(|c| {
        c.query_row(
            "SELECT state_json, connected, last_error, last_ok_at, cursor FROM agents_state
             WHERE id = 1",
            [],
            |r| {
                let json: String = r.get(0)?;
                let mut st = serde_json::from_str::<State>(&json).unwrap_or_default();
                st.cursor = r.get(4)?;
                Ok((
                    st,
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

/// Save `state` (and its cursor) after a good answer from the runner.
/// Nothing is saved once signed out. A run stored at a newer version than
/// `state` has (a route stored the runner's answer while the follower held
/// an older copy) is kept, so a save never takes a run back.
pub fn save(store: &Store, state: &State, now: i64) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        let stored: Option<State> = c
            .query_row(
                "SELECT state_json FROM agents_state WHERE id = 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|j| serde_json::from_str(&j).ok());
        let mut state = state.clone();
        for (id, run) in stored.map(|s| s.runs).unwrap_or_default() {
            if state.runs.get(&id).is_some_and(|r| r.version < run.version) {
                state.runs.insert(id, run);
            }
        }
        c.execute(
            "UPDATE agents_state SET state_json = ?1, cursor = ?2, connected = 1,
             last_error = NULL, last_ok_at = ?3, updated_at = ?3 WHERE id = 1",
            params![
                serde_json::to_string(&state).expect("serializes"),
                state.cursor,
                now
            ],
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

/// A `start_run` for a Tasks row: its request id, and the run it answered
/// (empty until answered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub task_id: String,
    pub request_id: String,
    pub run_id: String,
    pub at: i64,
}

/// The launches sent (or being sent) for `collection`'s rows.
pub fn launches(store: &Store, collection: &str) -> rusqlite::Result<Vec<Launch>> {
    store.with_conn(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT task_id, request_id, run_id, at FROM agents_launches WHERE collection = ?1",
        )?;
        stmt.query_map([collection], |r| {
            Ok(Launch {
                task_id: r.get(0)?,
                request_id: r.get(1)?,
                run_id: r.get(2)?,
                at: r.get(3)?,
            })
        })?
        .collect()
    })
}

/// Remember that `start_run` for row `task_id` is sent with `request_id`
/// (before it is sent: a resend after a crash reuses it) and, once
/// answered, the run `run_id` it answered.
pub fn put_launch(
    store: &Store,
    collection: &str,
    task_id: &str,
    request_id: &str,
    run_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR REPLACE INTO agents_launches (task_id, collection, at, run_id, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task_id, collection, now, run_id, request_id],
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

// ---------- schedules ----------

/// A schedule the pod owns (decision `agents-cap-v3`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Schedule {
    /// `sch_` and random letters: the work item id of its runs.
    pub id: String,
    pub name: String,
    pub cron: String,
    /// An IANA zone name.
    pub timezone: String,
    pub workspace: String,
    /// Empty: the Sprite's default.
    pub model: String,
    pub max_runtime_min: i64,
    pub enabled: bool,
    pub prompt: String,
    /// The name of the schedule this one follows (decision
    /// `chained-schedules`): its next time is sent early, as soon as that
    /// schedule's run ends. Empty: none.
    pub after: String,
    /// Milliseconds: occurrences up to here were taken up (sent, queued to
    /// send or skipped).
    pub looked_until: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

const SCHEDULE_COLS: &str = "id, name, cron, timezone, workspace, model, max_runtime_min, \
    enabled, prompt, looked_until, created_at, updated_at, after_schedule";

fn schedule_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: r.get(0)?,
        name: r.get(1)?,
        cron: r.get(2)?,
        timezone: r.get(3)?,
        workspace: r.get(4)?,
        model: r.get(5)?,
        max_runtime_min: r.get(6)?,
        enabled: r.get::<_, i64>(7)? != 0,
        prompt: r.get(8)?,
        looked_until: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        after: r.get(12)?,
    })
}

/// Every schedule, by name.
pub fn schedules(store: &Store) -> rusqlite::Result<Vec<Schedule>> {
    store.with_conn(|c| {
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {SCHEDULE_COLS} FROM agents_schedules ORDER BY name"
        ))?;
        stmt.query_map([], schedule_from)?.collect()
    })
}

/// The schedule named `key` (its name, or its id).
pub fn schedule(store: &Store, key: &str) -> rusqlite::Result<Option<Schedule>> {
    store.with_conn(|c| {
        c.query_row(
            &format!("SELECT {SCHEDULE_COLS} FROM agents_schedules WHERE name = ?1 OR id = ?1"),
            [key],
            schedule_from,
        )
        .optional()
    })
}

/// Insert or replace schedule `s` (by id). `Ok(false)`: another schedule
/// has its name.
pub fn put_schedule(store: &Store, s: &Schedule) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        let taken: Option<String> = c
            .query_row(
                "SELECT id FROM agents_schedules WHERE name = ?1 AND id != ?2",
                params![s.name, s.id],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Ok(false);
        }
        c.execute(
            &format!(
                "INSERT OR REPLACE INTO agents_schedules ({SCHEDULE_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
            ),
            params![
                s.id,
                s.name,
                s.cron,
                s.timezone,
                s.workspace,
                s.model,
                s.max_runtime_min,
                s.enabled as i64,
                s.prompt,
                s.looked_until,
                s.created_at,
                s.updated_at,
                s.after
            ],
        )?;
        Ok(true)
    })
}

/// Delete schedule `id` and its occurrences.
pub fn delete_schedule(store: &Store, id: &str) -> rusqlite::Result<bool> {
    store.transaction(|tx| {
        tx.execute(
            "DELETE FROM agents_occurrences WHERE schedule_id = ?1",
            [id],
        )?;
        Ok(tx.execute("DELETE FROM agents_schedules WHERE id = ?1", [id])? > 0)
    })
}

/// Occurrence states.
pub const OCC_PENDING: &str = "pending";
pub const OCC_SENT: &str = "sent";
pub const OCC_MISSED: &str = "missed";
pub const OCC_REFUSED: &str = "refused";

/// One occurrence of a schedule the timer (or Run now) took up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub schedule_id: String,
    /// Its time, RFC 3339 with its offset ([`crate::cron::name`]).
    pub occurrence: String,
    /// Milliseconds.
    pub due_at: i64,
    /// [`OCC_PENDING`], [`OCC_SENT`] or [`OCC_MISSED`].
    pub state: String,
    pub request_id: String,
    pub run_id: String,
    /// Run now, not the cron.
    pub manual: bool,
    pub updated_at: i64,
}

fn occurrence_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Occurrence> {
    Ok(Occurrence {
        schedule_id: r.get(0)?,
        occurrence: r.get(1)?,
        due_at: r.get(2)?,
        state: r.get(3)?,
        request_id: r.get(4)?,
        run_id: r.get(5)?,
        manual: r.get::<_, i64>(6)? != 0,
        updated_at: r.get(7)?,
    })
}

const OCC_COLS: &str =
    "schedule_id, occurrence, due_at, state, request_id, run_id, manual, updated_at";

/// Occurrences still to send, oldest first.
pub fn pending(store: &Store) -> rusqlite::Result<Vec<Occurrence>> {
    store.with_conn(|c| {
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {OCC_COLS} FROM agents_occurrences WHERE state = 'pending' ORDER BY due_at"
        ))?;
        stmt.query_map([], occurrence_from)?.collect()
    })
}

/// Schedule `id`'s newest occurrence.
pub fn last_occurrence(store: &Store, id: &str) -> rusqlite::Result<Option<Occurrence>> {
    store.with_conn(|c| {
        c.query_row(
            &format!(
                "SELECT {OCC_COLS} FROM agents_occurrences WHERE schedule_id = ?1
                 ORDER BY due_at DESC, updated_at DESC LIMIT 1"
            ),
            [id],
            occurrence_from,
        )
        .optional()
    })
}

/// Schedule `id`'s newest occurrence from its cron (not Run now) that the
/// runner answered with a run.
pub fn last_sent(store: &Store, id: &str) -> rusqlite::Result<Option<Occurrence>> {
    store.with_conn(|c| {
        c.query_row(
            &format!(
                "SELECT {OCC_COLS} FROM agents_occurrences WHERE schedule_id = ?1
                 AND manual = 0 AND state = 'sent' AND run_id != ''
                 ORDER BY due_at DESC LIMIT 1"
            ),
            [id],
            occurrence_from,
        )
        .optional()
    })
}

/// Take up occurrences of schedule `id` and move its `looked_until` to
/// `until`, in one transaction: an occurrence already taken up is kept as
/// it is.
pub fn take_up(
    store: &Store,
    id: &str,
    occurrences: &[Occurrence],
    until: i64,
) -> rusqlite::Result<()> {
    store.transaction(|tx| {
        for o in occurrences {
            tx.execute(
                &format!(
                    "INSERT OR IGNORE INTO agents_occurrences ({OCC_COLS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
                params![
                    o.schedule_id,
                    o.occurrence,
                    o.due_at,
                    o.state,
                    o.request_id,
                    o.run_id,
                    o.manual as i64,
                    o.updated_at
                ],
            )?;
        }
        tx.execute(
            "UPDATE agents_schedules SET looked_until = MAX(looked_until, ?2) WHERE id = ?1",
            params![id, until],
        )?;
        // The newest 200 occurrences of a schedule are kept.
        tx.execute(
            "DELETE FROM agents_occurrences WHERE schedule_id = ?1 AND state != 'pending'
             AND (occurrence, manual) NOT IN (SELECT occurrence, manual FROM agents_occurrences
             WHERE schedule_id = ?1 ORDER BY due_at DESC LIMIT 200)",
            [id],
        )?;
        Ok(())
    })
}

/// Take up one occurrence unless it was already: `Ok(true)` when it is new.
pub fn take_up_one(store: &Store, o: &Occurrence) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        c.execute(
            &format!(
                "INSERT OR IGNORE INTO agents_occurrences ({OCC_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            params![
                o.schedule_id,
                o.occurrence,
                o.due_at,
                o.state,
                o.request_id,
                o.run_id,
                o.manual as i64,
                o.updated_at
            ],
        )
        .map(|n| n > 0)
    })
}

/// Drop schedule `id`'s occurrences still waiting to be sent (paused).
pub fn drop_pending(store: &Store, id: &str) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "DELETE FROM agents_occurrences WHERE schedule_id = ?1 AND state = 'pending'",
            [id],
        )
        .map(|_| ())
    })
}

/// Set an occurrence's state (and the run it answered).
pub fn set_occurrence(
    store: &Store,
    o: &Occurrence,
    state: &str,
    run_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE agents_occurrences SET state = ?1, run_id = ?2, updated_at = ?3
             WHERE schedule_id = ?4 AND occurrence = ?5 AND manual = ?6",
            params![
                state,
                run_id,
                now,
                o.schedule_id,
                o.occurrence,
                o.manual as i64
            ],
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_v2() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(SCHEMA)?;
                c.execute_batch(CAP_V2)?;
                c.execute_batch(REVIEWED)
            })
            .unwrap();
        store
    }

    #[test]
    fn the_v3_step_upgrades_a_v2_database() {
        let store = migrated_v2();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO agents_runner (id, url, token, signed_in_at)
                     VALUES (1, 'https://runner.example', 'sealed', 1);
                     INSERT INTO agents_state (id, state_json, connected, last_ok_at, updated_at)
                     VALUES (1, '{\"cursor\": 42, \"runs\": {\"run_1\": {\"id\": \"run_1\",
                       \"state\": \"working\", \"task_id\": \"r1\"}},
                       \"schedules\": {\"nightly\": {\"name\": \"nightly\"}},
                       \"notified\": [\"ended:run_0\"]}', 1, 5, 5);
                     INSERT INTO agents_launches (task_id, collection, at, run_id)
                     VALUES ('r1', 'tasks', 1, 'run_1');
                     INSERT INTO agents_reviewed (run_id, at) VALUES ('run_0', 1);",
                )
            })
            .unwrap();
        store.transaction(cap_v3).unwrap();
        store.with_conn(|c| c.execute_batch(CHAINED)).unwrap();
        let (st, h) = state(&store).unwrap().unwrap();
        assert_eq!(st.cursor, 0, "a v3 snapshot is taken");
        assert!(st.runs.is_empty());
        assert_eq!(st.notified, ["ended:run_0"], "sent notices are kept");
        assert!(!h.connected && h.last_ok_at.is_none());
        assert!(launches(&store, "tasks").unwrap().is_empty());
        let reviewed: rusqlite::Result<i64> = store
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM agents_reviewed", [], |r| r.get(0)));
        assert!(reviewed.is_err(), "the pod-only marks are gone");
        assert!(schedules(&store).unwrap().is_empty());
        // The cursor is its own column from now on.
        let mut st = st;
        st.cursor = 77;
        save(&store, &st, 9).unwrap();
        assert_eq!(state(&store).unwrap().unwrap().0.cursor, 77);
    }

    #[test]
    fn the_whole_list_runs_on_a_new_database() {
        let store = Store::open_in_memory().unwrap();
        pod_core::migrations::check_names("agents", MIGRATIONS).unwrap();
        store
            .transaction(|tx| {
                for step in MIGRATIONS {
                    match &step.run {
                        pod_core::migrations::Migration::Sql(sql) => tx.execute_batch(sql)?,
                        pod_core::migrations::Migration::Rust(f) => f(tx)?,
                    }
                }
                Ok(())
            })
            .unwrap();
        let runner = Runner {
            url: "https://runner.example".into(),
            token: "t".into(),
        };
        let st = State {
            cursor: 5,
            ..Default::default()
        };
        sign_in(&store, &runner, &st, 1).unwrap();
        assert_eq!(state(&store).unwrap().unwrap().0.cursor, 5);
        assert!(mark(&store, "stop:run_1", 1).unwrap());
        assert!(!mark(&store, "stop:run_1", 2).unwrap());
        assert!(marked(&store, "stop:run_1").unwrap());
        put_launch(&store, "tasks", "r1", "t_r1_1", "", 1).unwrap();
        put_launch(&store, "tasks", "r1", "t_r1_1", "run_1", 2).unwrap();
        let l = launches(&store, "tasks").unwrap();
        assert_eq!((l.len(), l[0].run_id.as_str()), (1, "run_1"));
        let s = Schedule {
            id: "sch_a".into(),
            name: "nightly".into(),
            cron: "0 3 * * *".into(),
            timezone: "UTC".into(),
            workspace: "w".into(),
            max_runtime_min: 30,
            enabled: true,
            prompt: "p".into(),
            ..Default::default()
        };
        assert!(put_schedule(&store, &s).unwrap());
        let other = Schedule {
            id: "sch_b".into(),
            ..s.clone()
        };
        assert!(!put_schedule(&store, &other).unwrap(), "names are unique");
        assert_eq!(schedule(&store, "nightly").unwrap().unwrap().id, "sch_a");
        assert!(delete_schedule(&store, "sch_a").unwrap());
        assert!(schedules(&store).unwrap().is_empty());
    }

    /// The follower saves its whole copy of the state; a run a route
    /// stored since at a newer version (the runner's answer to a review)
    /// stays, and a run the copy drops still goes.
    #[test]
    fn a_save_never_takes_a_run_back() {
        let store = Store::open_in_memory().unwrap();
        store
            .transaction(|tx| {
                for step in MIGRATIONS {
                    match &step.run {
                        pod_core::migrations::Migration::Sql(sql) => tx.execute_batch(sql)?,
                        pod_core::migrations::Migration::Rust(f) => f(tx)?,
                    }
                }
                Ok(())
            })
            .unwrap();
        let runner = Runner {
            url: "https://runner.example".into(),
            token: "t".into(),
        };
        let run = |id: &str, version: i64, title: &str| crate::state::Run {
            id: id.into(),
            version,
            title: title.into(),
            ..Default::default()
        };
        let mut old = State::default();
        old.runs
            .insert("run_a".into(), run("run_a", 1, "needs review"));
        old.runs.insert("run_b".into(), run("run_b", 1, "gone"));
        sign_in(&store, &runner, &old, 1).unwrap();
        let mut newer = old.clone();
        newer
            .runs
            .insert("run_a".into(), run("run_a", 2, "reviewed"));
        save(&store, &newer, 2).unwrap();
        // The follower's older copy, without run_b.
        old.runs.remove("run_b");
        save(&store, &old, 3).unwrap();
        let st = state(&store).unwrap().unwrap().0;
        assert_eq!(st.runs["run_a"].title, "reviewed");
        assert!(!st.runs.contains_key("run_b"));
    }
}
