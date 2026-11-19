//! The `devices` feed (decision `device-versions`): the user's devices, the
//! build each runs against the server's, and devices per day by build for
//! a `core://Bars` chart. The rows are the pod's own
//! ([`pod_core::devices`]), so unlike `overview` the feed needs no operator
//! switch.

use std::collections::HashMap;

use pod_core::devices::{Device, DeviceDay, Relation, platform_text};
use protocol::{BuildId, DataBundle};
use serde_json::json;

use crate::record;

pub const DEVICES_FEED: &str = "devices";
pub const DEVICE_SUMMARY_QUERY: &str = "monitor:device_summary";
pub const DEVICES_QUERY: &str = "monitor:devices";
pub const DEVICE_DAYS_QUERY: &str = "monitor:device_days";
pub const BUILDS_QUERY: &str = "monitor:builds";
/// Days the chart shows, today included.
pub const CHART_DAYS: i64 = 14;

/// Builds up to the server's, oldest to newest (the server's darkest).
const RAMP: [&str; 6] = [
    "#DAD5F0", "#C2BAE8", "#A69BDC", "#8A7DCF", "#6A5CBE", "#4537A6",
];
/// Builds newer than the server's.
const AHEAD: [&str; 2] = ["#1F8A8A", "#4CB8B5"];
const UNKNOWN: &str = "#9AA0A6";

/// `(text, background)` of a relation's badge.
fn relation_colors(r: Relation) -> (&'static str, &'static str) {
    match r {
        Relation::Latest => ("#2E7A4C", "#E2F1E6"),
        Relation::Behind(_) => ("#9A5B12", "#FBEBD3"),
        Relation::Ahead => ("#136B6B", "#D8EFEE"),
        Relation::Unknown => ("#5F6368", "#ECEDEF"),
    }
}

/// Each build's colour: older builds lighter, the server's darkest, newer
/// ones teal, one with no time grey.
pub fn build_colors(builds: &[&str], server: &str) -> HashMap<String, &'static str> {
    let s = BuildId::parse(server);
    let mut known: Vec<(BuildId, &str)> = builds
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| (BuildId::parse(b), *b))
        .filter(|(id, _)| id.stamp.is_some())
        .collect();
    known.sort_by(|a, b| a.0.order(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    known.dedup_by(|a, b| a.1 == b.1);
    let ahead = |id: &BuildId| id.order(&s) == Some(std::cmp::Ordering::Greater);
    let behind: Vec<&str> = known
        .iter()
        .filter(|(id, _)| !ahead(id))
        .map(|(_, b)| *b)
        .collect();
    let mut out = HashMap::new();
    // The newest up to the server's get the darkest shades.
    let skip = RAMP.len().saturating_sub(behind.len());
    for (i, b) in behind.iter().enumerate() {
        let shade = (skip + i).min(RAMP.len() - 1);
        // Older than the ramp holds: the lightest.
        let shade = if behind.len() > RAMP.len() {
            (i + RAMP.len()).saturating_sub(behind.len())
        } else {
            shade
        };
        out.insert((*b).to_owned(), RAMP[shade]);
    }
    for (i, (_, b)) in known.iter().filter(|(id, _)| ahead(id)).enumerate() {
        out.insert((*b).to_owned(), AHEAD[i.min(AHEAD.len() - 1)]);
    }
    for b in builds {
        out.entry((*b).to_owned()).or_insert(UNKNOWN);
    }
    out
}

fn seen_text(secs: i64) -> String {
    match secs.max(0) {
        s if s < 90 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 2 * 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// `Thu Nov 19` for `2026-11-19`.
fn day_text(day: &str) -> String {
    match day.parse::<pod_core::tz::jiff::civil::Date>() {
        Ok(d) => format!("{} {}", d.strftime("%a %b"), d.day()),
        Err(_) => day.to_owned(),
    }
}

fn build_label(build: &str) -> String {
    if build.is_empty() {
        "Version unknown".into()
    } else {
        BuildId::parse(build).label()
    }
}

/// The feed's data: `devices` the pod knows, `days` their builds per day
/// (`day_keys`, oldest first, the last one today), against server build
/// `server`, at `now`.
pub fn bundle(
    devices: &[Device],
    days: &[DeviceDay],
    day_keys: &[String],
    server: &str,
    now: i64,
) -> DataBundle {
    let mut b = DataBundle::default();
    let today = day_keys.last().cloned().unwrap_or_default();
    let by_id: HashMap<&str, &Device> = devices.iter().map(|d| (d.id.as_str(), d)).collect();

    // Bars: (day, device, build, seen today) for the days before today,
    // then every device at its latest build for today.
    let mut items: Vec<(String, &Device, String, bool)> = Vec::new();
    for d in days {
        if d.day == today || !day_keys.contains(&d.day) {
            continue;
        }
        if let Some(dev) = by_id.get(d.device.as_str()) {
            items.push((d.day.clone(), dev, d.build.clone(), true));
        }
    }
    for dev in devices {
        let seen_today = days.iter().any(|d| d.day == today && d.device == dev.id);
        items.push((today.clone(), dev, dev.build.clone(), seen_today));
    }
    let mut builds: Vec<&str> = items.iter().map(|i| i.2.as_str()).collect();
    builds.push(server);
    let colors = build_colors(&builds, server);
    let order = |build: &str| BuildId::parse(build).stamp.unwrap_or(0);
    // Within a day: older builds at the bottom (first).
    items.sort_by(|a, b| {
        (
            day_keys.iter().position(|k| *k == a.0),
            order(&a.2),
            &a.1.name,
        )
            .cmp(&(
                day_keys.iter().position(|k| *k == b.0),
                order(&b.2),
                &b.1.name,
            ))
    });
    let mut uris = Vec::new();
    for (day, dev, build, seen) in &items {
        let uri = format!("monitor://device_days/{day}/{}", dev.id);
        let is_today = *day == today;
        b.entities.insert(
            uri.clone(),
            record(json!({
                "bar": day,
                "bar_label": if is_today { "Now".to_owned() } else {
                    day.rsplit('-').next().unwrap_or_default().trim_start_matches('0').to_owned()
                },
                "day_text": if is_today { "Now".to_owned() } else { day_text(day) },
                "device": dev.name,
                "device_id": dev.id,
                "kind": dev.platform,
                "build_label": build_label(build),
                "color": colors.get(build.as_str()).copied().unwrap_or(UNKNOWN),
                "hatched": !seen,
                "note": if *seen { String::new() } else {
                    format!("seen {}", seen_text(now - dev.last_seen))
                },
                "detail": if *seen { build_label(build) } else {
                    format!("{} · seen {}", build_label(build), seen_text(now - dev.last_seen))
                },
            })),
        );
        uris.push(uri);
    }
    b.queries.insert(DEVICE_DAYS_QUERY.into(), uris);

    // The legend: every build the chart shows, oldest first.
    let mut shown: Vec<&str> = items.iter().map(|i| i.2.as_str()).collect();
    shown.sort_by_key(|b| (order(b) == 0 && !b.is_empty(), order(b)));
    shown.dedup();
    let s = BuildId::parse(server);
    let mut uris = Vec::new();
    for (i, build) in shown.iter().enumerate() {
        let id = BuildId::parse(build);
        let note = if *build == server {
            "server"
        } else if build.is_empty() || id.order(&s).is_none() {
            ""
        } else if id.order(&s) == Some(std::cmp::Ordering::Greater) {
            "ahead"
        } else {
            ""
        };
        let label = build_label(build);
        let uri = format!("monitor://builds/{i}");
        b.entities.insert(
            uri.clone(),
            record(json!({
                "build": build,
                "label": label,
                "note": note,
                "legend": if note.is_empty() { label.clone() } else { format!("{label} · {note}") },
                "color": colors.get(*build).copied().unwrap_or(UNKNOWN),
            })),
        );
        uris.push(uri);
    }
    b.queries.insert(BUILDS_QUERY.into(), uris);

    // The device list.
    let mut uris = Vec::new();
    let mut counts = [0usize; 4];
    for d in devices {
        let (fg, bg) = relation_colors(d.relation);
        counts[match d.relation {
            Relation::Latest => 0,
            Relation::Ahead => 1,
            Relation::Behind(_) => 2,
            Relation::Unknown => 3,
        }] += 1;
        let uri = format!("monitor://devices/{}", d.id);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "id": d.id,
                "name": d.name,
                "kind": d.platform,
                "kind_text": platform_text(&d.platform),
                "build": d.build,
                "build_label": build_label(&d.build),
                "color": colors.get(d.build.as_str()).copied().unwrap_or(UNKNOWN),
                "relation": d.relation.id(),
                "relation_text": d.relation.text(),
                "relation_color": fg,
                "relation_bg": bg,
                "seen_text": format!("seen {}", seen_text(now - d.last_seen)),
                "subtitle": format!("{} · {} · seen {}", platform_text(&d.platform),
                    build_label(&d.build), seen_text(now - d.last_seen)),
            })),
        );
        uris.push(uri);
    }
    b.queries.insert(DEVICES_QUERY.into(), uris);

    let mut parts = Vec::new();
    for (n, what) in counts.iter().zip([
        "on the server's build",
        "ahead",
        "behind",
        "version unknown",
    ]) {
        if *n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let uri = "monitor://device_summary/pod".to_owned();
    b.entities.insert(
        uri.clone(),
        record(json!({
            "today": today,
            "server_build": server,
            "server_build_label": build_label(server),
            "count_text": match devices.len() {
                0 => "No devices yet".to_owned(),
                1 => "1 device".to_owned(),
                n => format!("{n} devices"),
            },
            "status_text": parts.join(" · "),
            "empty": devices.is_empty(),
        })),
    );
    b.queries.insert(DEVICE_SUMMARY_QUERY.into(), vec![uri]);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const SERVER: &str = "0.6.0+20261118100000.c41d9e2";

    fn dev(id: &str, platform: &str, build: &str, rel: Relation, last_seen: i64) -> Device {
        Device {
            id: id.into(),
            platform: platform.into(),
            name: format!("{id} name"),
            build: build.into(),
            features: Vec::new(),
            first_seen: 0,
            last_seen,
            build_since: 0,
            relation: rel,
        }
    }

    #[test]
    fn builds_get_darker_toward_the_servers_and_teal_past_it() {
        let c = build_colors(
            &[
                "0.6.0+20261114100000.9be03a1",
                SERVER,
                "0.6.0+20261119100000.f02b77e",
                "",
            ],
            SERVER,
        );
        assert_eq!(c[SERVER], "#4537A6");
        assert_eq!(c["0.6.0+20261114100000.9be03a1"], "#6A5CBE");
        assert_eq!(c["0.6.0+20261119100000.f02b77e"], AHEAD[0]);
        assert_eq!(c[""], UNKNOWN);
    }

    #[test]
    fn today_lists_every_device_and_hatches_the_unseen() {
        let now = 1_000_000;
        let devices = vec![
            dev("phone0001", "android", SERVER, Relation::Latest, now - 60),
            dev(
                "laptop001",
                "web",
                "0.6.0+20261114100000.9be03a1",
                Relation::Behind(1),
                now - 3 * 86_400,
            ),
        ];
        let days = vec![
            DeviceDay {
                day: "2026-11-16".into(),
                device: "laptop001".into(),
                build: "0.6.0+20261114100000.9be03a1".into(),
                pings: 4,
            },
            DeviceDay {
                day: "2026-11-19".into(),
                device: "phone0001".into(),
                build: SERVER.into(),
                pings: 9,
            },
        ];
        let keys: Vec<String> = ["2026-11-16", "2026-11-17", "2026-11-18", "2026-11-19"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b = bundle(&devices, &days, &keys, SERVER, now);
        let bars = &b.queries[DEVICE_DAYS_QUERY];
        assert_eq!(bars.len(), 3);
        let today: Vec<&Value> = bars
            .iter()
            .map(|u| &b.entities[u].0)
            .filter(|e| e["bar"] == "2026-11-19")
            .map(|e| &e["hatched"])
            .collect();
        assert_eq!(today.len(), 2);
        let laptop = &b.entities["monitor://device_days/2026-11-19/laptop001"].0;
        assert_eq!(laptop["hatched"], true);
        assert_eq!(laptop["note"], "seen 3 days ago");
        assert_eq!(laptop["bar_label"], "Now");
        let past = &b.entities["monitor://device_days/2026-11-16/laptop001"].0;
        assert_eq!(past["bar_label"], "16");
        assert_eq!(past["day_text"], "Mon Nov 16");
        let summary = &b.entities["monitor://device_summary/pod"].0;
        assert_eq!(summary["count_text"], "2 devices");
        assert_eq!(summary["status_text"], "1 on the server's build · 1 behind");
        let legend: Vec<&Value> = b.queries[BUILDS_QUERY]
            .iter()
            .map(|u| &b.entities[u].0["legend"])
            .collect();
        assert_eq!(legend.len(), 2);
        assert_eq!(legend[1], "Nov 18 · c41d9e2 · server");
    }
}
