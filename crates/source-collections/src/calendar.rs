//! Calendar feeds of Collections rows (decision `ical-feeds`): each feed
//! source names a collection, which of its rows go (`where`, as a view's),
//! the fields an event's start and end come from, and templates for its
//! title, location and description. Only those fields are read: no other
//! value of a row reaches the feed. `pod_core::ical` stores the feeds,
//! reviews them and writes the iCalendar text.
//!
//! A date with no time is an all-day event; a date with a time (a datetime
//! field, or a date field and a text `time` such as "8:00 AM" or "15:45")
//! is a moment in the pod's time zone, daylight saving included. A timed
//! event ends at its `end`, else its `end_time` that day (the next day when
//! that is earlier), else after `duration_minutes`, else an hour later.

use std::collections::HashMap;

use pod_core::Pod;
use pod_core::ical::{Event, EventTime};
use pod_core::tz::jiff::civil::Date;
use serde_json::{Value, json};
use web_extract::when::{When, Zone};

use crate::db::{self, Row};
use crate::model::{Condition, Definition, Field, Kind};

/// Longest title, location or description an event carries.
const MAX_TEXT: usize = 4000;
/// A timed event with no end lasts this long.
const DEFAULT_MINUTES: i64 = 60;
/// Longest `duration_minutes`: a week.
const MAX_MINUTES: i64 = 7 * 24 * 60;
/// The keys a source may have.
const KEYS: [&str; 10] = [
    "collection",
    "where",
    "start",
    "end",
    "time",
    "end_time",
    "duration_minutes",
    "title",
    "location",
    "description",
];

/// How long a timed event lasts without an end.
#[derive(Debug, Clone, PartialEq)]
enum Duration {
    Minutes(i64),
    /// A number field (its id).
    Field(String),
}

/// One source of a feed, checked against its collection.
#[derive(Debug, Clone, PartialEq)]
struct Source {
    collection: String,
    conditions: Vec<Condition>,
    /// Field ids.
    start: String,
    end: Option<String>,
    time: Option<String>,
    end_time: Option<String>,
    duration: Option<Duration>,
    title: String,
    location: Option<String>,
    description: Option<String>,
}

fn refuse(at: String, detail: impl Into<String>) -> Value {
    json!({ "error": "invalid_source", "pointer": at, "detail": detail.into() })
}

fn fields_list(def: &Definition) -> String {
    def.live_fields()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn field_of<'d>(
    def: &'d Definition,
    spec: &Value,
    key: &str,
    at: &str,
    kinds: &[Kind],
) -> Result<Option<&'d Field>, Value> {
    let Some(v) = spec.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let name = v.as_str().unwrap_or("").trim();
    let f = def.field(name).ok_or_else(|| {
        refuse(
            format!("{at}/{key}"),
            format!(
                "{key}: '{name}' is not a field of {} ({})",
                def.name,
                fields_list(def)
            ),
        )
    })?;
    if !kinds.contains(&f.kind) {
        return Err(refuse(
            format!("{at}/{key}"),
            format!(
                "{key}: '{}' is a {} field; it must be {}",
                f.name,
                f.kind.as_str(),
                kinds
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        ));
    }
    Ok(Some(f))
}

/// The `{name}` placeholders of a template.
fn placeholders(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        out.push(after[..close].trim());
        rest = &after[close + 1..];
    }
    out
}

fn template(def: &Definition, spec: &Value, key: &str, at: &str) -> Result<Option<String>, Value> {
    let Some(v) = spec.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(t) = v.as_str().map(str::trim) else {
        return Err(refuse(
            format!("{at}/{key}"),
            format!("{key} is a template such as \"{{title}}\""),
        ));
    };
    if t.chars().count() > 500 {
        return Err(refuse(
            format!("{at}/{key}"),
            format!("{key} is at most 500 characters"),
        ));
    }
    for p in placeholders(t) {
        if def.field(p).is_none() {
            return Err(refuse(
                format!("{at}/{key}"),
                format!(
                    "{key} names {{{p}}}, which is not a field of {} ({})",
                    def.name,
                    fields_list(def)
                ),
            ));
        }
    }
    Ok(Some(t.to_owned()))
}

fn parse(defs: &HashMap<String, Definition>, spec: &Value, at: &str) -> Result<Source, Value> {
    let Some(o) = spec.as_object() else {
        return Err(refuse(
            at.into(),
            "a source is {collection, start, title, ...}",
        ));
    };
    if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(refuse(
            format!("{at}/{k}"),
            format!("a source has no '{k}' (it has {})", KEYS.join(", ")),
        ));
    }
    let name = spec["collection"].as_str().unwrap_or("").trim();
    let def = defs.get(name).ok_or_else(|| {
        let mut names: Vec<&str> = defs.keys().map(String::as_str).collect();
        names.sort_unstable();
        refuse(
            format!("{at}/collection"),
            format!("'{name}' is not a collection ({})", names.join(", ")),
        )
    })?;
    let dates = [Kind::Date, Kind::Datetime];
    let clocks = [Kind::Text, Kind::Choice];
    let start = field_of(def, spec, "start", at, &dates)?.ok_or_else(|| {
        refuse(
            format!("{at}/start"),
            "start names a date or datetime field",
        )
    })?;
    let end = field_of(def, spec, "end", at, &dates)?;
    let time = field_of(def, spec, "time", at, &clocks)?;
    let end_time = field_of(def, spec, "end_time", at, &clocks)?;
    let duration = match spec.get("duration_minutes") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(Duration::Minutes(
            n.as_i64()
                .filter(|m| (1..=MAX_MINUTES).contains(m))
                .ok_or_else(|| {
                    refuse(
                        format!("{at}/duration_minutes"),
                        format!(
                            "duration_minutes is 1 to {MAX_MINUTES} minutes, or a number field"
                        ),
                    )
                })?,
        )),
        Some(Value::String(_)) => Some(Duration::Field(
            field_of(def, spec, "duration_minutes", at, &[Kind::Number])?
                .map(|f| f.id.clone())
                .unwrap_or_default(),
        )),
        Some(_) => {
            return Err(refuse(
                format!("{at}/duration_minutes"),
                "duration_minutes is a number of minutes or a number field",
            ));
        }
    };
    let items: Vec<&Value> = match spec.get("where") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(one @ Value::Object(_)) => vec![one],
        Some(_) => {
            return Err(refuse(
                format!("{at}/where"),
                "where is {field, equals} or a list of them",
            ));
        }
    };
    let mut conditions = Vec::new();
    for (i, c) in items.iter().enumerate() {
        let here = format!("{at}/where/{i}");
        let field = c["field"].as_str().unwrap_or("").trim();
        let f = def.field(field).ok_or_else(|| {
            refuse(
                format!("{here}/field"),
                format!(
                    "where: '{field}' is not a field of {} ({})",
                    def.name,
                    fields_list(def)
                ),
            )
        })?;
        let equals = match c.get("equals") {
            Some(Value::String(s)) => s.clone(),
            Some(v @ (Value::Bool(_) | Value::Number(_))) => v.to_string(),
            _ => {
                return Err(refuse(
                    format!("{here}/equals"),
                    "where needs equals (text, true, false or a number)",
                ));
            }
        };
        conditions.push(Condition {
            field: f.id.clone(),
            equals,
        });
    }
    let title = template(def, spec, "title", at)?
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            refuse(
                format!("{at}/title"),
                "title is a template such as \"{title}\"",
            )
        })?;
    Ok(Source {
        collection: def.name.clone(),
        conditions,
        start: start.id.clone(),
        end: end.map(|f| f.id.clone()),
        time: time.map(|f| f.id.clone()),
        end_time: end_time.map(|f| f.id.clone()),
        duration,
        title,
        location: template(def, spec, "location", at)?,
        description: template(def, spec, "description", at)?,
    })
}

/// A row's value of field `id` (its default when empty).
fn value<'r>(def: &'r Definition, row: &'r Row, id: &str) -> Option<&'r Value> {
    row.values
        .get(id)
        .filter(|v| !v.is_null())
        .or_else(|| def.by_id(id).and_then(|f| f.default.as_ref()))
        .filter(|v| !v.is_null())
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        other => crate::quantity::text_of(other),
    }
}

/// `t` with each `{name}` replaced by the row's value of that field.
fn render(def: &Definition, t: &str, row: &Row, lines: bool) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        if let Some(f) = def.field(after[..close].trim())
            && let Some(v) = value(def, row, &f.id)
        {
            out.push_str(&text(v));
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    // A title or a location is one line; a description keeps its lines.
    let one_line = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = if lines {
        out.lines()
            .map(one_line)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    } else {
        one_line(&out)
    };
    out.chars().take(MAX_TEXT).collect()
}

/// A wall-clock time as people write it: `15:45`, `09:00`, `8:00 AM`,
/// `8am`, `3:45 p.m.`, `noon`. `None` for anything else.
pub fn clock(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().to_ascii_lowercase().replace('.', "");
    match s.as_str() {
        "noon" => return Some((12, 0)),
        "midnight" => return Some((0, 0)),
        _ => {}
    }
    let (body, half) = if let Some(b) = s.strip_suffix("am") {
        (b.trim(), Some(false))
    } else if let Some(b) = s.strip_suffix("pm") {
        (b.trim(), Some(true))
    } else {
        (s.as_str(), None)
    };
    let mut parts = body.split(':');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let m: u32 = match parts.next() {
        Some(m) if m.len() == 2 => m.parse().ok()?,
        Some(_) => return None,
        // An hour alone only with am or pm ("8am").
        None if half.is_some() => 0,
        None => return None,
    };
    // Seconds are dropped.
    if let Some(sec) = parts.next() {
        sec.parse::<u32>().ok()?;
    }
    if parts.next().is_some() || m > 59 {
        return None;
    }
    let h = match half {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            (h % 12) + if pm { 12 } else { 0 }
        }
        None if h <= 23 => h,
        None => return None,
    };
    Some((h, m))
}

fn date_of(w: &When) -> Option<Date> {
    Date::new(
        i16::try_from(w.year).ok()?,
        i8::try_from(w.month).ok()?,
        i8::try_from(w.day).ok()?,
    )
    .ok()
}

/// The moment `date` at `(h, m)` on the wall clock of `zone`.
fn at(date: Date, (h, m): (u32, u32), zone: &Zone) -> i64 {
    When {
        year: i32::from(date.year()),
        month: date.month() as u32,
        day: date.day() as u32,
        time: Some((h, m)),
        offset_min: None,
    }
    .timestamp(Some(zone))
}

fn next_day(d: Date) -> Date {
    d.tomorrow().unwrap_or(d)
}

/// The event `row` of `def` maps to, if its start reads as a date.
fn event(src: &Source, def: &Definition, row: &Row, zone: &Zone) -> Option<Event> {
    let clock_of = |id: &Option<String>| {
        id.as_ref()
            .and_then(|id| value(def, row, id))
            .and_then(Value::as_str)
            .and_then(clock)
    };
    let w = When::parse(value(def, row, &src.start)?.as_str()?)?;
    let end_when = src
        .end
        .as_ref()
        .and_then(|id| value(def, row, id))
        .and_then(Value::as_str)
        .and_then(When::parse);
    // The start: a moment, or a whole day.
    let (start, day) = match w.time {
        Some(_) => {
            let t = w.timestamp(Some(zone));
            (EventTime::At(t), date_of(&w.shifted_to(Some(zone)))?)
        }
        None => {
            let d = date_of(&w)?;
            match clock_of(&src.time) {
                Some(c) => (EventTime::At(at(d, c, zone)), d),
                None => (EventTime::Day(d), d),
            }
        }
    };
    let end = match start {
        EventTime::Day(d) => {
            // An all-day end date is the last day it covers.
            let last = end_when
                .as_ref()
                .and_then(|e| date_of(&e.shifted_to(Some(zone))))
                .filter(|e| *e >= d)
                .unwrap_or(d);
            EventTime::Day(next_day(last))
        }
        EventTime::At(s) => {
            let end_clock = clock_of(&src.end_time);
            let from_end = end_when.and_then(|e| match e.time {
                Some(_) => Some(e.timestamp(Some(zone))),
                None => end_clock.and_then(|c| Some(at(date_of(&e)?, c, zone))),
            });
            let from_clock = || {
                end_clock.map(|c| {
                    let t = at(day, c, zone);
                    // Ends earlier on the clock than it starts: the next day.
                    if t <= s {
                        at(next_day(day), c, zone)
                    } else {
                        t
                    }
                })
            };
            let minutes = match &src.duration {
                Some(Duration::Minutes(m)) => Some(*m),
                Some(Duration::Field(id)) => value(def, row, id)
                    .and_then(Value::as_f64)
                    .map(|m| m.round() as i64)
                    .filter(|m| (1..=MAX_MINUTES).contains(m)),
                None => None,
            };
            let t = from_end
                .or_else(from_clock)
                .or(minutes.map(|m| s + m * 60))
                .filter(|t| *t > s)
                .unwrap_or(s + DEFAULT_MINUTES * 60);
            EventTime::At(t)
        }
    };
    let opt = |t: &Option<String>, lines: bool| {
        t.as_ref()
            .map(|t| render(def, t, row, lines))
            .filter(|s| !s.is_empty())
    };
    let summary = render(def, &src.title, row, false);
    Some(Event {
        key: format!("{}/{}", src.collection, row.id),
        summary: if summary.is_empty() {
            def.title.clone()
        } else {
            summary
        },
        location: opt(&src.location, false),
        description: opt(&src.description, true),
        start,
        end,
        created: row.created_at,
        modified: row.updated_at,
    })
}

/// The events of a feed's `sources` from the pod's rows (any order).
fn events_on(
    conn: &rusqlite::Connection,
    sources: &[Value],
) -> rusqlite::Result<Result<Vec<Event>, Value>> {
    let defs: HashMap<String, Definition> = db::defs_by_name(conn)?.into_iter().collect();
    let zone = crate::views::zone_on(conn)?;
    let mut parsed = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        match parse(&defs, s, &format!("/sources/{i}")) {
            Ok(p) => parsed.push(p),
            Err(e) => return Ok(Err(e)),
        }
    }
    let mut out = Vec::new();
    for src in &parsed {
        let def = &defs[&src.collection];
        for row in db::rows(conn, def)? {
            if src.conditions.iter().all(|c| c.matches(def, &row.values))
                && let Some(e) = event(src, def, &row, &zone)
            {
                out.push(e);
            }
        }
    }
    Ok(Ok(out))
}

/// [`pod_core::Source::calendar_events`] for Collections.
pub async fn events(pod: &Pod, sources: &[Value]) -> Result<Vec<Event>, Value> {
    let sources = sources.to_vec();
    match pod
        .blocking(move |s| s.with_conn(|c| events_on(c, &sources)))
        .await
    {
        Ok(r) => r,
        Err(_) => {
            Err(json!({ "error": "store_error", "detail": "the collections could not be read" }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_read_both_ways_people_write_them() {
        assert_eq!(clock("8:00 AM"), Some((8, 0)));
        assert_eq!(clock("8:00am"), Some((8, 0)));
        assert_eq!(clock("8am"), Some((8, 0)));
        assert_eq!(clock("12:30 PM"), Some((12, 30)));
        assert_eq!(clock("12:15 a.m."), Some((0, 15)));
        assert_eq!(clock("3:45 pm"), Some((15, 45)));
        assert_eq!(clock("15:45"), Some((15, 45)));
        assert_eq!(clock("09:00"), Some((9, 0)));
        assert_eq!(clock("09:00:30"), Some((9, 0)));
        assert_eq!(clock("noon"), Some((12, 0)));
        for bad in ["", "8", "25:00", "13:00 pm", "8:5", "soon", "8:60"] {
            assert_eq!(clock(bad), None, "{bad}");
        }
    }
}
