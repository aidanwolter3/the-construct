//! Process-wide counters for monitoring (decision `server-monitoring`). Two readers:
//! the server's private Prometheus listener (`CONSTRUCT_METRICS_ADDR`,
//! [`render`]) and the `monitor` data source, which shows them in the pod
//! with their last hour and last day ([`series`]).
//!
//! Every counter is listed in [`COUNTERS`], so a reader sees it (at 0)
//! before it first counts. Label values are fixed strings chosen by the
//! caller (a status code, a feature name), never user input, so the number
//! of series stays small.
//!
//! The counters cover what an operator wants to notice: failed and
//! rate-limited sign-in attempts, new credentials, temporary pod churn,
//! server errors, and model calls and tokens (the one thing a caller can
//! spend money on). Values measured when read (pods, free disk) are gauges
//! the host supplies ([`set_gauge_source`]). Everything lives in memory: a
//! restart starts again from 0.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use crate::util::now_secs;

/// Responses of the host, by status code (`status`).
pub const HTTP_RESPONSES: &str = "construct_http_responses_total";
/// Requests refused for a credential: `kind` is `pod_code` (the code
/// screen), `oauth_pod_code` (Claude's connect page), `unknown_token` (a
/// bearer token no pod has), `mcp_scope` (an MCP credential used outside
/// `/v1/mcp`) or `open_pod_host` (the open pod asked for by another `Host`).
pub const AUTH_FAILURES: &str = "construct_auth_failures_total";
/// Requests refused by a rate limit (`limit`): `pod_code`,
/// `oauth_register`, `oauth_pod_code` or `temporary_pod`.
pub const RATE_LIMITED: &str = "construct_rate_limited_total";
/// Credentials and clients made (`kind`): `oauth_client` (a registration,
/// open to anyone), `oauth_connect` (a pod code entered on the connect
/// page) or `mcp` (an MCP credential handed to Claude).
pub const CREDENTIALS_ISSUED: &str = "construct_credentials_issued_total";
/// Temporary pod requests (`outcome`): `created`, `full` or `low_disk`.
pub const TEMPORARY_PODS: &str = "construct_temporary_pod_requests_total";
/// Model calls (`feature`: what asked: `collections` (computed fields),
/// `fill_page` (a filler reading a page), `fill_agent` (an agent fill);
/// `outcome`: `ok`, `error` or `refused`).
pub const LLM_REQUESTS: &str = "construct_llm_requests_total";
/// Model tokens (`feature`; `direction`: `input` or `output`), from the
/// answer's `usage`.
pub const LLM_TOKENS: &str = "construct_llm_tokens_total";
/// Web searches and fetches the model ran (`feature`; `tool`:
/// `web_search` or `web_fetch`), billed per use on top of tokens.
pub const LLM_WEB_TOOLS: &str = "construct_llm_web_tools_total";

type Labels = &'static [(&'static str, &'static str)];

/// Every counter: its name, help text and the series always shown.
pub const COUNTERS: &[(&str, &str, &[Labels])] = &[
    (
        HTTP_RESPONSES,
        "Responses of the host, by status code.",
        &[],
    ),
    (
        AUTH_FAILURES,
        "Requests refused for a wrong or out-of-scope credential.",
        &[
            &[("kind", "pod_code")],
            &[("kind", "oauth_pod_code")],
            &[("kind", "unknown_token")],
            &[("kind", "mcp_scope")],
            &[("kind", "open_pod_host")],
        ],
    ),
    (
        RATE_LIMITED,
        "Requests refused by a rate limit.",
        &[
            &[("limit", "pod_code")],
            &[("limit", "oauth_register")],
            &[("limit", "oauth_pod_code")],
            &[("limit", "temporary_pod")],
        ],
    ),
    (
        CREDENTIALS_ISSUED,
        "OAuth clients registered and credentials issued.",
        &[
            &[("kind", "oauth_client")],
            &[("kind", "oauth_connect")],
            &[("kind", "mcp")],
        ],
    ),
    (
        TEMPORARY_PODS,
        "Temporary pod requests, by outcome.",
        &[
            &[("outcome", "created")],
            &[("outcome", "full")],
            &[("outcome", "low_disk")],
        ],
    ),
    (LLM_REQUESTS, "Model calls, by feature and outcome.", &[]),
    (LLM_TOKENS, "Model tokens, by feature and direction.", &[]),
    (
        LLM_WEB_TOOLS,
        "Web searches and fetches the model ran, by feature and tool.",
        &[],
    ),
];

/// A series' labels, sorted by name.
pub type LabelSet = Vec<(String, String)>;
/// Counter values by series.
type Values = BTreeMap<(&'static str, LabelSet), u64>;

static VALUES: LazyLock<Mutex<Values>> = LazyLock::new(|| {
    let mut v = Values::new();
    for (name, _, series) in COUNTERS {
        for labels in *series {
            v.insert((*name, labels_of(labels)), 0);
        }
    }
    Mutex::new(v)
});

static STARTED: LazyLock<i64> = LazyLock::new(now_secs);

fn values() -> std::sync::MutexGuard<'static, Values> {
    VALUES.lock().unwrap_or_else(|e| e.into_inner())
}

fn labels_of(labels: &[(&str, &str)]) -> LabelSet {
    let mut l: LabelSet = labels
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    l.sort();
    l
}

/// When counting started (the process start, once [`started_at`] is first
/// called; the server does that at startup).
pub fn started_at() -> i64 {
    *STARTED
}

/// Add 1 to `name` with `labels`.
pub fn inc(name: &'static str, labels: &[(&str, &str)]) {
    add(name, labels, 1);
}

/// Add `n` to `name` with `labels`.
pub fn add(name: &'static str, labels: &[(&str, &str)], n: u64) {
    let mut v = values();
    let c = v.entry((name, labels_of(labels))).or_insert(0);
    *c = c.saturating_add(n);
}

/// The value of `name` with `labels` (tests).
pub fn get(name: &str, labels: &[(&str, &str)]) -> u64 {
    let key = labels_of(labels);
    values()
        .iter()
        .find(|((n, l), _)| *n == name && *l == key)
        .map_or(0, |(_, v)| *v)
}

/// Count one model call of `feature` from whether it succeeded and its
/// answer: the outcome (`refused` when the model declined) and the tokens
/// in `usage`.
pub fn record_llm(feature: &str, success: bool, answer: Option<&serde_json::Value>) {
    let refused = answer.is_some_and(|a| a["stop_reason"] == "refusal");
    let outcome = match (success, refused) {
        (true, false) => "ok",
        (true, true) => "refused",
        (false, _) => "error",
    };
    inc(LLM_REQUESTS, &[("feature", feature), ("outcome", outcome)]);
    let Some(usage) = answer.map(|a| &a["usage"]) else {
        return;
    };
    let tokens = |k: &str| usage[k].as_u64().unwrap_or(0);
    let input = tokens("input_tokens")
        + tokens("cache_creation_input_tokens")
        + tokens("cache_read_input_tokens");
    for (direction, n) in [("input", input), ("output", tokens("output_tokens"))] {
        if n > 0 {
            add(
                LLM_TOKENS,
                &[("feature", feature), ("direction", direction)],
                n,
            );
        }
    }
}

// ---------- gauges ----------

/// A value measured when read (pods, free disk).
#[derive(Debug, Clone)]
pub struct Gauge {
    pub name: &'static str,
    pub help: &'static str,
    pub series: Vec<(LabelSet, f64)>,
}

impl Gauge {
    /// A gauge with one unlabelled value.
    pub fn one(name: &'static str, help: &'static str, value: f64) -> Self {
        Self {
            name,
            help,
            series: vec![(Vec::new(), value)],
        }
    }
}

type GaugeSource = Arc<dyn Fn() -> Vec<Gauge> + Send + Sync>;

static GAUGES: RwLock<Option<GaugeSource>> = RwLock::new(None);

/// Who measures the gauges (the pod host). Blocking is fine: readers call
/// it off the async runtime.
pub fn set_gauge_source(f: impl Fn() -> Vec<Gauge> + Send + Sync + 'static) {
    *GAUGES.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// The gauges now (none without a source). Blocking.
pub fn gauges() -> Vec<Gauge> {
    let f = GAUGES.read().unwrap_or_else(|e| e.into_inner()).clone();
    f.map_or_else(Vec::new, |f| f())
}

// ---------- history ----------

/// How often [`series`] keeps a copy of the counters to measure windows
/// against (also on each read; the server ticks it in between).
pub const SNAPSHOT_EVERY_SECS: i64 = 300;
pub const HOUR: i64 = 3600;
pub const DAY: i64 = 24 * HOUR;

/// Copies of the counters, oldest first, back to just over a day.
#[derive(Debug, Default)]
pub struct History {
    snaps: VecDeque<(i64, Values)>,
}

impl History {
    /// Keep a copy of `values` taken at `now` when the last is old enough,
    /// and drop copies no window needs.
    pub fn record(&mut self, now: i64, values: &Values) {
        if self
            .snaps
            .back()
            .is_none_or(|(t, _)| now - t >= SNAPSHOT_EVERY_SECS)
        {
            self.snaps.push_back((now, values.clone()));
        }
        // The newest copy at or before a day ago is the day's baseline.
        while self.snaps.len() > 1 && self.snaps[1].0 <= now - DAY {
            self.snaps.pop_front();
        }
    }

    /// How much `key` grew in the `window` before `now`: against the newest
    /// copy at least that old, else against 0 when counting started within
    /// the window, else against the oldest copy.
    fn growth(
        &self,
        key: &(&'static str, LabelSet),
        value: u64,
        now: i64,
        window: i64,
        started: i64,
    ) -> u64 {
        let base = self
            .snaps
            .iter()
            .rev()
            .find(|(t, _)| *t <= now - window)
            .or_else(|| {
                (started < now - window)
                    .then(|| self.snaps.front())
                    .flatten()
            })
            .map_or(0, |(_, v)| v.get(key).copied().unwrap_or(0));
        value.saturating_sub(base)
    }
}

static HISTORY: LazyLock<Mutex<History>> = LazyLock::new(Default::default);

/// Keep a copy of the counters if one is due (the server calls this every
/// [`SNAPSHOT_EVERY_SECS`]).
pub fn snapshot() {
    let v = values().clone();
    HISTORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .record(now_secs(), &v);
}

/// One counter series and its growth.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: &'static str,
    pub labels: LabelSet,
    /// Since counting started.
    pub total: u64,
    pub last_hour: u64,
    pub last_day: u64,
}

impl Series {
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Every counter series with its last hour and last day.
pub fn series() -> Vec<Series> {
    let now = now_secs();
    let v = values().clone();
    let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    h.record(now, &v);
    let started = started_at();
    v.into_iter()
        .map(|(key, total)| Series {
            last_hour: h.growth(&key, total, now, HOUR, started),
            last_day: h.growth(&key, total, now, DAY, started),
            name: key.0,
            labels: key.1,
            total,
        })
        .collect()
}

// ---------- Prometheus ----------

/// Every counter and gauge in the Prometheus text format (0.0.4).
/// Blocking (the gauges read the host's database).
pub fn render() -> String {
    let mut out = String::new();
    let v = values().clone();
    for (name, help, _) in COUNTERS {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} counter");
        for ((_, labels), value) in v.iter().filter(|((n, _), _)| n == name) {
            let _ = writeln!(out, "{name}{} {value}", label_text(labels));
        }
    }
    for g in gauges() {
        let _ = writeln!(out, "# HELP {} {}\n# TYPE {} gauge", g.name, g.help, g.name);
        for (labels, value) in &g.series {
            let _ = writeln!(out, "{}{} {value}", g.name, label_text(labels));
        }
    }
    out
}

/// `{a="1",b="2"}`, or "" without labels.
fn label_text(labels: &LabelSet) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_series_show_before_they_count() {
        let text = render();
        assert!(text.contains("# TYPE construct_auth_failures_total counter"));
        assert!(text.contains("construct_rate_limited_total{limit=\"temporary_pod\"}"));
    }

    #[test]
    fn counts_and_renders_labels() {
        let labels = [("status", "418"), ("a", "x\"y")];
        let before = get(HTTP_RESPONSES, &labels);
        inc(HTTP_RESPONSES, &labels);
        add(HTTP_RESPONSES, &labels, 2);
        assert_eq!(get(HTTP_RESPONSES, &labels), before + 3);
        let line = format!(
            "construct_http_responses_total{{a=\"x\\\"y\",status=\"418\"}} {}",
            before + 3
        );
        assert!(render().contains(&line));
        let s = series()
            .into_iter()
            .find(|s| s.name == HTTP_RESPONSES && s.label("status") == Some("418"))
            .unwrap();
        // Counting started in this test process, within the hour.
        assert_eq!(
            (s.total, s.last_hour, s.last_day),
            (before + 3, before + 3, before + 3)
        );
    }

    #[test]
    fn model_calls_and_tokens() {
        let f = "metrics_test";
        let ok = json!({ "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "cache_read_input_tokens": 20, "output_tokens": 7 } });
        record_llm(f, true, Some(&ok));
        record_llm(f, true, Some(&json!({ "stop_reason": "refusal" })));
        record_llm(f, false, None);
        let req = |o| get(LLM_REQUESTS, &[("feature", f), ("outcome", o)]);
        assert_eq!((req("ok"), req("refused"), req("error")), (1, 1, 1));
        let tok = |d| get(LLM_TOKENS, &[("feature", f), ("direction", d)]);
        assert_eq!((tok("input"), tok("output")), (120, 7));
    }

    #[test]
    fn windows_measure_against_old_copies() {
        let key = (AUTH_FAILURES, labels_of(&[("kind", "x")]));
        let at = |n: u64| Values::from([(key.clone(), n)]);
        let mut h = History::default();
        let t0 = 1_000_000;
        // Counting started two days ago; copies every 5 min from t0.
        let started = t0 - 2 * DAY;
        for i in 0..=(DAY / SNAPSHOT_EVERY_SECS + 24) {
            let t = t0 + i * SNAPSHOT_EVERY_SECS;
            h.record(t, &at(i as u64));
        }
        let now = t0 + DAY + 2 * HOUR;
        let value = ((now - t0) / SNAPSHOT_EVERY_SECS) as u64;
        assert_eq!(h.growth(&key, value, now, HOUR, started), 12);
        assert_eq!(h.growth(&key, value, now, DAY, started), 288);
        // Only copies back to just over a day are kept.
        assert!(h.snaps.front().unwrap().0 <= now - DAY);
        assert!(h.snaps.len() <= (DAY / SNAPSHOT_EVERY_SECS) as usize + 2);
        // Started 10 minutes ago: everything is within the hour.
        let mut fresh = History::default();
        fresh.record(now, &at(5));
        assert_eq!(fresh.growth(&key, 5, now, HOUR, now - 600), 5);
    }

    #[test]
    fn gauges_come_from_the_source() {
        set_gauge_source(|| {
            vec![Gauge {
                name: "construct_test_pods",
                help: "Pods.",
                series: vec![(labels_of(&[("kind", "admin")]), 2.0)],
            }]
        });
        let text = render();
        assert!(
            text.contains(
                "# TYPE construct_test_pods gauge\nconstruct_test_pods{kind=\"admin\"} 2\n"
            )
        );
    }
}
