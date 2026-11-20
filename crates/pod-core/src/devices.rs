//! The devices that reach the pod and the build each runs (decision
//! `device-versions`).
//!
//! Every engine request carries the device's id ([`protocol::DEVICE_HEADER`],
//! one per install: per app install, per browser), what it is and which build
//! it runs ([`protocol::BUILD_HEADER`]), the name it found for itself
//! ([`protocol::DEVICE_NAME_HEADER`]) and the features it draws
//! ([`protocol::FEATURES_HEADER`]). [`note_layer`] keeps one row per device
//! (`client_devices`), the build each ran each day in the pod's time zone
//! (`client_days`) and the server builds the pod has run (`server_builds`).
//! A ping costs no write: what changed is written at once, the rest at most
//! once a minute per device.
//!
//! The server, the web client and the apps share one build id
//! ([`protocol::BuildId`]), so a device is on the server's build
//! ([`Relation::Latest`]), behind it by some builds, or ahead of it (an app
//! built before its deploy landed).
//!
//! Agents watch for a device to update ([`watch_tool`]): when the device
//! draws a feature or runs a build at least as new as one named, the pod runs
//! a source action as its owner (e.g. reopens the agent's run with a message)
//! or sends a notification on the "Device updates" channel, once.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::store::Store;
use crate::util::now_secs;
use crate::{NewNotification, Pod};
use protocol::BuildId;

/// The notification channel a source declares for the layout that shows
/// the devices (`monitor`'s `devices` feed); watches with `notify` send on it.
pub const DEVICE_CHANNEL: &str = "device_updates";
/// How often an unchanged device's last-seen time and pings are written.
const WRITE_EVERY_SECS: i64 = 60;
/// A device not seen for this long is forgotten.
pub const FORGET_AFTER_SECS: i64 = 60 * 86_400;
/// Days of history kept.
const KEEP_DAYS: i64 = 90;
/// Watches: how long one lasts by default, and at most.
const WATCH_DAYS: i64 = 30;
const MAX_WATCH_DAYS: i64 = 90;
/// Open watches per pod.
const MAX_WATCHES: i64 = 50;
const MAX_NAME: usize = 60;

// ---------- recording ----------

/// What a request said about its device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub device: String,
    /// `android`, `macos`, `web`, … (`unknown` for a build that doesn't say).
    pub platform: String,
    /// The build id (empty for a build that doesn't say).
    pub build: String,
    pub name: String,
    /// The features it draws, sorted, comma-separated.
    pub features: String,
}

impl Ping {
    /// The device a request comes from, when it names one.
    pub fn of(headers: &HeaderMap) -> Option<Ping> {
        let text = |h: &str| headers.get(h).and_then(|v| v.to_str().ok());
        let device = text(protocol::DEVICE_HEADER).filter(|d| valid_device_id(d))?;
        let (platform, build) = text(protocol::BUILD_HEADER)
            .and_then(protocol::parse_build_header)
            .unwrap_or(("unknown", ""));
        let name = text(protocol::DEVICE_NAME_HEADER)
            .map(clean_name)
            .unwrap_or_default();
        let features = text(protocol::FEATURES_HEADER)
            .map(|f| {
                protocol::parse_features(f)
                    .into_iter()
                    .filter(|f| f.len() <= 64)
                    .take(512)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        Some(Ping {
            device: device.to_owned(),
            platform: platform.to_owned(),
            build: build.to_owned(),
            name,
            features,
        })
    }
}

/// `[A-Za-z0-9_-]{8,64}`, as the engine makes them.
fn valid_device_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A name trimmed to one line of at most [`MAX_NAME`] characters.
fn clean_name(n: &str) -> String {
    n.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// What the pod remembers between writes, per device.
#[derive(Default)]
pub struct Recorder {
    seen: Mutex<HashMap<String, Seen>>,
    /// Whether this process noted its own build and pruned old rows.
    started: Mutex<Option<i64>>,
}

struct Seen {
    ping: Ping,
    written_at: i64,
    pings: i64,
}

/// What to write for one ping, when anything.
struct Due {
    ping: Ping,
    pings: i64,
    /// Its build, name or features changed (or it is new to this process).
    changed: bool,
}

impl Recorder {
    fn due(&self, ping: Ping, now: i64) -> Option<Due> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        match seen.get_mut(&ping.device) {
            Some(s) if s.ping == ping && now - s.written_at < WRITE_EVERY_SECS => {
                s.pings += 1;
                None
            }
            Some(s) => {
                let changed = s.ping != ping;
                let pings = s.pings + 1;
                *s = Seen {
                    ping: ping.clone(),
                    written_at: now,
                    pings: 0,
                };
                Some(Due {
                    ping,
                    pings,
                    changed,
                })
            }
            None => {
                if seen.len() > 1000 {
                    seen.clear();
                }
                seen.insert(
                    ping.device.clone(),
                    Seen {
                        ping: ping.clone(),
                        written_at: now,
                        pings: 0,
                    },
                );
                Some(Due {
                    ping,
                    pings: 1,
                    changed: true,
                })
            }
        }
    }

    /// True once per day per process: note the server's build, prune.
    fn housekeeping(&self, now: i64) -> bool {
        let mut s = self.started.lock().unwrap_or_else(|e| e.into_inner());
        let due = s.is_none_or(|at| now - at >= 86_400);
        if due {
            *s = Some(now);
        }
        due
    }
}

/// The pod's day (`YYYY-MM-DD` in its time zone) at `now`.
fn day_of(store: &Store, now: i64) -> rusqlite::Result<String> {
    let tz = crate::tz::time_zone(store)?;
    Ok(jiff::Timestamp::from_second(now)
        .map(|t| t.to_zoned(tz).date().to_string())
        .unwrap_or_default())
}

/// Middleware of the pod's routes: notes the calling device, off the
/// request's path (the write runs after, on its own). Only a caller with a
/// session is one of the pod's devices: a public path (an upload, a
/// Pixelfed image, the OAuth callback) is open to anyone who has the pod's
/// id, and what they send must not reach its devices or fire its watches.
pub(crate) async fn note_layer(State(pod): State<Pod>, req: Request, next: Next) -> Response {
    let skip = req.uri().path() == "/v1/mcp"
        || req.headers().contains_key(crate::session::INTERNAL_HEADER);
    if !skip
        && let Some(token) = crate::session::bearer(req.headers())
        && let Some(ping) = Ping::of(req.headers())
    {
        note(&pod, crate::session::session_hash(token), ping, now_secs());
    }
    next.run(req).await
}

/// Note a ping from the session whose hash is `session`: written now when
/// due ([`Recorder`]) and the session is valid, then watches checked when
/// the device changed.
pub fn note(pod: &Pod, session: String, ping: Ping, now: i64) {
    let rec = pod.local::<Recorder>();
    let house = rec.housekeeping(now);
    let Some(due) = rec.due(ping, now) else {
        return;
    };
    let pod = pod.clone();
    let ttl = pod.session_ttl_secs();
    tokio::spawn(async move {
        let device = due.ping.device.clone();
        let changed = due.changed;
        let r = pod
            .blocking(move |s| {
                if house {
                    note_server_build(s, protocol::build(), now)?;
                    prune(s, now)?;
                }
                if !s.session_valid(&session, ttl)? {
                    return Ok(false);
                }
                let day = day_of(s, now)?;
                s.with_conn(|c| write(c, &due.ping, due.pings, &day, now))?;
                Ok(true)
            })
            .await;
        let written = match r {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(status = ?e.0, "device ping not recorded");
                return;
            }
        };
        if written && changed {
            check_watches(&pod, &device).await;
        }
    });
}

/// Record that this pod ran server build `build`.
pub fn note_server_build(store: &Store, build: &str, now: i64) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR IGNORE INTO server_builds (build, first_seen) VALUES (?1, ?2)",
            params![build, now],
        )
        .map(|_| ())
    })
}

fn prune(store: &Store, now: i64) -> rusqlite::Result<()> {
    let oldest_day = day_of(store, now - KEEP_DAYS * 86_400)?;
    store.with_conn(|c| {
        c.execute(
            "DELETE FROM client_devices WHERE last_seen < ?1",
            [now - FORGET_AFTER_SECS],
        )?;
        c.execute("DELETE FROM client_days WHERE day < ?1", [oldest_day])?;
        c.execute(
            "DELETE FROM device_watches WHERE expires_at < ?1 OR fired_at < ?1",
            [now - KEEP_DAYS * 86_400],
        )?;
        Ok(())
    })
}

/// Write one device's ping (and `pings` since the last write) for `day`.
pub fn write(c: &Connection, p: &Ping, pings: i64, day: &str, now: i64) -> rusqlite::Result<()> {
    let old: Option<(String, String)> = c
        .query_row(
            "SELECT build, platform FROM client_devices WHERE id = ?1",
            [&p.device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // A build that doesn't say what it is keeps what an earlier one said.
    let (build, platform) = match &old {
        Some((b, pl)) if p.build.is_empty() => (b.clone(), pl.clone()),
        _ => (p.build.clone(), p.platform.clone()),
    };
    match old {
        None => {
            c.execute(
                "INSERT INTO client_devices (id, platform, name, build, features, first_seen, last_seen,
                 build_since) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?6)",
                params![p.device, platform, p.name, build, p.features, now],
            )?;
        }
        Some((old_build, _)) => {
            c.execute(
                "UPDATE client_devices SET platform = ?2, name = CASE WHEN ?3 = '' THEN name ELSE ?3 END,
                 build = ?4, features = CASE WHEN ?5 = '' THEN features ELSE ?5 END,
                 last_seen = ?6, build_since = CASE WHEN build = ?4 THEN build_since ELSE ?6 END
                 WHERE id = ?1",
                params![p.device, platform, p.name, build, p.features, now],
            )?;
            let _ = old_build;
        }
    }
    c.execute(
        "INSERT INTO client_days (day, device, build, pings) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (day, device) DO UPDATE SET build = ?3, pings = pings + ?4",
        params![day, p.device, build, pings],
    )?;
    Ok(())
}

// ---------- reading ----------

/// Where a device's build stands against the server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// The server's build.
    Latest,
    /// Older, by this many server builds (at least 1).
    Behind(u32),
    /// Newer than the server's (built before its deploy landed).
    Ahead,
    /// Its build doesn't say, or has no time to order it by.
    Unknown,
}

impl Relation {
    pub fn id(self) -> &'static str {
        match self {
            Relation::Latest => "latest",
            Relation::Behind(_) => "behind",
            Relation::Ahead => "ahead",
            Relation::Unknown => "unknown",
        }
    }

    pub fn text(self) -> String {
        match self {
            Relation::Latest => "Latest".into(),
            Relation::Behind(n) => format!("{n} behind"),
            Relation::Ahead => "Ahead of server".into(),
            Relation::Unknown => "Version unknown".into(),
        }
    }
}

/// Where `build` stands against the server's build `server`, counting the
/// server builds the pod ran (`ran`) that are newer than it.
pub fn relation(build: &str, server: &str, ran: &[String]) -> Relation {
    if build.is_empty() {
        return Relation::Unknown;
    }
    if build == server {
        return Relation::Latest;
    }
    let (d, s) = (BuildId::parse(build), BuildId::parse(server));
    match d.order(&s) {
        Some(std::cmp::Ordering::Equal) => Relation::Latest,
        Some(std::cmp::Ordering::Greater) => Relation::Ahead,
        Some(std::cmp::Ordering::Less) => {
            let newer = ran
                .iter()
                .map(|b| BuildId::parse(b))
                .filter(|b| {
                    b.order(&d) == Some(std::cmp::Ordering::Greater)
                        && b.order(&s) != Some(std::cmp::Ordering::Greater)
                })
                .count() as u32;
            Relation::Behind(newer.max(1))
        }
        None => Relation::Unknown,
    }
}

/// One device as the pod knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub platform: String,
    /// The user's name for it, else its own, else its platform's.
    pub name: String,
    pub build: String,
    pub features: Vec<String>,
    pub first_seen: i64,
    pub last_seen: i64,
    /// When it first reported its current build.
    pub build_since: i64,
    pub relation: Relation,
}

impl Device {
    pub fn to_json(&self) -> Value {
        let b = BuildId::parse(&self.build);
        let mut v = json!({
            "id": self.id,
            "name": self.name,
            "kind": self.platform,
            "build": self.build,
            "build_label": if self.build.is_empty() { "unknown".into() } else { b.label() },
            "relation": self.relation.id(),
            "relation_text": self.relation.text(),
            "features": self.features,
            "first_seen": iso(self.first_seen),
            "last_seen": iso(self.last_seen),
            "updated_at": iso(self.build_since),
        });
        if let Relation::Behind(n) = self.relation {
            v["behind_by"] = json!(n);
        }
        v
    }
}

fn iso(t: i64) -> String {
    jiff::Timestamp::from_second(t)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// What a platform is called, for a device that names nothing better.
pub fn platform_text(platform: &str) -> &'static str {
    match platform {
        "android" => "Android",
        "ios" => "iPhone",
        "macos" => "Mac app",
        "windows" => "Windows app",
        "linux" => "Linux app",
        "web" => "Browser",
        "sim" => "Simulator",
        _ => "Device",
    }
}

/// The server builds the pod ran, oldest first.
pub fn server_builds(store: &Store) -> rusqlite::Result<Vec<String>> {
    store.with_conn(|c| {
        let mut st = c.prepare_cached("SELECT build FROM server_builds ORDER BY first_seen")?;
        st.query_map([], |r| r.get(0))?.collect()
    })
}

/// Every device, the most recently seen first, against server build
/// `server`.
pub fn devices(store: &Store, server: &str) -> rusqlite::Result<Vec<Device>> {
    let ran = server_builds(store)?;
    store.with_conn(|c| {
        let mut st = c.prepare_cached(
            "SELECT id, platform, name, nickname, build, features, first_seen, last_seen,
                    build_since FROM client_devices ORDER BY last_seen DESC",
        )?;
        st.query_map([], |r| {
            let platform: String = r.get(1)?;
            let own: String = r.get(2)?;
            let nickname: Option<String> = r.get(3)?;
            let build: String = r.get(4)?;
            let features: String = r.get(5)?;
            let name = nickname
                .filter(|n| !n.is_empty())
                .or(Some(own).filter(|n| !n.is_empty()))
                .unwrap_or_else(|| platform_text(&platform).to_owned());
            Ok(Device {
                id: r.get(0)?,
                relation: relation(&build, server, &ran),
                platform,
                name,
                build,
                features: features
                    .split(',')
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned)
                    .collect(),
                first_seen: r.get(6)?,
                last_seen: r.get(7)?,
                build_since: r.get(8)?,
            })
        })?
        .collect()
    })
}

/// One device's build on one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDay {
    pub day: String,
    pub device: String,
    pub build: String,
    pub pings: i64,
}

/// The days from `from` (`YYYY-MM-DD`) on, oldest first.
pub fn days(store: &Store, from: &str) -> rusqlite::Result<Vec<DeviceDay>> {
    store.with_conn(|c| {
        let mut st = c.prepare_cached(
            "SELECT day, device, build, pings FROM client_days WHERE day >= ?1
             ORDER BY day, device",
        )?;
        st.query_map([from], |r| {
            Ok(DeviceDay {
                day: r.get(0)?,
                device: r.get(1)?,
                build: r.get(2)?,
                pings: r.get(3)?,
            })
        })?
        .collect()
    })
}

/// The pod's day `days_ago` days before `now` (`YYYY-MM-DD`).
pub fn day_before(store: &Store, now: i64, days_ago: i64) -> rusqlite::Result<String> {
    day_of(store, now - days_ago * 86_400)
}

/// The device `which` names: its id, or its name (any case).
fn find<'a>(all: &'a [Device], which: &str) -> Option<&'a Device> {
    let w = which.trim();
    all.iter().find(|d| d.id == w).or_else(|| {
        let mut named = all.iter().filter(|d| d.name.eq_ignore_ascii_case(w));
        let first = named.next()?;
        named.next().is_none().then_some(first)
    })
}

// ---------- MCP ----------

/// The device tools' specs.
pub fn tool_specs() -> Vec<Value> {
    let kind = json!({ "type": "string", "description": "A device type: android, ios, macos, windows, linux or web" });
    let device = json!({ "type": "string", "description": "A device: its id or its name from list_devices" });
    vec![
        crate::mcp::tool(
            "list_devices",
            "The user's devices that reach the pod (phone, desktop app, each browser), with the build each runs against the server's: relation latest, behind (behind_by: how many server builds), ahead (built before its deploy landed) or unknown (a build that doesn't say); features: the layout features it draws (check one before using it in a layout the user sees on that device); updated_at: when it first ran its current build. days adds each day's devices and builds (pod's time zone); watches lists open watch_device watches.",
            json!({ "kind": kind, "days": { "type": "integer", "description": "Days of history, 0 to 90 (default 0)" } }),
            &[],
        ),
        crate::mcp::tool(
            "watch_device",
            "Wait for a device to update without polling: when the device (or any device of a kind) draws a feature or runs a build at least as new as one, the pod does `then` once. then: {action: {path, body}} runs a source action as the user (list_data_sources lists actions; e.g. {path: \"/v1/agents/tasks/reopen\", body: {run_id, message}} reopens an agent run with a message, so a run can register a watch, end, and carry on later), or {notify: \"text\"} sends a notification on the Device updates channel. until: {feature: \"bars\"} or {build_at_least: \"server\" (the server's build now) or a build id}. Answers the watch, or already_met when the device already qualifies (nothing is registered). {cancel: id} removes a watch. Watches last expires_in_days (default 30, at most 90).",
            json!({
                "device": device, "kind": kind,
                "until": { "type": "object", "description": "{feature} or {build_at_least}" },
                "then": { "type": "object", "description": "{action: {path, body}} or {notify: text}" },
                "note": { "type": "string", "description": "What the watch is for (shown in list_devices)" },
                "expires_in_days": { "type": "integer" },
                "cancel": { "type": "string", "description": "A watch id to remove" },
            }),
            &[],
        ),
        crate::mcp::tool(
            "rename_device",
            "Name a device as the user calls it (\"my phone\", \"work laptop\"); an empty name goes back to the name the device found for itself.",
            json!({ "device": device, "name": { "type": "string" } }),
            &["device", "name"],
        ),
    ]
}

/// A device tool's answer, when `name` is one.
pub async fn tool(pod: &Pod, name: &str, args: &Value) -> Option<Result<Value, Value>> {
    Some(match name {
        "list_devices" => list_tool(pod, args).await,
        "watch_device" => watch_tool(pod, args).await,
        "rename_device" => rename_tool(pod, args).await,
        _ => return None,
    })
}

fn bad(detail: &str) -> Value {
    json!({ "error": "invalid_arguments", "detail": detail })
}

fn store_err(e: crate::ApiError) -> Value {
    json!({ "error": "store_error", "status": e.0.as_u16() })
}

async fn list_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let kind = args["kind"].as_str().map(str::to_owned);
    let n_days = args["days"].as_i64().unwrap_or(0).clamp(0, KEEP_DAYS);
    let now = now_secs();
    let server = protocol::build();
    pod.blocking(move |s| {
        let all = devices(s, server)?;
        let keep = |platform: &str| kind.as_deref().is_none_or(|k| k == platform);
        let shown: Vec<Value> = all
            .iter()
            .filter(|d| keep(&d.platform))
            .map(Device::to_json)
            .collect();
        let mut out = json!({
            "server_build": server,
            "server_build_label": BuildId::parse(server).label(),
            "devices": shown,
            "watches": open_watches(s, now)?,
        });
        if n_days > 0 {
            let from = day_before(s, now, n_days - 1)?;
            let mut by_day: Vec<(String, Vec<Value>)> = Vec::new();
            for d in days(s, &from)? {
                let Some(dev) = all.iter().find(|x| x.id == d.device) else {
                    continue;
                };
                if !keep(&dev.platform) {
                    continue;
                }
                let row =
                    json!({ "device": dev.name, "id": dev.id, "build": d.build, "pings": d.pings });
                match by_day.last_mut() {
                    Some((day, rows)) if *day == d.day => rows.push(row),
                    _ => by_day.push((d.day, vec![row])),
                }
            }
            out["days"] = by_day
                .into_iter()
                .map(|(day, devices)| json!({ "day": day, "devices": devices }))
                .collect();
        }
        Ok(out)
    })
    .await
    .map_err(store_err)
}

async fn rename_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let which = args["device"].as_str().unwrap_or_default().to_owned();
    let name = clean_name(args["name"].as_str().unwrap_or_default());
    pod.blocking(move |s| {
        let all = devices(s, protocol::build())?;
        let Some(d) = find(&all, &which) else {
            return Ok(Err(json!({ "error": "unknown_device", "device": which,
                "devices": all.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>() })));
        };
        let nick = (!name.is_empty()).then_some(name.clone());
        s.with_conn(|c| {
            c.execute(
                "UPDATE client_devices SET nickname = ?2 WHERE id = ?1",
                params![d.id, nick],
            )
        })?;
        Ok(Ok(json!({ "ok": true, "id": d.id, "name": if name.is_empty() { Value::Null } else { json!(name) } })))
    })
    .await
    .map_err(store_err)?
}

// ---------- watches ----------

/// What a watch waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Until {
    Feature(String),
    /// A build id with a time.
    Build(String),
}

impl Until {
    fn parse(v: &Value, server: &str) -> Result<Until, Value> {
        if let Some(f) = v["feature"].as_str() {
            let f = f.trim();
            if f.is_empty()
                || f.len() > 64
                || !f
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(bad("until.feature is a feature name such as bars"));
            }
            return Ok(Until::Feature(f.to_owned()));
        }
        if let Some(b) = v["build_at_least"].as_str() {
            let b = if b.trim() == "server" {
                server
            } else {
                b.trim()
            };
            if BuildId::parse(b).stamp.is_none() {
                return Err(bad(
                    "until.build_at_least is \"server\" or a build id with a time, such as 0.6.0+20261118153000.c41d9e2",
                ));
            }
            return Ok(Until::Build(b.to_owned()));
        }
        Err(bad("until is {feature} or {build_at_least}"))
    }

    fn to_json(&self) -> Value {
        match self {
            Until::Feature(f) => json!({ "feature": f }),
            Until::Build(b) => json!({ "build_at_least": b }),
        }
    }

    fn met_by(&self, d: &Device) -> bool {
        match self {
            Until::Feature(f) => d.features.iter().any(|x| x == f),
            Until::Build(b) => {
                BuildId::parse(&d.build).order(&BuildId::parse(b))
                    >= Some(std::cmp::Ordering::Equal)
            }
        }
    }
}

/// What a watch does: a source action's path and body, or a notification.
fn parse_then(pod: &Pod, v: &Value) -> Result<Value, Value> {
    if let Some(text) = v["notify"].as_str() {
        let text = text.trim();
        if text.is_empty() || text.len() > 500 {
            return Err(bad(
                "then.notify is the notification's text (at most 500 characters)",
            ));
        }
        return Ok(json!({ "notify": text }));
    }
    let a = &v["action"];
    let Some(path) = a["path"].as_str() else {
        return Err(bad("then is {action: {path, body}} or {notify: text}"));
    };
    let known = pod.catalog().sources().any(|(_, m)| {
        m.actions
            .iter()
            .any(|p| crate::source::path_matches(p.pattern, path))
    });
    if !known || path.contains('$') {
        return Err(json!({ "error": "unknown_action", "path": path,
            "detail": "then.action.path is an action of a source (see list_data_sources), with real values" }));
    }
    if !(a["body"].is_null() || a["body"].is_object()) {
        return Err(bad("then.action.body is an object"));
    }
    Ok(json!({ "action": { "path": path, "body": a["body"] } }))
}

async fn watch_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let now = now_secs();
    if let Some(id) = args["cancel"].as_str() {
        let id = id.to_owned();
        let gone = pod
            .blocking(move |s| {
                s.with_conn(|c| {
                    c.execute(
                        "DELETE FROM device_watches WHERE id = ?1 AND fired_at IS NULL",
                        [&id],
                    )
                })
            })
            .await
            .map_err(store_err)?;
        return Ok(json!({ "ok": true, "cancelled": gone > 0 }));
    }
    let server = protocol::build();
    let until = Until::parse(&args["until"], server)?;
    let then = parse_then(pod, &args["then"])?;
    let which = args["device"].as_str().map(str::to_owned);
    let kind = args["kind"].as_str().map(str::to_owned);
    if which.is_none() == kind.is_none() {
        return Err(bad("name a device or a kind (not both)"));
    }
    if let Some(k) = &kind
        && !protocol::PLATFORMS.contains(&k.as_str())
    {
        return Err(bad(
            "kind is android, ios, macos, windows, linux, web or sim",
        ));
    }
    let note = args["note"]
        .as_str()
        .map(|n| n.chars().take(300).collect::<String>());
    let days = args["expires_in_days"]
        .as_i64()
        .unwrap_or(WATCH_DAYS)
        .clamp(1, MAX_WATCH_DAYS);
    let id = format!("w-{}", &crate::layouts::new_layout_id()[2..]);
    pod.blocking(move |s| {
            let all = devices(s, server)?;
            let device = match &which {
                Some(w) => match find(&all, w) {
                    Some(d) => Some(d.clone()),
                    None => {
                        return Ok(Err(json!({ "error": "unknown_device", "device": w,
                            "devices": all.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>() })));
                    }
                },
                None => None,
            };
            let met: Vec<&Device> = match &device {
                Some(d) => vec![d].into_iter().filter(|d| until.met_by(d)).collect(),
                None => all
                    .iter()
                    .filter(|d| Some(&d.platform) == kind.as_ref() && until.met_by(d))
                    .collect(),
            };
            if !met.is_empty() {
                return Ok(Ok(json!({ "already_met": true,
                    "devices": met.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
                    "detail": "nothing was registered: the device already qualifies" })));
            }
            let open: i64 = s.with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM device_watches WHERE fired_at IS NULL AND expires_at > ?1",
                    [now],
                    |r| r.get(0),
                )
            })?;
            if open >= MAX_WATCHES {
                return Ok(Err(json!({ "error": "too_many_watches", "max": MAX_WATCHES })));
            }
            let device_id = device.as_ref().map(|d| d.id.clone());
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO device_watches (id, device, platform, until_do, then_do, note,
                     created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        device_id,
                        kind,
                        until.to_json().to_string(),
                        then.to_string(),
                        note,
                        now,
                        now + days * 86_400
                    ],
                )
            })?;
            Ok(Ok(json!({ "ok": true, "watch": {
                "id": id,
                "device": device.as_ref().map(|d| json!({"id": d.id, "name": d.name})),
                "kind": kind, "until": until.to_json(), "then": then,
                "expires_at": iso(now + days * 86_400),
            } })))
        })
        .await
        .map_err(store_err)?
}

/// The watches that haven't fired or expired.
fn open_watches(store: &Store, now: i64) -> rusqlite::Result<Vec<Value>> {
    store.with_conn(|c| {
        let mut st = c.prepare_cached(
            "SELECT id, device, platform, until_do, then_do, note, expires_at
             FROM device_watches WHERE fired_at IS NULL AND expires_at > ?1 ORDER BY created_at",
        )?;
        st.query_map([now], |r| {
            let until: String = r.get(3)?;
            let then: String = r.get(4)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "device": r.get::<_, Option<String>>(1)?,
                "kind": r.get::<_, Option<String>>(2)?,
                "until": serde_json::from_str::<Value>(&until).unwrap_or(Value::Null),
                "then": serde_json::from_str::<Value>(&then).unwrap_or(Value::Null),
                "note": r.get::<_, Option<String>>(5)?,
                "expires_at": iso(r.get(6)?),
            }))
        })?
        .collect()
    })
}

/// Run the open watches `device` now meets, each once.
pub async fn check_watches(pod: &Pod, device: &str) {
    let now = now_secs();
    let device = device.to_owned();
    let due = pod
        .blocking(move |s| {
            let all = devices(s, protocol::build())?;
            let Some(d) = all.into_iter().find(|d| d.id == device) else {
                return Ok(Vec::new());
            };
            let watches: Vec<(String, String, String)> = s.with_conn(|c| {
                let mut st = c.prepare_cached(
                    "SELECT id, until_do, then_do FROM device_watches
                     WHERE fired_at IS NULL AND expires_at > ?1
                       AND (device = ?2 OR (device IS NULL AND platform = ?3))",
                )?;
                st.query_map(params![now, d.id, d.platform], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()
            })?;
            let mut due = Vec::new();
            for (id, until, then) in watches {
                let until = serde_json::from_str::<Value>(&until).unwrap_or(Value::Null);
                let Ok(until) = Until::parse(&until, "") else {
                    continue;
                };
                if !until.met_by(&d) {
                    continue;
                }
                // Claimed before it runs, so a second ping doesn't run it again.
                let claimed = s.with_conn(|c| {
                    c.execute(
                        "UPDATE device_watches SET fired_at = ?2 WHERE id = ?1 AND fired_at IS NULL",
                        params![id, now],
                    )
                })?;
                if claimed == 1 {
                    let then = serde_json::from_str::<Value>(&then).unwrap_or(Value::Null);
                    due.push((id, then, d.clone()));
                }
            }
            Ok(due)
        })
        .await;
    let Ok(due) = due else {
        tracing::warn!("device watches could not be read");
        return;
    };
    for (id, then, d) in due {
        let outcome = fire(pod, &then, &d, now).await;
        let _ = pod
            .blocking(move |s| {
                s.with_conn(|c| {
                    c.execute(
                        "UPDATE device_watches SET outcome = ?2 WHERE id = ?1",
                        params![id, outcome],
                    )
                })
            })
            .await;
    }
}

/// Do a watch's `then` for device `d`; answers what happened.
async fn fire(pod: &Pod, then: &Value, d: &Device, now: i64) -> String {
    if let Some(text) = then["notify"].as_str() {
        let Some(ch) = pod
            .notification_channels()
            .into_iter()
            .find(|c| c.id == DEVICE_CHANNEL)
        else {
            return "not sent: no layout shows the devices feed".into();
        };
        let n = NewNotification {
            layout: ch.layout,
            channel: DEVICE_CHANNEL.into(),
            dedupe_key: format!("device-watch:{}:{now}", d.id),
            title: format!("{} updated", d.name),
            body: text.to_owned(),
            image_url: None,
            target_uri: None,
            group_key: None,
            created_at: now,
            subject: None,
            quiet: false,
        };
        return match pod.notify(vec![n]).await {
            Ok(_) => "notified".into(),
            Err(e) => format!("not sent: {}", e.0),
        };
    }
    let (Some(path), body) = (then["action"]["path"].as_str(), &then["action"]["body"]) else {
        return "nothing to do".into();
    };
    let Some(owner) = crate::mcp::owner(pod).await else {
        return "not run: the pod has no owner session".into();
    };
    let body = if body.is_null() {
        json!({})
    } else {
        body.clone()
    };
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .expect("a valid request");
    if let Ok(v) = HeaderValue::from_str(&pod.internal_as(&owner.token_hash)) {
        req.headers_mut().insert(crate::session::INTERNAL_HEADER, v);
    }
    let resp = crate::app(pod.clone())
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {});
    let status = resp.status();
    if status.is_success() {
        format!("ran {path}")
    } else {
        let detail = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .chars()
                    .take(300)
                    .collect::<String>()
            })
            .unwrap_or_default();
        format!("{path} answered {}: {detail}", status.as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "0.6.0+20261118153000.c41d9e2";

    #[test]
    fn a_device_is_latest_behind_or_ahead_of_the_server() {
        let ran = vec![
            "0.5.1+20261103100000.4a7e210".to_owned(),
            "0.6.0+20261111100000.e81c5d4".to_owned(),
            "0.6.0+20261114100000.9be03a1".to_owned(),
            S.to_owned(),
        ];
        assert_eq!(relation(S, S, &ran), Relation::Latest);
        assert_eq!(
            relation("0.6.0+20261114100000.9be03a1", S, &ran),
            Relation::Behind(1)
        );
        assert_eq!(
            relation("0.5.1+20261103100000.4a7e210", S, &ran),
            Relation::Behind(3)
        );
        // A build the pod never ran as its server still counts as behind.
        assert_eq!(
            relation("0.6.0+20261117100000.aaaaaaa", S, &ran),
            Relation::Behind(1)
        );
        assert_eq!(
            relation("0.6.0+20261119080000.f02b77e", S, &ran),
            Relation::Ahead
        );
        assert_eq!(relation("", S, &ran), Relation::Unknown);
        assert_eq!(relation("0.1.0+dev", S, &ran), Relation::Unknown);
        assert_eq!(Relation::Behind(2).text(), "2 behind");
    }

    #[test]
    fn a_ping_reads_its_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(Ping::of(&h), None);
        h.insert(
            protocol::DEVICE_HEADER,
            HeaderValue::from_static("abcdef0123456789"),
        );
        let p = Ping::of(&h).expect("a device");
        assert_eq!((p.platform.as_str(), p.build.as_str()), ("unknown", ""));
        h.insert(
            protocol::BUILD_HEADER,
            HeaderValue::from_static("android 0.6.0+20261118153000.c41d9e2"),
        );
        h.insert(
            protocol::DEVICE_NAME_HEADER,
            HeaderValue::from_static(" Pixel 9 "),
        );
        h.insert(
            protocol::FEATURES_HEADER,
            HeaderValue::from_static("list,bars"),
        );
        let p = Ping::of(&h).expect("a device");
        assert_eq!(p.platform, "android");
        assert_eq!(p.name, "Pixel 9");
        assert_eq!(p.features, "bars,list");
        h.insert(protocol::DEVICE_HEADER, HeaderValue::from_static("short"));
        assert_eq!(Ping::of(&h), None);
    }

    #[test]
    fn a_ping_is_written_when_it_changes_or_a_minute_passed() {
        let rec = Recorder::default();
        let p = Ping {
            device: "abcdef0123456789".into(),
            platform: "web".into(),
            build: S.into(),
            name: String::new(),
            features: String::new(),
        };
        let first = rec.due(p.clone(), 1000).expect("new");
        assert!(first.changed);
        assert!(rec.due(p.clone(), 1010).is_none());
        assert!(rec.due(p.clone(), 1020).is_none());
        let later = rec.due(p.clone(), 1061).expect("a minute on");
        assert!(!later.changed);
        assert_eq!(later.pings, 3);
        let updated = Ping {
            build: "0.6.0+20261119080000.f02b77e".into(),
            ..p
        };
        assert!(rec.due(updated, 1062).expect("changed").changed);
    }

    #[test]
    fn a_watch_waits_for_a_feature_or_a_build() {
        let d = Device {
            id: "abcdef0123456789".into(),
            platform: "android".into(),
            name: "Pixel 9".into(),
            build: "0.6.0+20261114100000.9be03a1".into(),
            features: vec!["list".into()],
            first_seen: 0,
            last_seen: 0,
            build_since: 0,
            relation: Relation::Behind(1),
        };
        let f = Until::parse(&json!({"feature": "bars"}), S).expect("valid");
        assert!(!f.met_by(&d));
        let b = Until::parse(&json!({"build_at_least": "server"}), S).expect("valid");
        assert_eq!(b, Until::Build(S.into()));
        assert!(!b.met_by(&d));
        let updated = Device {
            build: S.into(),
            features: vec!["bars".into()],
            ..d
        };
        assert!(f.met_by(&updated) && b.met_by(&updated));
        assert!(Until::parse(&json!({"build_at_least": "0.1.0+dev"}), S).is_err());
        assert!(Until::parse(&json!({"feature": "Bad Name"}), S).is_err());
    }
}
