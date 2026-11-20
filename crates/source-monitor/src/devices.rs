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

/// Builds behind the server's that get a series of their own (2 to 7);
/// older ones are `other`.
const BEHIND: usize = 6;
/// The series of builds newer than the server's: red, on top of its blue.
const AHEAD: &str = "8";
/// The series of a build older than [`BEHIND`] holds, or with no time.
const OTHER: &str = "other";

/// A series' light colour, the theme's `chart_<series>` (decision
/// `chart-colours`): the `color` field, for layouts that bind it rather
/// than `${local://theme#chart_$item.series}`.
pub fn series_color(series: &str) -> &'static str {
    let token = format!("chart_{series}");
    pod_core::porcelain::THEME_TOKENS
        .iter()
        .find(|t| t.0 == token)
        .map_or("#9AA0A6", |t| t.1)
}

/// `(text, background)` of a relation's badge.
fn relation_colors(r: Relation) -> (&'static str, &'static str) {
    match r {
        Relation::Latest => ("#2E7A4C", "#E2F1E6"),
        Relation::Behind(_) => ("#9A5B12", "#FBEBD3"),
        Relation::Ahead => ("#136B6B", "#D8EFEE"),
        Relation::Unknown => ("#5F6368", "#ECEDEF"),
    }
}

/// Each build's chart series (decision `chart-colours`): `1` the
/// server's, `2` to `7` the builds behind it, newest first (so the builds
/// most devices run get the strongest colours), `8` the ones past it, and
/// `other` older builds and one with no time.
pub fn build_series(builds: &[&str], server: &str) -> HashMap<String, &'static str> {
    const BEHIND_SERIES: [&str; BEHIND] = ["2", "3", "4", "5", "6", "7"];
    let s = BuildId::parse(server);
    let mut known: Vec<(BuildId, &str)> = builds
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| (BuildId::parse(b), *b))
        .filter(|(id, _)| id.stamp.is_some())
        .collect();
    known.sort_by(|a, b| a.0.order(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    known.dedup_by(|a, b| a.1 == b.1);
    let mut out = HashMap::new();
    let mut behind = 0;
    for (id, b) in known.iter().rev() {
        let series = match id.order(&s) {
            _ if *b == server => "1",
            Some(std::cmp::Ordering::Greater) => AHEAD,
            _ => {
                behind += 1;
                BEHIND_SERIES.get(behind - 1).copied().unwrap_or(OTHER)
            }
        };
        out.insert((*b).to_owned(), series);
    }
    for b in builds {
        out.entry((*b).to_owned()).or_insert(OTHER);
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

/// Slack for [`fold_replaced`]: a device's last ping can be written up to a
/// minute late.
const REPLACED_SLACK: i64 = 60;

/// Folds a device that a newer one replaced into it: a browser whose stored
/// id was lost (its site data cleared, another origin) or an app installed
/// again comes back as a new device with the same kind and name, first seen
/// after the old one was last seen. The two never ran side by side, so they
/// are one device to the user: the old one's days count as the new one's
/// and it leaves the list. Two that overlapped stay two.
pub fn fold_replaced(devices: &[Device], days: &[DeviceDay]) -> (Vec<Device>, Vec<DeviceDay>) {
    // The device that replaced each one, if any: the first of its kind and
    // name to start after it stopped.
    let next = |old: &Device| {
        devices
            .iter()
            .filter(|d| {
                d.id != old.id
                    && d.platform == old.platform
                    && d.name == old.name
                    && (d.first_seen, d.id.as_str()) > (old.first_seen, old.id.as_str())
                    && old.last_seen <= d.first_seen + REPLACED_SLACK
            })
            .min_by_key(|d| (d.first_seen, d.id.as_str()))
    };
    let mut into: HashMap<&str, &str> = HashMap::new();
    for d in devices {
        // Follow the replacements to the newest (each starts later than
        // the one before, so this ends).
        let mut cur = d;
        while let Some(n) = next(cur) {
            cur = n;
        }
        if cur.id != d.id {
            into.insert(d.id.as_str(), cur.id.as_str());
        }
    }
    let kept: Vec<Device> = devices
        .iter()
        .filter(|d| !into.contains_key(d.id.as_str()))
        .cloned()
        .collect();
    // A day both ran on keeps the newer one's build.
    let mut out: Vec<DeviceDay> = Vec::new();
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    for d in days {
        let owner = into.get(d.device.as_str()).copied().unwrap_or(&d.device);
        let row = DeviceDay {
            device: owner.to_owned(),
            ..d.clone()
        };
        let key = (d.day.clone(), owner.to_owned());
        match at.get(&key) {
            Some(&i) => {
                out[i].pings += d.pings;
                if d.device == owner {
                    out[i].build = d.build.clone();
                }
            }
            None => {
                at.insert(key, out.len());
                out.push(row);
            }
        }
    }
    (kept, out)
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
    let (devices, days) = fold_replaced(devices, days);
    let (devices, days) = (devices.as_slice(), days.as_slice());
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
    let series = build_series(&builds, server);
    let series_of = |b: &str| series.get(b).copied().unwrap_or(OTHER);
    let order = |build: &str| BuildId::parse(build).stamp.unwrap_or(0);
    // Within a day: older builds at the bottom (first), each build's
    // devices together, the ones seen that day before the rest, so a chart
    // that merges by build draws one block of each.
    items.sort_by(|a, b| {
        (
            day_keys.iter().position(|k| *k == a.0),
            order(&a.2),
            &a.2,
            !a.3,
            &a.1.name,
        )
            .cmp(&(
                day_keys.iter().position(|k| *k == b.0),
                order(&b.2),
                &b.2,
                !b.3,
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
                "build": build,
                "build_label": build_label(build),
                "series": series_of(build),
                "color": series_color(series_of(build)),
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
                "series": series_of(build),
                "color": series_color(series_of(build)),
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
                "series": series_of(&d.build),
                "color": series_color(series_of(&d.build)),
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
    fn each_build_gets_its_own_series_the_servers_first() {
        let older: Vec<String> = (10..18)
            .map(|d| format!("0.6.0+202611{d}090000.aaaa0{d}"))
            .collect();
        let mut builds: Vec<&str> = older.iter().map(String::as_str).collect();
        builds.extend([SERVER, "0.6.0+20261119100000.f02b77e", ""]);
        let c = build_series(&builds, SERVER);
        assert_eq!(c[SERVER], "1");
        assert_eq!(c["0.6.0+20261119100000.f02b77e"], "8");
        // Newest behind first: the 17th 2, the 16th 3, ...
        assert_eq!(c[older[7].as_str()], "2");
        assert_eq!(c[older[6].as_str()], "3");
        assert_eq!(c[older[2].as_str()], "7");
        // Past the six: other, like a build with no time.
        assert_eq!(c[older[1].as_str()], "other");
        assert_eq!(c[""], "other");
        // The colour field is the theme's light chart colour.
        assert_eq!(series_color("1"), "#2A78D6");
        assert_eq!(series_color("other"), "#9AA0A6");
        let shown: std::collections::HashSet<&str> = older[2..]
            .iter()
            .map(|b| series_color(c[b.as_str()]))
            .collect();
        assert_eq!(shown.len(), 6, "six builds, six colours");
    }

    fn browser(id: &str, first_seen: i64, last_seen: i64) -> Device {
        Device {
            name: "Firefox on macOS".into(),
            first_seen,
            ..dev(id, "web", SERVER, Relation::Latest, last_seen)
        }
    }

    fn day(day: &str, device: &str, build: &str) -> DeviceDay {
        DeviceDay {
            day: day.into(),
            device: device.into(),
            build: build.into(),
            pings: 1,
        }
    }

    #[test]
    fn a_browser_that_lost_its_id_is_one_device() {
        let old_build = "0.6.0+20261114100000.9be03a1";
        // The old id stopped at 1000, the new one started 20 s later.
        let devices = vec![
            browser("newid0001", 1020, 5000),
            browser("oldid0001", 100, 1000),
        ];
        let days = vec![
            day("2026-11-18", "oldid0001", old_build),
            day("2026-11-19", "newid0001", SERVER),
            day("2026-11-19", "oldid0001", old_build),
        ];
        let (kept, days) = fold_replaced(&devices, &days);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "newid0001");
        assert_eq!(
            days,
            vec![
                DeviceDay {
                    pings: 1,
                    ..day("2026-11-18", "newid0001", old_build)
                },
                DeviceDay {
                    pings: 2,
                    ..day("2026-11-19", "newid0001", SERVER)
                },
            ]
        );
    }

    #[test]
    fn two_browsers_used_side_by_side_stay_two() {
        let devices = vec![
            browser("aaaa00001", 100, 5000),
            browser("bbbb00001", 200, 4000),
        ];
        let (kept, _) = fold_replaced(&devices, &[]);
        assert_eq!(kept.len(), 2);
        // Another name (a nickname, another browser) is never folded.
        let mut other = browser("cccc00001", 6000, 7000);
        other.name = "Work Firefox".into();
        let devices = vec![browser("aaaa00001", 100, 5000), other];
        assert_eq!(fold_replaced(&devices, &[]).0.len(), 2);
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
