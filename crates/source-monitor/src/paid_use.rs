//! The `paid_use` feed (decision `monthly-paid-use`): this month's use of
//! services billed per use ([`pod_core::paid_use`]), in two parts the
//! screen keeps apart:
//!
//! - `monitor://paid_use/pod`: **this pod's** own: its web search
//!   provider's requests (its own key; Brave against its free monthly
//!   credit) and the model calls it made on the server's key.
//! - `monitor://paid_use/server`: **the server's**: the model calls every
//!   pod made on the server's one key. The whole server's, so only in pods
//!   the operator turned monitoring on for, like the `overview` feed.

use pod_core::paid_use::{
    MODEL_CACHE_READ, MODEL_CACHE_WRITE, MODEL_CALLS, MODEL_INPUT, MODEL_OUTPUT, MODEL_WEB_FETCH,
    MODEL_WEB_SEARCH, Month,
};
use pod_core::tz::jiff::{self, tz::TimeZone};
use protocol::DataBundle;
use serde_json::{Map, Value, json};

use crate::record;

pub const PAID_USE_FEED: &str = "paid_use";
pub const PAID_USE_QUERY: &str = "monitor:paid_use";
pub const POD_URI: &str = "monitor://paid_use/pod";
pub const SERVER_URI: &str = "monitor://paid_use/server";

/// Brave Search: the searches its free monthly credit covers, and the
/// price per 1,000 in cents (its $5 credit is 1,000 searches).
pub const BRAVE_FREE_SEARCHES: u64 = 1_000;
pub const BRAVE_CENTS_PER_1000: u64 = 500;

/// What the feed reads: the pod's month, its plugged search provider, the
/// server's month (none where monitoring is off) and the pod's zone.
pub struct Input<'a> {
    pub pod: &'a Month,
    pub provider: Option<&'a str>,
    pub server: Option<&'a Month>,
    pub zone: &'a TimeZone,
}

/// `1,234`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `950`, `12K`, `1.2M`.
pub fn short(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => {
            let k = n as f64 / 1_000.0;
            if k < 10.0 {
                format!("{:.1}K", k).replace(".0K", "K")
            } else {
                format!("{:.0}K", k)
            }
        }
        _ => {
            let m = n as f64 / 1_000_000.0;
            if m < 10.0 {
                format!("{:.1}M", m).replace(".0M", "M")
            } else {
                format!("{:.0}M", m)
            }
        }
    }
}

/// `$1.75`.
fn dollars(cents: u64) -> String {
    format!("${}.{:02}", thousands(cents / 100), cents % 100)
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

fn provider_name(p: &str) -> &str {
    match p {
        "brave" => "Brave",
        "searxng" => "SearXNG",
        "duckduckgo" => "DuckDuckGo",
        other => other,
    }
}

/// `November` and, when counting began after the month's first day,
/// `November, counted since Nov 26`.
fn period(m: &Month, zone: &TimeZone) -> (String, String) {
    let (y, mo) = m.month.split_once('-').unwrap_or(("1970", "01"));
    let name = jiff::civil::date(y.parse().unwrap_or(1970), mo.parse().unwrap_or(1), 1)
        .strftime("%B")
        .to_string();
    let start = jiff::civil::date(y.parse().unwrap_or(1970), mo.parse().unwrap_or(1), 1)
        .to_zoned(TimeZone::UTC)
        .map(|z| z.timestamp().as_second())
        .unwrap_or(0);
    let since = m.since.filter(|s| *s > start + 86_400).and_then(|s| {
        jiff::Timestamp::from_second(s)
            .ok()
            .map(|t| t.to_zoned(zone.clone()).strftime("%b %-d").to_string())
    });
    let text = match since {
        Some(d) => format!("{name}, counted since {d}"),
        None => format!("{name} so far"),
    };
    (name, text)
}

/// The model fields of a month: calls, tokens and the model's own web use.
fn model_fields(m: &Month, out: &mut Map<String, Value>) {
    let calls = m.get(MODEL_CALLS);
    let input = m.get(MODEL_INPUT);
    let output = m.get(MODEL_OUTPUT);
    let read = m.get(MODEL_CACHE_READ);
    let write = m.get(MODEL_CACHE_WRITE);
    let tokens = input + output + write;
    let searches = m.get(MODEL_WEB_SEARCH);
    let fetches = m.get(MODEL_WEB_FETCH);
    let mut detail = vec![
        format!("{} in", short(input)),
        format!("{} out", short(output)),
    ];
    if write > 0 {
        detail.push(format!("{} cache writes", short(write)));
    }
    if read > 0 {
        detail.push(format!("{} cache reads", short(read)));
    }
    let web = match (searches, fetches) {
        (0, 0) => String::new(),
        (s, 0) => format!("{} by the model", plural(s, "web search", "web searches")),
        (0, f) => format!("{} by the model", plural(f, "web fetch", "web fetches")),
        (s, f) => format!(
            "{} and {} by the model",
            plural(s, "web search", "web searches"),
            plural(f, "fetch", "fetches")
        ),
    };
    out.insert("model_calls".into(), json!(calls));
    out.insert("model_tokens".into(), json!(tokens));
    out.insert("model_cache_reads".into(), json!(read));
    out.insert("model_web_searches".into(), json!(searches));
    out.insert("model_web_fetches".into(), json!(fetches));
    out.insert(
        "model_text".into(),
        json!(if calls == 0 {
            "No model requests yet".to_owned()
        } else {
            format!(
                "{} · {} tokens",
                plural(calls, "request", "requests"),
                short(tokens)
            )
        }),
    );
    out.insert(
        "model_detail".into(),
        json!(if calls == 0 {
            String::new()
        } else {
            detail.join(" · ")
        }),
    );
    out.insert("model_web_text".into(), json!(web));
}

fn pod_entity(i: &Input) -> Value {
    let (month_name, period_text) = period(i.pod, i.zone);
    let searches = i.pod.searches();
    let brave = searches.get("brave").copied().unwrap_or(0);
    let cents = brave * BRAVE_CENTS_PER_1000 / 1_000;
    let free_cents = BRAVE_FREE_SEARCHES * BRAVE_CENTS_PER_1000 / 1_000;
    let over = brave > BRAVE_FREE_SEARCHES;
    let left = BRAVE_FREE_SEARCHES.saturating_sub(brave);
    let others: Vec<String> = searches
        .iter()
        .filter(|(p, n)| **p != "brave" && **n > 0)
        .map(|(p, n)| {
            format!(
                "{} {} {} (free)",
                thousands(*n),
                provider_name(p),
                if *n == 1 { "search" } else { "searches" }
            )
        })
        .collect();
    let provider = i.provider.unwrap_or("");
    let mut m = Map::new();
    m.insert("scope".into(), json!("pod"));
    m.insert("scope_text".into(), json!("Pod"));
    m.insert("month".into(), json!(i.pod.month));
    m.insert("month_text".into(), json!(month_name));
    m.insert("period_text".into(), json!(period_text));
    m.insert("search_provider".into(), json!(provider));
    m.insert(
        "search_provider_text".into(),
        json!(if provider.is_empty() {
            "Web search isn't plugged in".to_owned()
        } else {
            format!("Searching with {}", provider_name(provider))
        }),
    );
    m.insert("brave_show".into(), json!(provider == "brave" || brave > 0));
    m.insert("brave_searches".into(), json!(brave));
    m.insert("brave_max".into(), json!(BRAVE_FREE_SEARCHES));
    m.insert("brave_over".into(), json!(over));
    m.insert(
        "brave_text".into(),
        json!(format!(
            "{} of {} free searches",
            thousands(brave),
            thousands(BRAVE_FREE_SEARCHES)
        )),
    );
    m.insert(
        "brave_cost_text".into(),
        json!(if over {
            format!(
                "About {} this month, {} past the free {}",
                dollars(cents),
                dollars(cents - free_cents),
                dollars(free_cents)
            )
        } else {
            format!(
                "About {} of the free {} · {} left",
                dollars(cents),
                dollars(free_cents),
                plural(left, "search", "searches")
            )
        }),
    );
    m.insert("other_search_text".into(), json!(others.join(" · ")));
    model_fields(i.pod, &mut m);
    Value::Object(m)
}

fn server_entity(month: &Month, zone: &TimeZone) -> Value {
    let (month_name, period_text) = period(month, zone);
    let mut m = Map::new();
    m.insert("scope".into(), json!("server"));
    m.insert("scope_text".into(), json!("Server"));
    m.insert("month".into(), json!(month.month));
    m.insert("month_text".into(), json!(month_name));
    m.insert("period_text".into(), json!(period_text));
    model_fields(month, &mut m);
    Value::Object(m)
}

/// The feed's data: the pod's entity, and the server's where it may be read.
pub fn bundle(i: &Input) -> DataBundle {
    let mut b = DataBundle::default();
    let mut order = vec![POD_URI.to_owned()];
    b.entities.insert(POD_URI.into(), record(pod_entity(i)));
    if let Some(s) = i.server {
        b.entities
            .insert(SERVER_URI.into(), record(server_entity(s, i.zone)));
        order.push(SERVER_URI.to_owned());
    }
    b.queries.insert(PAID_USE_QUERY.into(), order);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn month(counts: &[(&str, u64)], since: Option<i64>) -> Month {
        Month {
            month: "2026-11".into(),
            counts: counts
                .iter()
                .map(|(k, n)| ((*k).to_owned(), *n))
                .collect::<BTreeMap<_, _>>(),
            since,
        }
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
        assert_eq!(short(950), "950");
        assert_eq!(short(1_000), "1K");
        assert_eq!(short(12_400), "12K");
        assert_eq!(short(1_250_000), "1.2M");
        assert_eq!(dollars(175), "$1.75");
    }

    #[test]
    fn the_pod_shows_brave_against_its_free_credit_and_the_server_its_model_use() {
        // Nov 26 2026, 16:00 UTC: counting began after the 1st.
        let since = 1_795_708_800;
        let pod = month(
            &[
                ("search:brave", 350),
                ("search:searxng", 2),
                (MODEL_CALLS, 35),
                (MODEL_INPUT, 70_000),
                (MODEL_OUTPUT, 5_000),
                (MODEL_CACHE_READ, 118_000),
                (MODEL_WEB_SEARCH, 9),
            ],
            Some(since),
        );
        let server = month(&[(MODEL_CALLS, 1), (MODEL_INPUT, 10)], Some(1));
        let zone = TimeZone::get("America/Denver").unwrap();
        let b = bundle(&Input {
            pod: &pod,
            provider: Some("brave"),
            server: Some(&server),
            zone: &zone,
        });
        assert_eq!(b.queries[PAID_USE_QUERY], vec![POD_URI, SERVER_URI]);
        let p = &b.entities[POD_URI];
        assert_eq!(p.get_str("scope_text"), Some("Pod"));
        assert_eq!(
            p.get_str("period_text"),
            Some("November, counted since Nov 26")
        );
        assert_eq!(p.get_str("brave_text"), Some("350 of 1,000 free searches"));
        assert_eq!(
            p.get_str("brave_cost_text"),
            Some("About $1.75 of the free $5.00 · 650 searches left")
        );
        assert_eq!(
            p.get_str("other_search_text"),
            Some("2 SearXNG searches (free)")
        );
        assert_eq!(p.get_str("model_text"), Some("35 requests · 75K tokens"));
        assert_eq!(
            p.get_str("model_detail"),
            Some("70K in · 5K out · 118K cache reads")
        );
        assert_eq!(
            p.get_str("model_web_text"),
            Some("9 web searches by the model")
        );
        let s = &b.entities[SERVER_URI];
        assert_eq!(s.get_str("scope_text"), Some("Server"));
        assert_eq!(s.get_str("period_text"), Some("November so far"));
        assert_eq!(s.get_str("model_text"), Some("1 request · 10 tokens"));
    }

    #[test]
    fn past_the_free_credit_and_without_monitoring() {
        let pod = month(&[("search:brave", 1_240)], None);
        let b = bundle(&Input {
            pod: &pod,
            provider: None,
            server: None,
            zone: &TimeZone::UTC,
        });
        assert_eq!(b.queries[PAID_USE_QUERY], vec![POD_URI]);
        let p = &b.entities[POD_URI];
        assert_eq!(
            p.get_str("brave_cost_text"),
            Some("About $6.20 this month, $1.20 past the free $5.00")
        );
        assert_eq!(p.get("brave_over"), Some(&json!(true)));
        assert_eq!(p.get("brave_show"), Some(&json!(true)));
        assert_eq!(
            p.get_str("search_provider_text"),
            Some("Web search isn't plugged in")
        );
        assert_eq!(p.get_str("model_text"), Some("No model requests yet"));
        assert_eq!(p.get_str("period_text"), Some("November so far"));
    }
}
