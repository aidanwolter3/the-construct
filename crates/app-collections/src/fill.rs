//! Running fillers (decision `collection-fillers`; the spec is
//! [`crate::filler`]): fetch or ask, read the items, write the filler's
//! rows, record how it went, then send what alerts owe.
//!
//! A fill (of one filler) starts on its schedule (the adapter's poll,
//! [`run_due`]), when an app showing the collection opens and the last fill
//! is older than `on_open_minutes` or a scheduled one is overdue
//! ([`due_on_open`]), from a layout's refresh action, and from Claude
//! (`collections_refresh`, and after a filler is set). One fill per filler
//! runs at a time.
//!
//! - **fetch**: a page fetch ([`web_extract::fetch`]): conditional
//!   headers, size and time caps, public hosts only, a second between
//!   requests to one host. A 304 or an unchanged body ends the fill with no
//!   work (unless the recipe changed). The extractor reads the items (or,
//!   for `llm`, a model reads the page's text), the map picks each field's
//!   value, values are coerced to the fields' types (one that doesn't fit is
//!   skipped and counted; a time without an offset is placed in the
//!   filler's zone). `upcoming` drops past items. With `url_field`, each
//!   row's own page fills that row. `details` then reads each new row's
//!   own page (at most [`MAX_DETAILS`] a fill).
//! - **agent**: [`crate::agent`], within the filler's daily budget and the
//!   pod's.
//!
//! The filler's rows are written in one transaction ([`store`]): `replace`
//! makes them the answer (rows it no longer lists are deleted for good, not
//! kept for 30 days: they were never anyone's), `upsert` adds and updates
//! by key and leaves other rows alone. Other fillers' rows and people's
//! rows are never touched. Computed fields compute as after any write.
//! Failures back off (doubling from the interval, at
//! most a day, at least `Retry-After`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use pod_core::Pod;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::agent::{AgentField, AgentRequest, PageRequest};
use crate::db::{self, Row};
use crate::filler::{Filler, Source};
use crate::model::{self, Definition, Kind};
use crate::{Caps, CollectionsAdapter, NAME};

/// Pod setting: `0` turns fillers off (temporary pods, which anyone can
/// make, must not make the server fetch URLs of their choosing or spend its
/// model key).
pub const FILL_SETTING: &str = "collections.fill";
/// The error a fill records when fillers are off.
pub const FILL_OFF: &str =
    "fillers are turned off on this pod (temporary pods can't fetch or ask a model)";
/// The error of a model fill without a model.
pub const NO_AGENT: &str = "this filler needs a model: set CONSTRUCT_INFER_API_KEY on the server";
/// The least time between two requests to one host from a pod.
pub const HOST_SPACING: Duration = Duration::from_secs(1);
/// A whole fetch fill's time limit.
pub const FETCH_FILL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// A whole model fill's time limit.
pub const AGENT_FILL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Empty fills in a row before a filler reads "empty".
pub const EMPTY_AFTER: i64 = 3;
/// New rows' own pages read per fill (`details`).
pub const MAX_DETAILS: usize = 10;
/// A dated item this long past is left out (`upcoming`).
pub const PAST_SLACK_SECS: i64 = 3 * 3600;

// ---------- state ----------

/// One filler's fill state (`collections_fills`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub fingerprint: String,
    pub next_at: Option<i64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    pub last_fill_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
    pub failures: i64,
    pub empty_runs: i64,
    pub last_rows: Option<i64>,
    pub last_skipped: Option<i64>,
    /// Items of the last fill left out as past (`upcoming`).
    pub last_past: Option<i64>,
    pub day: Option<String>,
    pub day_fills: i64,
    /// The next fill records alert matches without notifying (rows a
    /// migration brings in were announced before).
    pub quiet_next: bool,
}

/// The fill state of `filler` of `collection` (default: never filled).
pub fn state(conn: &Connection, collection: &str, filler: &str) -> rusqlite::Result<State> {
    Ok(conn
        .query_row(
            "SELECT fingerprint, next_at, etag, last_modified, content_hash, last_fill_at, last_ok_at,
                    last_error, failures, empty_runs, last_rows, last_skipped, day, day_fills,
                    quiet_next, last_past
             FROM collections_fills WHERE collection = ?1 AND filler = ?2",
            params![collection, filler],
            |r| {
                Ok(State {
                    fingerprint: r.get(0)?,
                    next_at: r.get(1)?,
                    etag: r.get(2)?,
                    last_modified: r.get(3)?,
                    content_hash: r.get(4)?,
                    last_fill_at: r.get(5)?,
                    last_ok_at: r.get(6)?,
                    last_error: r.get(7)?,
                    failures: r.get(8)?,
                    empty_runs: r.get(9)?,
                    last_rows: r.get(10)?,
                    last_skipped: r.get(11)?,
                    day: r.get(12)?,
                    day_fills: r.get(13)?,
                    quiet_next: r.get(14)?,
                    last_past: r.get(15)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

/// Every filler's state of `def`, by filler id.
pub fn states(conn: &Connection, def: &Definition) -> rusqlite::Result<HashMap<String, State>> {
    def.fillers
        .iter()
        .map(|f| Ok((f.id.clone(), state(conn, &def.name, &f.id)?)))
        .collect()
}

/// Store `s` as the state of `filler` of `collection`.
pub fn save_state(
    conn: &Connection,
    collection: &str,
    filler: &str,
    s: &State,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO collections_fills (collection, filler, fingerprint, next_at, etag,
            last_modified, content_hash, last_fill_at, last_ok_at, last_error, failures,
            empty_runs, last_rows, last_skipped, day, day_fills, quiet_next, last_past)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT (collection, filler) DO UPDATE SET fingerprint = excluded.fingerprint,
            next_at = excluded.next_at, etag = excluded.etag, last_modified = excluded.last_modified,
            content_hash = excluded.content_hash, last_fill_at = excluded.last_fill_at,
            last_ok_at = excluded.last_ok_at, last_error = excluded.last_error,
            failures = excluded.failures, empty_runs = excluded.empty_runs,
            last_rows = excluded.last_rows, last_skipped = excluded.last_skipped,
            day = excluded.day, day_fills = excluded.day_fills, quiet_next = excluded.quiet_next,
            last_past = excluded.last_past",
        params![
            collection,
            filler,
            s.fingerprint,
            s.next_at,
            s.etag,
            s.last_modified,
            s.content_hash,
            s.last_fill_at,
            s.last_ok_at,
            s.last_error,
            s.failures,
            s.empty_runs,
            s.last_rows,
            s.last_skipped,
            s.day,
            s.day_fills,
            s.quiet_next,
            s.last_past
        ],
    )?;
    Ok(())
}

/// Forget the state of fillers of `collection` no longer in `keep`, and
/// hand their rows to people (they stay, no longer locked).
pub fn forget_removed(tx: &Connection, collection: &str, keep: &[String]) -> rusqlite::Result<()> {
    let ids: Vec<String> = {
        let mut stmt =
            tx.prepare_cached("SELECT filler FROM collections_fills WHERE collection = ?1")?;
        stmt.query_map([collection], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in ids.iter().filter(|i| !keep.contains(i)) {
        tx.execute(
            "DELETE FROM collections_fills WHERE collection = ?1 AND filler = ?2",
            params![collection, id],
        )?;
    }
    let tagged: Vec<String> = {
        let mut stmt = tx.prepare_cached(
            "SELECT DISTINCT filler FROM collections_rows WHERE collection = ?1 AND filler IS NOT NULL",
        )?;
        stmt.query_map([collection], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in tagged.iter().filter(|i| !keep.contains(i)) {
        tx.execute(
            "UPDATE collections_rows SET filler = NULL WHERE collection = ?1 AND filler = ?2",
            params![collection, id],
        )?;
    }
    Ok(())
}

/// Model fills made today across the pod (`today`: `YYYY-MM-DD`).
fn pod_model_fills_today(conn: &Connection, today: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(day_fills), 0) FROM collections_fills WHERE day = ?1",
        [today],
        |r| r.get(0),
    )
}

// ---------- secrets ----------

fn secret_context(name: &str) -> String {
    format!("collections_secret:{name}")
}

/// Store secret `name`, sealed, sent only to `host` (or with `None`,
/// forget it).
pub fn set_secret(
    store: &pod_core::store::Store,
    name: &str,
    value: Option<(&str, &str)>,
    now: i64,
) -> rusqlite::Result<()> {
    let sealed = value.map(|(v, host)| (store.secrets().seal(&secret_context(name), v), host));
    store.with_conn(|c| {
        match sealed {
            Some((s, host)) => c.execute(
                "INSERT INTO collections_secrets (name, value, updated_at, host) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value,
                     updated_at = excluded.updated_at, host = excluded.host",
                params![name, s, now, host],
            )?,
            None => c.execute("DELETE FROM collections_secrets WHERE name = ?1", [name])?,
        };
        Ok(())
    })
}

/// The host a secret is bound to, from what `collections_set_secret` was
/// given: a host name or a URL (its host), lowercase.
pub fn secret_host(given: &str) -> Option<String> {
    let given = given.trim();
    let url = if given.contains("://") {
        given.to_owned()
    } else {
        format!("https://{given}")
    };
    let host = web_extract::fetch::host_of(&url);
    let host = host.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_owned())
}

/// The names of the stored secrets.
pub fn secret_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT name FROM collections_secrets ORDER BY name")?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

/// Request headers with their secrets filled in.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub headers: Vec<(String, String)>,
    /// A secret is in them: send with
    /// [`crate::CollectionsAdapter::secret_fetch_client`].
    pub has_secret: bool,
}

/// `headers` for a request to `url`, with each `{secret:<name>}` replaced
/// by the secret's value. A secret goes only to the host it was stored
/// for, so a recipe can't send it anywhere else, and only over `https://`
/// (`allow_http`: plain HTTP too, for a pod that may fetch private hosts),
/// so it never crosses the network in the clear.
fn resolve_headers(
    store: &pod_core::store::Store,
    headers: &std::collections::BTreeMap<String, String>,
    url: &str,
    allow_http: bool,
) -> Result<Resolved, String> {
    let mut out = Resolved::default();
    let to = web_extract::fetch::host_of(url);
    let https = reqwest::Url::parse(url).is_ok_and(|u| u.scheme() == "https");
    for (k, v) in headers {
        let mut value = v.clone();
        for name in crate::filler::secret_refs(v) {
            let stored: Option<(String, Option<String>)> = store
                .with_conn(|c| {
                    c.query_row(
                        "SELECT value, host FROM collections_secrets WHERE name = ?1",
                        [&name],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                })
                .map_err(|_| "the secrets could not be read".to_owned())?;
            let Some((stored, host)) = stored else {
                return Err(format!(
                    "the secret '{name}' is not set (collections_set_secret sets it)"
                ));
            };
            match host {
                None => {
                    return Err(format!(
                        "the secret '{name}' has no host: set it again with collections_set_secret \
                         and the host it is for"
                    ));
                }
                Some(h) if h != to => {
                    tracing::warn!(secret = %name, "a secret was not sent: the request is for another host");
                    return Err(format!(
                        "the secret '{name}' is only sent to {h}, not to {to}"
                    ));
                }
                Some(_) => {}
            }
            if !https && !allow_http {
                tracing::warn!(secret = %name, "a secret was not sent: the request isn't https");
                return Err(format!(
                    "the secret '{name}' is only sent over https://, not in the clear: use https://{to}"
                ));
            }
            let plain = store
                .secrets()
                .open(&secret_context(&name), &stored)
                .map_err(|_| {
                    format!(
                        "the secret '{name}' can't be opened (the server key changed?); set it again"
                    )
                })?;
            value = value.replace(&format!("{{secret:{name}}}"), &plain);
            out.has_secret = true;
        }
        out.headers.push((k.clone(), value));
    }
    Ok(out)
}

/// [`resolve_headers`] for a preview.
pub fn resolve_headers_for(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    headers: &std::collections::BTreeMap<String, String>,
    url: &str,
) -> Result<Resolved, String> {
    resolve_headers(pod.store(), headers, url, adapter.allow_private_hosts())
}

/// Up to a few raw items of `body` (no map), for writing paths: what the
/// extractor sees before the map picks values.
pub fn sample_items(spec: &web_extract::items::Spec, body: &str) -> Result<Vec<Value>, String> {
    use web_extract::items::{self, Kind as X};
    let items = match spec.kind {
        X::Json => {
            let doc: Value = serde_json::from_str(body.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("the body is not JSON: {e}"))?;
            web_extract::path::parse(&spec.items)?.items(&doc)
        }
        X::EmbeddedJson => {
            let id = if spec.script.is_empty() {
                items::DEFAULT_SCRIPT
            } else {
                &spec.script
            };
            let text = items::script_by_id(body, id)
                .ok_or_else(|| format!("the page has no <script id=\"{id}\">"))?;
            let doc: Value = serde_json::from_str(text.trim()).map_err(|e| e.to_string())?;
            web_extract::path::parse(&spec.items)?.items(&doc)
        }
        X::Jsonld => items::jsonld(body, &spec.types),
        X::Rss => items::rss(body),
        X::Ics => items::ics_in(body, spec.zone().as_ref()),
        X::Events => web_extract::events::items(
            web_extract::events::read(body, ""),
            spec.zone().as_ref(),
            "",
        ),
        X::Selectors => return Err("the selectors extractor needs map".into()),
    };
    Ok(items.into_iter().map(|v| shorten(v, 0)).collect())
}

/// `v` with long text cut and long lists trimmed, so a sample stays short.
fn shorten(v: Value, depth: usize) -> Value {
    match v {
        Value::String(s) => json!(web_extract::html::truncate(&s, 200)),
        Value::Array(a) if depth > 6 => json!(format!("[{} items]", a.len())),
        Value::Array(a) => {
            let n = a.len();
            let mut out: Vec<Value> = a
                .into_iter()
                .take(3)
                .map(|x| shorten(x, depth + 1))
                .collect();
            if n > 3 {
                out.push(json!(format!("… {} more", n - 3)));
            }
            Value::Array(out)
        }
        Value::Object(o) if depth > 6 => json!(format!("{{{} keys}}", o.len())),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .take(40)
                .map(|(k, x)| (k, shorten(x, depth + 1)))
                .collect(),
        ),
        other => other,
    }
}

// ---------- values ----------

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A date in a form feeds and calendars use, as `YYYY-MM-DD`: iCalendar
/// `20261106T193000`, RFC 2822 `Tue, 03 Nov 2026 20:00:00 GMT`.
fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) && b.get(8).is_none_or(|c| *c == b'T')
    {
        return Some(format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8]));
    }
    // `[Day,] DD Mon YYYY ...`
    let words: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let words = if words
        .first()
        .is_some_and(|w| w.chars().all(char::is_alphabetic))
    {
        &words[1..]
    } else {
        &words[..]
    };
    if let [d, m, y, ..] = words
        && let (Ok(d), Some(m), Ok(y)) = (
            d.parse::<u32>(),
            MONTHS
                .iter()
                .position(|x| m.to_ascii_lowercase().starts_with(x)),
            y.parse::<u32>(),
        )
    {
        return Some(format!("{y:04}-{:02}-{d:02}", m + 1));
    }
    None
}

/// `v` made readable as a value of `kind` where web data commonly differs:
/// prices with currency signs and thousands separators, feed and calendar
/// dates, schema.org enumeration URLs for choices, and a datetime without an
/// offset placed in `zone`.
fn normalize(kind: Kind, v: Value, zone: Option<&web_extract::when::Zone>) -> Value {
    let Value::String(s) = &v else { return v };
    match kind {
        Kind::Number => {
            let t: String = s
                .trim()
                .trim_matches(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
                .chars()
                .filter(|c| *c != ',')
                .collect();
            if t.parse::<f64>().is_ok() {
                json!(t)
            } else {
                v
            }
        }
        Kind::Date => normalize_date(s).map_or(v, |d| json!(d)),
        Kind::Datetime => match (web_extract::when::When::parse(s), zone) {
            (Some(w), Some(z)) => json!(w.placed_in(z).stored()),
            _ => v,
        },
        Kind::Choice if s.contains('/') => {
            json!(s.rsplit('/').next().unwrap_or(s))
        }
        _ => v,
    }
}

/// An item (values by field id) as stored values: each coerced to its
/// field's type. Returns the values and how many didn't fit.
fn to_values(
    def: &Definition,
    item: Map<String, Value>,
    max_chars: usize,
    zone: Option<&web_extract::when::Zone>,
) -> (Map<String, Value>, usize) {
    let mut out = Map::new();
    let mut skipped = 0;
    for (id, v) in item {
        let Some(f) = def.by_id(&id).filter(|f| f.live()) else {
            continue;
        };
        let v = normalize(f.kind, v, zone);
        let v = match (&v, f.kind) {
            // Text keeps what it can.
            (Value::String(s), Kind::Text) if s.chars().count() > max_chars => {
                json!(s.chars().take(max_chars).collect::<String>())
            }
            _ => v,
        };
        match model::coerce(f, &v, max_chars) {
            Ok(Some(x)) => {
                out.insert(id, x);
            }
            Ok(None) => {}
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

/// Rows of a preview as the collection would store them.
pub fn coerce_preview(
    def: &Definition,
    rows: Vec<Map<String, Value>>,
    max_chars: usize,
    zone: Option<&web_extract::when::Zone>,
) -> (Vec<Map<String, Value>>, usize) {
    coerce_all(def, rows, usize::MAX, max_chars, zone)
}

/// A key value as a match key.
fn key_text(v: &Value) -> Option<String> {
    let t = match v {
        Value::String(s) => s.trim().to_lowercase(),
        Value::Null => return None,
        other => other.to_string(),
    };
    (!t.is_empty()).then_some(t)
}

/// Whether an item's `upcoming` value has passed at `now`.
fn is_past(v: Option<&Value>, now: i64) -> bool {
    let Some(w) = v
        .and_then(Value::as_str)
        .and_then(web_extract::when::When::parse)
    else {
        return false;
    };
    let end = if w.time.is_some() {
        w.timestamp(None) + PAST_SLACK_SECS
    } else {
        // A date lasts the day, with a day of slack for zones.
        w.timestamp(None) + 2 * 86_400
    };
    end < now
}

// ---------- storing ----------

/// What storing a fill did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stored {
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Items left out: no key, a repeated key, a required filled value
    /// missing, or past (`upcoming`).
    pub dropped: usize,
    /// Of `dropped`, the items left out as past.
    pub past: usize,
}

impl Stored {
    pub fn changed(&self) -> bool {
        self.created + self.updated + self.removed > 0
    }
}

fn hard_delete(tx: &Connection, collection: &str, id: &str) -> rusqlite::Result<()> {
    db::note_change(tx, collection, id)?;
    tx.execute(
        "DELETE FROM collections_rows WHERE collection = ?1 AND id = ?2",
        params![collection, id],
    )?;
    Ok(())
}

/// The write `after_write` sees: `fields` by name, null when the item has no
/// value.
fn write_of(
    def: &Definition,
    fields: &[String],
    values: &Map<String, Value>,
) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|id| def.by_id(id))
        .map(|f| {
            (
                f.name.clone(),
                values.get(&f.id).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// Set `row`'s `fields` to `item`'s; whether anything changed.
fn apply_item(row: &mut Row, fields: &[String], item: &Map<String, Value>) -> bool {
    let before = row.values.clone();
    for id in fields {
        match item.get(id) {
            Some(v) => {
                row.values.insert(id.clone(), v.clone());
            }
            None => {
                row.values.remove(id);
            }
        }
    }
    row.values != before
}

/// Whether `values` lacks a required field of `fields` without a default.
fn missing_required(def: &Definition, fields: &[String], values: &Map<String, Value>) -> bool {
    fields.iter().any(|id| {
        def.by_id(id)
            .is_some_and(|f| !f.optional && f.default.is_none() && !values.contains_key(id))
    })
}

/// What [`store`] answers: what it did, the compute jobs, and the rows it
/// created (for `details`).
pub type StoreResult = (Stored, Vec<crate::compute::Job>, Vec<String>);

/// Write `items` (values by field id, coerced) as filler `filler`'s rows of
/// `def` by its mode. `Err`: the pod's row or byte cap would be passed (the
/// caller rolls back).
pub fn store(
    tx: &Connection,
    def: &Definition,
    filler: &Filler,
    items: Vec<Map<String, Value>>,
    now: i64,
    caps: Caps,
) -> rusqlite::Result<Result<StoreResult, String>> {
    let name = &def.name;
    let mapped = filler.mapped();
    let mut out = Stored::default();
    let mut jobs = Vec::new();
    let mut created = Vec::new();
    let new_row = |values: Map<String, Value>| Row {
        id: db::new_id("r"),
        client_id: None,
        values,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        filler: Some(filler.id.clone()),
    };
    // Past items are left out.
    let items: Vec<Map<String, Value>> = match &filler.upcoming {
        Some(u) => items
            .into_iter()
            .filter(|i| {
                let past = is_past(i.get(u), now);
                if past {
                    out.dropped += 1;
                    out.past += 1;
                }
                !past
            })
            .collect(),
        None => items,
    };
    let existing: Vec<Row> = db::all_rows(tx, name, true)?
        .into_iter()
        .filter(|r| r.filler.as_deref() == Some(filler.id.as_str()))
        .collect();
    if let Some(key) = &filler.key {
        let mut by_key: HashMap<String, Row> = HashMap::new();
        let mut unkeyed: Vec<Row> = Vec::new();
        for r in existing {
            match r.values.get(key).and_then(key_text) {
                Some(k) if !by_key.contains_key(&k) => {
                    by_key.insert(k, r);
                }
                _ => unkeyed.push(r),
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        for item in items {
            let Some(k) = item.get(key).and_then(key_text) else {
                out.dropped += 1;
                continue;
            };
            if !seen.insert(k.clone()) {
                out.dropped += 1;
                continue;
            }
            match by_key.get_mut(&k) {
                // Deleted by a person (upsert): it stays deleted.
                Some(r) if r.deleted_at.is_some() => out.unchanged += 1,
                Some(r) => {
                    let before = r.values.clone();
                    if apply_item(r, &mapped, &item) {
                        if missing_required(def, &mapped, &r.values) {
                            r.values = before;
                            out.dropped += 1;
                            continue;
                        }
                        let write = write_of(def, &mapped, &item);
                        jobs.extend(crate::compute::after_write(
                            tx,
                            def,
                            Some(&before),
                            r,
                            &write,
                            now,
                            caps,
                        )?);
                        r.updated_at = now;
                        db::save_row(tx, name, r)?;
                        out.updated += 1;
                    } else {
                        out.unchanged += 1;
                    }
                }
                None => {
                    if missing_required(def, &mapped, &item) {
                        out.dropped += 1;
                        continue;
                    }
                    let mut r = new_row(item.clone());
                    let write = write_of(def, &mapped, &item);
                    jobs.extend(crate::compute::after_write(
                        tx, def, None, &mut r, &write, now, caps,
                    )?);
                    db::insert_row(tx, name, &r)?;
                    created.push(r.id.clone());
                    out.created += 1;
                }
            }
        }
        if filler.owns_rows() {
            for (k, r) in &by_key {
                if !seen.contains(k) {
                    hard_delete(tx, name, &r.id)?;
                    out.removed += 1;
                }
            }
            for r in &unkeyed {
                hard_delete(tx, name, &r.id)?;
                out.removed += 1;
            }
        }
    } else {
        // Replace without a key: its rows are the items, in order.
        let mut items: Vec<Map<String, Value>> = items
            .into_iter()
            .filter(|i| {
                let keep = !missing_required(def, &mapped, i);
                if !keep {
                    out.dropped += 1;
                }
                keep
            })
            .collect();
        let live: Vec<&Row> = existing.iter().filter(|r| r.deleted_at.is_none()).collect();
        let same = live.len() == items.len()
            && live
                .iter()
                .zip(&items)
                .all(|(r, i)| mapped.iter().all(|id| r.values.get(id) == i.get(id)));
        if same {
            out.unchanged = items.len();
        } else {
            for r in &existing {
                hard_delete(tx, name, &r.id)?;
                out.removed += 1;
            }
            // Inserted in the answer's order: rows made in one second list
            // in insertion order ([`db::all_rows`]).
            for item in items.drain(..) {
                let mut r = new_row(item.clone());
                let write = write_of(def, &mapped, &item);
                jobs.extend(crate::compute::after_write(
                    tx, def, None, &mut r, &write, now, caps,
                )?);
                db::insert_row(tx, name, &r)?;
                created.push(r.id.clone());
                out.created += 1;
            }
        }
    }
    db::trim_retained(tx, crate::retained_cap(caps))?;
    let (n, b) = db::usage(tx)?;
    if n > caps.rows as i64 {
        return Ok(Err(format!(
            "the fill would pass the pod's {} rows (across its collections)",
            caps.rows
        )));
    }
    if b > caps.bytes as i64 {
        return Ok(Err(format!(
            "the fill would pass the pod's {} bytes of collection data",
            caps.bytes
        )));
    }
    Ok(Ok((out, jobs, created)))
}

// ---------- running ----------

/// Fillers filling now, as `collection/filler` (one fill each at a time).
#[derive(Default)]
pub struct Busy(std::sync::Mutex<HashSet<String>>);

struct BusyGuard<'a>(&'a Busy, String);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

fn busy_key(collection: &str, filler: &str) -> String {
    format!("{collection}/{filler}")
}

/// Whether a fill of `collection` (any filler) is running.
pub fn is_busy(pod: &Pod, collection: &str) -> bool {
    let prefix = format!("{collection}/");
    pod.local::<Busy>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|k| k.starts_with(&prefix))
}

/// When the pod last sent a request to each host.
#[derive(Default)]
pub struct Hosts(tokio::sync::Mutex<HashMap<String, Instant>>);

/// Wait until a request to `url`'s host keeps [`HOST_SPACING`] from the
/// last one.
async fn space(pod: &Pod, url: &str) {
    let hosts = pod.local::<Hosts>();
    let host = web_extract::fetch::host_of(url);
    let wait = {
        let mut m = hosts.0.lock().await;
        let now = Instant::now();
        let at = m
            .get(&host)
            .map_or(now, |last| (*last + HOST_SPACING).max(now));
        m.insert(host, at);
        at.saturating_duration_since(now)
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

/// How a fill went.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Rows written (`items`: what the source answered).
    Filled {
        items: usize,
        skipped: usize,
        stored: Stored,
    },
    /// The page didn't change since the last fill.
    Unchanged,
    Failed(String),
    /// Another fill of the filler is running.
    Busy,
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Filled {
                items,
                skipped,
                stored,
            } => json!({
                "ok": true, "items": items, "values_skipped": skipped,
                "created": stored.created, "updated": stored.updated,
                "removed": stored.removed, "unchanged": stored.unchanged,
                "dropped": stored.dropped,
            }),
            Outcome::Unchanged => json!({ "ok": true, "unchanged": true,
                "detail": "the page hasn't changed since the last fill" }),
            Outcome::Failed(e) => json!({ "ok": false, "error": e }),
            Outcome::Busy => json!({ "ok": true, "busy": true,
                "detail": "a fill of this filler is running; its rows update when it ends" }),
        }
    }
}

/// When to try again after `failures` failures in a row.
fn backoff(f: &Filler, failures: i64, retry_after: Option<i64>, now: i64) -> i64 {
    let base = i64::from(f.every_minutes.unwrap_or(15)).max(15) * 60;
    let exp = base << (failures.clamp(1, 6) - 1);
    now + exp.min(86_400).max(retry_after.unwrap_or(0)).max(60)
}

/// Today in the pod's zone (`YYYY-MM-DD`).
pub fn today(store: &pod_core::store::Store, now: i64) -> String {
    let tz = pod_core::tz::time_zone(store).unwrap_or(pod_core::tz::jiff::tz::TimeZone::UTC);
    pod_core::tz::jiff::Timestamp::from_second(now)
        .map(|t| t.to_zoned(tz).date().to_string())
        .unwrap_or_default()
}

/// Whether fillers may run on this pod.
pub async fn allowed(pod: &Pod) -> bool {
    pod.blocking(|s| s.pod_setting(FILL_SETTING))
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v != "0")
}

/// What a source answered before storing.
enum Fetch {
    Items {
        items: Vec<Map<String, Value>>,
        skipped: usize,
        etag: Option<String>,
        last_modified: Option<String>,
        hash: Option<String>,
    },
    /// `url_field`: each row's own values.
    PerRow {
        rows: Vec<(String, Map<String, Value>)>,
        items: usize,
        skipped: usize,
        failed: Vec<String>,
    },
    Unchanged,
    Failed(String, Option<i64>),
}

fn hash_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Map extracted rows (keyed by field id) to stored values.
fn coerce_all(
    def: &Definition,
    rows: Vec<Map<String, Value>>,
    max: usize,
    max_chars: usize,
    zone: Option<&web_extract::when::Zone>,
) -> (Vec<Map<String, Value>>, usize) {
    let mut skipped = 0;
    let items = rows
        .into_iter()
        .take(max)
        .map(|r| {
            let (v, s) = to_values(def, r, max_chars, zone);
            skipped += s;
            v
        })
        .filter(|v| !v.is_empty())
        .collect();
    (items, skipped)
}

/// The fields a model answers for `ids`.
fn agent_fields(def: &Definition, ids: &[String]) -> Vec<AgentField> {
    ids.iter()
        .filter_map(|id| def.by_id(id).filter(|f| f.live()))
        .map(|f| AgentField {
            name: f.name.clone(),
            kind: f.kind,
            options: f.options.clone(),
        })
        .collect()
}

/// A model's rows (by field name) as rows by field id, only of `ids`.
fn by_ids(
    def: &Definition,
    rows: Vec<Map<String, Value>>,
    ids: &[String],
) -> Vec<Map<String, Value>> {
    rows.into_iter()
        .map(|r| {
            r.into_iter()
                .filter_map(|(n, v)| {
                    let f = def.field(&n)?;
                    ids.contains(&f.id).then(|| (f.id.clone(), v))
                })
                .collect()
        })
        .collect()
}

/// Count a model fill toward today's budgets: the filler's own (an agent's
/// `fills_per_day`) and, for agents, the pod's. Answers today's date; `Err`
/// when a budget is spent.
async fn take_model_budget(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    filler: &Filler,
    st: &State,
    now: i64,
) -> Result<String, String> {
    let day = today(pod.store(), now);
    let mine = if st.day.as_deref() == Some(day.as_str()) {
        st.day_fills
    } else {
        0
    };
    if let Source::Agent { fills_per_day, .. } = &filler.source
        && mine >= i64::from(*fills_per_day)
    {
        return Err(format!(
            "this filler already filled {mine} times today (fills_per_day {fills_per_day})"
        ));
    }
    if filler.is_agent() {
        let d = day.clone();
        let pod_total = pod
            .blocking(move |s| s.with_conn(|c| pod_model_fills_today(c, &d)))
            .await
            .unwrap_or(0);
        let cap = i64::from(adapter.caps().agent_fills_per_day);
        if pod_total >= cap {
            return Err(format!(
                "the pod's agents already filled {pod_total} times today (the operator's limit is {cap})"
            ));
        }
    }
    Ok(day)
}

#[allow(clippy::too_many_arguments)]
async fn run_fetch(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    force: bool,
    now: i64,
    day: &mut Option<String>,
) -> Fetch {
    let Source::Fetch {
        url,
        url_field,
        headers,
        map,
        hint,
        ..
    } = &filler.source
    else {
        return Fetch::Failed("not a fetch filler".into(), None);
    };
    let spec = filler.extract_spec();
    if spec.is_none() && !filler.is_llm() {
        return Fetch::Failed("the filler's extractor is unknown".into(), None);
    }
    if filler.is_llm() && adapter.agent().is_none() {
        return Fetch::Failed(NO_AGENT.into(), None);
    }
    let pairs: Vec<(String, String)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let ids: Vec<String> = map.keys().cloned().collect();
    let caps = adapter.caps();
    let zone = filler.zone();
    let get = |u: String, etag: Option<String>, lm: Option<String>| async move {
        // Per URL: a secret goes only to its own host.
        let resolved = resolve_headers(pod.store(), headers, &u, adapter.allow_private_hosts())
            .map_err(|message| web_extract::fetch::FetchError {
                message,
                retry_after: None,
            })?;
        let client = if resolved.has_secret {
            adapter.secret_fetch_client()
        } else {
            adapter.fetch_client()
        };
        space(pod, &u).await;
        web_extract::fetch::fetch(
            client,
            &u,
            etag.as_deref(),
            lm.as_deref(),
            &resolved.headers,
        )
        .await
    };
    // One page's rows (by field id).
    let read = |body: String, page: String, day: String| {
        let spec = spec.clone();
        let pairs = pairs.clone();
        let ids = ids.clone();
        async move {
            match spec {
                Some(spec) => web_extract::items::extract(&spec, &body, &page, &pairs),
                None => {
                    let Some(agent) = adapter.agent() else {
                        return Err(NO_AGENT.to_owned());
                    };
                    let fields = agent_fields(def, &ids);
                    let text = web_extract::html::to_text(&body);
                    let rows = agent
                        .read_page(PageRequest {
                            title: &def.title,
                            fields: &fields,
                            page_text: &text,
                            url: &page,
                            hint,
                            today: &day,
                            with_past: filler.upcoming.is_some(),
                        })
                        .await?;
                    Ok(by_ids(def, rows, &ids))
                }
            }
        }
    };
    if let Some(url) = url {
        if let Err(e) = web_extract::fetch::check_url(url, adapter.allow_private_hosts()) {
            return Fetch::Failed(e, None);
        }
        let fresh = force || state.fingerprint != filler.fingerprint();
        let (etag, lm) = if fresh {
            (None, None)
        } else {
            (state.etag.clone(), state.last_modified.clone())
        };
        let (bytes, etag, last_modified) = match get(url.clone(), etag, lm).await {
            Ok(web_extract::fetch::Fetched::NotModified) => return Fetch::Unchanged,
            Ok(web_extract::fetch::Fetched::Body {
                bytes,
                etag,
                last_modified,
            }) => (bytes, etag, last_modified),
            Err(e) => return Fetch::Failed(e.message, e.retry_after),
        };
        let hash = hash_of(&bytes);
        if !fresh && state.content_hash.as_deref() == Some(hash.as_str()) {
            return Fetch::Unchanged;
        }
        if filler.is_llm() {
            match take_model_budget(pod, adapter, filler, state, now).await {
                Ok(d) => *day = Some(d),
                Err(e) => return Fetch::Failed(e, None),
            }
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let today = day.clone().unwrap_or_default();
        return match read(body, url.clone(), today).await {
            Ok(rows) => {
                let (items, skipped) =
                    coerce_all(def, rows, caps.fill_items, caps.text_chars, zone.as_ref());
                Fetch::Items {
                    items,
                    skipped,
                    etag,
                    last_modified,
                    hash: Some(hash),
                }
            }
            Err(e) => Fetch::Failed(e, None),
        };
    }
    let Some(field) = url_field else {
        return Fetch::Failed("the filler has no URL".into(), None);
    };
    if filler.is_llm() {
        match take_model_budget(pod, adapter, filler, state, now).await {
            Ok(d) => *day = Some(d),
            Err(e) => return Fetch::Failed(e, None),
        }
    }
    // Each row from its own page.
    let name = def.name.clone();
    let rows = pod
        .blocking(move |s| s.with_conn(|c| db::all_rows(c, &name, false)))
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    let (mut items, mut skipped, mut failed) = (0, 0, Vec::new());
    for r in rows
        .into_iter()
        .filter(|r| {
            r.values
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|u| !u.trim().is_empty())
        })
        .take(caps.fill_urls)
    {
        let u = r.values[field].as_str().unwrap_or("").trim().to_owned();
        let u = match web_extract::fetch::check_url(&u, adapter.allow_private_hosts()) {
            Ok(u) => u,
            Err(e) => {
                failed.push(format!("{u}: {e}"));
                continue;
            }
        };
        let body = match get(u.clone(), None, None).await {
            Ok(web_extract::fetch::Fetched::Body { bytes, .. }) => {
                String::from_utf8_lossy(&bytes).into_owned()
            }
            Ok(web_extract::fetch::Fetched::NotModified) => continue,
            Err(e) => {
                failed.push(format!("{u}: {}", e.message));
                continue;
            }
        };
        match read(body, u.clone(), day.clone().unwrap_or_default()).await {
            Ok(found) => {
                let (mut vals, s) = coerce_all(def, found, 1, caps.text_chars, zone.as_ref());
                skipped += s;
                if let Some(v) = vals.pop() {
                    items += 1;
                    out.push((r.id.clone(), v));
                } else {
                    out.push((r.id.clone(), Map::new()));
                }
            }
            Err(e) => failed.push(format!("{u}: {e}")),
        }
    }
    Fetch::PerRow {
        rows: out,
        items,
        skipped,
        failed,
    }
}

async fn run_agent(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    now: i64,
) -> Result<(Fetch, String), String> {
    let Source::Agent {
        prompt,
        fields,
        max_searches,
        max_fetches,
        ..
    } = &filler.source
    else {
        return Err("not an agent filler".into());
    };
    let Some(agent) = adapter.agent() else {
        return Err(NO_AGENT.into());
    };
    let day = take_model_budget(pod, adapter, filler, state, now).await?;
    let agent_fields = agent_fields(def, fields);
    let name = def.name.clone();
    let current_rows = pod
        .blocking(move |s| s.with_conn(|c| db::all_rows(c, &name, false)))
        .await
        .unwrap_or_default();
    let current: Vec<Map<String, Value>> = current_rows
        .iter()
        .filter(|r| r.filler.as_deref() == Some(filler.id.as_str()))
        .take(crate::agent::MAX_CURRENT)
        .map(|r| {
            fields
                .iter()
                .filter_map(|id| {
                    let f = def.by_id(id)?;
                    Some((
                        f.name.clone(),
                        r.values.get(id).cloned().unwrap_or(Value::Null),
                    ))
                })
                .collect()
        })
        .collect();
    let key_name = filler.key.as_ref().map(|k| def.name_of(k));
    let answer = agent
        .fill(AgentRequest {
            title: &def.title,
            prompt,
            fields: &agent_fields,
            today: &day,
            max_searches: *max_searches,
            max_fetches: *max_fetches,
            key: key_name.as_deref(),
            current: &current,
        })
        .await;
    let answer = match answer {
        Ok(a) => a,
        // A failed call still counts toward the budget: it may have cost.
        Err(e) => return Ok((Fetch::Failed(e, None), day)),
    };
    let caps = adapter.caps();
    let (items, skipped) = coerce_all(
        def,
        by_ids(def, answer.rows, fields),
        caps.fill_items,
        caps.text_chars,
        None,
    );
    Ok((
        Fetch::Items {
            items,
            skipped,
            etag: None,
            last_modified: None,
            hash: None,
        },
        day,
    ))
}

/// Read each new row's own page for the filler's `details` (at most
/// [`MAX_DETAILS`]) and store what it found.
async fn fill_details(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    created: &[String],
) -> Vec<crate::compute::Job> {
    let Source::Fetch {
        details: Some(d),
        zone,
        ..
    } = &filler.source
    else {
        return Vec::new();
    };
    // A path extractor, or (extract llm) a model reading the page's text.
    let spec = if d.is_llm() {
        if adapter.agent().is_none() {
            return Vec::new();
        }
        None
    } else {
        let Some(kind) = web_extract::items::Kind::parse(&d.extract) else {
            return Vec::new();
        };
        Some(web_extract::items::Spec {
            kind,
            items: d.items.clone(),
            types: d.types.clone(),
            script: String::new(),
            zone: zone.clone(),
        })
    };
    let ids: Vec<String> = d.map.keys().cloned().collect();
    let today = today(pod.store(), pod_core::util::now_secs());
    let pairs: Vec<(String, String)> = d.map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let caps = adapter.caps();
    let mut found: Vec<(String, Map<String, Value>)> = Vec::new();
    for id in created.iter().take(MAX_DETAILS) {
        let (n, rid) = (def.name.clone(), id.clone());
        let Ok(Some(row)) = pod
            .blocking(move |s| s.with_conn(|c| db::row(c, &n, &rid)))
            .await
        else {
            continue;
        };
        let Some(link) = row.values.get(&d.link).and_then(Value::as_str) else {
            continue;
        };
        let Ok(link) = web_extract::fetch::check_url(link, adapter.allow_private_hosts()) else {
            tracing::warn!("a details page was not fetched: not a public website");
            continue;
        };
        space(pod, &link).await;
        let Ok(web_extract::fetch::Fetched::Body { bytes, .. }) =
            web_extract::fetch::fetch(adapter.fetch_client(), &link, None, None, &[]).await
        else {
            continue;
        };
        let page = String::from_utf8_lossy(&bytes);
        let found_items = match &spec {
            Some(spec) => web_extract::items::extract(spec, &page, &link, &pairs).ok(),
            None => {
                let Some(agent) = adapter.agent() else {
                    continue;
                };
                let fields = agent_fields(def, &ids);
                let text = web_extract::html::to_text(&page);
                match agent
                    .read_page(PageRequest {
                        title: &def.title,
                        fields: &fields,
                        page_text: &text,
                        url: &link,
                        hint: &d.hint,
                        today: &today,
                        // One row's own page: keep what it says even if dated.
                        with_past: true,
                    })
                    .await
                {
                    Ok(rows) => Some(by_ids(def, rows, &ids)),
                    Err(e) => {
                        tracing::warn!("a details page was not read by the model: {e}");
                        None
                    }
                }
            }
        };
        let mut item = found_items
            .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))
            .unwrap_or_default();
        if item.is_empty()
            && let Some(t) = &d.text
        {
            item.insert(t.clone(), json!(web_extract::html::to_text(&page)));
        }
        let (vals, _) = to_values(def, item, caps.text_chars, filler.zone().as_ref());
        if !vals.is_empty() {
            found.push((id.clone(), vals));
        }
    }
    if found.is_empty() {
        return Vec::new();
    }
    let mapped = filler.mapped();
    let detail_fields: Vec<String> = filler
        .filled()
        .into_iter()
        .filter(|f| !mapped.contains(f))
        .collect();
    let d2 = def.clone();
    let now = pod_core::util::now_secs();
    let stored = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                let mut jobs = Vec::new();
                for (id, vals) in &found {
                    let Some(mut r) = db::row(tx, &d2.name, id)? else {
                        continue;
                    };
                    let before = r.values.clone();
                    if apply_item(&mut r, &detail_fields, vals) {
                        let write = write_of(&d2, &detail_fields, vals);
                        jobs.extend(crate::compute::after_write(
                            tx,
                            &d2,
                            Some(&before),
                            &mut r,
                            &write,
                            now,
                            caps,
                        )?);
                        db::save_row(tx, &d2.name, &r)?;
                    }
                }
                Ok((jobs, crate::follow::settle(tx, now, caps)?))
            })
        })
        .await;
    match stored {
        Ok((jobs, followed)) => {
            followed.finish(pod);
            jobs
        }
        Err(_) => Vec::new(),
    }
}

/// Fill `filler_id` of `collection` now. `force`: refetch even when the
/// page looks unchanged.
pub async fn fill(pod: &Pod, collection: &str, filler_id: &str, force: bool) -> Outcome {
    let Some(adapter) = pod.adapter::<CollectionsAdapter>() else {
        return Outcome::Failed("no Collections".into());
    };
    let busy = pod.local::<Busy>();
    let key = busy_key(collection, filler_id);
    {
        let mut b = busy.0.lock().unwrap_or_else(|e| e.into_inner());
        if !b.insert(key.clone()) {
            return Outcome::Busy;
        }
    }
    let _guard = BusyGuard(&busy, key);
    let (name, fid) = (collection.to_owned(), filler_id.to_owned());
    let read = pod
        .blocking(move |s| s.with_conn(|c| Ok((db::latest_def(c, &name)?, state(c, &name, &fid)?))))
        .await;
    let (def, mut st) = match read {
        Ok((Some((_, def)), st)) => (def, st),
        Ok((None, _)) => return Outcome::Failed(format!("no collection '{collection}'")),
        Err(_) => return Outcome::Failed("the collection could not be read".into()),
    };
    let Some(filler) = crate::filler::find(&def, filler_id).cloned() else {
        return Outcome::Failed(format!(
            "collection '{collection}' has no filler '{filler_id}'"
        ));
    };
    let now = pod_core::util::now_secs();
    let mut day = None;
    let fetched = if !allowed(pod).await {
        Fetch::Failed(FILL_OFF.into(), Some(86_400))
    } else if filler.is_agent() {
        match tokio::time::timeout(
            AGENT_FILL_TIMEOUT,
            run_agent(pod, adapter, &def, &filler, &st, now),
        )
        .await
        {
            Ok(Ok((f, d))) => {
                day = Some(d);
                f
            }
            Ok(Err(e)) => Fetch::Failed(e, None),
            Err(_) => Fetch::Failed("the agent took too long".into(), None),
        }
    } else {
        let limit = if filler.is_llm() {
            AGENT_FILL_TIMEOUT
        } else {
            FETCH_FILL_TIMEOUT
        };
        match tokio::time::timeout(
            limit,
            run_fetch(pod, adapter, &def, &filler, &st, force, now, &mut day),
        )
        .await
        {
            Ok(f) => f,
            Err(_) => Fetch::Failed("the fill timed out".into(), None),
        }
    };
    let caps = adapter.caps();
    st.last_fill_at = Some(now);
    if let Some(d) = day {
        if st.day.as_deref() != Some(d.as_str()) {
            st.day = Some(d);
            st.day_fills = 0;
        }
        st.day_fills += 1;
    }
    let scheduled = filler.every_minutes.map(|m| now + i64::from(m) * 60);
    let fingerprint = filler.fingerprint();
    let quiet = st.quiet_next;
    let mut created_rows = Vec::new();
    let (outcome, mut jobs) = match fetched {
        Fetch::Unchanged => {
            st.failures = 0;
            st.last_error = None;
            st.last_ok_at = Some(now);
            st.next_at = scheduled;
            (Outcome::Unchanged, Vec::new())
        }
        Fetch::Failed(e, retry_after) => {
            st.failures += 1;
            st.last_error = Some(web_extract::html::truncate(&e, 300));
            st.next_at = Some(backoff(&filler, st.failures, retry_after, now));
            (Outcome::Failed(e), Vec::new())
        }
        Fetch::Items {
            items,
            skipped,
            etag,
            last_modified,
            hash,
        } => {
            let count = items.len();
            let (d2, f2) = (def.clone(), filler.clone());
            let stored = crate::refusable(pod, move |tx| {
                let out = store(tx, &d2, &f2, items, now, caps)?;
                if out.is_ok() && quiet {
                    crate::alerts::mark_sent(tx, &d2, &f2.id, now)?;
                }
                Ok(out.map_err(|e| json!({ "error": e })))
            })
            .await;
            match stored {
                Ok((stored, jobs, created)) => {
                    created_rows = created;
                    st.failures = 0;
                    st.last_error = None;
                    st.last_ok_at = Some(now);
                    st.next_at = scheduled;
                    st.etag = etag;
                    st.last_modified = last_modified;
                    st.content_hash = hash;
                    st.fingerprint = fingerprint.clone();
                    st.last_rows = Some(count as i64);
                    st.last_skipped = Some((skipped + stored.dropped) as i64);
                    st.last_past = Some(stored.past as i64);
                    // Items that were all past still matched the recipe.
                    st.empty_runs = if count == 0 { st.empty_runs + 1 } else { 0 };
                    st.quiet_next = false;
                    (
                        Outcome::Filled {
                            items: count,
                            skipped,
                            stored,
                        },
                        jobs,
                    )
                }
                Err(e) => {
                    let msg = e["error"]
                        .as_str()
                        .or(e["detail"].as_str())
                        .unwrap_or("the rows could not be stored")
                        .to_owned();
                    st.failures += 1;
                    st.last_error = Some(msg.clone());
                    st.next_at = Some(backoff(&filler, st.failures, None, now));
                    (Outcome::Failed(msg), Vec::new())
                }
            }
        }
        Fetch::PerRow {
            rows,
            items,
            skipped,
            failed,
        } => {
            let (d2, f2) = (def.clone(), filler.clone());
            let total = rows.len() + failed.len();
            let any_page = !rows.is_empty();
            let stored = pod
                .blocking(move |s| {
                    s.transaction(|tx| {
                        let mapped = f2.mapped();
                        let mut out = Stored::default();
                        let mut jobs = Vec::new();
                        for (id, item) in &rows {
                            let Some(mut r) = db::row(tx, &d2.name, id)? else {
                                continue;
                            };
                            if r.deleted_at.is_some() {
                                continue;
                            }
                            let before = r.values.clone();
                            if apply_item(&mut r, &mapped, item) {
                                let write = write_of(&d2, &mapped, item);
                                jobs.extend(crate::compute::after_write(
                                    tx,
                                    &d2,
                                    Some(&before),
                                    &mut r,
                                    &write,
                                    now,
                                    caps,
                                )?);
                                r.updated_at = now;
                                db::save_row(tx, &d2.name, &r)?;
                                out.updated += 1;
                            } else {
                                out.unchanged += 1;
                            }
                        }
                        Ok((out, jobs, crate::follow::settle(tx, now, caps)?))
                    })
                })
                .await
                .map(|(out, jobs, followed)| {
                    followed.finish(pod);
                    (out, jobs)
                });
            match stored {
                Ok((stored, jobs)) if failed.is_empty() || any_page => {
                    st.failures = 0;
                    st.last_ok_at = Some(now);
                    st.last_error = (!failed.is_empty()).then(|| {
                        web_extract::html::truncate(
                            &format!(
                                "{} of {total} pages failed: {}",
                                failed.len(),
                                failed.join("; ")
                            ),
                            300,
                        )
                    });
                    st.next_at = scheduled;
                    st.fingerprint = fingerprint.clone();
                    st.last_rows = Some(items as i64);
                    st.last_skipped = Some(skipped as i64);
                    st.last_past = None;
                    st.empty_runs = if items == 0 && total > 0 {
                        st.empty_runs + 1
                    } else {
                        0
                    };
                    (
                        Outcome::Filled {
                            items,
                            skipped,
                            stored,
                        },
                        jobs,
                    )
                }
                Ok(_) => {
                    let e = format!("every page failed: {}", failed.join("; "));
                    st.failures += 1;
                    st.last_error = Some(web_extract::html::truncate(&e, 300));
                    st.next_at = Some(backoff(&filler, st.failures, None, now));
                    (Outcome::Failed(e), Vec::new())
                }
                Err(_) => {
                    st.failures += 1;
                    st.last_error = Some("the rows could not be stored".into());
                    st.next_at = Some(backoff(&filler, st.failures, None, now));
                    (
                        Outcome::Failed("the rows could not be stored".into()),
                        Vec::new(),
                    )
                }
            }
        }
    };
    if !created_rows.is_empty() {
        jobs.extend(fill_details(pod, adapter, &def, &filler, &created_rows).await);
    }
    let st2 = st.clone();
    let (name, fid) = (def.name.clone(), filler.id.clone());
    let _ = pod
        .blocking(move |s| s.with_conn(|c| save_state(c, &name, &fid, &st2)))
        .await;
    if let Outcome::Filled { stored, .. } = &outcome
        && stored.changed()
    {
        pod.publish_data_all(NAME);
        crate::alerts::run(pod, Some(collection.to_owned())).await;
    }
    crate::compute::spawn(pod, collection, jobs);
    if let Outcome::Failed(e) = &outcome {
        tracing::info!(collection, filler = filler_id, error = %e, "a collection fill failed");
    }
    outcome
}

/// Start fills in the background: `filler`, else every filler of the
/// collection.
pub fn spawn(pod: &Pod, collection: &str, filler: Option<&str>, force: bool) {
    let pod = pod.clone();
    let c = collection.to_owned();
    let only = filler.map(str::to_owned);
    tokio::spawn(async move {
        let ids: Vec<String> = match only {
            Some(f) => vec![f],
            None => {
                let n = c.clone();
                pod.blocking(move |s| s.with_conn(|conn| db::latest_def(conn, &n)))
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, d)| d.fillers.iter().map(|f| f.id.clone()).collect())
                    .unwrap_or_default()
            }
        };
        for f in ids {
            fill(&pod, &c, &f, force).await;
        }
    });
}

/// The fillers of `def` that opening an app showing it should start: the
/// last fill is older than `on_open_minutes`, or a scheduled fill is
/// overdue. Not while a failure's backoff runs.
pub fn due_on_open(def: &Definition, states: &HashMap<String, State>, now: i64) -> Vec<String> {
    def.fillers
        .iter()
        .filter(|f| {
            let st = states.get(&f.id).cloned().unwrap_or_default();
            if st.failures > 0 {
                return st.next_at.is_none_or(|t| t <= now)
                    && (f.on_open_minutes.is_some() || f.every_minutes.is_some());
            }
            let stale = f
                .on_open_minutes
                .is_some_and(|m| st.last_fill_at.is_none_or(|t| now - t >= i64::from(m) * 60));
            let overdue = f.every_minutes.is_some() && st.next_at.is_none_or(|t| t <= now);
            stale || overdue
        })
        .map(|f| f.id.clone())
        .collect()
}

/// Per pod: one scheduled run at a time.
#[derive(Default)]
pub struct RunLock(tokio::sync::Mutex<()>);

/// Fill every filler whose scheduled fill is due, then send what alerts
/// owe. Returns at once when a run is going.
pub async fn run_due(pod: &Pod) {
    let lock = pod.local::<RunLock>();
    let Ok(_guard) = lock.0.try_lock() else {
        return;
    };
    let now = pod_core::util::now_secs();
    let due: Vec<(String, String)> = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let mut out = Vec::new();
                for (_, def) in db::all_defs(c)? {
                    for f in &def.fillers {
                        let st = state(c, &def.name, &f.id)?;
                        let scheduled = f.every_minutes.is_some() || st.failures > 0;
                        if scheduled && st.next_at.is_none_or(|t| t <= now) {
                            out.push((def.name.clone(), f.id.clone()));
                        }
                    }
                }
                Ok(out)
            })
        })
        .await
        .unwrap_or_default();
    for (c, f) in due {
        fill(pod, &c, &f, false).await;
    }
    crate::alerts::run(pod, None).await;
}

/// Whether any collection has a scheduled filler or an alert (the poll then
/// comes back within [`crate::remind::MAX_WAIT`]).
pub fn any_scheduled(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(db::all_defs(conn)?
        .iter()
        .any(|(_, d)| !d.alerts.is_empty() || d.fillers.iter().any(|f| f.every_minutes.is_some())))
}

/// "5 min ago" style.
fn ago(now: i64, t: i64) -> String {
    let m = (now - t).max(0) / 60;
    match m {
        0 => "just now".to_owned(),
        1..=59 => format!("{m} min ago"),
        60..=2879 => format!("{} h ago", m / 60),
        _ => format!("{} days ago", m / 1440),
    }
}

/// A filler's health: `(state, detail)`: pending, ok, failing, empty, off
/// or unavailable.
pub fn health_state(
    f: &Filler,
    st: &State,
    now: i64,
    off: bool,
    agent: bool,
) -> (&'static str, String) {
    if off {
        ("off", FILL_OFF.to_owned())
    } else if f.uses_model() && !agent {
        ("unavailable", NO_AGENT.to_owned())
    } else if st.failures > 0 {
        (
            "failing",
            format!(
                "{} ({} failed fills in a row)",
                st.last_error.clone().unwrap_or_default(),
                st.failures
            ),
        )
    } else if st.empty_runs >= EMPTY_AFTER {
        (
            "empty",
            format!(
                "the last {} fills found no items: check the recipe with collections_preview",
                st.empty_runs
            ),
        )
    } else if let Some(t) = st.last_fill_at {
        let rows = st.last_rows.unwrap_or(0);
        let past = st.last_past.unwrap_or(0).min(rows);
        let ones = |n: i64| {
            if n == 1 {
                "1 past one".to_owned()
            } else {
                format!("{n} past ones")
            }
        };
        let found = match past {
            0 => format!("{rows} items"),
            p if p == rows => format!("no upcoming items listed · {} on the page", ones(p)),
            p => format!("{} items · {} left out", rows - p, ones(p)),
        };
        (
            "ok",
            format!(
                "{found} · filled {}{}",
                ago(now, t),
                st.last_error
                    .as_ref()
                    .map(|e| format!(" · {e}"))
                    .unwrap_or_default()
            ),
        )
    } else {
        ("pending", "not filled yet".to_owned())
    }
}

/// A filler's health for Claude: state, detail and the times.
pub fn health(def: &Definition, f: &Filler, st: &State, now: i64, off: bool, agent: bool) -> Value {
    let (state_name, detail) = health_state(f, st, now, off, agent);
    let at = |t: Option<i64>| t.map(pod_core::layouts::rfc3339);
    json!({
        "collection": def.name, "filler": f.id, "title": f.title, "kind": f.kind(),
        "url": f.url(), "state": state_name, "detail": detail,
        "last_fill_at": at(st.last_fill_at), "last_ok_at": at(st.last_ok_at),
        "next_fill_at": at(st.next_at.filter(|_| f.every_minutes.is_some() || st.failures > 0)),
        "items_last_fill": st.last_rows, "skipped_last_fill": st.last_skipped,
        "model_fills_today": f.uses_model().then_some(st.day_fills),
        "alerts": crate::alerts::of_filler(def, &f.id).map(|a| a.describe(def)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_values_are_normalized() {
        assert_eq!(
            normalize(Kind::Number, json!(" $1,299.00 "), None),
            json!("1299.00")
        );
        assert_eq!(normalize(Kind::Number, json!("-3"), None), json!("-3"));
        assert_eq!(normalize(Kind::Number, json!("free"), None), json!("free"));
        assert_eq!(
            normalize(Kind::Date, json!("20261106T193000"), None),
            json!("2026-11-06")
        );
        assert_eq!(
            normalize(Kind::Date, json!("Tue, 03 Nov 2026 20:00:00 GMT"), None),
            json!("2026-11-03")
        );
        let denver = web_extract::when::Zone::Named(pod_core::tz::parse("America/Denver").unwrap());
        assert_eq!(
            normalize(Kind::Datetime, json!("2026-11-06T19:30"), Some(&denver)),
            json!("2026-11-06T19:30-07:00")
        );
        assert_eq!(
            normalize(
                Kind::Datetime,
                json!("2026-11-06T19:30+01:00"),
                Some(&denver)
            ),
            json!("2026-11-06T19:30+01:00"),
            "a given offset stays"
        );
        assert_eq!(
            normalize(Kind::Choice, json!("https://schema.org/InStock"), None),
            json!("InStock")
        );
        assert_eq!(key_text(&json!(" Acme 27 ")).as_deref(), Some("acme 27"));
        let now = web_extract::when::When::parse("2026-11-06T12:00Z")
            .unwrap()
            .timestamp(None);
        assert!(is_past(Some(&json!("2026-11-06T08:00Z")), now));
        assert!(!is_past(Some(&json!("2026-11-06T10:00Z")), now), "slack");
        assert!(!is_past(Some(&json!("2026-11-05")), now), "a day's slack");
        assert!(is_past(Some(&json!("2026-11-03")), now));
    }

    #[test]
    fn opening_an_app_fills_when_stale_or_overdue() {
        let mut def: Definition = serde_json::from_value(json!({
            "name": "results", "title": "Results", "next_field": 2,
            "fields": [{"id": "f1", "name": "name", "type": "text", "optional": true}],
            "fillers": [{"id": "s1", "title": "R", "kind": "fetch", "url": "https://r.example/x",
                         "extract": "json", "map": {"f1": "name"}, "on_open_minutes": 5}]
        }))
        .unwrap();
        let mut st = State::default();
        let with = |st: &State| HashMap::from([("s1".to_owned(), st.clone())]);
        assert_eq!(
            due_on_open(&def, &with(&st), 1_000),
            vec!["s1"],
            "never filled"
        );
        st.last_fill_at = Some(1_000);
        assert!(due_on_open(&def, &with(&st), 1_200).is_empty());
        assert_eq!(due_on_open(&def, &with(&st), 1_300).len(), 1);
        st.failures = 2;
        st.next_at = Some(5_000);
        assert!(
            due_on_open(&def, &with(&st), 1_300).is_empty(),
            "not during a backoff"
        );
        def.fillers[0].on_open_minutes = None;
        st = State {
            last_fill_at: Some(1_000),
            next_at: Some(2_000),
            ..State::default()
        };
        assert!(due_on_open(&def, &with(&st), 1_500).is_empty());
        def.fillers[0].every_minutes = Some(15);
        assert_eq!(
            due_on_open(&def, &with(&st), 2_000).len(),
            1,
            "a scheduled fill is overdue"
        );
    }
}
