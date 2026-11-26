//! Paid use by the month (decision `monthly-paid-use`): what the pod spent
//! of services billed per use, counted when the request is made and kept,
//! so a screen can show this month's total against a free allowance.
//!
//! Two ledgers, one table each (`paid_use`, by UTC calendar month and kind):
//!
//! - **The pod's** (its store): its own web search provider's requests
//!   (`search:<provider>`, the pod's own key) and the model calls it made
//!   on the server's key (`model:*`).
//! - **The server's** (the host's store, through [`set_server_ledger`]):
//!   the model calls every pod made, since they all spend the server's one
//!   key. A pod's search provider is its own, so searches stay out of it.
//!
//! Only real requests count: an answer from a cache costs nothing. Months
//! older than [`KEEP_MONTHS`] are dropped as new ones are written.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use rusqlite::{Connection, params};

use crate::store::Store;
use crate::util::now_secs;

/// Months kept, this one included.
pub const KEEP_MONTHS: i64 = 13;

/// A web search provider's request: `search:<provider>`.
pub const SEARCH_PREFIX: &str = "search:";
/// Model requests (one per answer, retries included).
pub const MODEL_CALLS: &str = "model:calls";
/// Model tokens by direction, from the answer's `usage`.
pub const MODEL_INPUT: &str = "model:input";
pub const MODEL_CACHE_WRITE: &str = "model:cache_write";
pub const MODEL_CACHE_READ: &str = "model:cache_read";
pub const MODEL_OUTPUT: &str = "model:output";
/// The model's own web searches and fetches (billed per use).
pub const MODEL_WEB_SEARCH: &str = "model:web_search";
pub const MODEL_WEB_FETCH: &str = "model:web_fetch";

/// The UTC calendar month of `now` (Unix seconds), `YYYY-MM`.
pub fn month_of(now: i64) -> String {
    let d = jiff::Timestamp::from_second(now)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC);
    format!("{:04}-{:02}", d.year(), d.month())
}

/// The month `back` months before `month` (`YYYY-MM`).
fn months_before(month: &str, back: i64) -> String {
    let (y, m) = month.split_once('-').unwrap_or(("1970", "01"));
    let total = y.parse::<i64>().unwrap_or(1970) * 12 + m.parse::<i64>().unwrap_or(1) - 1 - back;
    format!(
        "{:04}-{:02}",
        total.div_euclid(12),
        total.rem_euclid(12) + 1
    )
}

/// Add `n` to `kind` in `month` on `conn` (a `paid_use` table), keeping
/// when the month's first count of it was, and drop months past
/// [`KEEP_MONTHS`].
pub fn add_on(
    conn: &Connection,
    month: &str,
    kind: &str,
    n: u64,
    now: i64,
) -> rusqlite::Result<()> {
    if n == 0 {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO paid_use (month, kind, n, first_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (month, kind) DO UPDATE SET n = n + ?3",
        params![month, kind, i64::try_from(n).unwrap_or(i64::MAX), now],
    )?;
    conn.execute(
        "DELETE FROM paid_use WHERE month <= ?1",
        [months_before(month, KEEP_MONTHS)],
    )?;
    Ok(())
}

/// One month's counts by kind on `conn`, and when counting began (the
/// earliest count kept, any month).
pub fn month_on(conn: &Connection, month: &str) -> rusqlite::Result<Month> {
    let mut stmt = conn.prepare("SELECT kind, n FROM paid_use WHERE month = ?1")?;
    let counts = stmt
        .query_map([month], |r| {
            Ok((
                r.get::<_, String>(0)?,
                u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let since = conn.query_row("SELECT MIN(first_at) FROM paid_use", [], |r| {
        r.get::<_, Option<i64>>(0)
    })?;
    Ok(Month {
        month: month.to_owned(),
        counts,
        since,
    })
}

/// One month of a ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Month {
    /// `YYYY-MM`.
    pub month: String,
    pub counts: BTreeMap<String, u64>,
    /// When the ledger's earliest kept count was made (Unix seconds); none
    /// before anything was counted.
    pub since: Option<i64>,
}

impl Month {
    pub fn get(&self, kind: &str) -> u64 {
        self.counts.get(kind).copied().unwrap_or(0)
    }

    /// Requests of each search provider, by provider name.
    pub fn searches(&self) -> BTreeMap<&str, u64> {
        self.counts
            .iter()
            .filter_map(|(k, n)| k.strip_prefix(SEARCH_PREFIX).map(|p| (p, *n)))
            .collect()
    }
}

/// The server's ledger: the host's store, shared by its pods.
pub trait ServerLedger: Send + Sync {
    fn add(&self, month: &str, kind: &str, n: u64, now: i64);
    fn month(&self, month: &str) -> Option<Month>;
}

static SERVER: RwLock<Option<Arc<dyn ServerLedger>>> = RwLock::new(None);

/// Who keeps the server's ledger (the pod host). Blocking is fine: writers
/// and readers are on blocking threads or the store's own lock.
pub fn set_server_ledger(l: Arc<dyn ServerLedger>) {
    *SERVER.write().unwrap_or_else(|e| e.into_inner()) = Some(l);
}

fn server() -> Option<Arc<dyn ServerLedger>> {
    SERVER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The server's `month` (none without a ledger, e.g. in a pod's own tests).
pub fn server_month(month: &str) -> Option<Month> {
    server()?.month(month)
}

fn add_pod(store: &Store, kind: &str, n: u64, now: i64) {
    let month = month_of(now);
    if let Err(e) = store.with_conn(|c| add_on(c, &month, kind, n, now)) {
        tracing::warn!(error = %e, kind, "paid use: the pod's count failed");
    }
}

/// Count one request of the pod's own search `provider` (its key).
pub fn add_search(store: &Store, provider: &str) {
    add_pod(store, &format!("{SEARCH_PREFIX}{provider}"), 1, now_secs());
}

/// Count one model answer on the server's key: one call, its tokens by
/// direction and the model's own web searches and fetches, in the pod's
/// ledger and the server's.
pub fn add_model_answer(store: &Store, answer: &serde_json::Value) {
    let now = now_secs();
    let usage = &answer["usage"];
    let tokens = |k: &str| usage[k].as_u64().unwrap_or(0);
    let mut tools = (0u64, 0u64);
    for b in answer["content"].as_array().into_iter().flatten() {
        if b["type"] == "server_tool_use" {
            match b["name"].as_str() {
                Some("web_search") => tools.0 += 1,
                Some("web_fetch") => tools.1 += 1,
                _ => {}
            }
        }
    }
    let counts = [
        (MODEL_CALLS, 1),
        (MODEL_INPUT, tokens("input_tokens")),
        (MODEL_CACHE_WRITE, tokens("cache_creation_input_tokens")),
        (MODEL_CACHE_READ, tokens("cache_read_input_tokens")),
        (MODEL_OUTPUT, tokens("output_tokens")),
        (MODEL_WEB_SEARCH, tools.0),
        (MODEL_WEB_FETCH, tools.1),
    ];
    let server = server();
    let month = month_of(now);
    for (kind, n) in counts {
        add_pod(store, kind, n, now);
        if let Some(s) = &server {
            s.add(&month, kind, n, now);
        }
    }
}

/// The pod's `month`. Blocking.
pub fn pod_month(store: &Store, month: &str) -> rusqlite::Result<Month> {
    store.with_conn(|c| month_on(c, month))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn months_are_utc_calendar_months() {
        // 2026-11-30T23:59:59Z and a second later.
        assert_eq!(month_of(1_796_083_199), "2026-11");
        assert_eq!(month_of(1_796_083_200), "2026-12");
        assert_eq!(months_before("2026-02", 3), "2025-11");
        assert_eq!(months_before("2026-12", 12), "2025-12");
    }

    #[test]
    fn searches_and_model_answers_add_up_by_month() {
        let store = Store::open_in_memory().unwrap();
        add_search(&store, "brave");
        add_search(&store, "brave");
        add_search(&store, "searxng");
        add_model_answer(
            &store,
            &json!({
                "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100},
                "content": [
                    {"type": "server_tool_use", "name": "web_search"},
                    {"type": "server_tool_use", "name": "web_fetch"},
                    {"type": "text", "text": "hi"}
                ]
            }),
        );
        let m = pod_month(&store, &month_of(now_secs())).unwrap();
        assert_eq!(m.searches(), BTreeMap::from([("brave", 2), ("searxng", 1)]));
        assert_eq!(m.get(MODEL_CALLS), 1);
        assert_eq!(m.get(MODEL_INPUT), 10);
        assert_eq!(m.get(MODEL_OUTPUT), 5);
        assert_eq!(m.get(MODEL_CACHE_READ), 100);
        assert_eq!(m.get(MODEL_CACHE_WRITE), 0);
        assert_eq!(m.get(MODEL_WEB_SEARCH), 1);
        assert_eq!(m.get(MODEL_WEB_FETCH), 1);
        assert!(m.since.is_some());
        // Another month is empty.
        assert!(pod_month(&store, "1999-01").unwrap().counts.is_empty());
    }

    #[test]
    fn old_months_are_dropped() {
        let store = Store::open_in_memory().unwrap();
        store
            .with_conn(|c| {
                add_on(c, "2025-01", "search:brave", 4, 1)?;
                add_on(c, "2026-02", "search:brave", 1, 2)
            })
            .unwrap();
        assert!(pod_month(&store, "2025-01").unwrap().counts.is_empty());
        assert_eq!(pod_month(&store, "2026-02").unwrap().get("search:brave"), 1);
    }
}
