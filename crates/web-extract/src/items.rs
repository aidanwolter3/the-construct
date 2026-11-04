//! The generic extractors (decision `collection-fillers`): a fetched body in,
//! rows out. Each extractor finds the *items* of a page and reads one value
//! per output key out of each item with a *map*:
//!
//! | Extractor | Items | A map value is |
//! |---|---|---|
//! | `json` | the body as JSON, then `items` (a [`crate::path`], default the whole body) | a path into the item |
//! | `embedded-json` | the JSON in a page's `<script id="…">` (`script`, default `__NEXT_DATA__`), then `items` | a path |
//! | `jsonld` | the schema.org objects of the page's JSON-LD whose `@type` is one of `types` (`Product`, `Event`, …), nested ones included | a path (`offers.price`) |
//! | `rss` | RSS `<item>`s and Atom `<entry>`s as `{title, link, description, published, updated, id, author, category}` | a path |
//! | `ics` | iCalendar `VEVENT`s as their lowercase property names (`summary`, `dtstart`, `location`, …; dates as `2026-11-06T19:30-07:00`, a `TZID` resolved), cancelled ones skipped | a path |
//! | `events` | events of an iCalendar feed or a page's schema.org `Event`s, normalized ([`crate::events::items`]: `id`, `title`, `start`, `end`, `venue`, `description`, `url`, `tickets_url`) | a path |
//! | `selectors` | the elements matching the CSS selector `items` | a CSS selector inside the item: its text; `sel@attr`: an attribute; `@attr`: the item's own; `""`: the item's text |
//!
//! Links (`href`, `src`, and `url`/`link` values) are made absolute against
//! the page's URL.

use serde_json::{Map, Value, json};

use crate::fetch::absolute;
use crate::html;
use crate::path::{self, Path};

/// Most items read from one body.
pub const MAX_ITEMS: usize = 500;
/// Longest value kept (characters).
pub const MAX_VALUE_CHARS: usize = 4_000;
/// The script `embedded-json` reads without `script`.
pub const DEFAULT_SCRIPT: &str = "__NEXT_DATA__";

/// The extractor kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Events,
    Json,
    EmbeddedJson,
    Jsonld,
    Rss,
    Ics,
    Selectors,
}

impl Kind {
    pub const ALL: &[&str] = &[
        "events",
        "json",
        "embedded-json",
        "jsonld",
        "rss",
        "ics",
        "selectors",
    ];

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.trim() {
            "events" => Kind::Events,
            "json" => Kind::Json,
            "embedded-json" => Kind::EmbeddedJson,
            "jsonld" => Kind::Jsonld,
            "rss" => Kind::Rss,
            "ics" => Kind::Ics,
            "selectors" => Kind::Selectors,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Events => "events",
            Kind::Json => "json",
            Kind::EmbeddedJson => "embedded-json",
            Kind::Jsonld => "jsonld",
            Kind::Rss => "rss",
            Kind::Ics => "ics",
            Kind::Selectors => "selectors",
        }
    }
}

/// How to read a body: the extractor and its options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub kind: Kind,
    /// `json`/`embedded-json`: a path to the items; `selectors`: a CSS
    /// selector. Empty: the whole document (`selectors`: required).
    pub items: String,
    /// `jsonld`: the schema.org types to read (`Product`, `Event`).
    pub types: Vec<String>,
    /// `embedded-json`: the script's `id`.
    pub script: String,
    /// `events`, `ics`: the zone (IANA name or `±HH:MM`) a time without an
    /// offset is in. Empty: left without one.
    pub zone: String,
}

impl Spec {
    /// The zone [`Self::zone`] names.
    pub fn zone(&self) -> Option<crate::when::Zone> {
        let z = self.zone.trim();
        if z.is_empty() {
            return None;
        }
        if let Ok(Some(tz)) = crate::when::parse_time_zone(z) {
            return Some(crate::when::Zone::Named(tz));
        }
        crate::when::parse_utc_offset(z)
            .ok()
            .flatten()
            .map(crate::when::Zone::Fixed)
    }
}

/// Check `spec` and the map values `map` (`(key, path or selector)`).
pub fn check(spec: &Spec, map: &[(String, String)]) -> Result<(), String> {
    match spec.kind {
        Kind::Selectors => {
            if spec.items.trim().is_empty() {
                return Err(
                    "the selectors extractor needs \"items\": a CSS selector for each item".into(),
                );
            }
            css(&spec.items).map_err(|e| format!("items: {e}"))?;
            for (k, v) in map {
                let (sel, _) = split_attr(v);
                if !sel.is_empty() {
                    css(sel).map_err(|e| format!("map.{k}: {e}"))?;
                }
            }
        }
        _ => {
            path::parse(&spec.items).map_err(|e| format!("items: {e}"))?;
            for (k, v) in map {
                path::parse(v).map_err(|e| format!("map.{k}: {e}"))?;
            }
        }
    }
    if !spec.zone.trim().is_empty() && spec.zone().is_none() {
        return Err(format!(
            "zone '{}' is not a time zone (an IANA name such as America/Denver, or ±HH:MM)",
            spec.zone
        ));
    }
    if spec.kind == Kind::Jsonld && spec.types.is_empty() {
        return Err(
            "the jsonld extractor needs \"types\": the schema.org types to read (e.g. [\"Product\"])"
                .into(),
        );
    }
    if spec.kind == Kind::Jsonld
        && spec
            .types
            .iter()
            .any(|t| t.trim().is_empty() || t.len() > 60)
    {
        return Err("\"types\" are schema.org type names such as Product or Event".into());
    }
    Ok(())
}

/// Read the rows of `body` (fetched from `base`): one map per item, its
/// keys those of `map` that have a value.
pub fn extract(
    spec: &Spec,
    body: &str,
    base: &str,
    map: &[(String, String)],
) -> Result<Vec<Map<String, Value>>, String> {
    if spec.kind == Kind::Selectors {
        return selectors(body, base, &spec.items, map);
    }
    let items = match spec.kind {
        Kind::Json => {
            let doc: Value = serde_json::from_str(body.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("the body is not JSON: {e}"))?;
            pick(&doc, &spec.items)?
        }
        Kind::EmbeddedJson => {
            let id = if spec.script.trim().is_empty() {
                DEFAULT_SCRIPT
            } else {
                spec.script.trim()
            };
            let Some(text) = script_by_id(body, id) else {
                return Err(format!("the page has no <script id=\"{id}\">"));
            };
            let doc: Value = serde_json::from_str(text.trim())
                .map_err(|e| format!("<script id=\"{id}\"> is not JSON: {e}"))?;
            pick(&doc, &spec.items)?
        }
        Kind::Jsonld => jsonld(body, &spec.types),
        Kind::Rss => rss(body),
        Kind::Ics => ics_in(body, spec.zone().as_ref()),
        Kind::Events => {
            let found = crate::events::read(body, base);
            pick(
                &Value::Array(crate::events::items(found, spec.zone().as_ref(), base)),
                &spec.items,
            )?
        }
        Kind::Selectors => unreachable!("handled above"),
    };
    // (key, path, the path's last key: whether it names a link).
    let paths: Vec<(String, Path, String)> = map
        .iter()
        .map(|(k, v)| {
            let last = v
                .rsplit('.')
                .next()
                .unwrap_or("")
                .split('[')
                .next()
                .unwrap_or("")
                .to_owned();
            Ok((k.clone(), path::parse(v)?, last))
        })
        .collect::<Result<_, String>>()?;
    Ok(items
        .iter()
        .take(MAX_ITEMS)
        .map(|item| {
            let mut row = Map::new();
            for (k, p, last) in &paths {
                if let Some(v) = p.value(item) {
                    let link = is_link_key(k) || is_link_key(last);
                    row.insert(k.clone(), tidy(link, v, base));
                }
            }
            row
        })
        .filter(|r| !r.is_empty())
        .collect())
}

fn pick(doc: &Value, items: &str) -> Result<Vec<Value>, String> {
    Ok(path::parse(items)?.items(doc))
}

/// A value as kept: text decoded and capped; links absolute.
fn tidy(link: bool, v: Value, base: &str) -> Value {
    match v {
        Value::String(s) => {
            let s = html::decode_entities(s.trim());
            let s = if link && !s.contains(char::is_whitespace) {
                let a = absolute(base, &s);
                if a.is_empty() { s } else { a }
            } else {
                s
            };
            json!(html::truncate(&s, MAX_VALUE_CHARS))
        }
        other => other,
    }
}

fn is_link_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k == "url" || k == "link" || k.ends_with("_url") || k.ends_with("_link") || k == "href"
}

// ---------- embedded JSON ----------

/// The text of the `<script>` whose `id` is `id`.
pub fn script_by_id<'a>(page: &'a str, id: &str) -> Option<&'a str> {
    let lower = page.to_ascii_lowercase();
    let want = id.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<script") {
        let start = at + i;
        let gt = lower[start..].find('>')?;
        let open_end = start + gt + 1;
        let close = lower[open_end..].find("</script")?;
        let attrs = &lower[start..open_end];
        if [
            format!("id=\"{want}\""),
            format!("id='{want}'"),
            format!("id={want}"),
        ]
        .iter()
        .any(|a| attrs.contains(a.as_str()))
        {
            return Some(&page[open_end..open_end + close]);
        }
        at = open_end + close;
    }
    None
}

// ---------- JSON-LD ----------

fn type_matches(v: &Value, types: &[String]) -> bool {
    let one = |t: &str| {
        let t = t.rsplit(['/', '#', ':']).next().unwrap_or(t);
        types.iter().any(|w| w.eq_ignore_ascii_case(t))
    };
    match v {
        Value::String(t) => one(t),
        Value::Array(a) => a.iter().any(|t| t.as_str().is_some_and(one)),
        _ => false,
    }
}

fn walk_ld(v: &Value, types: &[String], depth: usize, out: &mut Vec<Value>) {
    if out.len() >= MAX_ITEMS || depth > 12 {
        return;
    }
    match v {
        Value::Array(a) => a.iter().for_each(|x| walk_ld(x, types, depth + 1, out)),
        Value::Object(o) => {
            if o.get("@type").is_some_and(|t| type_matches(t, types)) {
                out.push(v.clone());
                return;
            }
            for x in o.values() {
                walk_ld(x, types, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// The schema.org objects of `types` in `page`'s JSON-LD, nested ones
/// included (an object of a wanted type isn't looked into further).
/// Unparseable blocks are skipped.
pub fn jsonld(page: &str, types: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for block in html::ld_json_blocks(page) {
        if let Ok(v) = serde_json::from_str::<Value>(block.trim()) {
            walk_ld(&v, types, 0, &mut out);
        }
    }
    out
}

// ---------- RSS and Atom ----------

/// The elements `<tag ...>...</tag>` of `xml` (case-insensitive tag),
/// `(attributes, inner text)`.
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let lower = xml.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find(&open) {
        let start = at + i;
        let after = start + open.len();
        // `<item` must not match `<itemref`.
        if !matches!(
            lower.as_bytes().get(after),
            Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/')
        ) {
            at = after;
            continue;
        }
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + gt + 1;
        let attrs = &xml[after..open_end - 1];
        if attrs.trim_end().ends_with('/') {
            out.push((attrs, ""));
            at = open_end;
            continue;
        }
        let Some(c) = lower[open_end..].find(&close) else {
            break;
        };
        out.push((attrs, &xml[open_end..open_end + c]));
        at = open_end + c + close.len();
    }
    out
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let lower = attrs.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find(name) {
        let start = at + i;
        let rest = &attrs[start + name.len()..];
        let before_ok = start == 0 || lower.as_bytes()[start - 1].is_ascii_whitespace();
        if let Some(r) = rest.trim_start().strip_prefix('=')
            && before_ok
        {
            let r = r.trim_start();
            let q = r.chars().next()?;
            if q == '"' || q == '\'' {
                let body = &r[1..];
                return body.find(q).map(|e| &body[..e]);
            }
            return r.split_whitespace().next();
        }
        at = start + name.len();
    }
    None
}

/// An element's text: CDATA unwrapped, tags dropped, entities decoded.
fn xml_text(inner: &str) -> String {
    let t = inner.trim();
    let t = t
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
        .unwrap_or(t);
    if t.contains('<') {
        html::to_text(t)
    } else {
        html::decode_entities(t).trim().to_owned()
    }
}

/// The first `tag` child's text (empty: none).
fn child(inner: &str, tags: &[&str]) -> String {
    tags.iter()
        .find_map(|t| {
            elements(inner, t)
                .first()
                .map(|(_, x)| xml_text(x))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

/// The items of an RSS or Atom feed.
pub fn rss(xml: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut entries = elements(xml, "item");
    let atom = entries.is_empty();
    if atom {
        entries = elements(xml, "entry");
    }
    for (_, inner) in entries.into_iter().take(MAX_ITEMS) {
        let link = if atom {
            elements(inner, "link")
                .iter()
                .find(|(a, _)| attr(a, "rel").is_none_or(|r| r == "alternate"))
                .and_then(|(a, _)| attr(a, "href"))
                .map(str::to_owned)
                .unwrap_or_default()
        } else {
            child(inner, &["link"])
        };
        let category = elements(inner, "category")
            .iter()
            .map(|(a, x)| {
                let t = xml_text(x);
                if t.is_empty() {
                    attr(a, "term").unwrap_or("").to_owned()
                } else {
                    t
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(json!({
            "title": child(inner, &["title"]),
            "link": link,
            "description": child(inner, &["description", "summary", "content:encoded", "content"]),
            "published": child(inner, &["pubdate", "published", "dc:date"]),
            "updated": child(inner, &["updated", "pubdate"]),
            "id": child(inner, &["guid", "id"]),
            "author": child(inner, &["author", "dc:creator"]),
            "category": category,
        }));
    }
    out
}

// ---------- iCalendar ----------

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

/// The `VEVENT`s of an iCalendar body, each its properties by lowercase
/// name (the first of each; parameters dropped, but a date's `TZID`
/// resolves to its offset), cancelled ones skipped.
pub fn ics(text: &str) -> Vec<Value> {
    ics_in(text, None)
}

/// [`ics`], a time without an offset or zone placed in `zone`.
pub fn ics_in(text: &str, zone: Option<&crate::when::Zone>) -> Vec<Value> {
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
    let mut cur: Option<Map<String, Value>> = None;
    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(Map::new());
        } else if line.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(props) = cur.take()
                && !props
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
            {
                out.push(Value::Object(props));
                if out.len() >= MAX_ITEMS {
                    break;
                }
            }
        } else if let Some(props) = cur.as_mut()
            && let Some((key, value)) = line.split_once(':')
        {
            let mut parts = key.split(';');
            let name = parts.next().unwrap_or("").to_ascii_lowercase();
            if name.is_empty() || props.contains_key(&name) {
                continue;
            }
            let tzid = parts
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| k.eq_ignore_ascii_case("TZID"))
                .and_then(|(_, v)| crate::when::tzid_zone(v));
            let v = match crate::when::When::parse(value) {
                Some(w) if name.starts_with("dt") || name == "due" => {
                    let w = match (&tzid, zone) {
                        (Some(tz), _) => w.placed_in(&crate::when::Zone::Named(tz.clone())),
                        (None, Some(z)) => w.placed_in(z),
                        _ => w,
                    };
                    w.stored()
                }
                _ => ics_unescape(value),
            };
            props.insert(name, json!(v));
        }
    }
    out
}

// ---------- CSS selectors ----------

fn css(s: &str) -> Result<scraper::Selector, String> {
    scraper::Selector::parse(s.trim()).map_err(|e| format!("'{s}' is not a CSS selector ({e})"))
}

/// `"a.title@href"` -> (`"a.title"`, `Some("href")`); `"@href"` ->
/// (`""`, `Some("href")`).
fn split_attr(spec: &str) -> (&str, Option<&str>) {
    let spec = spec.trim();
    match spec.rfind('@') {
        Some(i)
            if !spec[i + 1..].is_empty()
                && spec[i + 1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':') =>
        {
            (spec[..i].trim(), Some(&spec[i + 1..]))
        }
        _ => (spec, None),
    }
}

fn element_value(
    el: scraper::ElementRef<'_>,
    attr_name: Option<&str>,
    base: &str,
) -> Option<String> {
    let v = match attr_name {
        Some(a) => {
            let v = el.value().attr(a)?.trim().to_owned();
            if matches!(a, "href" | "src") {
                let abs = absolute(base, &v);
                if abs.is_empty() { v } else { abs }
            } else {
                v
            }
        }
        None => el
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    };
    (!v.is_empty()).then(|| html::truncate(&v, MAX_VALUE_CHARS))
}

fn selectors(
    page: &str,
    base: &str,
    items: &str,
    map: &[(String, String)],
) -> Result<Vec<Map<String, Value>>, String> {
    let doc = scraper::Html::parse_document(page);
    let item_sel = css(items)?;
    let fields: Vec<(String, Option<scraper::Selector>, Option<String>)> = map
        .iter()
        .map(|(k, v)| {
            let (sel, a) = split_attr(v);
            let sel = if sel.is_empty() {
                None
            } else {
                Some(css(sel)?)
            };
            Ok((k.clone(), sel, a.map(str::to_owned)))
        })
        .collect::<Result<_, String>>()?;
    let mut out = Vec::new();
    for item in doc.select(&item_sel).take(MAX_ITEMS) {
        let mut row = Map::new();
        for (k, sel, a) in &fields {
            let v = match sel {
                Some(s) => item
                    .select(s)
                    .find_map(|el| element_value(el, a.as_deref(), base)),
                None => element_value(item, a.as_deref(), base),
            };
            if let Some(v) = v {
                row.insert(k.clone(), json!(v));
            }
        }
        if !row.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: Kind, items: &str) -> Spec {
        Spec {
            kind,
            items: items.into(),
            types: Vec::new(),
            script: String::new(),
            zone: String::new(),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn json_results() {
        let body = r#"{"races":[{"office":"Mayor","candidates":[{"name":"A","votes":10,"url":"/a"},{"name":"B","votes":7}]}]}"#;
        let rows = extract(
            &spec(Kind::Json, "races[*].candidates"),
            body,
            "https://results.example/api",
            &map(&[("name", "name"), ("votes", "votes"), ("url", "url")]),
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["votes"], json!(10));
        assert_eq!(rows[0]["url"], json!("https://results.example/a"));
        assert!(!rows[1].contains_key("url"));
        assert!(extract(&spec(Kind::Json, ""), "<html>", "", &[]).is_err());
    }

    #[test]
    fn embedded_and_jsonld_products() {
        let page = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"items":[{"t":"x"}]}}</script>
            <script type="application/ld+json">{"@context":"https://schema.org","@type":"WebPage","mainEntity":
              {"@type":"Product","name":"Monitor 27","offers":[{"@type":"Offer","price":"329.00","availability":"https://schema.org/InStock"}]}}</script></html>"#;
        let rows = extract(
            &spec(Kind::EmbeddedJson, "props.items"),
            page,
            "",
            &map(&[("title", "t")]),
        )
        .unwrap();
        assert_eq!(
            rows,
            vec![json!({"title": "x"}).as_object().unwrap().clone()]
        );
        let mut ld = spec(Kind::Jsonld, "");
        ld.types = vec!["Product".into()];
        let rows = extract(
            &ld,
            page,
            "",
            &map(&[
                ("name", "name"),
                ("price", "offers.price"),
                ("stock", "offers.availability"),
            ]),
        )
        .unwrap();
        assert_eq!(rows[0]["price"], json!("329.00"));
        assert_eq!(rows[0]["stock"], json!("https://schema.org/InStock"));
        assert!(
            check(&spec(Kind::Jsonld, ""), &[]).is_err(),
            "types are required"
        );
    }

    #[test]
    fn feeds_and_calendars() {
        let rss_body = r#"<rss><channel><title>Feed</title>
            <item><title><![CDATA[Results &amp; more]]></title><link>https://n.example/1</link>
              <pubDate>Tue, 03 Nov 2026 20:00:00 GMT</pubDate><category>Politics</category></item>
            <item><title>Two</title><link>/2</link></item></channel></rss>"#;
        let rows = extract(
            &spec(Kind::Rss, ""),
            rss_body,
            "https://n.example/feed",
            &map(&[
                ("title", "title"),
                ("link", "link"),
                ("when", "published"),
                ("tag", "category"),
            ]),
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["title"], json!("Results & more"));
        assert_eq!(rows[0]["tag"], json!("Politics"));
        assert_eq!(rows[1]["link"], json!("https://n.example/2"));
        let atom = r#"<feed><entry><title>A</title><link rel="alternate" href="https://a.example/x"/><updated>2026-11-03</updated></entry></feed>"#;
        let rows = extract(&spec(Kind::Rss, ""), atom, "", &map(&[("link", "link")])).unwrap();
        assert_eq!(rows[0]["link"], json!("https://a.example/x"));
        let cal = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Show\\, late\r\nDTSTART;TZID=America/Denver:20261106T193000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nSUMMARY:Gone\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let rows = extract(
            &spec(Kind::Ics, ""),
            cal,
            "",
            &map(&[("title", "summary"), ("start", "dtstart")]),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], json!("Show, late"));
        assert_eq!(
            rows[0]["start"],
            json!("2026-11-06T19:30-07:00"),
            "TZID resolved"
        );
    }

    #[test]
    fn events_from_calendars_and_pages() {
        let cal = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Brass  Lanterns\r\nDTSTART:20261106T193000\r\nLOCATION:The Back Room\r\n\
DESCRIPTION:[Get tickets](https://tix.example/1)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut s = spec(Kind::Events, "");
        s.zone = "America/Denver".into();
        check(&s, &[]).unwrap();
        let m = map(&[
            ("id", "id"),
            ("title", "title"),
            ("start", "start"),
            ("tickets", "tickets_url"),
        ]);
        let rows = extract(&s, cal, "https://club.example/cal.ics", &m).unwrap();
        assert_eq!(rows[0]["title"], json!("Brass Lanterns"));
        assert_eq!(
            rows[0]["start"],
            json!("2026-11-06T19:30-07:00"),
            "placed in the zone"
        );
        assert_eq!(rows[0]["tickets"], json!("https://tix.example/1"));
        let page = r#"<script type="application/ld+json">{"@type":"Event","name":"Show","startDate":"2026-11-07T20:00:00-07:00","location":{"name":"Hall"}}</script>"#;
        let rows = extract(&spec(Kind::Events, ""), page, "https://club.example/", &m).unwrap();
        assert!(
            rows[0]["id"].as_str().unwrap().starts_with("event-"),
            "{rows:?}"
        );
        let mut bad = spec(Kind::Events, "");
        bad.zone = "Mars/Olympus".into();
        assert!(check(&bad, &[]).is_err());
    }

    #[test]
    fn css_selectors() {
        let page = r#"<ul><li class="p"><a href="/p/1">Lamp</a> <span class="price"> $12.50 </span></li>
            <li class="p"><a href="/p/2">Desk</a></li></ul>"#;
        let s = spec(Kind::Selectors, "li.p");
        let m = map(&[("name", "a"), ("link", "a@href"), ("price", ".price")]);
        check(&s, &m).unwrap();
        let rows = extract(&s, page, "https://shop.example/c", &m).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["link"], json!("https://shop.example/p/1"));
        assert_eq!(rows[0]["price"], json!("$12.50"));
        assert!(!rows[1].contains_key("price"));
        assert!(check(&spec(Kind::Selectors, "li[[["), &[]).is_err());
        assert!(check(&spec(Kind::Selectors, ""), &[]).is_err());
        assert_eq!(split_attr("@data-id"), ("", Some("data-id")));
        assert_eq!(split_attr("a.b"), ("a.b", None));
    }
}
