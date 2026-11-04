//! Named schema migrations (decision `named-migrations`).
//!
//! A database's schema is a list of [`Step`]s, each with a name that never
//! changes once deployed. The `schema_migrations` table records which names
//! ran, per scope (`core`, an adapter's name, `host`), so two branches that
//! each append a step only conflict on the list's last lines: keep both, and
//! nothing is renumbered.
//!
//! Databases made before names existed counted steps instead (`PRAGMA
//! user_version`, or an adapter's row in `adapter_migrations`). The first run
//! of a scope with no recorded names marks that many leading steps as run.
//! The counter is kept up to date (the number of leading steps that ran), so
//! an older build that still counts can open the database too.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

/// One schema upgrade: plain SQL, or Rust code for data conversions SQL
/// cannot do (hashing, compression).
pub enum Migration {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> rusqlite::Result<()>),
}

/// A named [`Migration`]. The name is the step's identity in every database
/// that ran it: never rename or remove a deployed step.
pub struct Step {
    pub name: &'static str,
    pub run: Migration,
}

impl Step {
    pub const fn sql(name: &'static str, sql: &'static str) -> Self {
        Self {
            name,
            run: Migration::Sql(sql),
        }
    }

    pub const fn rust(name: &'static str, f: fn(&Transaction<'_>) -> rusqlite::Result<()>) -> Self {
        Self {
            name,
            run: Migration::Rust(f),
        }
    }
}

/// Where a database made before names kept its step count.
#[derive(Clone, Copy)]
pub enum Counter<'a> {
    /// `PRAGMA user_version`.
    UserVersion,
    /// The adapter's row in `adapter_migrations`.
    Adapter(&'a str),
}

fn refuse(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(msg))
}

/// Check a list's names: lowercase `a-z`, `0-9` and `_`, and unique.
pub fn check_names(scope: &str, steps: &[Step]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for step in steps {
        let ok = !step.name.is_empty()
            && step
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !ok {
            return Err(format!(
                "migration name '{}' in '{scope}' must be lowercase letters, digits and _",
                step.name
            ));
        }
        if !seen.insert(step.name) {
            return Err(format!(
                "migration name '{}' appears twice in '{scope}'",
                step.name
            ));
        }
    }
    Ok(())
}

fn table_exists(tx: &Connection, table: &str) -> rusqlite::Result<bool> {
    tx.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([table])
}

fn read_counter(tx: &Connection, counter: Counter<'_>) -> rusqlite::Result<i64> {
    match counter {
        Counter::UserVersion => tx.query_row("PRAGMA user_version", [], |r| r.get(0)),
        Counter::Adapter(adapter) => {
            if !table_exists(tx, "adapter_migrations")? {
                return Ok(0);
            }
            Ok(tx
                .query_row(
                    "SELECT version FROM adapter_migrations WHERE adapter = ?1",
                    [adapter],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0))
        }
    }
}

fn write_counter(tx: &Connection, counter: Counter<'_>, value: i64) -> rusqlite::Result<()> {
    match counter {
        Counter::UserVersion => tx.pragma_update(None, "user_version", value),
        Counter::Adapter(adapter) => tx
            .execute(
                "INSERT INTO adapter_migrations (adapter, version) VALUES (?1, ?2)
                 ON CONFLICT(adapter) DO UPDATE SET version = excluded.version",
                params![adapter, value],
            )
            .map(|_| ()),
    }
}

/// The names of `scope` that ran.
pub fn applied(conn: &Connection, scope: &str) -> rusqlite::Result<HashSet<String>> {
    if !table_exists(conn, "schema_migrations")? {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare("SELECT name FROM schema_migrations WHERE scope = ?1")?;
    stmt.query_map([scope], |r| r.get(0))?.collect()
}

/// Run the first `upto` steps of `steps` that have not run in `scope`, in
/// list order. Each runs in an IMMEDIATE transaction (the write lock is taken
/// up front) that checks its name again, so two processes starting on the
/// same database never both run a step.
pub fn run(
    conn: &mut Connection,
    scope: &str,
    steps: &[Step],
    counter: Counter<'_>,
    upto: usize,
) -> rusqlite::Result<()> {
    check_names(scope, steps).map_err(refuse)?;
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 scope TEXT NOT NULL,
                 name  TEXT NOT NULL,
                 PRIMARY KEY (scope, name)
             );",
        )?;
        let any: bool = tx
            .prepare("SELECT 1 FROM schema_migrations WHERE scope = ?1")?
            .exists([scope])?;
        if !any {
            // A database from before names: its counter says how many
            // leading steps ran.
            let ran = read_counter(&tx, counter)?.max(0) as usize;
            for step in steps.iter().take(ran) {
                tx.execute(
                    "INSERT INTO schema_migrations (scope, name) VALUES (?1, ?2)",
                    params![scope, step.name],
                )?;
            }
        }
        tx.commit()?;
    }
    for step in steps.iter().take(upto) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let done: bool = tx
            .prepare("SELECT 1 FROM schema_migrations WHERE scope = ?1 AND name = ?2")?
            .exists(params![scope, step.name])?;
        if done {
            continue;
        }
        match &step.run {
            Migration::Sql(sql) => tx.execute_batch(sql)?,
            Migration::Rust(f) => f(&tx)?,
        }
        tx.execute(
            "INSERT INTO schema_migrations (scope, name) VALUES (?1, ?2)",
            params![scope, step.name],
        )?;
        let ran = applied(&tx, scope)?;
        let prefix = steps.iter().take_while(|s| ran.contains(s.name)).count();
        write_counter(&tx, counter, prefix as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// How many steps `name` is from the start of `steps`, itself included
/// (tests that build a database as an older build left it).
pub fn position(steps: &[Step], name: &str) -> Option<usize> {
    steps.iter().position(|s| s.name == name).map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_row(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO t (n) VALUES (1)", [])?;
        Ok(())
    }

    static STEPS: &[Step] = &[
        Step::sql("make_t", "CREATE TABLE t (n INTEGER NOT NULL);"),
        Step::rust("first_row", add_row),
        Step::sql("add_m", "ALTER TABLE t ADD COLUMN m INTEGER;"),
    ];

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn steps_run_once_and_keep_the_counter() {
        let mut conn = Connection::open_in_memory().unwrap();
        for _ in 0..2 {
            run(&mut conn, "s", STEPS, Counter::UserVersion, STEPS.len()).unwrap();
        }
        assert_eq!(count(&conn), 1, "the Rust step ran once");
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 3);
        assert_eq!(applied(&conn, "s").unwrap().len(), 3);
        assert!(applied(&conn, "other").unwrap().is_empty());
    }

    /// A database from before names: its counter marks the leading steps as
    /// run, and only the rest run.
    #[test]
    fn a_counted_database_is_seeded_from_its_counter() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (n INTEGER NOT NULL); INSERT INTO t VALUES (7);")
            .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        run(&mut conn, "s", STEPS, Counter::UserVersion, STEPS.len()).unwrap();
        assert_eq!(count(&conn), 1, "first_row did not run again");
        conn.execute("INSERT INTO t (n, m) VALUES (1, 2)", [])
            .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 3);
    }

    /// Two branches each appended a step; whichever order they merged in, a
    /// database that ran one runs the other, and the counter counts only the
    /// leading steps that ran.
    #[test]
    fn steps_merged_in_either_order_both_run() {
        static A_ONLY: &[Step] = &[
            Step::sql("make_t", "CREATE TABLE t (n INTEGER NOT NULL);"),
            Step::sql("a", "CREATE TABLE a (x INTEGER);"),
        ];
        static B_THEN_A: &[Step] = &[
            Step::sql("make_t", "CREATE TABLE t (n INTEGER NOT NULL);"),
            Step::sql("b", "CREATE TABLE b (x INTEGER);"),
            Step::sql("a", "CREATE TABLE a (x INTEGER);"),
        ];
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn, "s", A_ONLY, Counter::UserVersion, A_ONLY.len()).unwrap();
        run(
            &mut conn,
            "s",
            B_THEN_A,
            Counter::UserVersion,
            B_THEN_A.len(),
        )
        .unwrap();
        conn.execute_batch("INSERT INTO a VALUES (1); INSERT INTO b VALUES (1);")
            .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn upto_stops_after_that_many_steps() {
        let mut conn = Connection::open_in_memory().unwrap();
        let upto = position(STEPS, "first_row").unwrap();
        run(&mut conn, "s", STEPS, Counter::UserVersion, upto).unwrap();
        assert_eq!(applied(&conn, "s").unwrap().len(), 2);
        assert!(
            conn.execute("INSERT INTO t (n, m) VALUES (1, 2)", [])
                .is_err()
        );
    }

    #[test]
    fn bad_and_duplicate_names_are_refused() {
        static DUP: &[Step] = &[Step::sql("x", "SELECT 1;"), Step::sql("x", "SELECT 2;")];
        static BAD: &[Step] = &[Step::sql("Not-OK", "SELECT 1;")];
        assert!(check_names("s", DUP).unwrap_err().contains("appears twice"));
        assert!(check_names("s", BAD).unwrap_err().contains("lowercase"));
        let mut conn = Connection::open_in_memory().unwrap();
        assert!(run(&mut conn, "s", DUP, Counter::UserVersion, 2).is_err());
    }
}
