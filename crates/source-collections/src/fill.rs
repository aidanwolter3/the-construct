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
//!   pod's. With `for_each`, one request per row of the other collection
//!   (each counts toward the budgets), each answer stored as that row's own
//!   rows here ([`store`] scoped by the link), refreshed `once` (when the
//!   row's text or the context changes) or `every` fill.
//!
//! The filler's rows are written in one transaction ([`store`]): `replace`
//! makes them the answer (rows it no longer lists are deleted for good, not
//! kept for 30 days: they were never anyone's), `upsert` adds and updates
//! by key and leaves other rows alone. Other fillers' rows and people's
//! rows are never touched. Computed fields compute as after any write.
//! Failures back off (doubling from the interval, at
//! most a day, at least `Retry-After`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use pod_core::Pod;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::agent::{AgentField, AgentRequest, PageRequest};
use crate::db::{self, Row};
use crate::filler::{Filler, Source};
use crate::model::{self, Definition, Kind};
use crate::{Caps, CollectionsAdapter, NAME};

/// The pod setting that turned fillers off before jacks (decision `jacks`):
/// `0` became a lock on the `web` jack (a migration, [`crate::db`]).
pub const FILL_SETTING: &str = "collections.fill";
/// The error an `mcp` fill records when the pod's `mcp` jack is unplugged.
pub const MCP_OFF: &str = "the pod's mcp jack is unplugged, so this filler can't call its MCP server (plug_jack mcp, if the server allows it)";
/// The error a fill records when the pod's `web` jack is unplugged.
pub const FILL_OFF: &str = "the pod's web jack is unplugged, so fillers can't fetch websites (plug_jack web, if the server allows it)";
/// How many image results an `images` filler tries, in order, for one that
/// loads.
const IMAGE_CANDIDATES: usize = 5;

/// The error of a `query_field` fill without a search provider.
pub const NO_SEARCH: &str = "this filler searches the web for each row: plug the pod's web_search jack (plug_jack web_search with a provider and its key)";
/// The error of a model fill without a model.
pub const NO_AGENT: &str = "this filler needs a model: set CONSTRUCT_INFER_API_KEY on the server";
/// A whole fetch fill's time limit.
pub const FETCH_FILL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// A whole model fill's time limit.
pub const AGENT_FILL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// A `for_each` agent's fill: several researched rows, one after another.
pub const EACH_FILL_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long after a row is written its research starts (its computed
/// fields settle first, so it is researched once).
pub const RESEARCH_DELAY: Duration = Duration::from_secs(20);
/// While a fill runs, how often (and how many times) a write's research
/// tries again.
const RESEARCH_RETRY: Duration = Duration::from_secs(60);
const RESEARCH_TRIES: usize = 30;
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

/// Delete the rows filler `f` of `def` wrote, as `remove_filler` does for a
/// filler that owned them (`replace`): the same as a fill that lists
/// nothing, so a site set up again under a new filler doesn't leave the old
/// setup's copies beside the new ones. Rows its `keep` keeps stay. They go as
/// a person's delete does (restorable for the purge window), not for good.
/// How many went.
pub fn drop_rows_of(
    tx: &Connection,
    def: &Definition,
    f: &Filler,
    now: i64,
) -> rusqlite::Result<usize> {
    if !f.owns_rows() {
        return Ok(0);
    }
    let mut n = 0;
    for mut r in db::all_rows(tx, &def.name, false)? {
        if r.filler.as_deref() != Some(f.id.as_str()) || f.keeps(def, &r.values) {
            continue;
        }
        r.deleted_at = Some(now);
        r.updated_at = now;
        db::save_row(tx, &def.name, &r)?;
        n += 1;
    }
    Ok(n)
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
    /// What to scrub from an answer: each secret's value and each header
    /// value it went into (see [`redact`]).
    pub sensitive: Vec<String>,
}

const REDACTED: &str = "[redacted secret]";

fn b64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..=c.len() {
            out.push(alphabet[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        if pad {
            for _ in c.len()..3 {
                out.push('=');
            }
        }
    }
    out
}

fn forms(v: &str) -> Vec<String> {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut pct = String::new();
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            pct.push(b as char);
        } else {
            pct.push_str(&format!("%{b:02X}"));
        }
    }
    let json = serde_json::to_string(v).unwrap_or_default();
    let mut out = vec![
        v.to_owned(),
        pct,
        json.trim_matches('"').to_owned(),
        b64(v.as_bytes(), STD, true),
        b64(v.as_bytes(), STD, false),
        b64(v.as_bytes(), URL, true),
        b64(v.as_bytes(), URL, false),
    ];
    out.retain(|f| !f.is_empty());
    out.dedup();
    out
}

/// `text` with every secret the request carried, and its common encodings
/// (base64, URL-encoded, JSON-escaped), replaced by a marker: a host that
/// echoes the request's headers must not put the key in Claude's context
/// (issue #460). Longest first, so a header value goes before the secret
/// inside it.
pub fn redact(text: &str, sensitive: &[String]) -> String {
    let mut all: Vec<String> = sensitive.iter().flat_map(|v| forms(v)).collect();
    all.sort_by_key(|f| std::cmp::Reverse(f.len()));
    all.dedup();
    let mut out = text.to_owned();
    for f in all {
        out = out.replace(&f, REDACTED);
    }
    out
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
            if !plain.is_empty() {
                out.sensitive.push(plain);
            }
        }
        if value != *v {
            out.sensitive.push(value.clone());
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
/// offset placed in `zone` (a UTC one moved to `zone`'s clock).
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
            // A UTC time (`Z`) is an instant, not the source's wall clock:
            // shown on `zone`'s clock (a site's own offset stays).
            (Some(w), Some(z)) if w.offset_min == Some(0) => json!(w.shifted_to(Some(z)).stored()),
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
    coerce_all(def, rows, usize::MAX, max_chars, zone, None)
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

/// A filler's `upcoming` field and `now`, for [`coerce_all`].
fn upcoming(filler: &Filler, now: i64) -> Option<(&str, i64)> {
    filler.upcoming.as_deref().map(|u| (u, now))
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

/// Whether row `a` was made before row `b` (ties by id, so two fillers agree).
fn older(a: &Row, b: &Row) -> bool {
    (a.created_at, &a.id) < (b.created_at, &b.id)
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

/// The day of a date or datetime value as written (`YYYY-MM-DD`).
fn day_of(v: Option<&Value>) -> Option<&str> {
    let s = v?.as_str()?.trim();
    let d = s.get(..10)?;
    let b = d.as_bytes();
    (b[4] == b'-' && b[7] == b'-' && d.bytes().filter(u8::is_ascii_digit).count() == 8).then_some(d)
}

/// A value as [`pair_rows`] compares it: text by its letters and digits,
/// ignoring case ("Jazz Night!" is the same as "jazz night").
fn alike_text(v: &Value) -> Option<String> {
    let t: String = key_text(v)?
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    (!t.is_empty()).then_some(t)
}

/// For a replace without a key: the row of `rows` each item is the same item
/// as, if any (never a deleted row). An item and a row are the same when most
/// of the `fields` either has a value for are equal ([`alike_text`]) and,
/// with `upcoming`, they fall on the same day, so a model rewording a
/// description or a title keeps the row while next week's show of the same
/// name is a new one. Most alike first; each row pairs once.
fn pair_rows(
    rows: &[Row],
    items: &[Map<String, Value>],
    fields: &[String],
    upcoming: Option<&str>,
) -> Vec<Option<usize>> {
    let mut alike: Vec<(usize, usize, usize)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        for (j, r) in rows.iter().enumerate() {
            if r.deleted_at.is_some() {
                continue;
            }
            if let Some(u) = upcoming
                && day_of(item.get(u)) != day_of(r.values.get(u))
            {
                continue;
            }
            let (mut equal, mut either) = (0, 0);
            for id in fields {
                let a = item.get(id).and_then(alike_text);
                let b = r.values.get(id).and_then(alike_text);
                if a.is_none() && b.is_none() {
                    continue;
                }
                either += 1;
                if a == b {
                    equal += 1;
                }
            }
            if equal * 2 > either {
                alike.push((equal, i, j));
            }
        }
    }
    alike.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut pairs = vec![None; items.len()];
    let mut taken = vec![false; rows.len()];
    for (_, i, j) in alike {
        if pairs[i].is_none() && !taken[j] {
            pairs[i] = Some(j);
            taken[j] = true;
        }
    }
    pairs
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
    store_scoped(tx, def, filler, items, now, caps, None)
}

/// [`store`] among the rows whose field `scope.0` (a ref) holds `scope.1`
/// only, each item linked there (a `for_each` agent's answer for one row).
pub fn store_scoped(
    tx: &Connection,
    def: &Definition,
    filler: &Filler,
    items: Vec<Map<String, Value>>,
    now: i64,
    caps: Caps,
    scope: Option<(&str, &str)>,
) -> rusqlite::Result<Result<StoreResult, String>> {
    let name = &def.name;
    let mapped = filler.mapped();
    let items: Vec<Map<String, Value>> = match scope {
        Some((link, parent)) => items
            .into_iter()
            .map(|mut i| {
                i.insert(link.to_owned(), json!(parent));
                i
            })
            .collect(),
        None => items,
    };
    let in_scope = |r: &Row| {
        scope
            .is_none_or(|(link, parent)| r.values.get(link).and_then(Value::as_str) == Some(parent))
    };
    let writer = db::filler_writer(&filler.id);
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
        .filter(|r| r.filler.as_deref() == Some(filler.id.as_str()) && in_scope(r))
        .collect();
    if let Some(key) = &filler.key {
        // The same item another filler already stores (two pages or feeds of
        // one site listing it) isn't stored twice: the oldest row has it,
        // and a younger copy is left out, or deleted if it is this filler's
        // own (decision `fillers-dont-repeat-rows`). Scoped (`for_each`)
        // answers are each their own parent's, so only an unscoped fill
        // looks; rows a person added (no filler) aren't another filler's.
        let mut others: HashMap<String, Row> = HashMap::new();
        if scope.is_none() {
            let live: HashSet<&str> = def
                .fillers
                .iter()
                .map(|f| f.id.as_str())
                .filter(|f| *f != filler.id)
                .collect();
            for r in db::all_rows(tx, name, false)? {
                if !r.filler.as_deref().is_some_and(|f| live.contains(f)) {
                    continue;
                }
                let Some(k) = r.values.get(key).and_then(key_text) else {
                    continue;
                };
                if others.get(&k).is_none_or(|o| older(&r, o)) {
                    others.insert(k, r);
                }
            }
        }
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
            if let Some(o) = others.get(&k)
                && by_key.get(&k).is_none_or(|r| older(o, r))
            {
                // Another filler's row has it: this filler's own copy (if
                // any) goes with the rows the answer left out.
                seen.remove(&k);
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
                        db::note_writes(tx, name, Some(&before), r, Some(&writer), now)?;
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
                    db::note_writes(tx, name, None, &r, Some(&writer), now)?;
                    created.push(r.id.clone());
                    out.created += 1;
                }
            }
        }
        if filler.owns_rows() {
            // `keep`: a row someone marked (put in their bag) stays.
            for (k, r) in &by_key {
                if !seen.contains(k) && !filler.keeps(def, &r.values) {
                    hard_delete(tx, name, &r.id)?;
                    out.removed += 1;
                }
            }
            for r in &unkeyed {
                if filler.keeps(def, &r.values) {
                    continue;
                }
                hard_delete(tx, name, &r.id)?;
                out.removed += 1;
            }
        }
    } else {
        // Replace without a key: its rows are the items, each kept on the
        // row it is the same item as ([`pair_rows`]).
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
            // Each item keeps the row it is the same item as, so a refill
            // that rewords, adds or drops items doesn't remake the others:
            // their ids stay, and with them the alerts already sent for them,
            // people's own fields and what `details` read.
            let mut existing = existing;
            let pairs = pair_rows(&existing, &items, &mapped, filler.upcoming.as_deref());
            let mut kept = vec![false; existing.len()];
            for (i, item) in items.drain(..).enumerate() {
                let Some(j) = pairs[i] else {
                    let mut r = new_row(item.clone());
                    let write = write_of(def, &mapped, &item);
                    jobs.extend(crate::compute::after_write(
                        tx, def, None, &mut r, &write, now, caps,
                    )?);
                    db::insert_row(tx, name, &r)?;
                    db::note_writes(tx, name, None, &r, Some(&writer), now)?;
                    created.push(r.id.clone());
                    out.created += 1;
                    continue;
                };
                kept[j] = true;
                let r = &mut existing[j];
                let before = r.values.clone();
                if !apply_item(r, &mapped, &item) {
                    out.unchanged += 1;
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
                db::note_writes(tx, name, Some(&before), r, Some(&writer), now)?;
                out.updated += 1;
            }
            for (r, _) in existing.iter().zip(&kept).filter(|(_, k)| !**k) {
                if filler.keeps(def, &r.values) {
                    continue;
                }
                hard_delete(tx, name, &r.id)?;
                out.removed += 1;
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

/// Agent fills reserved against the pod's daily cap
/// (`Caps::agent_fills_per_day`) by fills still running, by day: a fill
/// writes its count to `collections_fills` only when it ends, so fillers
/// running at once would each see the whole budget left. A fill reserves
/// what it plans to spend before it calls the model (read together with
/// today's written count, under this lock) and releases it once its count
/// is written, which refunds what it didn't spend.
#[derive(Default)]
pub struct Reserved(std::sync::Mutex<HashMap<String, i64>>);

/// One fill's reservation (day and count), released when dropped: `fill`
/// drops it after it writes the fill's count.
struct Held {
    reserved: Arc<Reserved>,
    taken: std::sync::Mutex<Option<(String, i64)>>,
}

impl Held {
    fn new(pod: &Pod) -> Self {
        Self {
            reserved: pod.local::<Reserved>(),
            taken: std::sync::Mutex::new(None),
        }
    }

    /// Reserve up to `want` agent fills of the pod's `cap` on `day`:
    /// answers how many were granted (0 when the budget is spent or can't
    /// be read).
    async fn reserve(&self, pod: &Pod, day: &str, want: i64, cap: i64) -> i64 {
        let reserved = self.reserved.clone();
        let d = day.to_owned();
        let granted = pod
            .blocking(move |s| {
                s.with_conn(|c| {
                    let mut r = reserved.0.lock().unwrap_or_else(|e| e.into_inner());
                    let written = pod_model_fills_today(c, &d)?;
                    let running = r.get(&d).copied().unwrap_or(0);
                    let n = want.min(cap - written - running).max(0);
                    if n > 0 {
                        r.retain(|k, v| *k == d || *v > 0);
                        *r.entry(d).or_default() += n;
                    }
                    Ok(n)
                })
            })
            .await
            .unwrap_or(0);
        if granted > 0 {
            let mut t = self.taken.lock().unwrap_or_else(|e| e.into_inner());
            let e = t.get_or_insert_with(|| (day.to_owned(), 0));
            e.1 += granted;
        }
        granted
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let Some((day, n)) = self.taken.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let mut r = self.reserved.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = r.get_mut(&day) {
            *v -= n;
            if *v <= 0 {
                r.remove(&day);
            }
        }
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

/// The fillers of `collection` filling now (their ids).
pub fn filling(pod: &Pod, collection: &str) -> HashSet<String> {
    let prefix = format!("{collection}/");
    pod.local::<Busy>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(|k| k.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
        .collect()
}

/// Tell clients that filler `filler` of `collection` changed (a fill of it
/// started or ended): they reload the data holding its entity (decision
/// `scoped-data-events`), so a site's screen shows `refreshing` and its
/// health as they are.
pub fn publish_filler(pod: &Pod, collection: &str, filler: &str) {
    let scope = pod_core::layouts::DataScope {
        uri_prefixes: vec![db::filler_uri(collection, filler)],
        query_prefixes: Vec::new(),
        detail: Map::new(),
    };
    pod.publish_data_scoped(NAME, scope, None);
}

/// The pod's host policy (decision `web-and-agent-modules`): every
/// request to a website goes through it.
fn hosts(pod: &Pod) -> std::sync::Arc<web_extract::hosts::Hosts> {
    pod.local::<web_extract::hosts::Hosts>()
}

/// What a rendered filler reports on a server without a browser.
pub const NO_BROWSER: &str =
    "this server has no headless browser for rendered pages (CONSTRUCT_BROWSER)";

/// `url` rendered in the server's headless browser (decision
/// `rendered-fetches`), through the pod's host policy and render cap.
pub(crate) async fn render_page(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    url: &str,
    render: &crate::filler::Render,
) -> Result<web_extract::fetch::Fetched, web_extract::fetch::FetchError> {
    let Some(browser) = adapter.browser() else {
        return Err(web_extract::fetch::FetchError::new(NO_BROWSER));
    };
    web_extract::render::render(
        browser,
        &hosts(pod),
        &pod.local::<web_extract::render::Renders>(),
        url,
        &render.options(),
    )
    .await
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

/// [`today`] read through a connection.
pub fn today_on(conn: &Connection, now: i64) -> rusqlite::Result<String> {
    let tz = pod_core::store::pod_setting_on(conn, pod_core::tz::TIME_ZONE_SETTING)?
        .and_then(|n| pod_core::tz::parse(&n).ok())
        .unwrap_or(pod_core::tz::jiff::tz::TimeZone::UTC);
    Ok(pod_core::tz::jiff::Timestamp::from_second(now)
        .map(|t| t.to_zoned(tz).date().to_string())
        .unwrap_or_default())
}

/// Whether an agent filler already filled its `fills_per_day` times on
/// `today` (`YYYY-MM-DD`), so another fill today would be refused.
pub fn spent_today(f: &Filler, st: &State, today: &str) -> bool {
    let Source::Agent { fills_per_day, .. } = &f.source else {
        return false;
    };
    st.day.as_deref() == Some(today) && st.day_fills >= i64::from(*fills_per_day)
}

/// Whether fillers may run on this pod.
pub async fn allowed(pod: &Pod) -> bool {
    pod_core::jacks::core_plugged(pod.store(), pod_core::jacks::WEB)
}

/// Which jacks fillers need are unplugged: `web` (fetch and agent
/// fillers) and `mcp` (`mcp` fillers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Off {
    pub web: bool,
    pub mcp: bool,
}

impl Off {
    /// Whether `f` can't run for want of its jack (a `range` needs none).
    pub fn stops(self, f: &Filler) -> bool {
        if f.is_range() {
            false
        } else if f.is_mcp() {
            self.mcp
        } else {
            self.web
        }
    }
}

/// [`Off`] inside a connection.
pub fn off_on(conn: &Connection) -> rusqlite::Result<Off> {
    Ok(Off {
        web: !pod_core::jacks::core_plugged_on(conn, pod_core::jacks::WEB)?,
        mcp: !pod_core::jacks::core_plugged_on(conn, pod_core::jacks::MCP)?,
    })
}

/// [`Off`] of this pod.
pub async fn off(pod: &Pod) -> Off {
    pod.blocking(|s| s.with_conn(off_on))
        .await
        .unwrap_or_default()
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
    /// `url_field` / `query_field`: each row's own values, and what each
    /// filled row was filled from (`(row id, source, content key)`).
    PerRow {
        rows: Vec<(String, Map<String, Value>)>,
        marks: Vec<(String, String, String)>,
        items: usize,
        skipped: usize,
        failed: Vec<String>,
        /// Rows not fetched: Claude wrote their URL below outward access,
        /// on a host the filler doesn't reach yet.
        held: usize,
    },
    /// `for_each`: each researched row's answer, `(row id, items, source
    /// key)`; the rows that exist there (live), for removing the rows of
    /// those that don't; the model calls made.
    Each {
        answers: Vec<Researched>,
        parents: HashSet<String>,
        items: usize,
        skipped: usize,
        calls: i64,
        failed: Vec<String>,
    },
    Unchanged,
    Failed(String, Option<i64>),
}

/// One researched row's answer: `(row id, items, source key)`.
type Researched = (String, Vec<Map<String, Value>>, String);

fn hash_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Map extracted rows (keyed by field id) to stored values.
/// Rows as values by field id, at most `max` of them. With `upcoming` (the
/// filler's date field and now), past items don't count toward `max`: they
/// pass through for [`store`] to leave out and count, so a feed whose first
/// `max` items are mostly past still brings every upcoming one, and an
/// upcoming item doesn't drop out of one fill and come back as a new row
/// (with a new alert) in the next when the feed lists its items in another
/// order.
fn coerce_all(
    def: &Definition,
    rows: Vec<Map<String, Value>>,
    max: usize,
    max_chars: usize,
    zone: Option<&web_extract::when::Zone>,
    upcoming: Option<(&str, i64)>,
) -> (Vec<Map<String, Value>>, usize) {
    let (mut skipped, mut taken) = (0, 0);
    let mut items = Vec::new();
    for r in rows {
        if taken >= max {
            break;
        }
        let (v, s) = to_values(def, r, max_chars, zone);
        skipped += s;
        if v.is_empty() {
            continue;
        }
        if !upcoming.is_some_and(|(u, now)| is_past(v.get(u), now)) {
            taken += 1;
        }
        items.push(v);
    }
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
/// `fills_per_day`) and, for agents, the pod's, reserved in `held` (agent
/// fills pass one) so fills running at once can't both take its last fill.
/// Answers today's date; `Err` when a budget is spent.
async fn take_model_budget(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    filler: &Filler,
    st: &State,
    now: i64,
    held: Option<&Held>,
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
    if let Some(held) = held.filter(|_| filler.is_agent()) {
        let cap = i64::from(adapter.caps().agent_fills_per_day);
        if held.reserve(pod, &day, 1, cap).await == 0 {
            return Err(format!(
                "the pod's agents already used today's fills (the operator's limit is {cap})"
            ));
        }
    }
    Ok(day)
}

/// Call an `mcp` filler's tool and read its answer: the tool's value as a
/// JSON page for the filler's `items` and `map`, with `{today±N}` in the
/// arguments resolved in the pod's zone. An answer like the last one ends
/// the fill with no work (unless the recipe changed).
async fn run_mcp(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    force: bool,
    now: i64,
) -> Fetch {
    let (rows, hash) = match mcp_rows(pod, filler, now).await {
        Ok(r) => r,
        Err(e) => return Fetch::Failed(e, None),
    };
    let fresh = force || state.fingerprint != filler.fingerprint();
    if !fresh && state.content_hash.as_deref() == Some(hash.as_str()) {
        return Fetch::Unchanged;
    }
    let caps = adapter.caps();
    let (items, skipped) = coerce_all(
        def,
        rows,
        caps.fill_items,
        caps.text_chars,
        filler.zone().as_ref(),
        upcoming(filler, now),
    );
    Fetch::Items {
        items,
        skipped,
        etag: None,
        last_modified: None,
        hash: Some(hash),
    }
}

/// An `mcp` filler's rows (by field id, before coercion) and its answer's
/// hash.
pub(crate) async fn mcp_rows(
    pod: &Pod,
    filler: &Filler,
    now: i64,
) -> Result<(Vec<Map<String, Value>>, String), String> {
    let Source::Mcp {
        server,
        tool,
        arguments,
        map,
        ..
    } = &filler.source
    else {
        return Err("not an mcp filler".into());
    };
    let Some(spec) = filler.extract_spec() else {
        return Err("the filler's recipe is incomplete".into());
    };
    let tz = pod_core::tz::time_zone(pod.store()).unwrap_or(pod_core::tz::jiff::tz::TimeZone::UTC);
    let args = crate::filler::resolve_dates(
        &Value::Object(arguments.clone()),
        &today(pod.store(), now),
        &tz,
    );
    let value = pod_core::mcp_servers::call_tool(
        pod,
        server,
        tool,
        &args,
        pod_core::mcp_servers::Caller::Filler,
    )
    .await
    .map_err(|e| e.message)?;
    let body = value.to_string();
    let hash = hash_of(body.as_bytes());
    let pairs: Vec<(String, String)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let rows = web_extract::items::extract(&spec, &body, "", &pairs)?;
    Ok((rows, hash))
}

/// Make a `range` filler's items for today in the pod's zone. Items like
/// the last fill's (the same day) end the fill with no work (unless the
/// recipe changed).
fn run_range(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    force: bool,
    now: i64,
) -> Fetch {
    let (rows, hash) = match range_rows(pod, filler, now) {
        Ok(r) => r,
        Err(e) => return Fetch::Failed(e, None),
    };
    let fresh = force || state.fingerprint != filler.fingerprint();
    if !fresh && state.content_hash.as_deref() == Some(hash.as_str()) {
        return Fetch::Unchanged;
    }
    let caps = adapter.caps();
    let (items, skipped) = coerce_all(
        def,
        rows,
        caps.fill_items,
        caps.text_chars,
        None,
        upcoming(filler, now),
    );
    Fetch::Items {
        items,
        skipped,
        etag: None,
        last_modified: None,
        hash: Some(hash),
    }
}

/// A `range` filler's rows (by field id, before coercion) for today in the
/// pod's zone, and their hash.
pub(crate) fn range_rows(
    pod: &Pod,
    filler: &Filler,
    now: i64,
) -> Result<(Vec<Map<String, Value>>, String), String> {
    let Source::Range {
        from,
        to,
        step,
        map,
    } = &filler.source
    else {
        return Err("not a range filler".into());
    };
    let Some(spec) = filler.extract_spec() else {
        return Err("the filler's recipe is incomplete".into());
    };
    let items = crate::filler::range_items(*from, *to, *step, &today(pod.store(), now))?;
    let body = Value::Array(items).to_string();
    let hash = hash_of(body.as_bytes());
    let pairs: Vec<(String, String)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let rows = web_extract::items::extract(&spec, &body, "", &pairs)?;
    Ok((rows, hash))
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
        query_field,
        headers,
        map,
        hint,
        ..
    } = &filler.source
    else {
        return Fetch::Failed("not a fetch filler".into(), None);
    };
    let spec = filler.extract_spec();
    if spec.is_none() && !filler.is_llm() && !filler.is_images() {
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
        if let Some(r) = filler.render() {
            return render_page(pod, adapter, &u, r).await;
        }
        // Per URL: a secret goes only to its own host.
        let resolved = resolve_headers(pod.store(), headers, &u, adapter.allow_private_hosts())
            .map_err(web_extract::fetch::FetchError::new)?;
        let client = if resolved.has_secret {
            adapter.secret_fetch_client()
        } else {
            adapter.fetch_client()
        };
        let fetched = web_extract::hosts::fetch(
            &hosts(pod),
            client,
            &u,
            etag.as_deref(),
            lm.as_deref(),
            &resolved.headers,
        )
        .await?;
        // A host that echoes the request's headers would put the secret
        // into the rows (and a model's prompt): scrub it before anything is
        // extracted, as the preview does (issue #460).
        Ok(match fetched {
            web_extract::fetch::Fetched::Body {
                bytes,
                etag,
                last_modified,
            } if !resolved.sensitive.is_empty() => web_extract::fetch::Fetched::Body {
                bytes: redact(&String::from_utf8_lossy(&bytes), &resolved.sensitive).into_bytes(),
                etag,
                last_modified,
            },
            other => other,
        })
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
                            pod,
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
            match take_model_budget(pod, adapter, filler, state, now, None).await {
                Ok(d) => *day = Some(d),
                Err(e) => return Fetch::Failed(e, None),
            }
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let today = day.clone().unwrap_or_default();
        return match read(body, url.clone(), today).await {
            Ok(rows) => {
                let (items, skipped) = coerce_all(
                    def,
                    rows,
                    caps.fill_items,
                    caps.text_chars,
                    zone.as_ref(),
                    upcoming(filler, now),
                );
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
    // Each row from its own page or its own search (decision
    // `web-and-agent-modules`): a row whose page or search yields nothing
    // keeps its values (unless `on_empty: clear`), and with `refresh: once`
    // a row is filled only when its URL or query first appears or changes.
    let (field, searching) = match (url_field, query_field) {
        (Some(f), _) => (f, false),
        (None, Some(f)) => (f, true),
        (None, None) => return Fetch::Failed("the filler has no URL".into(), None),
    };
    let web = if searching {
        match crate::agent::pod_web(pod) {
            Some(w) => Some(w),
            None => return Fetch::Failed(NO_SEARCH.into(), None),
        }
    } else {
        None
    };
    if searching && !filler.is_images() && adapter.agent().is_none() {
        return Fetch::Failed(NO_AGENT.into(), None);
    }
    let name = def.name.clone();
    let fid = filler.id.clone();
    let (rows, marks, writers) = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                Ok((
                    db::all_rows(c, &name, false)?,
                    row_marks(c, &name, &fid)?,
                    db::field_writers(c, &name)?,
                ))
            })
        })
        .await
        .unwrap_or_default();
    // A URL Claude wrote below outward access (decision
    // `mcp-access-levels`) is fetched only from a host this filler already
    // reaches: one a person's row names, or one it filled a row from.
    // Reaching a new host is outward, so such a row waits (`held`).
    let by_claude = |r: &db::Row| {
        writers
            .get(&r.id)
            .and_then(|w| w.get(field))
            .is_some_and(|w| w == db::WRITER_CLAUDE)
    };
    let known_hosts: HashSet<String> = if searching {
        HashSet::new()
    } else {
        rows.iter()
            .filter(|r| !by_claude(r))
            .filter_map(|r| r.values.get(field).and_then(Value::as_str))
            .chain(marks.values().map(|(s, _, _)| s.as_str()))
            .map(|u| web_extract::fetch::host_of(u.trim()))
            .filter(|h| !h.is_empty())
            .collect()
    };
    let mut held = 0;
    let refresh = filler.refresh();
    let force_all = force || state.fingerprint != filler.fingerprint();
    let mut out = Vec::new();
    let mut new_marks = Vec::new();
    let (mut items, mut skipped, mut failed) = (0, 0, Vec::new());
    let mut budget_taken = day.is_some();
    // When there are more rows than one fill takes, the rows never filled
    // from their current source come first, then the ones filled longest
    // ago, so repeated fills reach every row.
    let src_of = |r: &db::Row| r.values[field].as_str().unwrap_or("").trim().to_owned();
    let filled_at = |r: &db::Row| {
        marks
            .get(&r.id)
            .filter(|(s, _, _)| *s == src_of(r))
            .map(|(_, _, at)| *at)
    };
    let mut rows: Vec<db::Row> = rows
        .into_iter()
        .filter(|r| {
            r.values
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|u| !u.trim().is_empty())
        })
        .filter(|r| {
            // `once`: only rows whose source is new or changed.
            force_all || refresh == crate::filler::Refresh::Every || filled_at(r).is_none()
        })
        .collect();
    rows.sort_by_key(|r| filled_at(r).map_or((0, 0), |at| (1, at)));
    for r in rows.into_iter().take(caps.fill_urls) {
        let src = src_of(&r);
        if !searching && by_claude(&r) && !known_hosts.contains(&web_extract::fetch::host_of(&src))
        {
            held += 1;
            continue;
        }
        let prev_key = marks
            .get(&r.id)
            .filter(|(s, _, _)| *s == src)
            .map(|(_, k, _)| k.clone());
        // What the model reads, and its content key.
        let (found, key) = if let (Some(web), true) = (&web, filler.is_images()) {
            // The first image result fills the mapped fields; no model
            // (decision `image-search`).
            let images = match web_extract::search::search_images(
                &web.hosts,
                &web.cache,
                &web.client,
                &web.provider,
                &src,
                IMAGE_CANDIDATES,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    failed.push(format!("{src}: {e}"));
                    // DuckDuckGo, a refusal or a spent cap holds for every
                    // other row too.
                    if e.contains("refused")
                        || e.contains("daily limit")
                        || e.contains("waits")
                        || e.contains("no image search")
                    {
                        break;
                    }
                    continue;
                }
            };
            // The first result whose picture loads: an index keeps pictures
            // a shop has since removed.
            let mut chosen = None;
            for img in images {
                if web_extract::fetch::check_url(&img.image, web.allow_private).is_ok()
                    && web_extract::search::image_loads(&web.hosts, &web.client, &img.image).await
                {
                    chosen = Some(img);
                    break;
                }
            }
            let key = hash_of(
                chosen
                    .as_ref()
                    .map(|i| i.image.as_str())
                    .unwrap_or("")
                    .as_bytes(),
            );
            if !force_all && prev_key.as_deref() == Some(key.as_str()) {
                continue;
            }
            let found: Vec<Map<String, Value>> = chosen
                .map(|img| {
                    pairs
                        .iter()
                        .filter_map(|(id, part)| {
                            let v = img.get(part)?.trim();
                            (!v.is_empty()).then(|| (id.clone(), json!(v)))
                        })
                        .collect::<Map<String, Value>>()
                })
                .filter(|m| !m.is_empty())
                .into_iter()
                .collect();
            (Ok(found), key)
        } else if let Some(web) = &web {
            // `every` searches afresh each run: a price can change within
            // the search cache's day.
            let fresh = web_extract::search::SearchCache::default();
            let cache = if refresh == crate::filler::Refresh::Every {
                &fresh
            } else {
                &web.cache
            };
            let results = match web_extract::search::search(
                &web.hosts,
                cache,
                &web.client,
                &web.provider,
                &src,
                8,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    failed.push(format!("{src}: {e}"));
                    // A refusal or a spent cap holds for every other row too.
                    if e.contains("refused") || e.contains("daily limit") || e.contains("waits") {
                        break;
                    }
                    continue;
                }
            };
            // What the model would read: a new price in a snippet asks it
            // again, the same results don't.
            let key = hash_of(web_extract::search::results_text(&results).as_bytes());
            if !force_all && prev_key.as_deref() == Some(key.as_str()) {
                continue;
            }
            if !budget_taken {
                match take_model_budget(pod, adapter, filler, state, now, None).await {
                    Ok(d) => *day = Some(d),
                    Err(e) => return Fetch::Failed(e, None),
                }
                budget_taken = true;
            }
            let Some(agent) = adapter.agent() else {
                return Fetch::Failed(NO_AGENT.into(), None);
            };
            let fields = agent_fields(def, &ids);
            let notes = r
                .values
                .iter()
                .filter(|(k, _)| k.as_str() != field && !ids.contains(k))
                .filter_map(|(k, v)| {
                    let f = def.by_id(k)?;
                    let t = v.as_str()?.trim();
                    (f.kind == Kind::Text && !t.is_empty()).then(|| format!("{}: {t}", f.name))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let item = if notes.is_empty() {
                src.clone()
            } else {
                format!("{src} ({notes})")
            };
            let today = day.clone().unwrap_or_default();
            let picked = agent
                .pick(crate::agent::PickRequest {
                    pod,
                    title: &def.title,
                    fields: &fields,
                    item: &item,
                    results: &results,
                    hint,
                    today: &today,
                })
                .await
                .map(|rows| by_ids(def, rows, &ids));
            (picked, key)
        } else {
            let u = match web_extract::fetch::check_url(&src, adapter.allow_private_hosts()) {
                Ok(u) => u,
                Err(e) => {
                    failed.push(format!("{src}: {e}"));
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
            let key = hash_of(web_extract::html::to_text(&body).as_bytes());
            if !force_all && filler.is_llm() && prev_key.as_deref() == Some(key.as_str()) {
                // The page's text didn't change: no model call.
                continue;
            }
            if filler.is_llm() && !budget_taken {
                match take_model_budget(pod, adapter, filler, state, now, None).await {
                    Ok(d) => *day = Some(d),
                    Err(e) => return Fetch::Failed(e, None),
                }
                budget_taken = true;
            }
            (
                read(body, u.clone(), day.clone().unwrap_or_default()).await,
                key,
            )
        };
        match found {
            Ok(found) => {
                let (mut vals, s) = coerce_all(def, found, 1, caps.text_chars, zone.as_ref(), None);
                skipped += s;
                if let Some(v) = vals.pop() {
                    items += 1;
                    out.push((r.id.clone(), v));
                    new_marks.push((r.id.clone(), src, key));
                } else if filler.on_empty == crate::filler::OnEmpty::Clear {
                    out.push((r.id.clone(), Map::new()));
                    new_marks.push((r.id.clone(), src, key));
                }
                // `keep`: an empty answer writes nothing and is tried again.
            }
            Err(e) => failed.push(format!("{src}: {e}")),
        }
    }
    Fetch::PerRow {
        rows: out,
        marks: new_marks,
        items,
        skipped,
        failed,
        held,
    }
}

/// Why a per-row filler held rows back (after their count).
const HELD: &str = "rows wait: Claude wrote their URL without outward access, on a host this filler hasn't \
reached yet. A row on that host the owner adds (or Claude with outward access adds) lets them be filled.";

/// What each row of a per-row filler was last filled from: `row id ->
/// (source, content key, when)`.
fn row_marks(
    conn: &Connection,
    collection: &str,
    filler: &str,
) -> rusqlite::Result<HashMap<String, (String, String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT row_id, source, content_key, filled_at FROM collections_row_fills
         WHERE collection = ?1 AND filler = ?2",
    )?;
    let rows = stmt.query_map(params![collection, filler], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
    })?;
    rows.collect()
}

fn save_marks(
    tx: &Connection,
    collection: &str,
    filler: &str,
    marks: &[(String, String, String)],
    now: i64,
) -> rusqlite::Result<()> {
    for (row, source, key) in marks {
        tx.execute(
            "INSERT INTO collections_row_fills (collection, filler, row_id, source, content_key, filled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (collection, filler, row_id) DO UPDATE SET source = excluded.source,
                 content_key = excluded.content_key, filled_at = excluded.filled_at",
            params![collection, filler, row, source, key, now],
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_agent(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    force: bool,
    now: i64,
    spent: &AtomicI64,
    held: &Held,
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
    if filler.for_each().is_some() {
        return run_each(pod, adapter, def, filler, state, force, now, spent, held).await;
    }
    let day = take_model_budget(pod, adapter, filler, state, now, Some(held)).await?;
    spent.store(1, Ordering::SeqCst);
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
            pod,
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
        upcoming(filler, now),
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

/// A value as one line of a request (`None`: empty, or a ref's bare id).
fn line_value(f: &model::Field, v: &Value) -> Option<String> {
    if f.kind == Kind::Ref {
        return None;
    }
    if f.kind == Kind::Quantity {
        let t = crate::quantity::text_of(v);
        return (!t.trim().is_empty()).then_some(t);
    }
    crate::lookup::text_of(v)
}

/// The fields of `def` named `names` (live; aliases too), or every live
/// field but true/false ones and refs when `names` is empty.
fn named_fields<'a>(def: &'a Definition, names: &[String]) -> Vec<&'a model::Field> {
    if names.is_empty() {
        return def
            .live_fields()
            .filter(|f| !matches!(f.kind, Kind::Bool | Kind::Ref))
            .collect();
    }
    names
        .iter()
        .filter_map(|n| def.field(n).filter(|f| f.live()))
        .collect()
}

/// Conditions by field name as conditions by `def`'s ids (`None`: a field
/// is gone).
fn resolved(def: &Definition, c: &[model::Condition]) -> Option<Vec<model::Condition>> {
    c.iter()
        .map(|c| {
            def.field(&c.field).map(|f| model::Condition {
                field: f.id.clone(),
                equals: c.equals.clone(),
            })
        })
        .collect()
}

/// A `for_each` agent's fill (decision `research-per-row`): each live row of
/// the other collection that matches, whose text (or the context) is new or
/// changed since its last research (`refresh: once`), or also once
/// `every_minutes` have passed since it (`every`), up to [`crate::filler::MAX_EACH_PER_FILL`] and the budgets
/// left today: rows never researched first, then the longest ago. Answers
/// `(fill, the day the budget was counted on, calls)`. Each call is also
/// counted in `spent` as it is made, so a fill that times out still counts.
#[allow(clippy::too_many_arguments)]
async fn run_each(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    filler: &Filler,
    state: &State,
    force: bool,
    now: i64,
    spent: &AtomicI64,
    held: &Held,
) -> Result<(Fetch, String), String> {
    let Source::Agent {
        prompt,
        fields,
        max_searches,
        max_fetches,
        fills_per_day,
        for_each: Some(fe),
    } = &filler.source
    else {
        return Err("not a for_each agent filler".into());
    };
    let Some(agent) = adapter.agent() else {
        return Err(NO_AGENT.into());
    };
    let fe = fe.clone();
    let (name, fid) = (def.name.clone(), filler.id.clone());
    let read = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let parent = db::latest_def(c, &fe.collection)?.map(|(_, d)| d);
                let parent_rows = match &parent {
                    Some(_) => db::all_rows(c, &fe.collection, false)?,
                    None => Vec::new(),
                };
                let mut contexts = Vec::new();
                for x in &fe.context {
                    if let Some((_, d)) = db::latest_def(c, &x.collection)? {
                        let rows = db::all_rows(c, &x.collection, false)?;
                        contexts.push((x.clone(), d, rows));
                    }
                }
                Ok((
                    fe,
                    parent,
                    parent_rows,
                    contexts,
                    db::all_rows(c, &name, false)?,
                    row_marks(c, &name, &fid)?,
                ))
            })
        })
        .await
        .map_err(|_| "the collections could not be read".to_owned())?;
    let (fe, parent, parent_rows, contexts, own_rows, marks) = read;
    let Some(parent) = parent else {
        return Err(format!(
            "for_each: there is no collection '{}' any more",
            fe.collection
        ));
    };
    let Some(conditions) = resolved(&parent, &fe.conditions) else {
        return Err(format!(
            "for_each.where names a field '{}' no longer has",
            fe.collection
        ));
    };
    let from = named_fields(&parent, &fe.from);
    // The context: the same for every row of this fill.
    let mut context = String::new();
    for (x, d, rows) in &contexts {
        let Some(cond) = resolved(d, &x.conditions) else {
            continue;
        };
        let fs = named_fields(d, &x.fields);
        let lines: Vec<String> = rows
            .iter()
            .filter(|r| cond.iter().all(|c| c.matches(d, &r.values)))
            .take(x.limit as usize)
            .filter_map(|r| {
                let o: Map<String, Value> = fs
                    .iter()
                    .filter_map(|f| {
                        let v = r.values.get(&f.id)?;
                        let t = line_value(f, v)
                            .or_else(|| (f.kind == Kind::Bool).then(|| v.to_string()))?;
                        Some((f.name.clone(), json!(t)))
                    })
                    .collect();
                (!o.is_empty()).then(|| Value::Object(o).to_string())
            })
            .collect();
        if !lines.is_empty() {
            context.push_str(&format!(
                "\n\nFrom \"{}\" ({} rows):\n{}",
                d.title,
                lines.len(),
                lines.join("\n")
            ));
        }
    }
    let parents: HashSet<String> = parent_rows.iter().map(|r| r.id.clone()).collect();
    let force_all = force || state.fingerprint != filler.fingerprint();
    let refresh = filler.refresh();
    // Each matching row: its request text and that text's key.
    let mut todo: Vec<(&Row, String, String, Option<i64>)> = parent_rows
        .iter()
        .filter(|r| conditions.iter().all(|c| c.matches(&parent, &r.values)))
        .filter_map(|r| {
            let lines: Vec<String> = from
                .iter()
                .filter_map(|f| {
                    let t = line_value(f, r.values.get(&f.id)?)?;
                    Some(format!("- {}: {t}", f.name))
                })
                .collect();
            if lines.is_empty() {
                return None;
            }
            let text = lines.join("\n");
            let key = hash_of(format!("{text}\n{context}").as_bytes());
            let filled = marks
                .get(&r.id)
                .filter(|(s, _, _)| *s == key)
                .map(|(_, _, at)| *at);
            Some((r, text, key, filled))
        })
        .filter(|(_, _, _, filled)| {
            // `every`: a row is due again once `every_minutes` have passed
            // since its research (each fill without them), so a write to
            // one row doesn't research the others again.
            let again = refresh == crate::filler::Refresh::Every
                && filled.is_none_or(|at| {
                    filler
                        .every_minutes
                        .is_none_or(|m| now - at >= i64::from(m) * 60)
                });
            force_all || again || filled.is_none()
        })
        .collect();
    let day = today(pod.store(), now);
    if todo.is_empty() {
        return Ok((
            Fetch::Each {
                answers: Vec::new(),
                parents,
                items: 0,
                skipped: 0,
                calls: 0,
                failed: Vec::new(),
            },
            day,
        ));
    }
    todo.sort_by_key(|(r, _, _, filled)| {
        let last = marks.get(&r.id).map(|(_, _, at)| *at);
        (filled.is_some(), last.is_some(), last.unwrap_or(0))
    });
    // Budgets left today: the filler's and the pod's.
    let mine = if state.day.as_deref() == Some(day.as_str()) {
        state.day_fills
    } else {
        0
    };
    // The pod's is reserved before any model call (released when the fill
    // has written its count), so fillers running at once share it.
    let want = (i64::from(*fills_per_day) - mine)
        .min(todo.len().min(crate::filler::MAX_EACH_PER_FILL) as i64)
        .max(0);
    let left = if want > 0 {
        let cap = i64::from(adapter.caps().agent_fills_per_day);
        held.reserve(pod, &day, want, cap).await as usize
    } else {
        0
    };
    if left == 0 {
        return Err(format!(
            "this filler already researched {mine} rows today (fills_per_day {fills_per_day}, or the pod's limit)"
        ));
    }
    let agent_fields = agent_fields(def, fields);
    let key_name = filler.key.as_ref().map(|k| def.name_of(k));
    let caps = adapter.caps();
    let mut answers = Vec::new();
    let (mut items, mut skipped, mut calls) = (0, 0, 0);
    let mut failed = Vec::new();
    for (r, text, key, _) in todo.into_iter().take(left) {
        let current: Vec<Map<String, Value>> = own_rows
            .iter()
            .filter(|o| {
                o.filler.as_deref() == Some(filler.id.as_str())
                    && o.values.get(&fe.link).and_then(Value::as_str) == Some(r.id.as_str())
            })
            .take(crate::agent::MAX_CURRENT)
            .map(|o| {
                fields
                    .iter()
                    .filter_map(|id| {
                        let f = def.by_id(id)?;
                        Some((
                            f.name.clone(),
                            o.values.get(id).cloned().unwrap_or(Value::Null),
                        ))
                    })
                    .collect()
            })
            .collect();
        let request = format!(
            "{prompt}\n\nResearch this row of \"{}\" (every row you save is for it):\n{text}{context}",
            parent.title
        );
        calls += 1;
        spent.fetch_add(1, Ordering::SeqCst);
        let answer = agent
            .fill(AgentRequest {
                pod,
                title: &def.title,
                prompt: &request,
                fields: &agent_fields,
                today: &day,
                max_searches: *max_searches,
                max_fetches: *max_fetches,
                key: key_name.as_deref(),
                current: &current,
            })
            .await;
        match answer {
            Ok(a) => {
                let (vals, s) = coerce_all(
                    def,
                    by_ids(def, a.rows, fields),
                    caps.fill_items,
                    caps.text_chars,
                    None,
                    upcoming(filler, now),
                );
                skipped += s;
                // An empty answer keeps the row's last options and is asked
                // again next time.
                if vals.is_empty() {
                    failed.push(format!("{}: no rows", r.id));
                    continue;
                }
                items += vals.len();
                answers.push((r.id.clone(), vals, key));
            }
            Err(e) => failed.push(format!("{}: {e}", r.id)),
        }
    }
    Ok((
        Fetch::Each {
            answers,
            parents,
            items,
            skipped,
            calls,
            failed,
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
        let fetched = match filler.render() {
            Some(r) => render_page(pod, adapter, &link, r).await,
            None => {
                web_extract::hosts::fetch(
                    &hosts(pod),
                    adapter.fetch_client(),
                    &link,
                    None,
                    None,
                    &[],
                )
                .await
            }
        };
        let Ok(web_extract::fetch::Fetched::Body { bytes, .. }) = fetched else {
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
                        pod,
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
    let writer = db::filler_writer(&filler.id);
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
                        db::note_writes(tx, &d2.name, Some(&before), &r, Some(&writer), now)?;
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
    let guard = BusyGuard(&busy, key);
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
    // Its screen shows it filling (`refreshing`).
    publish_filler(pod, collection, filler_id);
    let now = pod_core::util::now_secs();
    let mut day = None;
    let mut extra_calls = 0;
    // The pod's agent budget this fill reserves, released once its count
    // is written below.
    let held = Held::new(pod);
    let off = off(pod).await;
    let fetched = if off.stops(&filler) {
        Fetch::Failed(
            if filler.is_mcp() { MCP_OFF } else { FILL_OFF }.into(),
            Some(86_400),
        )
    } else if filler.is_range() {
        run_range(pod, adapter, &def, &filler, &st, force, now)
    } else if filler.is_mcp() {
        match tokio::time::timeout(
            FETCH_FILL_TIMEOUT,
            run_mcp(pod, adapter, &def, &filler, &st, force, now),
        )
        .await
        {
            Ok(f) => f,
            Err(_) => Fetch::Failed("the fill timed out".into(), None),
        }
    } else if filler.is_agent() {
        let limit = if filler.for_each().is_some() {
            EACH_FILL_TIMEOUT
        } else {
            AGENT_FILL_TIMEOUT
        };
        // The model calls made, counted as they go: a fill that times out
        // spent them too.
        let spent = AtomicI64::new(0);
        match tokio::time::timeout(
            limit,
            run_agent(pod, adapter, &def, &filler, &st, force, now, &spent, &held),
        )
        .await
        {
            Ok(Ok((f, d))) => {
                match &f {
                    // Each researched row counts toward the budgets; a fill
                    // with nothing to research counts nothing.
                    Fetch::Each { calls, .. } => {
                        if *calls > 0 {
                            day = Some(d);
                            extra_calls = calls - 1;
                        }
                    }
                    _ => day = Some(d),
                }
                f
            }
            Ok(Err(e)) => Fetch::Failed(e, None),
            Err(_) => {
                let n = spent.load(Ordering::SeqCst);
                if n > 0 {
                    day = Some(today(pod.store(), now));
                    extra_calls = n - 1;
                }
                Fetch::Failed("the agent took too long".into(), None)
            }
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
        st.day_fills += 1 + extra_calls;
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
        Fetch::Each {
            answers,
            parents,
            items,
            skipped,
            calls,
            failed,
        } => {
            let (d2, f2) = (def.clone(), filler.clone());
            let link = filler
                .for_each()
                .map(|fe| fe.link.clone())
                .unwrap_or_default();
            let any = !answers.is_empty();
            let stored = crate::refusable(pod, move |tx| {
                let mut out = Stored::default();
                let mut jobs = Vec::new();
                let mut created = Vec::new();
                let mut marks = Vec::new();
                for (parent, items, key) in answers {
                    match store_scoped(tx, &d2, &f2, items, now, caps, Some((&link, &parent)))? {
                        Ok((s, j, c)) => {
                            out.created += s.created;
                            out.updated += s.updated;
                            out.removed += s.removed;
                            out.unchanged += s.unchanged;
                            out.dropped += s.dropped;
                            jobs.extend(j);
                            created.extend(c);
                            marks.push((parent.clone(), key, String::new()));
                        }
                        Err(e) => return Ok(Err(json!({ "error": e }))),
                    }
                }
                // Rows of a researched row that's gone (deleted or removed)
                // go too, unless kept.
                for r in db::all_rows(tx, &d2.name, true)? {
                    if r.filler.as_deref() != Some(f2.id.as_str()) {
                        continue;
                    }
                    let p = r.values.get(&link).and_then(Value::as_str).unwrap_or("");
                    if !parents.contains(p) && !f2.keeps(&d2, &r.values) {
                        hard_delete(tx, &d2.name, &r.id)?;
                        out.removed += 1;
                    }
                }
                save_marks(tx, &d2.name, &f2.id, &marks, now)?;
                Ok(Ok((out, jobs, created)))
            })
            .await;
            let total = calls as usize;
            match stored {
                Ok((stored, jobs, created)) if failed.is_empty() || any => {
                    created_rows = created;
                    st.failures = 0;
                    st.last_ok_at = Some(now);
                    st.last_error = (!failed.is_empty()).then(|| {
                        web_extract::html::truncate(
                            &format!(
                                "{} of {total} rows found nothing: {}",
                                failed.len(),
                                failed.join("; ")
                            ),
                            300,
                        )
                    });
                    st.next_at = scheduled;
                    st.fingerprint = fingerprint.clone();
                    st.last_rows = Some(items as i64);
                    st.last_skipped = Some((skipped + stored.dropped) as i64);
                    st.last_past = None;
                    st.empty_runs = 0;
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
                    let e = format!("every row failed: {}", failed.join("; "));
                    st.failures += 1;
                    st.last_error = Some(web_extract::html::truncate(&e, 300));
                    st.next_at = Some(backoff(&filler, st.failures, None, now));
                    (Outcome::Failed(e), Vec::new())
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
            marks,
            items,
            skipped,
            failed,
            held,
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
                                db::note_writes(
                                    tx,
                                    &d2.name,
                                    Some(&before),
                                    &r,
                                    Some(&db::filler_writer(&f2.id)),
                                    now,
                                )?;
                                out.updated += 1;
                            } else {
                                out.unchanged += 1;
                            }
                        }
                        save_marks(tx, &d2.name, &f2.id, &marks, now)?;
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
                    if held > 0 {
                        let note = format!("{held} {HELD}");
                        st.last_error = Some(match st.last_error.take() {
                            Some(e) => web_extract::html::truncate(&format!("{note}; {e}"), 300),
                            None => note,
                        });
                    }
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
    // Its count is written: what it reserved is released (the rest refunded).
    drop(held);
    // Done: clients reload with `refreshing` false and the new health.
    drop(guard);
    if let Outcome::Filled { stored, .. } = &outcome
        && stored.changed()
    {
        pod.publish_data_all(NAME);
        crate::alerts::run(pod, Some(collection.to_owned())).await;
    } else {
        publish_filler(pod, collection, filler_id);
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

/// A row of `collection` was written (by a person, an action or Claude):
/// start, after [`RESEARCH_DELAY`], the `for_each` agents that research its
/// rows (decision `research-per-row`). A fill with nothing new or changed
/// calls no model; while one runs, this tries again.
pub fn research_soon(pod: &Pod, collection: &str) {
    let pod = pod.clone();
    let c = collection.to_owned();
    tokio::spawn(async move {
        let c2 = c.clone();
        let targets: Vec<(String, String)> = pod
            .blocking(move |s| {
                s.with_conn(|conn| {
                    Ok(db::all_defs(conn)?
                        .into_iter()
                        .flat_map(|(_, d)| {
                            d.fillers
                                .iter()
                                .filter(|f| f.for_each().is_some_and(|fe| fe.collection == c2))
                                .map(|f| (d.name.clone(), f.id.clone()))
                                .collect::<Vec<_>>()
                        })
                        .collect())
                })
            })
            .await
            .unwrap_or_default();
        if targets.is_empty() {
            return;
        }
        tokio::time::sleep(RESEARCH_DELAY).await;
        for (col, f) in targets {
            for _ in 0..RESEARCH_TRIES {
                if !matches!(fill(&pod, &col, &f, false).await, Outcome::Busy) {
                    break;
                }
                tokio::time::sleep(RESEARCH_RETRY).await;
            }
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
    off: Off,
    agent: bool,
) -> (&'static str, String) {
    if off.stops(f) {
        (
            "off",
            if f.is_mcp() { MCP_OFF } else { FILL_OFF }.to_owned(),
        )
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
pub fn health(def: &Definition, f: &Filler, st: &State, now: i64, off: Off, agent: bool) -> Value {
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
            normalize(
                Kind::Datetime,
                json!("2026-11-16T23:30:00.000Z"),
                Some(&denver)
            ),
            json!("2026-11-16T16:30-07:00"),
            "a UTC time shows on the zone's clock"
        );
        assert_eq!(
            normalize(Kind::Datetime, json!("2026-07-11T01:30Z"), Some(&denver)),
            json!("2026-07-10T19:30-06:00"),
            "in daylight saving too"
        );
        assert_eq!(
            normalize(Kind::Datetime, json!("2026-11-16T23:30Z"), None),
            json!("2026-11-16T23:30Z"),
            "without a zone it stays"
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
    fn keyless_items_pair_with_the_rows_they_are() {
        let fields: Vec<String> = ["t", "s", "v", "d"].map(String::from).to_vec();
        let item = |t: &str, s: &str, v: &str, d: &str| {
            json!({"t": t, "s": s, "v": v, "d": d})
                .as_object()
                .unwrap()
                .clone()
        };
        let row = |id: &str, values: Map<String, Value>| Row {
            id: id.into(),
            client_id: None,
            values,
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            filler: Some("s1".into()),
        };
        let mut rows = vec![
            row(
                "jazz1",
                item("Jazz night", "2099-12-04T20:00-07:00", "Bar", "Trio"),
            ),
            row(
                "jazz2",
                item("Jazz night", "2099-12-11T20:00-07:00", "Bar", "Trio"),
            ),
            row("gone", item("Disco", "2099-12-05T21:00-07:00", "Bar", "DJ")),
        ];
        let items = vec![
            // Reworded by the model: still the same show.
            item(
                "Jazz Night ",
                "2099-12-04T20:00-07:00",
                "bar",
                "A trio plays standards",
            ),
            item("Jazz night", "2099-12-18T20:00-07:00", "Bar", "Trio"),
            item("Jazz night!", "2099-12-11T20:00-07:00", "Bar", "Quartet"),
        ];
        let s = Some("s");
        assert_eq!(
            pair_rows(&rows, &items, &fields, s),
            [Some(0), None, Some(1)],
            "next week's show of the same name is new; the dropped show pairs with nothing"
        );
        rows[0].deleted_at = Some(1);
        assert_eq!(pair_rows(&rows, &items, &fields, s)[0], None, "deleted");
        assert_eq!(
            day_of(Some(&json!("2099-12-04T20:00-07:00"))),
            Some("2099-12-04")
        );
        assert_eq!(day_of(Some(&json!("2099-12-04"))), Some("2099-12-04"));
        assert_eq!(day_of(Some(&json!("Friday"))), None);
    }

    #[test]
    fn past_items_dont_count_toward_the_fill_cap() {
        let def: Definition = serde_json::from_value(json!({
            "name": "events", "title": "Events", "next_field": 3,
            "fields": [{"id": "f1", "name": "title", "type": "text", "optional": true},
                       {"id": "f2", "name": "start", "type": "datetime", "optional": true}]
        }))
        .unwrap();
        // A feed listing past events first, then two upcoming ones.
        let now = 1_794_900_000; // 2026-11-17
        let item =
            |t: &str, start: &str| json!({"f1": t, "f2": start}).as_object().unwrap().clone();
        let mut rows: Vec<_> = (1..=5)
            .map(|d| item(&format!("past {d}"), &format!("2026-10-0{d}T19:00Z")))
            .collect();
        rows.push(item("Sundance", "2027-01-21T19:00Z"));
        rows.push(item("Film festival", "2027-04-08T19:00Z"));
        rows.push(item("Too many", "2027-05-01T19:00Z"));
        let titles = |items: &[Map<String, Value>]| -> Vec<String> {
            items
                .iter()
                .map(|i| i["f1"].as_str().unwrap().to_owned())
                .collect()
        };
        let (items, _) = coerce_all(&def, rows.clone(), 2, 100, None, Some(("f2", now)));
        assert_eq!(items.len(), 7, "past items pass through for store to count");
        assert_eq!(
            titles(&items[5..]),
            ["Sundance", "Film festival"],
            "the cap counts only upcoming items"
        );
        let (items, _) = coerce_all(&def, rows, 2, 100, None, None);
        assert_eq!(titles(&items), ["past 1", "past 2"], "without upcoming");
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
