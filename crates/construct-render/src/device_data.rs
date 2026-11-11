//! The device data the shared UI gives layouts (`local:layouts`,
//! `local://inbox`, `local://connection`, …), built from the pod's
//! answers. The clients (`crate::remote`, `crate::engine`) and the pod's
//! layout previews build it the same way: a preview draws a layout as a
//! device with the user's data would.

use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};
use std::collections::HashMap;

/// The pod paths [`device`] and the clients read.
pub const LAYOUTS_PATH: &str = "/v1/apps?all=1";
pub const NOTIFICATIONS_PATH: &str = "/v1/notifications?limit=50";
pub const CHANNELS_PATH: &str = "/v1/notifications/channels";
pub const SIGN_INS_PATH: &str = "/v1/auth/sign-ins";

/// The path of a layout's versions ([`versions_bundle`]).
pub fn versions_path(app: &str) -> String {
    format!("/v1/apps/{app}/versions?limit=100")
}

/// The device data a device on `base` gives a layout besides its list of
/// layouts: connection, session, device, inbox and server; and, when
/// `needs` (a layout's JSON) binds them, the notifications, their channels
/// and the sign-ins, read with `get` (a pod path's JSON answer).
pub fn device(
    base: &str,
    needs: &str,
    titles: &HashMap<String, String>,
    get: impl Fn(&str) -> Result<Value, String>,
) -> DataBundle {
    let mut out = base_device(
        base,
        json!({ "online": true, "pending": 0, "label": "Online", "attention": false, "color": "#2E7D32" }),
        json!({ "open": true, "server": base, "error": "", "message": "", "status": "" }),
    );
    let mut inbox = false;
    if needs_notifications(needs)
        && let Ok(v) = get(NOTIFICATIONS_PATH)
    {
        merge_into(&mut out, notifications_bundle(&v, titles, now_secs()));
        inbox = true;
    }
    if !inbox {
        out.entities.insert("local://inbox".into(), inbox_entity(0));
    }
    if needs_channels(needs)
        && let Ok(v) = get(CHANNELS_PATH)
    {
        merge_into(&mut out, channels_bundle(&v, titles));
    }
    if needs_sign_ins(needs)
        && let Ok(v) = get(SIGN_INS_PATH)
    {
        merge_into(&mut out, sign_ins_bundle(&v));
    }
    out
}

fn merge_into(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

fn insert(out: &mut DataBundle, uri: &str, v: Value) {
    if let Ok(e) = serde_json::from_value(v) {
        out.entities.insert(uri.to_owned(), e);
    }
}

/// Whether a layout (its JSON) binds the notifications.
pub fn needs_notifications(needs: &str) -> bool {
    needs.contains("local:notifications") || needs.contains("local://inbox")
}

/// Whether a layout binds the notification channels.
pub fn needs_channels(needs: &str) -> bool {
    needs.contains("local:channels")
}

/// Whether a layout binds the sign-ins or the providers to sign in with.
pub fn needs_sign_ins(needs: &str) -> bool {
    needs.contains("local:sign_ins") || needs.contains("local:providers")
}

/// The device data of the pod's layouts (`GET /v1/apps?all=1`'s answer),
/// as the shared UI builds it: `local:layouts` (hidden ones left out),
/// `local:all_layouts`, `local:apps`, and an entity for each. Every
/// layout's title goes into `titles`.
pub fn layouts_bundle(v: &Value, titles: &mut HashMap<String, String>) -> DataBundle {
    let mut out = DataBundle::default();
    let (mut listed, mut all) = (Vec::new(), Vec::new());
    for a in v["apps"].as_array().into_iter().flatten() {
        let Some(id) = a["id"].as_str() else {
            continue;
        };
        titles.insert(id.to_owned(), a["title"].as_str().unwrap_or(id).to_owned());
        if id == "home" {
            continue;
        }
        let hidden = a["hidden"].as_bool().unwrap_or(false);
        let mut e = EntityRecord::new();
        e.set("id", id);
        e.set("title", a["title"].as_str().unwrap_or(id));
        e.set("icon", a["icon"].as_str().unwrap_or(""));
        e.set(
            "kind",
            if a["kind"] == "custom" {
                "custom"
            } else {
                "blueprint"
            },
        );
        let latest = a["latest_version"].as_i64();
        e.set(
            "detail",
            latest.map(|v| format!("v{v}")).unwrap_or_default(),
        );
        e.set(
            "version",
            latest.map(|v| format!("v{v} · latest")).unwrap_or_default(),
        );
        e.set("unread", 0);
        e.set("hidden", hidden);
        for prefix in ["local://layouts/", "local://apps/"] {
            out.entities.insert(format!("{prefix}{id}"), e.clone());
        }
        all.push(format!("local://layouts/{id}"));
        if !hidden {
            listed.push(id.to_owned());
        }
    }
    let mut count = EntityRecord::new();
    count.set("count", listed.len());
    out.entities.insert("local://layouts".into(), count.clone());
    out.entities.insert("local://apps".into(), count);
    out.queries.insert(
        "local:layouts".into(),
        listed
            .iter()
            .map(|i| format!("local://layouts/{i}"))
            .collect(),
    );
    out.queries.insert(
        "local:apps".into(),
        listed.iter().map(|i| format!("local://apps/{i}")).collect(),
    );
    out.queries.insert("local:all_layouts".into(), all);
    out
}

/// The device data every layout gets: `local://connection` and
/// `local://session` as given, the device and the server.
pub fn base_device(base: &str, connection: Value, session: Value) -> DataBundle {
    let mut out = DataBundle::default();
    insert(&mut out, "local://connection", connection);
    insert(&mut out, "local://session", session);
    insert(
        &mut out,
        "local://device",
        json!({ "platform": if cfg!(target_os = "android") { "android" } else { "desktop" },
                "notification_permission": "" }),
    );
    insert(
        &mut out,
        "local://servers/current",
        json!({ "id": "current", "title": base, "url": base, "selected": true }),
    );
    out.queries.insert(
        "local:servers".into(),
        vec!["local://servers/current".into()],
    );
    out
}

/// `local://inbox` with this many unread.
pub fn inbox_entity(unread: i64) -> EntityRecord {
    serde_json::from_value(json!({
        "unread": unread,
        "label": if unread == 0 { "All read".to_owned() } else { format!("{unread} unread") },
        "has_older": false, "loading": false }))
    .unwrap_or_default()
}

/// `local:notifications` and `local://inbox` of a `GET /v1/notifications`
/// answer, with `when` and `day` labels (UTC) as of `now`.
pub fn notifications_bundle(v: &Value, titles: &HashMap<String, String>, now: i64) -> DataBundle {
    let mut out = DataBundle::default();
    let mut list = Vec::new();
    for n in v["notifications"].as_array().into_iter().flatten() {
        let id = n["id"].as_str().unwrap_or_default();
        let at = n["created_at"].as_i64().unwrap_or(now);
        let uri = format!("local://notifications/{id}");
        let app = n["app_id"].as_str().unwrap_or_default();
        let mut e = n.clone();
        e["app_title"] = json!(titles.get(app).map_or(app, String::as_str));
        e["when"] = json!(time_label(at, now));
        e["day"] = json!(day_label(at, now));
        e["unread"] = json!(!n["read"].as_bool().unwrap_or(true));
        insert(&mut out, &uri, e);
        list.push(uri);
    }
    out.queries.insert("local:notifications".into(), list);
    out.entities.insert(
        "local://inbox".into(),
        inbox_entity(v["unread_total"].as_i64().unwrap_or(0)),
    );
    out
}

/// `local:channels` of a `GET /v1/notifications/channels` answer.
pub fn channels_bundle(v: &Value, titles: &HashMap<String, String>) -> DataBundle {
    let mut out = DataBundle::default();
    let mut list = Vec::new();
    for c in v["channels"].as_array().into_iter().flatten() {
        let (app, id) = (
            c["app_id"].as_str().unwrap_or_default(),
            c["id"].as_str().unwrap_or_default(),
        );
        let uri = format!("local://channels/{app}/{id}");
        let mut e = c.clone();
        e["app_title"] = json!(titles.get(app).map_or(app, String::as_str));
        insert(&mut out, &uri, e);
        list.push(uri);
    }
    out.queries.insert("local:channels".into(), list);
    out
}

/// `local:sign_ins` and `local:providers` of a `GET /v1/auth/sign-ins`
/// answer.
pub fn sign_ins_bundle(v: &Value) -> DataBundle {
    let mut out = DataBundle::default();
    let mut ins = Vec::new();
    let mut signed: Vec<String> = Vec::new();
    for si in v["sign_ins"].as_array().into_iter().flatten() {
        let id = si["id"].as_str().unwrap_or_default();
        let host = si["server"].as_str().map(|s| {
            s.split("://")
                .last()
                .unwrap_or(s)
                .trim_end_matches('/')
                .to_owned()
        });
        let detail: Vec<String> = si["username"]
            .as_str()
            .map(|u| format!("@{u}"))
            .into_iter()
            .chain(host)
            .collect();
        let uri = format!("local://sign_ins/{id}");
        insert(
            &mut out,
            &uri,
            json!({ "id": id, "title": si["title"], "detail": detail.join(" · ") }),
        );
        ins.push(uri);
        signed.push(id.to_owned());
    }
    let mut providers = Vec::new();
    for p in v["providers"].as_array().into_iter().flatten() {
        let id = p["id"].as_str().unwrap_or_default();
        if p["signed_in"].as_bool().unwrap_or(false)
            || p["intent"].is_null()
            || signed.iter().any(|s| s == id)
        {
            continue;
        }
        let uri = format!("local://providers/{id}");
        insert(
            &mut out,
            &uri,
            json!({ "id": id, "title": p["title"],
                "custom_server": !p["server_placeholder"].is_null(),
                "server_placeholder": p["server_placeholder"].as_str().unwrap_or("") }),
        );
        providers.push(uri);
    }
    out.queries.insert("local:sign_ins".into(), ins);
    out.queries.insert("local:providers".into(), providers);
    out
}

/// `local://versions` and `local:versions` for a layout of a
/// `GET /v1/apps/<id>/versions` answer, newest first; `pinned`: the version
/// picked on this device.
pub fn versions_bundle(
    app: &str,
    v: &Value,
    pinned: Option<i64>,
    title: &str,
    now: i64,
) -> DataBundle {
    let list = v["versions"].as_array().cloned().unwrap_or_default();
    let newest = list
        .iter()
        .find(|x| x["compatible"].as_bool().unwrap_or(true))
        .and_then(|x| x["version"].as_i64());
    let mut out = DataBundle::default();
    let mut uris = Vec::new();
    for x in &list {
        let n = x["version"].as_i64().unwrap_or(0);
        let detail = match (x["incompatible"].as_str(), x["note"].as_str()) {
            (Some(i), note) => {
                let why = match i {
                    "update_required" => "Needs a newer version of the app",
                    "retired" => "Made for an older version of the app",
                    _ => "This version of the app can't show it",
                };
                note.map_or(why.to_owned(), |n| format!("{why} · {n}"))
            }
            (None, Some(note)) => note.to_owned(),
            (None, None) => match x["source"].as_str().unwrap_or("") {
                "file" => "Published",
                "seed" => "Built-in",
                "agent" => "AI edit",
                "rollback" => "Rollback",
                "restore" => "Restore",
                "manual" => "Manual edit",
                "switch" => "Layout switch",
                _ => "Edit",
            }
            .to_owned(),
        };
        let uri = format!("local://versions/{app}/{n}");
        insert(
            &mut out,
            &uri,
            json!({
                "app_id": app, "version": n, "label": format!("v{n}"), "detail": detail,
                "when": version_time_label(
                    x["created_at"].as_i64().or_else(|| x["created_at"].as_str().and_then(parse_rfc3339)).unwrap_or(0),
                    now,
                ),
                "compatible": x["compatible"].as_bool().unwrap_or(true),
                "showing": match pinned { Some(p) => p == n, None => Some(n) == newest },
            }),
        );
        uris.push(uri);
    }
    out.queries.insert("local:versions".into(), uris);
    insert(
        &mut out,
        "local://versions",
        json!({
            "app_id": app, "app_title": title,
            "pinned": pinned.map(|p| p.to_string()).unwrap_or_default(), "loading": false,
        }),
    );
    out
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// (year, month 1-12, day) of days since 1970-01-01 (civil, proleptic).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];

/// A notification's time, as the shared UI's `notificationTimeLabel`
/// ("now", "5 min", "3 h", "Fri", "Nov 5"). Days are UTC days here.
pub fn time_label(at: i64, now: i64) -> String {
    let d = (now - at).max(0);
    match d {
        0..60 => "now".into(),
        60..3_600 => format!("{} min", d / 60),
        3_600..86_400 => format!("{} h", d / 3_600),
        86_400..604_800 => WEEKDAYS[at.div_euclid(86_400).rem_euclid(7) as usize][..3].to_owned(),
        _ => {
            let (_, m, day) = civil(at.div_euclid(86_400));
            format!("{} {day}", MONTHS[m as usize - 1])
        }
    }
}

/// Seconds since 1970 of an RFC 3339 UTC time (`2026-11-06T10:54:30Z`, or
/// with an offset).
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil (the inverse of `civil`).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut t = days * 86_400 + h * 3_600 + mi * 60 + sec;
    // An offset after the seconds (and any fraction): +hh:mm or -hh:mm.
    let rest = s.get(19..).unwrap_or("");
    if let Some(i) = rest.find(['+', '-']) {
        let sign = if rest.as_bytes()[i] == b'-' { -1 } else { 1 };
        let off = &rest[i + 1..];
        let oh = off
            .get(0..2)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let om = off
            .get(3..5)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        t -= sign * (oh * 3_600 + om * 60);
    }
    Some(t)
}

/// A version's time, as the shared UI's `versionTimeLabel`: "just now",
/// "5 min ago", "3 h ago", "yesterday", "4 days ago", else the date.
pub fn version_time_label(at: i64, now: i64) -> String {
    if at <= 0 {
        return String::new();
    }
    let d = (now - at).max(0);
    match d {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", d / 60),
        3_600..86_400 => format!("{} h ago", d / 3_600),
        86_400..172_800 => "yesterday".into(),
        172_800..2_592_000 => format!("{} days ago", d / 86_400),
        _ => {
            let (y, m, day) = civil(at.div_euclid(86_400));
            format!("{} {day}, {y}", MONTHS[m as usize - 1])
        }
    }
}

/// The inbox's day header ("Today", "Yesterday", "Thursday, Nov 5").
pub fn day_label(at: i64, now: i64) -> String {
    let (a, n) = (at.div_euclid(86_400), now.div_euclid(86_400));
    match n - a {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let (_, m, d) = civil(a);
            format!(
                "{}, {} {d}",
                WEEKDAYS[a.rem_euclid(7) as usize],
                MONTHS[m as usize - 1]
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_shared_ui() {
        // 2026-11-05 (a Thursday) 12:00 UTC.
        let thu = 1_793_880_000;
        assert_eq!(civil(thu / 86_400), (2026, 11, 5));
        assert_eq!(time_label(thu, thu + 30), "now");
        assert_eq!(time_label(thu, thu + 300), "5 min");
        assert_eq!(time_label(thu, thu + 7_200), "2 h");
        assert_eq!(time_label(thu, thu + 2 * 86_400), "Thu");
        assert_eq!(time_label(thu, thu + 30 * 86_400), "Nov 5");
        assert_eq!(day_label(thu, thu + 60), "Today");
        assert_eq!(day_label(thu, thu + 86_400), "Yesterday");
        assert_eq!(day_label(thu, thu + 3 * 86_400), "Thursday, Nov 5");
        assert_eq!(parse_rfc3339("2026-11-05T12:00:00Z"), Some(thu));
        assert_eq!(parse_rfc3339("2026-11-05T05:00:00-07:00"), Some(thu));
        assert_eq!(version_time_label(thu, thu + 7_200), "2 h ago");
    }
}
