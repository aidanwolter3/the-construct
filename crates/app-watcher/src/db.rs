//! The watcher's tables: targets (what to fetch, and how each is doing),
//! rules, the events found, and which rule matched which event (also the
//! record of what was notified).

use protocol::{DataBundle, EntityRecord};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use pod_core::store::Migration;

use crate::extract::RawEvent;
use crate::rules::{Candidate, RuleSpec};
use crate::when::{When, offset_text};

/// Append-only: never edit or reorder a step that has shipped.
pub static MIGRATIONS: &[Migration] = &[
    Migration::Sql(SCHEMA),
    // 2: every rule belongs to one site (decision `watcher-rules-per-site`).
    Migration::Rust(rules_per_site),
];

/// Give every rule exactly one site: a rule for all sites (or several)
/// becomes one rule per live site, matched again.
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
            let mut one = r.clone();
            one.spec.targets = vec![site.clone()];
            if i > 0 {
                one.id = new_id('r');
            }
            save_rule(tx, &one, None, now)?;
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

/// Events are kept this long after they are over, then deleted.
pub const KEEP_PAST_SECS: i64 = 30 * 86_400;

pub const MATCHES_QUERY: &str = "watcher:matches";
pub const TARGETS_QUERY: &str = "watcher:targets";
pub const RULES_QUERY: &str = "watcher:rules";

/// The event screen's one-item list (screens bind `$item` through a List).
pub fn event_query(id: &str) -> String {
    format!("watcher:event:{id}")
}

/// The target screen's one-item list.
pub fn target_query(id: &str) -> String {
    format!("watcher:target:{id}")
}

/// A site's live rules (the site screen).
pub fn target_rules_query(id: &str) -> String {
    format!("watcher:target:{id}:rules")
}

pub fn target_events_query(id: &str) -> String {
    format!("watcher:target:{id}:events")
}

pub fn event_uri(id: &str) -> String {
    format!("watcher://events/{id}")
}

pub fn target_uri(id: &str) -> String {
    format!("watcher://targets/{id}")
}

pub fn rule_uri(id: &str) -> String {
    format!("watcher://rules/{id}")
}

/// A new row id: `prefix` and 10 base32 characters.
pub fn new_id(prefix: char) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("OS random number generator");
    std::iter::once(prefix)
        .chain(buf.iter().map(|b| ALPHABET[(b & 31) as usize] as char))
        .collect()
}

fn hash_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    pod_core::util::hex(&h.finalize())
}

/// `sha256` of a fetched body, hex.
pub fn body_hash(body: &[u8]) -> String {
    pod_core::util::hex(&Sha256::digest(body))
}

// ---------- targets ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: String,
    pub title: String,
    pub url: String,
    pub extractor: String,
    pub hint: String,
    /// Fetch each new event's own page once for its full description.
    pub details: bool,
    pub interval_secs: i64,
    pub utc_offset: Option<i32>,
    pub archived: bool,
    pub created_at: i64,
    pub next_fetch_at: i64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    pub last_fetch_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
    pub failures: i64,
    pub empty_runs: i64,
    pub last_extractor: Option<String>,
}

const TARGET_COLS: &str = "id, title, url, extractor, config_json, interval_secs, utc_offset_min,
    archived, created_at, next_fetch_at, etag, last_modified, content_hash, last_fetch_at,
    last_ok_at, last_error, failures, empty_runs, last_extractor";

fn target_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Target> {
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
        created_at: r.get(8)?,
        next_fetch_at: r.get(9)?,
        etag: r.get(10)?,
        last_modified: r.get(11)?,
        content_hash: r.get(12)?,
        last_fetch_at: r.get(13)?,
        last_ok_at: r.get(14)?,
        last_error: r.get(15)?,
        failures: r.get(16)?,
        empty_runs: r.get(17)?,
        last_extractor: r.get(18)?,
    })
}

pub fn target(c: &Connection, id: &str) -> rusqlite::Result<Option<Target>> {
    c.query_row(
        &format!("SELECT {TARGET_COLS} FROM watcher_targets WHERE id = ?1"),
        [id],
        target_row,
    )
    .optional()
}

pub fn target_by_client(c: &Connection, client_id: &str) -> rusqlite::Result<Option<Target>> {
    c.query_row(
        &format!("SELECT {TARGET_COLS} FROM watcher_targets WHERE client_id = ?1"),
        [client_id],
        target_row,
    )
    .optional()
}

/// Every target, live ones first, then by title.
pub fn targets(c: &Connection) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {TARGET_COLS} FROM watcher_targets ORDER BY archived, title COLLATE NOCASE, id"
    ))?;
    stmt.query_map([], target_row)?.collect()
}

/// Live targets due at `now`, soonest first.
pub fn due_targets(c: &Connection, now: i64, limit: usize) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {TARGET_COLS} FROM watcher_targets WHERE archived = 0 AND next_fetch_at <= ?1
         ORDER BY next_fetch_at, id LIMIT ?2"
    ))?;
    stmt.query_map(params![now, limit as i64], target_row)?
        .collect()
}

/// When the next live target is due.
pub fn next_due(c: &Connection) -> rusqlite::Result<Option<i64>> {
    c.query_row(
        "SELECT MIN(next_fetch_at) FROM watcher_targets WHERE archived = 0",
        [],
        |r| r.get(0),
    )
}

pub fn live_target_count(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM watcher_targets WHERE archived = 0",
        [],
        |r| r.get(0),
    )
}

/// Insert `t` (its health fields start empty: due now).
pub fn insert_target(
    tx: &Transaction<'_>,
    t: &Target,
    client_id: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO watcher_targets (id, title, url, extractor, config_json, interval_secs,
             utc_offset_min, archived, client_id, created_at, updated_at, next_fetch_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, 0)",
        params![
            t.id,
            t.title,
            t.url,
            t.extractor,
            config_json(t),
            t.interval_secs,
            t.utc_offset,
            t.archived,
            client_id,
            t.created_at
        ],
    )?;
    Ok(())
}

fn config_json(t: &Target) -> String {
    json!({ "hint": t.hint, "details": t.details }).to_string()
}

/// Save `t`'s settings. A changed URL, extractor or hint forgets what was
/// fetched (the next check extracts again) and makes it due now.
pub fn update_target(tx: &Transaction<'_>, t: &Target, now: i64) -> rusqlite::Result<()> {
    let before = target(tx, &t.id)?;
    let refetch = before.as_ref().is_none_or(|b| {
        b.url != t.url || b.extractor != t.extractor || b.hint != t.hint || b.details != t.details
    });
    tx.execute(
        "UPDATE watcher_targets SET title = ?2, url = ?3, extractor = ?4, config_json = ?5,
             interval_secs = ?6, utc_offset_min = ?7, archived = ?8, updated_at = ?9
         WHERE id = ?1",
        params![
            t.id,
            t.title,
            t.url,
            t.extractor,
            config_json(t),
            t.interval_secs,
            t.utc_offset,
            t.archived,
            now
        ],
    )?;
    if refetch {
        tx.execute(
            "UPDATE watcher_targets SET etag = NULL, last_modified = NULL, content_hash = NULL,
                 next_fetch_at = 0, empty_runs = 0 WHERE id = ?1",
            [&t.id],
        )?;
    }
    Ok(())
}

/// Make target `id` (every live one with `None`) due now.
pub fn request_refresh(c: &Connection, id: Option<&str>) -> rusqlite::Result<usize> {
    match id {
        Some(id) => c.execute(
            "UPDATE watcher_targets SET next_fetch_at = 0 WHERE id = ?1 AND archived = 0",
            [id],
        ),
        None => c.execute(
            "UPDATE watcher_targets SET next_fetch_at = 0 WHERE archived = 0",
            [],
        ),
    }
}

/// How one check of a target went.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckResult {
    /// The page is as last time (304, or the same bytes).
    Unchanged,
    /// Extracted: `events` found (the extractor that ran).
    Extracted {
        content_hash: String,
        etag: Option<String>,
        last_modified: Option<String>,
        events: usize,
        extractor: &'static str,
    },
    /// It failed; try again at `retry_at`.
    Failed { error: String, retry_at: i64 },
}

/// Record a check of `t` and schedule the next one.
pub fn record_check(
    c: &Connection,
    t: &Target,
    result: &CheckResult,
    now: i64,
) -> rusqlite::Result<()> {
    match result {
        CheckResult::Unchanged => c.execute(
            "UPDATE watcher_targets SET last_fetch_at = ?2, last_ok_at = ?2, last_error = NULL,
                 failures = 0, next_fetch_at = ?3 WHERE id = ?1",
            params![t.id, now, now + t.interval_secs],
        )?,
        CheckResult::Extracted {
            content_hash,
            etag,
            last_modified,
            events,
            extractor,
        } => c.execute(
            "UPDATE watcher_targets SET last_fetch_at = ?2, last_ok_at = ?2, last_error = NULL,
                 failures = 0, next_fetch_at = ?3, content_hash = ?4, etag = ?5,
                 last_modified = ?6,
                 empty_runs = CASE WHEN ?7 = 0 THEN empty_runs + 1 ELSE 0 END,
                 last_extractor = ?8
             WHERE id = ?1",
            params![
                t.id,
                now,
                now + t.interval_secs,
                content_hash,
                etag,
                last_modified,
                *events as i64,
                extractor
            ],
        )?,
        CheckResult::Failed { error, retry_at } => c.execute(
            "UPDATE watcher_targets SET last_fetch_at = ?2, last_error = ?3,
                 failures = failures + 1, next_fetch_at = ?4 WHERE id = ?1",
            params![t.id, now, error, retry_at],
        )?,
    };
    Ok(())
}

/// `("ok" | "failing" | "empty" | "pending" | "archived", one line)`.
pub fn health(t: &Target, upcoming: i64, now: i64) -> (&'static str, String) {
    if t.archived {
        return ("archived", "Archived".to_owned());
    }
    if t.last_fetch_at.is_none() {
        return ("pending", "Not checked yet".to_owned());
    }
    if t.failures > 0 {
        let err = t.last_error.as_deref().unwrap_or("unknown error");
        let times = if t.failures == 1 {
            String::new()
        } else {
            format!(" ({} times)", t.failures)
        };
        return ("failing", format!("Failing{times}: {err}"));
    }
    if t.empty_runs >= 3 {
        return (
            "empty",
            format!("No events found in the last {} checks", t.empty_runs),
        );
    }
    let ago = ago_text(now - t.last_ok_at.unwrap_or(now));
    let n = if upcoming == 1 {
        "1 upcoming event".to_owned()
    } else {
        format!("{upcoming} upcoming events")
    };
    ("ok", format!("{n} · checked {ago}"))
}

fn ago_text(secs: i64) -> String {
    match secs {
        s if s < 90 => "just now".to_owned(),
        s if s < 5400 => format!("{} min ago", (s + 30) / 60),
        s if s < 36 * 3600 => format!("{} h ago", (s + 1800) / 3600),
        s => format!("{} days ago", (s + 43_200) / 86_400),
    }
}

fn interval_text(secs: i64) -> String {
    match secs {
        s if s % 86_400 == 0 => format!(
            "every {} day{}",
            s / 86_400,
            if s == 86_400 { "" } else { "s" }
        ),
        s if s % 3600 == 0 => format!("every {} h", s / 3600),
        s => format!("every {} min", s / 60),
    }
}

fn upcoming_count(c: &Connection, target_id: &str, now: i64) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM watcher_events WHERE target_id = ?1 AND relevant_until >= ?2",
        params![target_id, now],
        |r| r.get(0),
    )
}

pub fn target_entity(c: &Connection, t: &Target, now: i64) -> rusqlite::Result<EntityRecord> {
    let upcoming = upcoming_count(c, &t.id, now)?;
    let (status, text) = health(t, upcoming, now);
    let host = reqwest::Url::parse(&t.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let mut e = EntityRecord::new();
    let v = &mut e.0;
    v.insert("id".into(), json!(t.id));
    v.insert("title".into(), json!(t.title));
    v.insert("url".into(), json!(t.url));
    v.insert("host".into(), json!(host));
    v.insert("extractor".into(), json!(t.extractor));
    v.insert("hint".into(), json!(t.hint));
    v.insert("details".into(), json!(t.details));
    v.insert("interval_minutes".into(), json!(t.interval_secs / 60));
    v.insert(
        "interval_text".into(),
        json!(interval_text(t.interval_secs)),
    );
    v.insert(
        "utc_offset".into(),
        json!(t.utc_offset.map(offset_text).unwrap_or_default()),
    );
    v.insert("archived".into(), json!(t.archived));
    v.insert("health".into(), json!(status));
    v.insert("health_text".into(), json!(text));
    v.insert(
        "failing".into(),
        json!(status == "failing" || status == "empty"),
    );
    v.insert("upcoming_count".into(), json!(upcoming));
    let names: Vec<String> = site_rules(c, &t.id)?.into_iter().map(|r| r.title).collect();
    v.insert("rule_count".into(), json!(names.len()));
    v.insert(
        "rules_text".into(),
        json!(if names.is_empty() {
            "No rules yet".to_owned()
        } else {
            format!("Watching for {}", names.join(", "))
        }),
    );
    v.insert(
        "meta".into(),
        json!(format!(
            "{host} · {} · {}",
            t.last_extractor.as_deref().unwrap_or(&t.extractor),
            interval_text(t.interval_secs)
        )),
    );
    Ok(e)
}

// ---------- rules ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: String,
    pub title: String,
    pub spec: RuleSpec,
    pub archived: bool,
}

fn rule_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        title: r.get(1)?,
        spec: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
        archived: r.get(3)?,
    })
}

pub fn rule(c: &Connection, id: &str) -> rusqlite::Result<Option<Rule>> {
    c.query_row(
        "SELECT id, title, spec_json, archived FROM watcher_rules WHERE id = ?1",
        [id],
        rule_row,
    )
    .optional()
}

pub fn rule_by_client(c: &Connection, client_id: &str) -> rusqlite::Result<Option<Rule>> {
    c.query_row(
        "SELECT id, title, spec_json, archived FROM watcher_rules WHERE client_id = ?1",
        [client_id],
        rule_row,
    )
    .optional()
}

pub fn rules(c: &Connection) -> rusqlite::Result<Vec<Rule>> {
    let mut stmt = c.prepare(
        "SELECT id, title, spec_json, archived FROM watcher_rules
         ORDER BY archived, title COLLATE NOCASE, id",
    )?;
    stmt.query_map([], rule_row)?.collect()
}

pub fn live_rule_count(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM watcher_rules WHERE archived = 0",
        [],
        |r| r.get(0),
    )
}

pub fn save_rule(
    tx: &Transaction<'_>,
    r: &Rule,
    client_id: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    let spec = serde_json::to_string(&r.spec).unwrap_or_else(|_| "{}".into());
    tx.execute(
        "INSERT INTO watcher_rules (id, title, spec_json, archived, client_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (id) DO UPDATE SET title = ?2, spec_json = ?3, archived = ?4, updated_at = ?6",
        params![r.id, r.title, spec, r.archived, client_id, now],
    )?;
    rematch_rule(tx, r, now)
}

/// The live rules of site `target_id` (with any left for every site).
pub fn site_rules(c: &Connection, target_id: &str) -> rusqlite::Result<Vec<Rule>> {
    Ok(rules(c)?
        .into_iter()
        .filter(|r| {
            !r.archived
                && (r.spec.targets.is_empty() || r.spec.targets.iter().any(|t| t == target_id))
        })
        .collect())
}

/// The title of rule `r`'s site ("All sites" for a rule without one).
pub fn rule_site(c: &Connection, r: &Rule) -> rusqlite::Result<String> {
    match r.spec.targets.first() {
        None => Ok("All sites".to_owned()),
        Some(t) => Ok(target(c, t)?.map(|t| t.title).unwrap_or_default()),
    }
}

/// A rule's entity; `site` is its site's title ([`rule_site`]).
pub fn rule_entity(r: &Rule, site: &str) -> EntityRecord {
    let mut e = EntityRecord::new();
    let v = &mut e.0;
    v.insert("id".into(), json!(r.id));
    v.insert(
        "target_id".into(),
        json!(r.spec.targets.first().cloned().unwrap_or_default()),
    );
    v.insert("site".into(), json!(site));
    v.insert("title".into(), json!(r.title));
    v.insert("summary".into(), json!(r.spec.summary()));
    v.insert("archived".into(), json!(r.archived));
    v.insert("keywords".into(), json!(r.spec.any.join(", ")));
    v.insert("exclude".into(), json!(r.spec.none.join(", ")));
    e
}

// ---------- events ----------

/// An event's stable id: the source's own id when it has one, else its
/// normalized title, date and venue (so a re-scraped or reworded listing
/// is the same event).
pub fn event_id(target_id: &str, e: &RawEvent) -> String {
    let id = match &e.source_id {
        Some(s) => hash_hex(&["src", target_id, s.trim()]),
        None => {
            let date = format!(
                "{:04}-{:02}-{:02}",
                e.start.year, e.start.month, e.start.day
            );
            hash_hex(&[
                "key",
                target_id,
                &crate::rules::normalize(&e.title),
                &date,
                &crate::rules::normalize(&e.venue),
            ])
        }
    };
    id[..20].to_owned()
}

fn content_hash(e: &RawEvent) -> String {
    hash_hex(&[
        &e.title,
        &e.start.stored(),
        &e.end.map(|w| w.stored()).unwrap_or_default(),
        &e.venue,
        &e.description,
        &e.url,
    ])
}

/// Until when an event is listed: a few hours after a known start, a day
/// and a half after one whose zone is a guess or that has no time.
pub fn relevant_until(start: &When, start_ts: i64, offset_known: bool) -> i64 {
    match (start.time, offset_known) {
        (Some(_), true) => start_ts + 4 * 3600,
        (Some(_), false) => start_ts + 18 * 3600,
        (None, _) => start_ts + 38 * 3600,
    }
}

/// Which of `ids` are already stored.
pub fn existing_events(c: &Connection, ids: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stmt = c.prepare("SELECT 1 FROM watcher_events WHERE id = ?1")?;
    for id in ids {
        if stmt.exists([id])? {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Store `events` found on `t` (past ones dropped, duplicates merged) and
/// match the new and changed ones. Returns how many were new or changed.
pub fn store_events(
    tx: &Transaction<'_>,
    t: &Target,
    events: &[RawEvent],
    now: i64,
) -> rusqlite::Result<usize> {
    let rules: Vec<Rule> = rules(tx)?.into_iter().filter(|r| !r.archived).collect();
    let mut seen: Vec<String> = Vec::new();
    let mut changed = 0;
    for e in events {
        let start_ts = e.start.timestamp(t.utc_offset);
        let offset_known = e.start.offset_min.is_some() || t.utc_offset.is_some();
        let until = relevant_until(&e.start, start_ts, offset_known);
        if until < now {
            continue;
        }
        let id = event_id(&t.id, e);
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let hash = content_hash(e);
        let before: Option<String> = tx
            .query_row(
                "SELECT content_hash FROM watcher_events WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if before.as_deref() == Some(hash.as_str()) {
            tx.execute(
                "UPDATE watcher_events SET last_seen_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            continue;
        }
        tx.execute(
            "INSERT INTO watcher_events (id, target_id, source_id, title, start, end_at, start_ts,
                 relevant_until, venue, description, url, content_hash, first_seen_at, updated_at,
                 last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?13)
             ON CONFLICT (id) DO UPDATE SET title = ?4, start = ?5, end_at = ?6, start_ts = ?7,
                 relevant_until = ?8, venue = ?9, description = ?10, url = ?11, content_hash = ?12,
                 updated_at = ?13, last_seen_at = ?13",
            params![
                id,
                t.id,
                e.source_id,
                e.title,
                e.start.stored(),
                e.end.map(|w| w.stored()),
                start_ts,
                until,
                e.venue,
                e.description,
                e.url,
                hash,
                now
            ],
        )?;
        changed += 1;
        let cand = Candidate {
            target_id: &t.id,
            title: &e.title,
            venue: &e.venue,
            description: &e.description,
            start: e.start,
            start_ts,
        };
        for r in &rules {
            set_match(tx, &id, &r.id, r.spec.matches(&cand, now), now)?;
        }
    }
    Ok(changed)
}

/// Add (keeping its notified and seen state) or remove one match.
fn set_match(
    c: &Connection,
    event: &str,
    rule: &str,
    matched: bool,
    now: i64,
) -> rusqlite::Result<()> {
    if matched {
        c.execute(
            "INSERT OR IGNORE INTO watcher_matches (event_id, rule_id, matched_at) VALUES (?1, ?2, ?3)",
            params![event, rule, now],
        )?;
    } else {
        c.execute(
            "DELETE FROM watcher_matches WHERE event_id = ?1 AND rule_id = ?2",
            params![event, rule],
        )?;
    }
    Ok(())
}

/// A stored event, for matching and display.
struct StoredEvent {
    id: String,
    target_id: String,
    title: String,
    start: String,
    start_ts: i64,
    venue: String,
    description: String,
}

/// Match rule `r` against every upcoming event (a new or changed rule); an
/// archived rule loses its matches.
fn rematch_rule(tx: &Transaction<'_>, r: &Rule, now: i64) -> rusqlite::Result<()> {
    if r.archived {
        tx.execute("DELETE FROM watcher_matches WHERE rule_id = ?1", [&r.id])?;
        return Ok(());
    }
    let events: Vec<StoredEvent> = {
        let mut stmt = tx.prepare(
            "SELECT id, target_id, title, start, start_ts, venue, description FROM watcher_events
             WHERE relevant_until >= ?1",
        )?;
        stmt.query_map([now], |row| {
            Ok(StoredEvent {
                id: row.get(0)?,
                target_id: row.get(1)?,
                title: row.get(2)?,
                start: row.get(3)?,
                start_ts: row.get(4)?,
                venue: row.get(5)?,
                description: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for e in events {
        let Some(start) = When::parse(&e.start) else {
            continue;
        };
        let cand = Candidate {
            target_id: &e.target_id,
            title: &e.title,
            venue: &e.venue,
            description: &e.description,
            start,
            start_ts: e.start_ts,
        };
        set_match(tx, &e.id, &r.id, r.spec.matches(&cand, now), now)?;
    }
    Ok(())
}

/// Mark the matches of event `id` seen (or not).
pub fn set_seen(c: &Connection, id: &str, seen: bool) -> rusqlite::Result<bool> {
    let exists = c
        .prepare("SELECT 1 FROM watcher_events WHERE id = ?1")?
        .exists([id])?;
    c.execute(
        "UPDATE watcher_matches SET seen = ?2 WHERE event_id = ?1",
        params![id, seen],
    )?;
    Ok(exists)
}

/// Delete events over for [`KEEP_PAST_SECS`] and their matches.
pub fn prune(c: &Connection, now: i64) -> rusqlite::Result<usize> {
    let cutoff = now - KEEP_PAST_SECS;
    c.execute(
        "DELETE FROM watcher_matches WHERE event_id IN
             (SELECT id FROM watcher_events WHERE relevant_until < ?1)",
        [cutoff],
    )?;
    c.execute(
        "DELETE FROM watcher_events WHERE relevant_until < ?1",
        [cutoff],
    )
}

/// An event's entity: its fields, the display text (in the target's
/// offset when it has one) and its matches.
fn event_entity(c: &Connection, id: &str) -> rusqlite::Result<Option<EntityRecord>> {
    let row = c
        .query_row(
            "SELECT e.title, e.start, e.end_at, e.venue, e.description, e.url, e.target_id,
                    t.title, t.utc_offset_min, e.first_seen_at
             FROM watcher_events e JOIN watcher_targets t ON t.id = e.target_id
             WHERE e.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<i32>>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((title, start, end, venue, description, url, target_id, source, offset, first_seen)) =
        row
    else {
        return Ok(None);
    };
    let (rule_titles, seen): (Vec<String>, bool) = {
        let mut stmt = c.prepare(
            "SELECT r.title, m.seen FROM watcher_matches m JOIN watcher_rules r ON r.id = m.rule_id
             WHERE m.event_id = ?1 AND r.archived = 0 ORDER BY r.title",
        )?;
        let rows: Vec<(String, bool)> = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let seen = !rows.is_empty() && rows.iter().all(|(_, s)| *s);
        (rows.into_iter().map(|(t, _)| t).collect(), seen)
    };
    let shown = When::parse(&start).map(|w| w.shifted_to(offset));
    let date_text = shown.map(|w| w.date_text()).unwrap_or_default();
    let time_text = shown.and_then(|w| w.time_text()).unwrap_or_default();
    let when_text = [date_text.as_str(), time_text.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    let meta = [when_text.as_str(), venue.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    let mut e = EntityRecord::new();
    let v = &mut e.0;
    v.insert("id".into(), json!(id));
    v.insert("title".into(), json!(crate::html::clean_title(&title)));
    v.insert("start".into(), json!(start));
    v.insert("end".into(), json!(end.unwrap_or_default()));
    v.insert("date_text".into(), json!(date_text));
    v.insert("time_text".into(), json!(time_text));
    v.insert("when_text".into(), json!(when_text));
    v.insert("meta".into(), json!(meta));
    v.insert("venue".into(), json!(venue));
    // Plain text; without a first line that only repeats the site's name
    // (Meetup starts every description with the group's).
    let mut text = crate::html::markdown_to_text(&description);
    if let Some((first, rest)) = text.split_once('\n')
        && crate::rules::normalize(first) == crate::rules::normalize(&source)
    {
        text = rest.trim_start().to_owned();
    }
    v.insert("description".into(), json!(text));
    v.insert(
        "tickets_url".into(),
        json!(crate::html::tickets_url(&description).unwrap_or_default()),
    );
    v.insert("url".into(), json!(url));
    v.insert("target_id".into(), json!(target_id));
    v.insert("source".into(), json!(source));
    v.insert("matched".into(), json!(!rule_titles.is_empty()));
    v.insert(
        "rules_text".into(),
        json!(if rule_titles.is_empty() {
            String::new()
        } else {
            format!("Matched {}", rule_titles.join(", "))
        }),
    );
    v.insert(
        "match_text".into(),
        json!(if rule_titles.is_empty() {
            String::new()
        } else {
            format!("{source} · {}", rule_titles.join(", "))
        }),
    );
    v.insert("seen".into(), json!(seen));
    v.insert("first_seen_at".into(), json!(first_seen));
    Ok(Some(e))
}

fn push_event(c: &Connection, b: &mut DataBundle, query: &str, id: &str) -> rusqlite::Result<()> {
    if let Some(e) = event_entity(c, id)? {
        b.entities.insert(event_uri(id), e);
        b.queries
            .entry(query.to_owned())
            .or_default()
            .push(event_uri(id));
    }
    Ok(())
}

/// The `matches` feed: upcoming events some live rule matched, soonest first.
pub fn matches_bundle(c: &Connection, now: i64) -> rusqlite::Result<DataBundle> {
    let mut b = DataBundle::default();
    b.queries.insert(MATCHES_QUERY.into(), Vec::new());
    let ids: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT DISTINCT e.id, e.start_ts FROM watcher_events e
             JOIN watcher_matches m ON m.event_id = e.id
             JOIN watcher_rules r ON r.id = m.rule_id AND r.archived = 0
             JOIN watcher_targets t ON t.id = e.target_id AND t.archived = 0
             WHERE e.relevant_until >= ?1 ORDER BY e.start_ts, e.id LIMIT 200",
        )?;
        stmt.query_map([now], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in ids {
        push_event(c, &mut b, MATCHES_QUERY, &id)?;
    }
    Ok(b)
}

/// The `targets` feed: every target with its health, and every rule.
pub fn targets_bundle(c: &Connection, now: i64) -> rusqlite::Result<DataBundle> {
    let mut b = DataBundle::default();
    let mut tq = Vec::new();
    for t in targets(c)? {
        b.entities
            .insert(target_uri(&t.id), target_entity(c, &t, now)?);
        tq.push(target_uri(&t.id));
    }
    let mut rq = Vec::new();
    for r in rules(c)? {
        let site = rule_site(c, &r)?;
        b.entities.insert(rule_uri(&r.id), rule_entity(&r, &site));
        rq.push(rule_uri(&r.id));
    }
    b.queries.insert(TARGETS_QUERY.into(), tq);
    b.queries.insert(RULES_QUERY.into(), rq);
    Ok(b)
}

/// The event screen's data.
pub fn event_screen(c: &Connection, id: &str) -> rusqlite::Result<Option<DataBundle>> {
    let Some(e) = event_entity(c, id)? else {
        return Ok(None);
    };
    let mut b = DataBundle::default();
    b.entities.insert(event_uri(id), e);
    b.queries.insert(event_query(id), vec![event_uri(id)]);
    Ok(Some(b))
}

/// The target screen's data: the target and its upcoming events.
pub fn target_screen(c: &Connection, id: &str, now: i64) -> rusqlite::Result<Option<DataBundle>> {
    let Some(t) = target(c, id)? else {
        return Ok(None);
    };
    let mut b = DataBundle::default();
    b.entities
        .insert(target_uri(id), target_entity(c, &t, now)?);
    b.queries.insert(target_query(id), vec![target_uri(id)]);
    let mut rq = Vec::new();
    for r in site_rules(c, id)? {
        let site = rule_site(c, &r)?;
        b.entities.insert(rule_uri(&r.id), rule_entity(&r, &site));
        rq.push(rule_uri(&r.id));
    }
    b.queries.insert(target_rules_query(id), rq);
    let q = target_events_query(id);
    b.queries.insert(q.clone(), Vec::new());
    let ids: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT id FROM watcher_events WHERE target_id = ?1 AND relevant_until >= ?2
             ORDER BY start_ts, id LIMIT 200",
        )?;
        stmt.query_map(params![id, now], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for e in ids {
        push_event(c, &mut b, &q, &e)?;
    }
    Ok(Some(b))
}

/// A match waiting for its notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub event_id: String,
    pub title: String,
    pub meta: String,
    pub rules: Vec<String>,
}

/// Upcoming events with matches not notified yet (live rules and targets),
/// oldest match first; marks them notified.
pub fn take_pending(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<Vec<Pending>> {
    let ids: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT m.event_id, MIN(m.matched_at) AS first FROM watcher_matches m
             JOIN watcher_events e ON e.id = m.event_id AND e.relevant_until >= ?1
             JOIN watcher_rules r ON r.id = m.rule_id AND r.archived = 0
             JOIN watcher_targets t ON t.id = e.target_id AND t.archived = 0
             WHERE m.notified_at IS NULL GROUP BY m.event_id ORDER BY first, m.event_id",
        )?;
        stmt.query_map([now], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for id in ids {
        let Some(e) = event_entity(tx, &id)? else {
            continue;
        };
        let rules: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT r.title FROM watcher_matches m JOIN watcher_rules r ON r.id = m.rule_id
                 WHERE m.event_id = ?1 AND m.notified_at IS NULL AND r.archived = 0 ORDER BY r.title",
            )?;
            stmt.query_map([&id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "UPDATE watcher_matches SET notified_at = ?2 WHERE event_id = ?1 AND notified_at IS NULL",
            params![id, now],
        )?;
        out.push(Pending {
            event_id: id,
            title: e.get_str("title").unwrap_or("").to_owned(),
            meta: e.get_str("meta").unwrap_or("").to_owned(),
            rules,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(title: &str, start: &str, id: Option<&str>) -> RawEvent {
        RawEvent {
            source_id: id.map(str::to_owned),
            title: title.into(),
            start: When::parse(start).unwrap(),
            end: None,
            venue: "The End".into(),
            description: "Lineup: Budget Cuts".into(),
            url: String::new(),
        }
    }

    #[test]
    fn identity() {
        let a = raw("Improv  Night", "2026-11-06T19:30", None);
        let b = raw("improv night", "2026-11-06T20:00", None);
        assert_eq!(
            event_id("t", &a),
            event_id("t", &b),
            "same title, day and venue"
        );
        let c = raw("Other", "2026-11-06T19:30", Some("x1"));
        let d = raw("Renamed", "2026-11-07T19:30", Some("x1"));
        assert_eq!(event_id("t", &c), event_id("t", &d), "source ids win");
        assert_ne!(event_id("t", &c), event_id("u", &c));
    }

    #[test]
    fn ago() {
        assert_eq!(ago_text(30), "just now");
        assert_eq!(ago_text(600), "10 min ago");
        assert_eq!(ago_text(7200), "2 h ago");
        assert_eq!(interval_text(21_600), "every 6 h");
        assert_eq!(interval_text(86_400), "every 1 day");
    }
}
