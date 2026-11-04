//! Events: the extractors the Watcher had, now an extractor of fetch
//! fillers (`extract: "events"`, decision `collection-fillers`). A page
//! in, normalized events out: from an iCalendar feed (`VEVENT`s, a `TZID`
//! resolved to its offset on the day) or from a page's schema.org `Event`
//! JSON-LD (through `@graph`, `ItemList`s and nested events; Meetup and
//! most event sites embed it). Cancelled events are skipped.
//!
//! [`items`] turns them into the items a map reads: `id` (the source's own
//! id, else a hash of the title, date and venue), `title`, `start` and `end`
//! (`2026-11-06T19:30-07:00`; a time without an offset placed in the
//! filler's zone), `venue`, `description` (plain text: HTML and Markdown
//! reduced), `url` and `tickets_url` (a "tickets" link in the description).

use serde_json::{Value, json};

use crate::html;
use crate::when::{When, Zone, tzid_zone};

/// An event as an extractor found it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    /// The source's own id (an iCalendar `UID`, a JSON-LD `@id`/`url`).
    pub source_id: Option<String>,
    pub title: String,
    pub start: When,
    pub end: Option<When>,
    pub venue: String,
    pub description: String,
    pub url: String,
}

/// Most events kept from one page.
pub const MAX_EVENTS: usize = 200;
/// Longest description kept.
pub const MAX_DESCRIPTION: usize = 4_000;

fn clean(s: &str, max: usize) -> String {
    html::truncate(s.trim(), max)
}

/// Build an event; `None` without a title or a readable start.
fn event(
    source_id: Option<String>,
    title: &str,
    start: &str,
    end: &str,
    venue: &str,
    description: &str,
    url: &str,
) -> Option<RawEvent> {
    let title = clean(&html::decode_entities(title), 300);
    let start = When::parse(start)?;
    if title.is_empty() {
        return None;
    }
    Some(RawEvent {
        source_id: source_id.filter(|s| !s.trim().is_empty()),
        title,
        start,
        end: When::parse(end),
        venue: clean(&html::decode_entities(venue), 300),
        description: clean(description, MAX_DESCRIPTION),
        url: clean(url, 2_000),
    })
}

// ---------- ics ----------

fn ics_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The events of an iCalendar feed. Lines are unfolded; `DTSTART;TZID=…`
/// keeps its wall clock with that zone's offset at the time (an unknown
/// zone, such as a Windows name, leaves it to the target's), a `Z` time is
/// UTC.
pub fn ics(text: &str) -> Vec<RawEvent> {
    // Unfold: a line starting with a space or tab continues the previous.
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = raw.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(cont);
            }
        } else {
            lines.push(raw.to_owned());
        }
    }
    let mut out = Vec::new();
    // (name, TZID parameter, value)
    let mut cur: Option<Vec<(String, String, String)>> = None;
    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(Vec::new());
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(props) = cur.take() {
                let find = |k: &str| props.iter().find(|(n, _, _)| n == k);
                let get = |k: &str| find(k).map(|(_, _, v)| v.as_str()).unwrap_or("");
                // `DTSTART;TZID=America/Denver:20261106T193000` ->
                // `2026-11-06T19:30-07:00`.
                let date = |k: &str| match find(k) {
                    Some((_, tzid, v)) => match (tzid_zone(tzid), When::parse(v)) {
                        (Some(tz), Some(w)) => w.placed_in(&Zone::Named(tz)).stored(),
                        _ => v.clone(),
                    },
                    None => String::new(),
                };
                if get("STATUS").eq_ignore_ascii_case("CANCELLED") {
                    continue;
                }
                if let Some(e) = event(
                    Some(get("UID").to_owned()),
                    &ics_unescape(get("SUMMARY")),
                    &date("DTSTART"),
                    &date("DTEND"),
                    &ics_unescape(get("LOCATION")),
                    &ics_unescape(get("DESCRIPTION")),
                    get("URL"),
                ) {
                    out.push(e);
                }
            }
            continue;
        }
        if let Some(props) = cur.as_mut()
            && let Some((key, value)) = line.split_once(':')
        {
            // `DTSTART;TZID=America/Denver` -> `DTSTART`, `America/Denver`.
            let mut parts = key.split(';');
            let name = parts.next().unwrap_or("").to_ascii_uppercase();
            let tzid = parts
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| k.eq_ignore_ascii_case("TZID"))
                .map(|(_, v)| v.to_owned())
                .unwrap_or_default();
            props.push((name, tzid, value.to_owned()));
        }
        if out.len() >= MAX_EVENTS {
            break;
        }
    }
    out
}

// ---------- JSON-LD ----------

fn is_event_type(v: &Value) -> bool {
    let one = |t: &str| t == "Event" || t.ends_with("Event") && !t.contains(':');
    match v {
        Value::String(t) => one(t),
        Value::Array(a) => a.iter().any(|t| t.as_str().is_some_and(one)),
        _ => false,
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.first().map(text_of).unwrap_or_default(),
        Value::Object(o) => o
            .get("name")
            .or(o.get("@value"))
            .map(text_of)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn venue_of(v: &Value) -> String {
    match v {
        Value::Array(a) => a.first().map(venue_of).unwrap_or_default(),
        Value::Object(o) => {
            let name = o.get("name").map(text_of).unwrap_or_default();
            if !name.trim().is_empty() {
                return name;
            }
            match o.get("address") {
                Some(Value::Object(a)) => ["streetAddress", "addressLocality"]
                    .iter()
                    .filter_map(|k| a.get(*k).map(text_of))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
                Some(other) => text_of(other),
                None => String::new(),
            }
        }
        other => text_of(other),
    }
}

use crate::fetch::absolute;

fn walk_ld(v: &Value, base: &str, out: &mut Vec<RawEvent>) {
    if out.len() >= MAX_EVENTS {
        return;
    }
    match v {
        Value::Array(a) => a.iter().for_each(|x| walk_ld(x, base, out)),
        Value::Object(o) => {
            if o.get("@type").is_some_and(is_event_type) {
                let url = absolute(base, &o.get("url").map(text_of).unwrap_or_default());
                let id = o
                    .get("@id")
                    .map(text_of)
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!url.is_empty()).then(|| url.clone()));
                let status = o.get("eventStatus").map(text_of).unwrap_or_default();
                if !status.ends_with("EventCancelled")
                    && let Some(e) = event(
                        id,
                        &o.get("name").map(text_of).unwrap_or_default(),
                        &o.get("startDate").map(text_of).unwrap_or_default(),
                        &o.get("endDate").map(text_of).unwrap_or_default(),
                        &o.get("location").map(venue_of).unwrap_or_default(),
                        &html::to_text(&o.get("description").map(text_of).unwrap_or_default()),
                        &url,
                    )
                {
                    out.push(e);
                }
                return;
            }
            for k in [
                "@graph",
                "itemListElement",
                "item",
                "event",
                "events",
                "subEvent",
            ] {
                if let Some(x) = o.get(k) {
                    walk_ld(x, base, out);
                }
            }
        }
        _ => {}
    }
}

/// The schema.org events in `page`'s JSON-LD (`@graph`, `ItemList`s and
/// nested `event`s included). Unparseable blocks are skipped.
pub fn jsonld(page: &str, base: &str) -> Vec<RawEvent> {
    let mut out = Vec::new();
    for block in html::ld_json_blocks(page) {
        if let Ok(v) = serde_json::from_str::<Value>(block.trim()) {
            walk_ld(&v, base, &mut out);
        }
    }
    out
}

/// Read the events of `body` (an iCalendar feed, else a page's JSON-LD).
pub fn read(body: &str, base: &str) -> Vec<RawEvent> {
    if body.trim_start().starts_with("BEGIN:VCALENDAR") {
        ics(body)
    } else {
        jsonld(body, base)
    }
}

/// A stable id for an event without one: its title, date and venue.
fn derived_id(e: &RawEvent) -> String {
    use std::hash::{Hash, Hasher};
    let norm = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        norm(&e.title),
        e.start.iso().get(..10).unwrap_or(""),
        norm(&e.venue),
    )
        .hash(&mut h);
    format!("event-{:016x}", h.finish())
}

/// The events as items: one JSON object each (see the module docs). A
/// time without an offset is placed in `zone` when there is one; links are
/// made absolute against `base` (the page's URL).
pub fn items(events: Vec<RawEvent>, zone: Option<&Zone>, base: &str) -> Vec<Value> {
    events
        .into_iter()
        .map(|e| {
            let place = |w: When| zone.map_or(w, |z| w.placed_in(z)).stored();
            let description = html::markdown_to_text(&e.description);
            json!({
                "id": e.source_id.clone().unwrap_or_else(|| derived_id(&e)),
                "title": html::clean_title(&e.title),
                "start": place(e.start),
                "end": e.end.map(place),
                "venue": e.venue,
                "tickets_url": html::tickets_url(&e.description),
                "description": description,
                "url": if e.url.is_empty() { String::new() } else {
                    let a = crate::fetch::absolute(base, &e.url);
                    if a.is_empty() { e.url.clone() } else { a }
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ics_feed() {
        let feed = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc@x\r\nSUMMARY:Budget Cuts\\, live\r\n\
DTSTART;TZID=America/Denver:20261106T193000\r\nLOCATION:The End\\nLafayette\r\nDESCRIPTION:House te\r\n am night\r\n\
END:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Cancelled\r\nSTATUS:CANCELLED\r\nDTSTART:20261107\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nSUMMARY:No date\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let ev = ics(feed);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "Budget Cuts, live");
        assert_eq!(ev[0].start.iso(), "2026-11-06T19:30");
        assert_eq!(ev[0].venue, "The End\nLafayette");
        assert_eq!(ev[0].description, "House team night");
        assert_eq!(ev[0].source_id.as_deref(), Some("abc@x"));
        assert_eq!(ev[0].start.stored(), "2026-11-06T19:30-07:00");
    }

    #[test]
    fn ics_tzid_follows_daylight_saving() {
        let feed = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\nUID:jul\r\nSUMMARY:July\r\nDTSTART;TZID=America/Denver:20260710T193000\r\n\
DTEND;TZID=America/Denver:20260710T213000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:dec\r\nSUMMARY:December\r\nDTSTART;TZID=America/Denver:20261210T193000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:win\r\nSUMMARY:Windows zone\r\nDTSTART;TZID=Mountain Standard Time:20261210T193000\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let ev = ics(feed);
        let utc = |s: &str| When::parse(s).unwrap().timestamp(None);
        // 7:30 PM MDT (UTC-6) and 7:30 PM MST (UTC-7).
        assert_eq!(ev[0].start.timestamp(None), utc("2026-07-11T01:30Z"));
        assert_eq!(ev[0].end.unwrap().stored(), "2026-07-10T21:30-06:00");
        assert_eq!(ev[1].start.timestamp(None), utc("2026-12-11T02:30Z"));
        assert_eq!(ev[1].start.time_text().as_deref(), Some("7:30 PM"));
        // A zone we can't resolve keeps its wall clock for the target's zone.
        assert_eq!(ev[2].start.stored(), "2026-12-10T19:30");
        // A `Z` time on a Denver target shows in Denver time.
        let z = ics("BEGIN:VEVENT\r\nSUMMARY:Z\r\nDTSTART:20260711T013000Z\r\nEND:VEVENT\r\n");
        let shown = z[0].start.shifted_to(Some(&Zone::Named(
            pod_core::tz::parse("America/Denver").unwrap(),
        )));
        assert_eq!(
            (shown.date_text(), shown.time_text()),
            ("Fri Jul 10".into(), Some("7:30 PM".into()))
        );
    }

    #[test]
    fn jsonld_events() {
        let page = r#"<script type="application/ld+json">{"@context":"https://schema.org","@graph":[
            {"@type":"Organization","name":"x"},
            {"@type":"ItemList","itemListElement":[{"@type":"ListItem","item":{"@type":"SocialEvent",
              "name":"Improv Night","startDate":"2026-11-06T19:30:00-07:00","url":"/e/1",
              "location":{"@type":"Place","name":"The End","address":{"addressLocality":"Lafayette"}},
              "description":"<p>Budget Cuts &amp; more</p>"}}]},
            {"@type":"Event","name":"Gone","startDate":"2026-11-07","eventStatus":"https://schema.org/EventCancelled"}]}
            </script>"#;
        let ev = jsonld(page, "https://example.org/group/events/");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].url, "https://example.org/e/1");
        assert_eq!(ev[0].source_id.as_deref(), Some("https://example.org/e/1"));
        assert_eq!(ev[0].venue, "The End");
        assert_eq!(ev[0].description, "Budget Cuts & more");
    }
}
