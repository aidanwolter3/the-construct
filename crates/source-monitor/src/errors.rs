//! The `errors` feed (decision `recent-server-errors`): the pod's recent
//! failed requests ([`pod_core::recent_errors`]), and the line the
//! `server_errors` check adds about them. Like the devices, they are the
//! pod's own (its own requests, its own devices), so the feed needs no
//! operator switch; another pod's never show.

use std::collections::{BTreeMap, HashMap};

use pod_core::recent_errors::RecentError;
use protocol::DataBundle;
use serde_json::json;

use crate::record;

pub const ERRORS_FEED: &str = "errors";
pub const RECENT_ERRORS_QUERY: &str = "monitor:recent_errors";
/// How many groups of failures the check's line names.
const NAMED: usize = 2;

/// A device's name by its id; unknown ids show by their first characters.
pub fn device_label(id: &str, names: &HashMap<String, String>) -> String {
    names
        .get(id)
        .cloned()
        .unwrap_or_else(|| format!("device {}", id.chars().take(8).collect::<String>()))
}

/// `POST /v1/taste/react → 502 taste_unreachable`.
fn what(e: &RecentError) -> String {
    let mut s = format!("{} {} → {}", e.method, e.route, e.status);
    if let Some(c) = &e.code {
        s.push(' ');
        s.push_str(c);
    }
    s
}

/// The `server_errors` check's line about this pod's own 5xx answers since
/// `since`, the most frequent first: `POST /v1/taste/react → 502
/// taste_unreachable ×3 (Pixel 10 Pro)`. `None` without any.
pub fn server_errors_note(
    recent: &[RecentError],
    names: &HashMap<String, String>,
    since: i64,
) -> Option<String> {
    // (what) → (count, newest, devices in first-seen order)
    let mut groups: BTreeMap<String, (u64, i64, Vec<String>)> = BTreeMap::new();
    for e in recent.iter().filter(|e| e.at >= since && e.status >= 500) {
        let g = groups.entry(what(e)).or_default();
        g.0 += 1;
        g.1 = g.1.max(e.at);
        if let Some(d) = &e.device {
            let label = device_label(d, names);
            if !g.2.contains(&label) {
                g.2.push(label);
            }
        }
    }
    if groups.is_empty() {
        return None;
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
    let more = groups.len().saturating_sub(NAMED);
    let mut parts: Vec<String> = groups
        .into_iter()
        .take(NAMED)
        .map(|(w, (n, _, devices))| {
            let mut s = format!("{w} ×{n}");
            if !devices.is_empty() {
                s.push_str(&format!(" ({})", devices.join(", ")));
            }
            s
        })
        .collect();
    if more > 0 {
        parts.push(format!("and {more} more"));
    }
    Some(format!("in this pod: {}", parts.join("; ")))
}

/// The feed's data: one entity per failed request, newest first.
pub fn bundle(recent: &[RecentError], names: &HashMap<String, String>, now: i64) -> DataBundle {
    let mut b = DataBundle::default();
    let mut order = Vec::new();
    for (n, e) in recent.iter().enumerate() {
        let uri = format!("monitor://errors/{n}");
        let device = e.device.as_deref().map(|d| device_label(d, names));
        let ago = crate::devices::seen_text(now - e.at);
        b.entities.insert(
            uri.clone(),
            record(json!({
                "time": e.at,
                "time_text": ago,
                "method": e.method,
                "route": e.route,
                "status": e.status,
                "code": e.code.clone().unwrap_or_default(),
                "device": device.clone().unwrap_or_default(),
                "device_id": e.device.clone().unwrap_or_default(),
                "server_error": e.status >= 500,
                "title": what(e),
                "subtitle": match device {
                    Some(d) => format!("{ago} · {d}"),
                    None => ago,
                },
            })),
        );
        order.push(uri);
    }
    b.queries.insert(RECENT_ERRORS_QUERY.into(), order);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(at: i64, status: u16, code: Option<&str>, device: Option<&str>) -> RecentError {
        RecentError {
            at,
            method: "POST".into(),
            route: "/v1/taste/react".into(),
            status,
            code: code.map(str::to_owned),
            device: device.map(str::to_owned),
        }
    }

    #[test]
    fn the_note_names_the_most_frequent_failures() {
        let names = HashMap::from([("phone0000device01".to_owned(), "Pixel 10 Pro".to_owned())]);
        let mut recent = vec![
            e(
                100,
                502,
                Some("taste_unreachable"),
                Some("phone0000device01"),
            ),
            e(
                110,
                502,
                Some("taste_unreachable"),
                Some("phone0000device01"),
            ),
            e(
                120,
                502,
                Some("taste_unreachable"),
                Some("phone0000device01"),
            ),
            e(130, 500, None, Some("laptop000device02")),
            // 4xx and old ones are not server errors of the window.
            e(140, 409, Some("not_signed_in"), None),
            e(10, 503, None, None),
        ];
        recent.reverse();
        assert_eq!(
            server_errors_note(&recent, &names, 50).unwrap(),
            "in this pod: POST /v1/taste/react → 502 taste_unreachable ×3 (Pixel 10 Pro); \
             POST /v1/taste/react → 500 ×1 (device laptop00)"
        );
        recent.push(e(60, 504, None, None));
        assert!(
            server_errors_note(&recent, &names, 50)
                .unwrap()
                .ends_with("; and 1 more")
        );
        assert_eq!(server_errors_note(&recent, &names, 1000), None);

        let b = bundle(&recent, &names, 200);
        assert_eq!(b.queries[RECENT_ERRORS_QUERY].len(), recent.len());
        let first = &b.entities["monitor://errors/0"];
        assert_eq!(first.get_str("title"), Some("POST /v1/taste/react → 503"));
        let like = &b.entities["monitor://errors/5"];
        assert_eq!(like.get_str("device"), Some("Pixel 10 Pro"));
        assert_eq!(like.get_str("code"), Some("taste_unreachable"));
        assert_eq!(like.get_str("subtitle"), Some("1 min ago · Pixel 10 Pro"));
    }
}
