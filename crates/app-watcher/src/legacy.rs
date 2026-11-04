//! The Watcher's old tables (decision `watcher`), kept for their
//! migrations and read once when a pod's sites and rules move into the
//! Events app's `events` collection ([`crate::events::import`], decision
//! `collection-fillers`). Nothing writes them any more.

use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pod_core::store::Migration;

/// Append-only: never edit or reorder a step that has shipped.
pub static MIGRATIONS: &[Migration] = &[
    Migration::Sql(SCHEMA),
    // 2: every rule belongs to one site (decision `watcher-rules-per-site`).
    Migration::Rust(rules_per_site),
    // 3: a target's zone by name (issue #66); `utc_offset_min` stays as the
    // fallback for targets saved before.
    Migration::Sql("ALTER TABLE watcher_targets ADD COLUMN time_zone TEXT;"),
];

/// Give every rule exactly one site: a rule for all sites (or several)
/// becomes one rule per live site. (Its matches were recomputed by the
/// Watcher; they are no longer read.)
pub fn rules_per_site(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let now = pod_core::util::now_secs();
    let live: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT id FROM watcher_targets WHERE archived = 0 ORDER BY created_at, id")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for r in rules(tx)? {
        let sites: Vec<String> = if r.spec.targets.is_empty() {
            live.clone()
        } else {
            r.spec.targets.clone()
        };
        if sites.len() <= 1 && r.spec.targets.len() == sites.len() {
            continue;
        }
        for (i, site) in sites.iter().enumerate() {
            let mut spec = r.spec.clone();
            spec.targets = vec![site.clone()];
            let id = if i == 0 { r.id.clone() } else { new_id() };
            tx.execute(
                "INSERT INTO watcher_rules (id, title, spec_json, archived, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT (id) DO UPDATE SET spec_json = ?3, updated_at = ?5",
                params![
                    id,
                    r.title,
                    serde_json::to_string(&spec).unwrap_or_default(),
                    r.archived,
                    now
                ],
            )?;
        }
    }
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE watcher_targets (
    id             TEXT PRIMARY KEY NOT NULL,
    title          TEXT NOT NULL,
    url            TEXT NOT NULL,
    extractor      TEXT NOT NULL,
    config_json    TEXT NOT NULL DEFAULT '{}',   -- {hint, details}
    fetch_mode     TEXT NOT NULL DEFAULT 'get',
    interval_secs  INTEGER NOT NULL,
    utc_offset_min INTEGER,
    archived       INTEGER NOT NULL DEFAULT 0,
    client_id      TEXT UNIQUE,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    -- polling and health
    next_fetch_at  INTEGER NOT NULL DEFAULT 0,
    etag           TEXT,
    last_modified  TEXT,
    content_hash   TEXT,
    last_fetch_at  INTEGER,
    last_ok_at     INTEGER,
    last_error     TEXT,
    failures       INTEGER NOT NULL DEFAULT 0,
    empty_runs     INTEGER NOT NULL DEFAULT 0,
    last_extractor TEXT
);
CREATE TABLE watcher_rules (
    id         TEXT PRIMARY KEY NOT NULL,
    title      TEXT NOT NULL,
    spec_json  TEXT NOT NULL,
    archived   INTEGER NOT NULL DEFAULT 0,
    client_id  TEXT UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE watcher_events (
    id            TEXT PRIMARY KEY NOT NULL,
    target_id     TEXT NOT NULL,
    source_id     TEXT,
    title         TEXT NOT NULL,
    start         TEXT NOT NULL,           -- the source's wall clock (When::stored)
    end_at        TEXT,
    start_ts      INTEGER NOT NULL,
    relevant_until INTEGER NOT NULL,       -- listed until then
    venue         TEXT NOT NULL,
    description   TEXT NOT NULL,
    url           TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL
);
CREATE INDEX watcher_events_target ON watcher_events (target_id, start_ts);
CREATE TABLE watcher_matches (
    event_id    TEXT NOT NULL,
    rule_id     TEXT NOT NULL,
    matched_at  INTEGER NOT NULL,
    notified_at INTEGER,
    seen        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (event_id, rule_id)
);
";

/// A rule id: `r` and 10 base32 characters.
fn new_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("OS random number generator");
    std::iter::once('r')
        .chain(buf.iter().map(|b| ALPHABET[(b & 31) as usize] as char))
        .collect()
}

/// A watched website, as the Watcher stored it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: String,
    pub title: String,
    pub url: String,
    /// `auto`, `ics`, `jsonld` or `llm`.
    pub extractor: String,
    pub hint: String,
    /// Fetch each new event's own page once for its full description.
    pub details: bool,
    pub interval_secs: i64,
    /// A fixed offset (minutes east of UTC), used without `time_zone`.
    pub utc_offset: Option<i32>,
    pub time_zone: Option<String>,
    pub archived: bool,
    /// What `auto` found last.
    pub last_extractor: Option<String>,
}

/// The live targets, oldest first.
pub fn targets(c: &Connection) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = c.prepare(
        "SELECT id, title, url, extractor, config_json, interval_secs, utc_offset_min, archived,
                last_extractor, time_zone
         FROM watcher_targets WHERE archived = 0 ORDER BY created_at, id",
    )?;
    stmt.query_map([], |r| {
        let config: Value = serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null);
        Ok(Target {
            id: r.get(0)?,
            title: r.get(1)?,
            url: r.get(2)?,
            extractor: r.get(3)?,
            hint: config["hint"].as_str().unwrap_or("").to_owned(),
            details: config["details"].as_bool().unwrap_or(false),
            interval_secs: r.get(5)?,
            utc_offset: r.get(6)?,
            archived: r.get(7)?,
            last_extractor: r.get(8)?,
            time_zone: r.get(9)?,
        })
    })?
    .collect()
}

/// A rule's conditions as the Watcher stored them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    #[serde(default)]
    pub any: Vec<String>,
    #[serde(default)]
    pub all: Vec<String>,
    #[serde(default)]
    pub none: Vec<String>,
    #[serde(default)]
    pub venue: String,
    #[serde(default)]
    pub days: Vec<String>,
    #[serde(default)]
    pub within_days: Option<u32>,
    #[serde(default)]
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: String,
    pub title: String,
    pub spec: RuleSpec,
    pub archived: bool,
}

/// Every rule, live ones first.
pub fn rules(c: &Connection) -> rusqlite::Result<Vec<Rule>> {
    let mut stmt = c.prepare(
        "SELECT id, title, spec_json, archived FROM watcher_rules
         ORDER BY archived, created_at, id",
    )?;
    stmt.query_map([], |r| {
        Ok(Rule {
            id: r.get(0)?,
            title: r.get(1)?,
            spec: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
            archived: r.get(3)?,
        })
    })?
    .collect()
}
