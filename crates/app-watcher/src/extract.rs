//! Extractors: a fetched page in, normalized events out. Each target names
//! one. Cheapest first:
//!
//! - `ics`: an iCalendar feed (`VEVENT`s).
//! - `jsonld`: schema.org `Event` markup in a page's JSON-LD (what most
//!   event sites, Meetup included, embed for search engines).
//! - `llm`: Claude reads the page's text and answers the events as JSON.
//!   The universal fallback for pages with no structure; it costs a model
//!   call, so it runs only when the page changed.

use serde_json::{Value, json};

use crate::html;
use crate::when::When;

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

/// The extractor kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ics,
    Jsonld,
    Llm,
}

impl Kind {
    pub const ALL: [&'static str; 3] = ["ics", "jsonld", "llm"];

    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim() {
            "ics" => Some(Kind::Ics),
            "jsonld" => Some(Kind::Jsonld),
            "llm" => Some(Kind::Llm),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Ics => "ics",
            Kind::Jsonld => "jsonld",
            Kind::Llm => "llm",
        }
    }
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
/// keeps its wall clock (no zone database), a `Z` time is UTC.
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
    let mut cur: Option<Vec<(String, String)>> = None;
    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(Vec::new());
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(props) = cur.take() {
                let get = |k: &str| {
                    props
                        .iter()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("")
                };
                if get("STATUS").eq_ignore_ascii_case("CANCELLED") {
                    continue;
                }
                if let Some(e) = event(
                    Some(get("UID").to_owned()),
                    &ics_unescape(get("SUMMARY")),
                    get("DTSTART"),
                    get("DTEND"),
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
            // `DTSTART;TZID=America/Denver` -> `DTSTART`.
            let name = key.split(';').next().unwrap_or("").to_ascii_uppercase();
            props.push((name, value.to_owned()));
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

/// Make `url` absolute against `base` (pages often give paths).
pub fn absolute(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    match reqwest::Url::parse(base).and_then(|b| b.join(url)) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u.to_string(),
        _ => String::new(),
    }
}

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

// ---------- LLM ----------

/// How the pod calls the model (`CONSTRUCT_INFER_*`, shared with
/// Collections; README).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub model: String,
    /// The Messages API endpoint (tests point it at a local fake).
    pub url: String,
}

/// The Messages API endpoint when `CONSTRUCT_INFER_URL` is unset.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
/// Default model of the `llm` extractor.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Most page text sent to the model.
pub const MAX_PAGE_CHARS: usize = 60_000;

const SYSTEM: &str = "You extract events from the text of a web page for a notification service. \
List every upcoming event the page announces: shows, performances, meetups, classes. Skip navigation, \
ads and events that are clearly over. For each, give the title as the page writes it, the start as \
ISO 8601 (YYYY-MM-DDTHH:MM, with the UTC offset only when the page states one; YYYY-MM-DD when no time \
is given), the end the same way or \"\", the venue, a description with everything the page says about \
the event (lineups, performers, prices), the event's own link if the page has one (else \"\"), and the \
page's own id for it if it shows one (else \"\"). Use \"\" for anything the page doesn't say; never \
guess a date. Answer {\"events\": []} when there are none.";

fn schema() -> Value {
    let s = json!({ "type": "string" });
    json!({
        "type": "object",
        "properties": {
            "events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": s, "start": s, "end": s, "venue": s,
                        "description": s, "url": s, "id": s,
                    },
                    "required": ["title", "start", "end", "venue", "description", "url", "id"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["events"],
        "additionalProperties": false,
    })
}

/// The request body for `page_text` of `url` (also what tests check).
pub fn llm_request(model: &str, page_text: &str, url: &str, hint: &str, today: &str) -> Value {
    let mut prompt = format!(
        "Today is {today}. The page is {url}.\n\n<page>\n{}\n</page>",
        html::truncate(page_text, MAX_PAGE_CHARS)
    );
    if !hint.trim().is_empty() {
        prompt.push_str(&format!("\n\nNotes about this page: {}", hint.trim()));
    }
    json!({
        "model": model,
        "max_tokens": 16000,
        "system": SYSTEM,
        "output_config": {
            "effort": "low",
            "format": { "type": "json_schema", "schema": schema() },
        },
        "fallbacks": "default",
        "messages": [{ "role": "user", "content": prompt }],
    })
}

/// Read the model's answer into events (dropping any without a title or a
/// date that parses).
pub fn llm_events(answer: &Value, base: &str) -> Result<Vec<RawEvent>, String> {
    match answer["stop_reason"].as_str() {
        Some("refusal") => return Err("the model declined to read this page".into()),
        Some("max_tokens") => return Err("the model's answer was cut off (too many events)".into()),
        _ => {}
    }
    let text: String = answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let parsed: Value = serde_json::from_str(text.trim())
        .map_err(|e| format!("the model's answer is not JSON: {e}"))?;
    let mut out = Vec::new();
    for e in parsed["events"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_EVENTS)
    {
        let s = |k: &str| e[k].as_str().unwrap_or("").to_owned();
        let url = absolute(base, &s("url"));
        let id = Some(s("id")).filter(|i| !i.trim().is_empty());
        if let Some(ev) = event(
            id,
            &s("title"),
            &s("start"),
            &s("end"),
            &s("venue"),
            &s("description"),
            &url,
        ) {
            out.push(ev);
        }
    }
    Ok(out)
}

/// Ask the model for the events of `page_text`.
pub async fn llm(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    page_text: &str,
    url: &str,
    hint: &str,
    today: &str,
) -> Result<Vec<RawEvent>, String> {
    let body = llm_request(&cfg.model, page_text, url, hint, today);
    let resp = client
        .post(&cfg.url)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2026-10-21")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("the model could not be reached: {e}"))?;
    let status = resp.status();
    let answer: Value = resp
        .json()
        .await
        .map_err(|e| format!("the model's answer could not be read: {e}"))?;
    if !status.is_success() {
        let msg = answer["error"]["message"].as_str().unwrap_or("");
        return Err(format!(
            "the model answered {status}: {}",
            html::truncate(msg, 200)
        ));
    }
    llm_events(&answer, url)
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

    #[test]
    fn llm_answers() {
        let answer = json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text":
            r#"{"events":[{"title":"Show","start":"2026-11-06T19:30","end":"","venue":"V","description":"d","url":"/x","id":""},
                          {"title":"Undated","start":"soon","end":"","venue":"","description":"","url":"","id":""}]}"# }] });
        let ev = llm_events(&answer, "https://a.example/").unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].url, "https://a.example/x");
        assert!(llm_events(&json!({"stop_reason": "refusal", "content": []}), "").is_err());
        let req = llm_request(
            "m",
            "text",
            "https://a.example/",
            "lineups are in the description",
            "2026-11-03",
        );
        assert_eq!(req["output_config"]["format"]["type"], "json_schema");
        assert!(
            req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("lineups")
        );
    }
}
