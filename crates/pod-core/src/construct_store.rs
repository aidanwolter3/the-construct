//! The construct's tables. Created by the first switch
//! ([`ensure_tables`]), so a pod that is never switched is never changed.
//!
//! - `objects(hash, kind, bytes, features, size)`: immutable objects, by id
//!   ([`construct_elements::object_id`]); `bytes` is the canonical JSON,
//!   `features` the vocabulary the object draws with (comma-separated).
//! - `names(name, hash, updated_at)`: where each name points now.
//! - `name_history(name, seq, hash, at, author, note)`: every move of a
//!   name, `seq` from 1; a deleted name's last entry has `hash` `""`.
//! - `follows(name, follower)`: `follower`'s element has an unpinned
//!   reference to `name`, so a move of `name` re-seals `follower`.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::MAX_HISTORY;
use crate::util::now_secs;

/// Create the tables if they aren't there.
pub fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
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
    )
}

/// Whether the tables exist (a pod switched at least once).
pub fn has_tables(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'names')",
        [],
        |r| r.get(0),
    )
}

/// Store an object (nothing if it is there already).
pub fn put_object(conn: &Connection, id: &str, v: &Value, features: &str) -> rusqlite::Result<()> {
    let bytes = construct_elements::canonical(v);
    conn.execute(
        "INSERT OR IGNORE INTO objects (hash, kind, bytes, features, size) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            v["kind"].as_str().unwrap_or("element"),
            bytes.as_bytes(),
            features,
            bytes.len() as i64
        ],
    )?;
    Ok(())
}

/// An object and its features.
pub fn object(conn: &Connection, id: &str) -> rusqlite::Result<Option<(Arc<Value>, String)>> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT bytes, features FROM objects WHERE hash = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(b, f)| {
        serde_json::from_slice::<Value>(&b)
            .ok()
            .map(|v| (Arc::new(v), f))
    }))
}

/// Where `name` points (`None`: no such name, or deleted).
pub fn name(conn: &Connection, name: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT hash FROM names WHERE name = ?1", [name], |r| {
        r.get(0)
    })
    .optional()
}

/// Every name and where it points, by name.
pub fn names(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached("SELECT name, hash FROM names ORDER BY name")?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// One move of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: i64,
    /// `""`: the name was deleted.
    pub hash: String,
    pub at: i64,
    pub author: String,
    pub note: Option<String>,
}

/// A name's history, newest first.
pub fn history(conn: &Connection, name: &str) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, hash, at, author, note FROM name_history WHERE name = ?1 ORDER BY seq DESC",
    )?;
    stmt.query_map([name], |r| {
        Ok(Entry {
            seq: r.get(0)?,
            hash: r.get(1)?,
            at: r.get(2)?,
            author: r.get(3)?,
            note: r.get(4)?,
        })
    })?
    .collect()
}

/// The newest entry's `seq` (0 for a name with no history).
pub fn last_seq(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM name_history WHERE name = ?1",
        [name],
        |r| r.get(0),
    )
}

/// Point `name` at `hash` (`""`: delete it), recording the move. Nothing
/// when it points there already. Answers the new `seq`, or `None`.
pub fn move_name(
    conn: &Connection,
    name: &str,
    hash: &str,
    author: &str,
    note: Option<&str>,
) -> rusqlite::Result<Option<i64>> {
    let now = self::name(conn, name)?;
    if now.as_deref() == Some(hash) || (now.is_none() && hash.is_empty()) {
        return Ok(None);
    }
    let seq = last_seq(conn, name)? + 1;
    let at = now_secs();
    conn.execute(
        "INSERT INTO name_history (name, seq, hash, at, author, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, seq, hash, at, author, note],
    )?;
    if hash.is_empty() {
        conn.execute("DELETE FROM names WHERE name = ?1", [name])?;
        conn.execute("DELETE FROM follows WHERE follower = ?1", [name])?;
    } else {
        conn.execute(
            "INSERT OR REPLACE INTO names (name, hash, updated_at) VALUES (?1, ?2, ?3)",
            params![name, hash, at],
        )?;
    }
    trim_history(conn, name)?;
    Ok(Some(seq))
}

/// The start of a re-seal's note in a name's history (`Re-sealed: <name>
/// moved`): the pod wrote the entry because something the element references
/// moved, not because anyone edited it.
pub const RESEAL_NOTE: &str = "Re-sealed:";

/// [`move_name`] for a re-seal. Re-seals in a row are one entry: when the
/// name's newest entry is a re-seal too, that entry takes the new element
/// and note, and its `seq` stays. So a name's history holds what was
/// written to it, with at most one re-seal between two writes, instead of
/// one entry per move of anything it reaches (decision
/// `element-roles-and-reuse`).
pub fn move_name_resealed(
    conn: &Connection,
    name: &str,
    hash: &str,
    note: &str,
) -> rusqlite::Result<Option<i64>> {
    let now = self::name(conn, name)?;
    if now.as_deref() == Some(hash) {
        return Ok(None);
    }
    let newest: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT seq, author, note FROM name_history WHERE name = ?1 ORDER BY seq DESC LIMIT 1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let author = crate::layouts::AUTHOR_POD;
    match newest {
        Some((seq, a, Some(n))) if a == author && n.starts_with(RESEAL_NOTE) => {
            let at = now_secs();
            conn.execute(
                "UPDATE name_history SET hash = ?3, at = ?4, note = ?5 WHERE name = ?1 AND seq = ?2",
                params![name, seq, hash, at, note],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO names (name, hash, updated_at) VALUES (?1, ?2, ?3)",
                params![name, hash, at],
            )?;
            Ok(Some(seq))
        }
        _ => move_name(conn, name, hash, author, Some(note)),
    }
}

/// Keep the newest [`MAX_HISTORY`] entries somebody wrote, and the re-seals
/// among them: re-seals don't count toward the limit, so they can't push a
/// real edit out of what `restore_version` reaches.
fn trim_history(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    let oldest_kept: Option<i64> = conn
        .query_row(
            "SELECT seq FROM name_history WHERE name = ?1
             AND NOT (author = ?2 AND COALESCE(note, '') LIKE ?3)
             ORDER BY seq DESC LIMIT 1 OFFSET ?4",
            params![
                name,
                crate::layouts::AUTHOR_POD,
                format!("{RESEAL_NOTE}%"),
                MAX_HISTORY - 1
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(seq) = oldest_kept {
        conn.execute(
            "DELETE FROM name_history WHERE name = ?1 AND seq < ?2",
            params![name, seq],
        )?;
    }
    Ok(())
}

/// The size in bytes of the element `name` points at now (0: none).
pub fn name_bytes(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT o.size FROM names n JOIN objects o ON o.hash = n.hash WHERE n.name = ?1",
            [name],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// The names whose elements follow `name`.
pub fn followers(conn: &Connection, name: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT follower FROM follows WHERE name = ?1 ORDER BY follower")?;
    stmt.query_map([name], |r| r.get(0))?.collect()
}

/// Record what `follower` follows now (replacing what it followed).
pub fn set_follows(conn: &Connection, follower: &str, names: &[String]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM follows WHERE follower = ?1", [follower])?;
    for n in names {
        conn.execute(
            "INSERT OR IGNORE INTO follows (name, follower) VALUES (?1, ?2)",
            params![n, follower],
        )?;
    }
    Ok(())
}

/// Count one write ([`super::GENERATION_SETTING`]).
pub fn bump_generation(conn: &Connection) -> rusqlite::Result<i64> {
    let g = generation(conn)? + 1;
    conn.execute(
        "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, ?2)",
        params![super::GENERATION_SETTING, g.to_string()],
    )?;
    Ok(g)
}

/// The construct's write count.
pub fn generation(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(
        crate::store::pod_setting_on(conn, super::GENERATION_SETTING)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// Delete the objects no name's history reaches (pod GC). Answers how many.
pub fn collect_garbage(conn: &Connection) -> rusqlite::Result<usize> {
    use std::collections::{BTreeSet, VecDeque};
    let mut keep: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT hash FROM name_history WHERE hash != ''")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    while let Some(id) = queue.pop_front() {
        if !keep.insert(id.clone()) {
            continue;
        }
        if let Some((v, _)) = object(conn, &id)? {
            for (r, _) in construct_elements::refs::all(&v["node"]) {
                if let Some(el) = r.el
                    && !keep.contains(&el)
                {
                    queue.push_back(el);
                }
            }
        }
    }
    let all: Vec<String> = {
        let mut stmt = conn.prepare("SELECT hash FROM objects")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut n = 0;
    for id in all {
        if !keep.contains(&id) {
            n += conn.execute("DELETE FROM objects WHERE hash = ?1", [&id])?;
        }
    }
    Ok(n)
}
