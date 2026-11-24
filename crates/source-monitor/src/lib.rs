//! The `monitor` data source (decision `server-monitoring`): the server's security and
//! usage counters ([`pod_core::metrics`]) for custom apps, so a pod can show
//! at a glance whether anything looks wrong.
//!
//! The counters are the whole server's (every pod's sign-in failures, model
//! use, pod counts), so the feed answers only in pods the operator turned
//! it on for (`server pods monitoring <name> on`, pod setting
//! [`ENABLED_SETTING`]); elsewhere it is 403 `monitoring_off`.
//!
//! The core layout `server_health` ([`APP`]) is the built-in screen over both
//! feeds; it says so where monitoring is off and still shows the devices.
//!
//! One feed, `overview`, fills three queries:
//! - `monitor:summary`: one entity, the overall status.
//! - `monitor:checks`: plain-language checks, each `ok` or `warn`
//!   ([`checks`]).
//! - `monitor:metrics`: every counter with its last hour and last day, and
//!   the gauges (pods, credentials, disk).
//!
//! A second feed, `errors` (decision `recent-server-errors`, [`errors`]),
//! lists the pod's own recent failed requests (`monitor:recent_errors`);
//! the `server_errors` check names the most frequent of them.
//!
//! Alerts (decision `server-monitoring-alerts`): in those pods, the one
//! layout that shows the source (decision `notification-owner`) gets a
//! "Server warnings" notification channel, and a check that turns to
//! warning sends one notification ([`alerts`]); it can send again only
//! after it has been back to ok.

use std::time::Duration;

pub mod devices;
pub mod errors;

use pod_core::layouts::LayoutSource;
use pod_core::metrics::{self, Gauge, Series};
use pod_core::store::Store;
use pod_core::{
    AuthPolicy, BoxFuture, Catalog, CoreLayoutInfo, EntityDoc, FeedSpec, Hydration,
    NewNotification, Pod, PollCtx, PollOutcome, RequestCtx, Source, SourceManifest,
};
use protocol::{DataBundle, EntityRecord, Importance, NotificationChannel};
use serde_json::{Value, json};

/// The adapter name and the entity scheme.
pub const NAME: &str = "monitor";
/// The feed.
pub const OVERVIEW_FEED: &str = "overview";
pub const SUMMARY_QUERY: &str = "monitor:summary";
pub const CHECKS_QUERY: &str = "monitor:checks";
pub const METRICS_QUERY: &str = "monitor:metrics";
/// Pod setting: `1` lets this pod read the server's monitoring data.
pub const ENABLED_SETTING: &str = "monitor.enabled";
/// Pod setting: the ids of the checks that warned at the last look (a JSON
/// list), so each warning notifies once.
pub const WARNED_SETTING: &str = "monitor.warned";
/// The notification channel of the layout that shows the source.
pub const ALERT_CHANNEL: &str = "server_warnings";
/// How often the checks are looked at for alerts.
pub const ALERT_EVERY: Duration = Duration::from_secs(300);

/// The core layout's id (`@core/server_health`).
pub const APP_ID: &str = "server_health";

/// Server health, built in: the status, the checks and every counter where
/// the operator turned monitoring on, and the user's devices everywhere
/// (`layouts/server_health/`).
pub const APP: CoreLayoutInfo = CoreLayoutInfo {
    id: APP_ID,
    title: "Server health",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[
        (NAME, OVERVIEW_FEED),
        (NAME, devices::DEVICES_FEED),
        (NAME, errors::ERRORS_FEED),
    ],
};

// `FILES`: every `layouts/*.json` of this crate, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Server monitoring",
    sign_in: None,
    entity_schemes: &[NAME],
    feeds: &[
        FeedSpec {
            name: OVERVIEW_FEED,
            query_keys: &[SUMMARY_QUERY, CHECKS_QUERY, METRICS_QUERY],
            doc: "The server's health at a glance: an overall status, checks that are ok or \
                  warn (sign-in failures, new Claude connections, model use, errors, disk) and \
                  every counter with its last hour and last day. Only in pods the operator \
                  turned monitoring on for; elsewhere the feed answers 403 monitoring_off.",
        },
        FeedSpec {
            name: devices::DEVICES_FEED,
            query_keys: &[
                devices::DEVICE_SUMMARY_QUERY,
                devices::DEVICES_QUERY,
                devices::DEVICE_DAYS_QUERY,
                devices::BUILDS_QUERY,
            ],
            doc: "The user's devices (phone, desktop app, each browser) and the build each \
                  runs against the server's (latest, behind, ahead), and devices per day by \
                  build for the last 14 days (a core://Bars chart, today's bar every device \
                  at its latest build). The pod's own data: no operator switch.",
        },
        FeedSpec {
            name: errors::ERRORS_FEED,
            query_keys: &[errors::RECENT_ERRORS_QUERY],
            doc: "The pod's recent failed requests (the last 50 answers of 400 or more, \
                  except 401 and 404), newest first: when, the route's pattern, the status, \
                  the error code and the device. Kept in memory since the server started. \
                  The pod's own data: no operator switch.",
        },
    ],
    data_paths: &[],
    actions: &[],
    mcp_actions: &[],
    entities: &[
        EntityDoc {
            uri: "monitor://summary/server",
            doc: "The overall status (monitor:summary).",
            fields: &[
                ("status", "ok or warn"),
                ("ok", "true when every check is ok"),
                ("headline", "e.g. All clear, or 2 checks need a look"),
                ("warn_count", "checks that warn"),
                ("color", "status colour, #RRGGBB"),
                ("uptime_text", "how long the server has run, e.g. 3 h"),
                (
                    "window_note",
                    "set while the server has run less than a day: counts are since it started",
                ),
                ("checked_at", "when this was measured (Unix seconds)"),
            ],
        },
        EntityDoc {
            uri: "monitor://checks/{id}",
            doc: "One check (monitor:checks), worst first.",
            fields: &[
                ("id", "e.g. sign_in_failures"),
                ("title", "what it checks"),
                ("status", "ok or warn"),
                ("ok", "true when ok"),
                ("status_text", "OK or Warning"),
                ("detail", "one line with the numbers"),
                ("value", "the number it judges"),
                ("color", "status colour, #RRGGBB"),
            ],
        },
        EntityDoc {
            uri: "monitor://metrics/{id}",
            doc: "A counter or gauge (monitor:metrics).",
            fields: &[
                ("id", "name and label values"),
                ("name", "the Prometheus name"),
                ("title", "readable name and labels"),
                ("kind", "counter or gauge"),
                ("labels", "e.g. kind=pod_code"),
                (
                    "total",
                    "counters: since the server started; gauges: the value now",
                ),
                ("last_hour", "counters only"),
                ("last_day", "counters only"),
            ],
        },
        EntityDoc {
            uri: "monitor://device_summary/pod",
            doc: "The devices at a glance (monitor:device_summary).",
            fields: &[
                ("today", "today's bar key (YYYY-MM-DD, the pod's time zone)"),
                ("server_build", "the server's build id"),
                ("server_build_label", "e.g. Nov 18 · c41d9e2"),
                ("count_text", "e.g. 5 devices"),
                (
                    "status_text",
                    "e.g. 1 on the server's build · 1 ahead · 3 behind",
                ),
                ("empty", "true before any device reached the pod"),
            ],
        },
        EntityDoc {
            uri: "monitor://devices/{id}",
            doc: "One device (monitor:devices), the most recently seen first.",
            fields: &[
                (
                    "name",
                    "the user's name for it, else its own (Pixel 9, Chrome on Windows)",
                ),
                (
                    "kind",
                    "android, ios, macos, windows, linux, web or unknown",
                ),
                ("kind_text", "e.g. Android, Mac app, Browser"),
                ("build", "its build id"),
                ("build_label", "e.g. Nov 18 · c41d9e2, or Version unknown"),
                ("color", "its build's colour in the chart, #RRGGBB"),
                ("relation", "latest, behind, ahead or unknown"),
                (
                    "relation_text",
                    "Latest, 1 behind, Ahead of server, Version unknown",
                ),
                ("relation_color", "the badge's text colour, #RRGGBB"),
                ("relation_bg", "the badge's background, #RRGGBB"),
                ("seen_text", "e.g. seen 2 min ago"),
                (
                    "subtitle",
                    "e.g. Android · Nov 18 · c41d9e2 · seen 2 min ago",
                ),
            ],
        },
        EntityDoc {
            uri: "monitor://device_days/{day}/{id}",
            doc: "One device on one day of the last 14 (monitor:device_days), for core://Bars with bar: \"bar\" and color: \"$item.color\": today's bar holds every device at its latest build, hatched when not seen today.",
            fields: &[
                ("bar", "the day, YYYY-MM-DD"),
                ("bar_label", "the day of the month, or Now"),
                ("day_text", "e.g. Thu Nov 19, or Now"),
                ("device", "the device's name"),
                ("device_id", "its id"),
                (
                    "kind",
                    "its kind (filter the chart with where kind equals ...)",
                ),
                ("build_label", "the build it ran that day"),
                ("color", "that build's colour, #RRGGBB"),
                ("hatched", "true on today's bar for a device not seen today"),
                ("note", "e.g. seen 3 days ago, on today's bar"),
                (
                    "detail",
                    "the build and the note, e.g. Nov 14 · 9be03a1 · seen 3 days ago",
                ),
            ],
        },
        EntityDoc {
            uri: "monitor://errors/{n}",
            doc: "One failed request of this pod (monitor:recent_errors), newest first.",
            fields: &[
                ("time", "when it was answered (Unix seconds)"),
                ("time_text", "e.g. 2 min ago"),
                ("method", "e.g. POST"),
                (
                    "route",
                    "the route's pattern, e.g. /v1/taste/react (never the path asked for)",
                ),
                ("status", "the answer's status, e.g. 502"),
                (
                    "code",
                    "the answer's error code, e.g. taste_unreachable, or empty",
                ),
                (
                    "device",
                    "the device's name (or a short id), empty when none was named",
                ),
                ("device_id", "the device's id, or empty"),
                ("server_error", "true for a 5xx"),
                ("title", "e.g. POST /v1/taste/react → 502 taste_unreachable"),
                ("subtitle", "e.g. 2 min ago · Pixel 10 Pro"),
            ],
        },
        EntityDoc {
            uri: "monitor://builds/{n}",
            doc: "The builds the chart shows, oldest first (monitor:builds), for its legend.",
            fields: &[
                ("build", "the build id"),
                ("label", "e.g. Nov 18 · c41d9e2"),
                ("note", "server, ahead or empty"),
                ("legend", "the label with its note"),
                ("color", "its colour, #RRGGBB"),
            ],
        },
    ],
    dev: false,
};

const OK_COLOR: &str = "#2E7D32";
const WARN_COLOR: &str = "#B26A00";

/// Thresholds (`CONSTRUCT_MONITOR_*`, docs/server.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Wrong pod codes and unknown tokens per hour before a warning.
    pub auth_failures_per_hour: u64,
    /// Model tokens per day before a warning.
    pub llm_tokens_per_day: u64,
    /// 5xx responses per hour before a warning.
    pub server_errors_per_hour: u64,
    /// Failed or refused model calls per day before a warning.
    pub llm_failures_per_day: u64,
    /// Live temporary pods, as a percentage of the most allowed, at which
    /// the check warns (80: at 40 of 50), so there is time to clean up
    /// before new ones are refused.
    pub temporary_pods_percent: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            auth_failures_per_hour: 20,
            llm_tokens_per_day: 1_000_000,
            server_errors_per_hour: 10,
            llm_failures_per_day: 5,
            temporary_pods_percent: 80,
        }
    }
}

impl MonitorConfig {
    /// From `CONSTRUCT_MONITOR_AUTH_FAILURES_PER_HOUR`,
    /// `CONSTRUCT_MONITOR_LLM_TOKENS_PER_DAY` and
    /// `CONSTRUCT_MONITOR_TEMP_PODS_PERCENT` (1..=100; defaults otherwise).
    pub fn from_env() -> Self {
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse().ok());
        let d = Self::default();
        Self {
            auth_failures_per_hour: num("CONSTRUCT_MONITOR_AUTH_FAILURES_PER_HOUR")
                .unwrap_or(d.auth_failures_per_hour),
            llm_tokens_per_day: num("CONSTRUCT_MONITOR_LLM_TOKENS_PER_DAY")
                .unwrap_or(d.llm_tokens_per_day),
            temporary_pods_percent: num("CONSTRUCT_MONITOR_TEMP_PODS_PERCENT")
                .filter(|p| (1..=100).contains(p))
                .unwrap_or(d.temporary_pods_percent),
            ..d
        }
    }
}

/// The monitor adapter: a data source and its one core layout (no tables or routes).
#[derive(Debug, Default)]
pub struct MonitorAdapter {
    config: MonitorConfig,
}

impl MonitorAdapter {
    pub fn new(config: MonitorConfig) -> Self {
        Self { config }
    }
}

impl Source for MonitorAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn element_catalog(&self) -> &'static str {
        include_str!("../catalog.json")
    }

    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if feed == devices::DEVICES_FEED {
                return devices_feed(pod).await;
            }
            if feed == errors::ERRORS_FEED {
                return errors_feed(pod).await;
            }
            if feed != OVERVIEW_FEED {
                return pod_core::unknown_feed(NAME, feed);
            }
            let p = pod.clone();
            let now = pod_core::util::now_secs();
            let read = pod
                .blocking(move |s| {
                    let on = s.pod_setting(ENABLED_SETTING)?.as_deref() == Some("1");
                    if !on {
                        return Ok(None);
                    }
                    let note = pod_errors_note(&p, s, now)?;
                    // The gauges read host.db: off the async runtime too.
                    Ok(Some((metrics::series(), metrics::gauges(), note)))
                })
                .await;
            match read {
                Ok(Some((series, gauges, note))) => Hydration::Data(bundle(
                    &series,
                    &gauges,
                    &self.config,
                    now,
                    metrics::started_at(),
                    note.as_deref(),
                )),
                Ok(None) => Hydration::Error(pod_core::json_error(
                    axum::http::StatusCode::FORBIDDEN,
                    json!({
                        "error": "monitoring_off",
                        "detail": "this pod can't read the server's monitoring data; the \
                                   operator turns it on with `server pods monitoring <pod> on`",
                    }),
                )),
                Err(e) => Hydration::Error(axum::response::IntoResponse::into_response(e)),
            }
        })
    }

    fn notifies_layouts(&self) -> bool {
        true
    }

    fn layout_notification_channels(
        &self,
        store: &Store,
        catalog: &Catalog,
    ) -> Vec<NotificationChannel> {
        let mut out: Vec<NotificationChannel> = match monitored_app(store, catalog) {
            Ok(app) => app.iter().map(|app| channel(app)).collect(),
            Err(e) => {
                tracing::error!(error = %e, "monitoring alerts: apps could not be read");
                Vec::new()
            }
        };
        match pod_core::layouts::feed_owners(store, catalog, NAME) {
            Ok(mut owners) => {
                if let Some(app) = owners.remove(devices::DEVICES_FEED) {
                    out.push(device_channel(&app));
                }
            }
            Err(e) => tracing::error!(error = %e, "device updates: apps could not be read"),
        }
        out
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(ALERT_EVERY)
    }

    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        _ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        Box::pin(async move {
            let config = self.config;
            let p = pod.clone();
            let found = pod
                .blocking(move |s| {
                    let apps: Vec<String> = monitored_app(s, p.catalog())?.into_iter().collect();
                    if apps.is_empty() {
                        return Ok(Vec::new());
                    }
                    let warned: Vec<String> = s
                        .pod_setting(WARNED_SETTING)?
                        .and_then(|v| serde_json::from_str(&v).ok())
                        .unwrap_or_default();
                    let now = pod_core::util::now_secs();
                    let mut now_checks = checks(&metrics::series(), &metrics::gauges(), &config);
                    note_errors(&mut now_checks, pod_errors_note(&p, s, now)?.as_deref());
                    let (new, still) = alerts(&now_checks, &warned, &apps, now);
                    if still != warned {
                        s.set_pod_setting(WARNED_SETTING, &json!(still).to_string())?;
                    }
                    Ok(new)
                })
                .await;
            match found {
                Ok(new) => PollOutcome {
                    new,
                    ..Default::default()
                },
                Err(e) => {
                    tracing::error!(status = ?e.0, "monitoring alerts could not be checked");
                    PollOutcome::default()
                }
            }
        })
    }
}

// ---------- alerts ----------

/// The layout that gets the "Server warnings" channel, when the pod may
/// read the source: the one layout the `overview` feed belongs to
/// ([`pod_core::layouts::feed_owners`], decision `notification-owner`), or
/// none. Every layout has home's feeds (decision `layouts-not-apps`), so
/// "has the feed" would be every layout. Blocking.
pub fn monitored_app(store: &Store, catalog: &Catalog) -> rusqlite::Result<Option<String>> {
    if store.pod_setting(ENABLED_SETTING)?.as_deref() != Some("1") {
        return Ok(None);
    }
    Ok(pod_core::layouts::feed_owners(store, catalog, NAME)?.remove(OVERVIEW_FEED))
}

fn channel(app: &str) -> NotificationChannel {
    NotificationChannel {
        layout: app.to_owned(),
        id: ALERT_CHANNEL.into(),
        title: "Server warnings".into(),
        description: "When a check on the server turns to a warning (wrong pod codes, new \
                      Claude connections, model use, errors, disk)"
            .into(),
        default_enabled: true,
        importance: Importance::High,
    }
}

/// The channel agents' device watches notify on (decision `device-versions`).
fn device_channel(app: &str) -> NotificationChannel {
    NotificationChannel {
        layout: app.to_owned(),
        id: pod_core::devices::DEVICE_CHANNEL.into(),
        title: "Device updates".into(),
        description: "When a device an agent is waiting on updates (watch_device)".into(),
        default_enabled: true,
        importance: Importance::Default,
    }
}

/// The `devices` feed: the pod's own rows, for any pod.
async fn devices_feed(pod: &Pod) -> Hydration {
    let now = pod_core::util::now_secs();
    let server = protocol::build();
    let read = pod
        .blocking(move |s| {
            pod_core::devices::note_server_build(s, server, now)?;
            let all = pod_core::devices::devices(s, server)?;
            let keys = (0..devices::CHART_DAYS)
                .rev()
                .map(|ago| pod_core::devices::day_before(s, now, ago))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let days = pod_core::devices::days(s, keys.first().map_or("", String::as_str))?;
            let zone = pod_core::tz::time_zone(s)?;
            let releases = pod_core::devices::server_build_times(s)?;
            Ok(devices::bundle(
                &all, &days, &keys, &releases, &zone, server, now,
            ))
        })
        .await;
    match read {
        Ok(b) => Hydration::Data(b),
        Err(e) => Hydration::Error(axum::response::IntoResponse::into_response(e)),
    }
}

/// The pod's device names by id. Blocking.
fn device_names(store: &Store) -> rusqlite::Result<std::collections::HashMap<String, String>> {
    Ok(pod_core::devices::devices(store, protocol::build())?
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect())
}

/// The `errors` feed: the pod's own failed requests, for any pod.
async fn errors_feed(pod: &Pod) -> Hydration {
    let recent = pod_core::recent_errors::recent(pod);
    let now = pod_core::util::now_secs();
    match pod.blocking(device_names).await {
        Ok(names) => Hydration::Data(errors::bundle(&recent, &names, now)),
        Err(e) => Hydration::Error(axum::response::IntoResponse::into_response(e)),
    }
}

/// The `server_errors` check's line about `pod`'s own 5xx answers in the
/// last hour ([`errors::server_errors_note`]). Blocking (device names).
fn pod_errors_note(pod: &Pod, store: &Store, now: i64) -> rusqlite::Result<Option<String>> {
    let recent = pod_core::recent_errors::recent(pod);
    if !recent.iter().any(|e| e.status >= 500) {
        return Ok(None);
    }
    let names = device_names(store)?;
    Ok(errors::server_errors_note(
        &recent,
        &names,
        now - metrics::HOUR,
    ))
}

/// Add `note` (this pod's failures) to the `server_errors` check's detail.
pub fn note_errors(checks: &mut [Check], note: Option<&str>) {
    let Some(note) = note else { return };
    if let Some(c) = checks.iter_mut().find(|c| c.id == "server_errors") {
        c.detail = format!("{} · {note}", c.detail);
    }
}

/// What the checks owe at `now`: a notification for each app per check
/// that warns now and didn't at the last look (`warned`), and the ids
/// that warn now (the next look's `warned`).
pub fn alerts(
    checks: &[Check],
    warned: &[String],
    apps: &[String],
    now: i64,
) -> (Vec<NewNotification>, Vec<String>) {
    let mut out = Vec::new();
    let mut still = Vec::new();
    for c in checks.iter().filter(|c| c.warn) {
        still.push(c.id.to_owned());
        if warned.iter().any(|w| w == c.id) {
            continue;
        }
        for app in apps {
            out.push(NewNotification {
                layout: app.clone(),
                channel: ALERT_CHANNEL.into(),
                // One per warning episode: it started at `now`.
                dedupe_key: format!("monitor:{}:{now}", c.id),
                title: format!("Server: {}", c.title),
                body: c.detail.clone(),
                image_url: None,
                target_uri: None,
                group_key: Some("monitor".into()),
                created_at: now,
                subject: None,
                quiet: false,
            });
        }
    }
    still.sort();
    (out, still)
}

// ---------- checks ----------

/// One check of [`checks`].
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub warn: bool,
    pub detail: String,
    pub value: f64,
}

/// The sum of counter `name`'s series whose label `label` is one of
/// `values` (any, when empty): `(last hour, last day)`.
fn sum(series: &[Series], name: &str, label: &str, values: &[&str]) -> (u64, u64) {
    series
        .iter()
        .filter(|s| s.name == name)
        .filter(|s| values.is_empty() || s.label(label).is_some_and(|v| values.contains(&v)))
        .fold((0, 0), |(h, d), s| (h + s.last_hour, d + s.last_day))
}

fn gauge(gauges: &[Gauge], name: &str, label: Option<(&str, &str)>) -> Option<f64> {
    let g = gauges.iter().find(|g| g.name == name)?;
    g.series
        .iter()
        .find(|(l, _)| match label {
            None => l.is_empty(),
            Some((k, v)) => l.iter().any(|(lk, lv)| lk == k && lv == v),
        })
        .map(|(_, v)| *v)
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn mib(bytes: f64) -> String {
    format!("{:.0} MiB", bytes / (1024.0 * 1024.0))
}

/// The checks, worst first (warnings, then the rest, each in a fixed
/// order).
pub fn checks(series: &[Series], gauges: &[Gauge], cfg: &MonitorConfig) -> Vec<Check> {
    use metrics::{
        AUTH_FAILURES, CREDENTIALS_ISSUED, HTTP_RESPONSES, LLM_REQUESTS, LLM_TOKENS, LLM_WEB_TOOLS,
        RATE_LIMITED, TEMPORARY_PODS,
    };
    let mut out = Vec::new();

    let (h, d) = sum(
        series,
        AUTH_FAILURES,
        "kind",
        &["pod_code", "oauth_pod_code", "unknown_token"],
    );
    out.push(Check {
        id: "sign_in_failures",
        title: "Wrong pod codes and unknown tokens",
        warn: h > cfg.auth_failures_per_hour,
        detail: format!("{h} in the last hour, {d} in the last day"),
        value: h as f64,
    });

    let (h, d) = sum(series, RATE_LIMITED, "limit", &[]);
    out.push(Check {
        id: "rate_limits",
        title: "Rate limits hit",
        warn: h > 0,
        detail: format!("{h} in the last hour, {d} in the last day (someone retrying fast)"),
        value: h as f64,
    });

    let (_, d) = sum(
        series,
        AUTH_FAILURES,
        "kind",
        &["mcp_scope", "open_pod_host"],
    );
    out.push(Check {
        id: "misused_credentials",
        title: "Credentials used where they don't belong",
        warn: d > 0,
        detail: format!(
            "{} in the last day (an MCP credential outside /v1/mcp, or the open pod \
             from another host)",
            d
        ),
        value: d as f64,
    });

    let (_, d) = sum(
        series,
        CREDENTIALS_ISSUED,
        "kind",
        &["oauth_connect", "mcp"],
    );
    let held = gauge(gauges, "construct_mcp_credentials", None).unwrap_or(0.0);
    out.push(Check {
        id: "new_connections",
        title: "New Claude connections",
        warn: d > 0,
        detail: format!(
            "{} in the last day; {} held in all{}",
            d,
            held,
            if d > 0 { ". Was it you?" } else { "" }
        ),
        value: d as f64,
    });

    let (h5, d5) = series
        .iter()
        .filter(|s| {
            s.name == HTTP_RESPONSES && s.label("status").is_some_and(|c| c.starts_with('5'))
        })
        .fold((0, 0), |(h, d), s| (h + s.last_hour, d + s.last_day));
    out.push(Check {
        id: "server_errors",
        title: "Server errors",
        warn: h5 > cfg.server_errors_per_hour,
        detail: format!("{h5} in the last hour, {d5} in the last day"),
        value: h5 as f64,
    });

    // Cache reads cost a twentieth of other tokens, so they are left out
    // of the warning (and the split) and only mentioned.
    let cached = |s: &Series| s.label("direction") == Some("cache_read");
    let (tokens, cache_reads) =
        series
            .iter()
            .filter(|s| s.name == LLM_TOKENS)
            .fold((0, 0), |(t, c), s| {
                if cached(s) {
                    (t, c + s.last_day)
                } else {
                    (t + s.last_day, c)
                }
            });
    let by_feature: std::collections::BTreeMap<&str, u64> = series
        .iter()
        .filter(|s| s.name == LLM_TOKENS && s.last_day > 0 && !cached(s))
        .fold(Default::default(), |mut m, s| {
            *m.entry(s.label("feature").unwrap_or("?")).or_default() += s.last_day;
            m
        });
    let (_, calls) = sum(series, LLM_REQUESTS, "outcome", &[]);
    let (_, web) = sum(series, LLM_WEB_TOOLS, "tool", &[]);
    let split: Vec<String> = by_feature.iter().map(|(f, n)| format!("{f} {n}")).collect();
    out.push(Check {
        id: "model_use",
        title: "Model use",
        warn: tokens > cfg.llm_tokens_per_day,
        detail: format!(
            "{}{} over {}{} in the last day{}",
            plural(tokens, "token", "tokens"),
            if cache_reads > 0 {
                format!(" (and {cache_reads} read from the cache)")
            } else {
                String::new()
            },
            plural(calls, "call", "calls"),
            if web > 0 {
                format!(
                    " and {}",
                    plural(web, "web search or fetch", "web searches and fetches")
                )
            } else {
                String::new()
            },
            if split.is_empty() {
                String::new()
            } else {
                format!(" ({})", split.join(", "))
            }
        ),
        value: tokens as f64,
    });

    let (_, failed) = sum(series, LLM_REQUESTS, "outcome", &["error"]);
    let (_, refused) = sum(series, LLM_REQUESTS, "outcome", &["refused"]);
    out.push(Check {
        id: "model_failures",
        title: "Model errors and refusals",
        warn: failed + refused > cfg.llm_failures_per_day,
        detail: format!(
            "{failed} failed and {refused} refused in the last day (a refusal can mean a \
             page tried to steer the model)"
        ),
        value: (failed + refused) as f64,
    });

    let live = gauge(gauges, "construct_pods", Some(("kind", "temporary")));
    let max = gauge(gauges, "construct_temporary_pods_max", None);
    let (_, refused_pods) = sum(series, TEMPORARY_PODS, "outcome", &["full", "low_disk"]);
    let (_, made) = sum(series, TEMPORARY_PODS, "outcome", &["created"]);
    // Warn before the cap: at `temporary_pods_percent` of it, rounded up.
    let near = |m: f64| {
        (m * cfg.temporary_pods_percent as f64 / 100.0)
            .ceil()
            .max(1.0)
    };
    out.push(Check {
        id: "temporary_pods",
        title: "Temporary pods",
        warn: refused_pods > 0 || matches!((live, max), (Some(l), Some(m)) if l >= near(m)),
        detail: match (live, max) {
            (Some(l), Some(m)) => {
                format!(
                    "{l} of {m} in use (warns at {}); {made} made and {refused_pods} refused \
                     in the last day",
                    near(m)
                )
            }
            _ => "turned off on this server".to_owned(),
        },
        value: live.unwrap_or(0.0),
    });

    let free = gauge(gauges, "construct_disk_free_bytes", None);
    let floor = gauge(gauges, "construct_disk_min_free_bytes", None).unwrap_or(0.0);
    out.push(Check {
        id: "disk",
        title: "Disk space",
        warn: free.is_some_and(|f| f < 2.0 * floor),
        detail: match free {
            Some(f) => format!("{} free (uploads stop under {})", mib(f), mib(floor)),
            None => "unknown".to_owned(),
        },
        value: free.unwrap_or(0.0),
    });

    out.sort_by_key(|c| !c.warn);
    out
}

// ---------- the bundle ----------

fn duration_text(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 2 * metrics::DAY => format!("{} h", s / 3600),
        s => format!("{} days", s / metrics::DAY),
    }
}

pub(crate) fn record(v: Value) -> EntityRecord {
    match v {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord::new(),
    }
}

/// A readable metric title: `construct_auth_failures_total` with
/// `kind=pod_code` is `auth failures (pod_code)`.
fn metric_title(name: &str, labels: &[(String, String)]) -> String {
    let base = name
        .trim_start_matches("construct_")
        .trim_end_matches("_total")
        .replace('_', " ");
    if labels.is_empty() {
        base
    } else {
        let vals: Vec<&str> = labels.iter().map(|(_, v)| v.as_str()).collect();
        format!("{base} ({})", vals.join(", "))
    }
}

fn metric_id(name: &str, labels: &[(String, String)]) -> String {
    let mut id = name
        .trim_start_matches("construct_")
        .trim_end_matches("_total")
        .to_owned();
    for (_, v) in labels {
        id.push('-');
        id.extend(v.chars().map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        }));
    }
    id
}

/// The feed's data at `now`, for a server that started counting at
/// `started`; `errors_note` is this pod's own failures
/// ([`errors::server_errors_note`]).
pub fn bundle(
    series: &[Series],
    gauges: &[Gauge],
    cfg: &MonitorConfig,
    now: i64,
    started: i64,
    errors_note: Option<&str>,
) -> DataBundle {
    let mut b = DataBundle::default();
    let mut checks = checks(series, gauges, cfg);
    note_errors(&mut checks, errors_note);
    let warn_count = checks.iter().filter(|c| c.warn).count();

    let uptime = now - started;
    let summary_uri = "monitor://summary/server".to_owned();
    b.entities.insert(
        summary_uri.clone(),
        record(json!({
            "status": if warn_count == 0 { "ok" } else { "warn" },
            "ok": warn_count == 0,
            "headline": match warn_count {
                0 => "All clear".to_owned(),
                1 => "1 check needs a look".to_owned(),
                n => format!("{n} checks need a look"),
            },
            "warn_count": warn_count,
            "color": if warn_count == 0 { OK_COLOR } else { WARN_COLOR },
            "uptime_text": duration_text(uptime),
            "window_note": if uptime < metrics::DAY {
                format!("Counts are since the server started {} ago.", duration_text(uptime))
            } else {
                String::new()
            },
            "checked_at": now,
        })),
    );
    b.queries.insert(SUMMARY_QUERY.into(), vec![summary_uri]);

    let mut order = Vec::new();
    for c in &checks {
        let uri = format!("monitor://checks/{}", c.id);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "id": c.id,
                "title": c.title,
                "status": if c.warn { "warn" } else { "ok" },
                "ok": !c.warn,
                "status_text": if c.warn { "Warning" } else { "OK" },
                "detail": c.detail,
                "value": c.value,
                "color": if c.warn { WARN_COLOR } else { OK_COLOR },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(CHECKS_QUERY.into(), order);

    let mut order = Vec::new();
    for s in series {
        let id = metric_id(s.name, &s.labels);
        let uri = format!("monitor://metrics/{id}");
        let labels: Vec<String> = s.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        b.entities.insert(
            uri.clone(),
            record(json!({
                "id": id,
                "name": s.name,
                "title": metric_title(s.name, &s.labels),
                "kind": "counter",
                "labels": labels.join(", "),
                "total": s.total,
                "last_hour": s.last_hour,
                "last_day": s.last_day,
            })),
        );
        order.push(uri);
    }
    for g in gauges {
        for (labels, value) in &g.series {
            let id = metric_id(g.name, labels);
            let uri = format!("monitor://metrics/{id}");
            let text: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
            b.entities.insert(
                uri.clone(),
                record(json!({
                    "id": id,
                    "name": g.name,
                    "title": metric_title(g.name, labels),
                    "kind": "gauge",
                    "labels": text.join(", "),
                    "total": value,
                })),
            );
            order.push(uri);
        }
    }
    b.queries.insert(METRICS_QUERY.into(), order);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::{
        AUTH_FAILURES, CREDENTIALS_ISSUED, DAY, HTTP_RESPONSES, LLM_REQUESTS, LLM_TOKENS,
        LLM_WEB_TOOLS, RATE_LIMITED, TEMPORARY_PODS,
    };

    fn s(name: &'static str, label: (&str, &str), hour: u64, day: u64) -> Series {
        Series {
            name,
            labels: vec![(label.0.to_owned(), label.1.to_owned())],
            total: day,
            last_hour: hour,
            last_day: day,
        }
    }

    fn quiet_gauges() -> Vec<Gauge> {
        vec![
            Gauge {
                name: "construct_pods",
                help: "",
                series: vec![
                    (vec![("kind".into(), "admin".into())], 1.0),
                    (vec![("kind".into(), "temporary".into())], 3.0),
                ],
            },
            Gauge::one("construct_mcp_credentials", "", 1.0),
            Gauge::one("construct_temporary_pods_max", "", 50.0),
            Gauge::one("construct_disk_free_bytes", "", 800.0 * 1024.0 * 1024.0),
            Gauge::one("construct_disk_min_free_bytes", "", 100.0 * 1024.0 * 1024.0),
        ]
    }

    fn warned(series: &[Series], gauges: &[Gauge]) -> Vec<&'static str> {
        checks(series, gauges, &MonitorConfig::default())
            .into_iter()
            .filter(|c| c.warn)
            .map(|c| c.id)
            .collect()
    }

    #[test]
    fn temporary_pods_warn_before_the_cap() {
        let with_live = |n: f64| {
            let mut g = quiet_gauges();
            g[0].series[1].1 = n;
            g
        };
        assert!(warned(&[], &with_live(39.0)).is_empty());
        assert_eq!(warned(&[], &with_live(40.0)), ["temporary_pods"]);
        let c = checks(&[], &with_live(40.0), &MonitorConfig::default());
        let t = c.iter().find(|c| c.id == "temporary_pods").unwrap();
        assert_eq!(
            t.detail,
            "40 of 50 in use (warns at 40); 0 made and 0 refused in the last day"
        );
        // Configurable: at 100 it warns only when full.
        let full_only = MonitorConfig {
            temporary_pods_percent: 100,
            ..MonitorConfig::default()
        };
        let c = checks(&[], &with_live(49.0), &full_only);
        assert!(!c.iter().any(|c| c.id == "temporary_pods" && c.warn));
    }

    #[test]
    fn a_quiet_server_is_all_clear() {
        let series = vec![
            s(HTTP_RESPONSES, ("status", "200"), 500, 9000),
            s(AUTH_FAILURES, ("kind", "pod_code"), 2, 5),
            s(LLM_TOKENS, ("feature", "fill_page"), 1000, 20_000),
            s(CREDENTIALS_ISSUED, ("kind", "oauth_client"), 1, 1),
        ];
        assert!(warned(&series, &quiet_gauges()).is_empty());
        let b = bundle(
            &series,
            &quiet_gauges(),
            &MonitorConfig::default(),
            10_000,
            0,
            None,
        );
        let summary = &b.entities["monitor://summary/server"];
        assert_eq!(summary.get_str("headline"), Some("All clear"));
        assert_eq!(
            summary.get_str("window_note"),
            Some("Counts are since the server started 2 h ago.")
        );
        assert_eq!(b.queries[CHECKS_QUERY].len(), 9);
        let metric = &b.entities["monitor://metrics/auth_failures-pod_code"];
        assert_eq!(metric.get_str("title"), Some("auth failures (pod_code)"));
        assert_eq!(metric.get("last_hour"), Some(&json!(2)));
        let gauge = &b.entities["monitor://metrics/pods-temporary"];
        assert_eq!(gauge.get("total"), Some(&json!(3.0)));
    }

    #[test]
    fn suspicious_activity_warns_worst_first() {
        let series = vec![
            s(AUTH_FAILURES, ("kind", "unknown_token"), 30, 30),
            s(RATE_LIMITED, ("limit", "pod_code"), 1, 1),
            s(AUTH_FAILURES, ("kind", "mcp_scope"), 0, 1),
            s(CREDENTIALS_ISSUED, ("kind", "mcp"), 0, 1),
            s(HTTP_RESPONSES, ("status", "502"), 11, 11),
            s(LLM_TOKENS, ("feature", "collections"), 0, 2_000_000),
            s(LLM_REQUESTS, ("outcome", "refused"), 0, 6),
            s(LLM_WEB_TOOLS, ("tool", "web_search"), 0, 3),
            s(TEMPORARY_PODS, ("outcome", "full"), 0, 1),
        ];
        let mut g = quiet_gauges();
        g[3] = Gauge::one("construct_disk_free_bytes", "", 150.0 * 1024.0 * 1024.0);
        assert_eq!(
            warned(&series, &g),
            [
                "sign_in_failures",
                "rate_limits",
                "misused_credentials",
                "new_connections",
                "server_errors",
                "model_use",
                "model_failures",
                "temporary_pods",
                "disk",
            ]
        );
        let c = checks(&series, &g, &MonitorConfig::default());
        let model = c.iter().find(|c| c.id == "model_use").unwrap();
        assert_eq!(
            model.detail,
            "2000000 tokens over 6 calls and 3 web searches and fetches in the last day \
             (collections 2000000)"
        );
        let b = bundle(&series, &g, &MonitorConfig::default(), 2 * DAY, 0, None);
        let summary = &b.entities["monitor://summary/server"];
        assert_eq!(summary.get_str("headline"), Some("9 checks need a look"));
        assert_eq!(summary.get_str("window_note"), Some(""));
    }

    #[test]
    fn cache_reads_do_not_count_toward_the_token_warning() {
        let tok = |direction: &str, day: u64| Series {
            name: LLM_TOKENS,
            labels: vec![
                ("direction".to_owned(), direction.to_owned()),
                ("feature".to_owned(), "fill_agent".to_owned()),
            ],
            total: day,
            last_hour: 0,
            last_day: day,
        };
        let series = vec![
            tok("input", 300_000),
            tok("cache_write", 100_000),
            tok("cache_read", 5_000_000),
            tok("output", 20_000),
        ];
        assert!(warned(&series, &quiet_gauges()).is_empty());
        let c = checks(&series, &quiet_gauges(), &MonitorConfig::default());
        let model = c.iter().find(|c| c.id == "model_use").unwrap();
        assert_eq!(
            model.detail,
            "420000 tokens (and 5000000 read from the cache) over 0 calls in the last day \
             (fill_agent 420000)"
        );
        // Uncached tokens over the limit still warn.
        let mut heavy = series.clone();
        heavy.push(tok("input", 700_000));
        assert_eq!(warned(&heavy, &quiet_gauges()), ["model_use"]);
    }

    #[test]
    fn a_warning_notifies_once_until_it_clears() {
        let apps = vec!["c-1".to_owned()];
        let calm = vec![s(AUTH_FAILURES, ("kind", "pod_code"), 0, 0)];
        let noisy = vec![s(AUTH_FAILURES, ("kind", "pod_code"), 50, 50)];
        let cfg = MonitorConfig::default();
        let g = quiet_gauges();
        let (sent, warned) = alerts(&checks(&noisy, &g, &cfg), &[], &apps, 100);
        assert_eq!(warned, ["sign_in_failures"]);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].title, "Server: Wrong pod codes and unknown tokens");
        assert_eq!(sent[0].channel, ALERT_CHANNEL);
        assert_eq!(sent[0].dedupe_key, "monitor:sign_in_failures:100");
        // Still warning: nothing new.
        let (sent, warned) = alerts(&checks(&noisy, &g, &cfg), &warned, &apps, 400);
        assert!(sent.is_empty());
        // Clears, then warns again: a new notification.
        let (_, warned) = alerts(&checks(&calm, &g, &cfg), &warned, &apps, 700);
        assert!(warned.is_empty());
        let (sent, _) = alerts(&checks(&noisy, &g, &cfg), &warned, &apps, 1000);
        assert_eq!(sent[0].dedupe_key, "monitor:sign_in_failures:1000");
        // No apps on the source: nothing is sent, but the state still moves.
        let (sent, warned) = alerts(&checks(&noisy, &g, &cfg), &[], &[], 1300);
        assert!(sent.is_empty());
        assert_eq!(warned, ["sign_in_failures"]);
    }

    #[test]
    fn the_manifest_is_valid() {
        pod_core::source::check_manifest(NAME, &MANIFEST).unwrap();
    }
}
