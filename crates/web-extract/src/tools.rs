//! The pod's own web tools for research tasks (decision
//! `web-and-agent-modules`): `web_search` and `web_fetch` as client tools the
//! agent module runs ([`construct_agent::Tool`]), so every page an agent
//! reads goes through the pod's host policy, cache and block detection, and
//! searches use the pod's own `web_search` jack.
//!
//! `web_fetch` reads only pages the task has already seen (decision
//! `web-fetch-provenance`): a URL in the request, a search result, or a
//! link or URL on a page it read. A page can't have the model carry the
//! task's data (a collection's rows) out in a URL it writes itself.
//!
//! What the tools read is kept with the URLs, so an answer's citations can be
//! checked against it ([`Seen::check_rows`], decision `research-per-row`): a
//! link must be one the task saw, and a quote must be on a page it read.
//! `web_fetch` also lists a page's main pictures (its preview image and its
//! JSON-LD product images), so a product's photo is the store's own.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use construct_agent::Tool;
use pod_core::BoxFuture;
use serde_json::{Map, Value, json};

use crate::hosts::Hosts;
use crate::search::{Provider, SearchCache};

/// Most page text a fetch hands the model.
pub const MAX_FETCH_CHARS: usize = 12_000;
/// Most text one task keeps of what it read, for checking quotes.
pub const MAX_READ_CHARS: usize = 2_000_000;
/// Most pictures a fetch lists.
pub const MAX_FETCH_IMAGES: usize = 4;
/// A quote this many words long or longer must be on a page the task read.
pub const MIN_QUOTE_WORDS: usize = 3;

/// Most links a fetch lists after the page's text.
pub const MAX_FETCH_LINKS: usize = 40;

/// What the tools share: the pod's host policy and search cache, an HTTP
/// client, the search provider, and the URLs this task has seen.
#[derive(Clone)]
pub struct Web {
    pub hosts: Arc<Hosts>,
    pub cache: Arc<SearchCache>,
    pub client: pod_core::net::OutboundClient,
    pub provider: Provider,
    pub allow_private: bool,
    /// What `web_fetch` may read: seed it with the request's text
    /// ([`Seen::add_text`]); search results and fetched pages add theirs.
    pub seen: Seen,
    /// The pod whose paid use counts the provider's requests (decision
    /// `monthly-paid-use`); none counts nothing.
    pub meter: Option<pod_core::Pod>,
}

impl Web {
    /// The store that counts this pod's searches.
    pub fn meter(&self) -> Option<&pod_core::store::Store> {
        self.meter.as_ref().map(|p| p.store())
    }
}

/// The URLs one research task has seen (link provenance): only these can
/// be fetched. Shared by the task's tools, with the text they read.
#[derive(Clone, Default, Debug)]
pub struct Seen {
    urls: Arc<Mutex<HashSet<String>>>,
    /// What the task read (search results and pages), normalized
    /// ([`quote_text`]), at most [`MAX_READ_CHARS`].
    read: Arc<Mutex<String>>,
}

/// What [`Seen::check_rows`] took out of an answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cited {
    /// Links the task never saw (a whole value, or a line of one).
    pub urls_dropped: usize,
    /// Lines quoting text that isn't on any page the task read.
    pub quotes_dropped: usize,
}

impl Seen {
    /// Add one URL.
    pub fn add(&self, url: &str) {
        if let Some(k) = seen_key(url) {
            self.urls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(k);
        }
    }

    /// Keep `text` as read: its quotes count as found.
    pub fn add_read(&self, text: &str) {
        let mut r = self.read.lock().unwrap_or_else(|e| e.into_inner());
        if r.len() >= MAX_READ_CHARS {
            return;
        }
        r.push('\n');
        r.push_str(&quote_text(text));
    }

    /// Whether `quote` is on a page (or in a search result) the task read,
    /// spacing, case and punctuation aside.
    pub fn read_has(&self, quote: &str) -> bool {
        let q = quote_text(quote);
        !q.is_empty()
            && self
                .read
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&q)
    }

    /// Check an answer's citations in place: a value that is a link the task
    /// never saw becomes null; in other text, a line holding such a link, or
    /// quoting [`MIN_QUOTE_WORDS`] or more words that are on no page it read,
    /// is left out (the value becomes null when no line is left).
    pub fn check_rows(&self, rows: &mut [Map<String, Value>]) -> Cited {
        let mut out = Cited::default();
        for row in rows.iter_mut() {
            for v in row.values_mut() {
                let Some(s) = v.as_str().map(str::trim).map(str::to_owned) else {
                    continue;
                };
                if !s.contains(char::is_whitespace)
                    && (s.starts_with("http://") || s.starts_with("https://"))
                {
                    if !self.contains(&s) {
                        *v = Value::Null;
                        out.urls_dropped += 1;
                    }
                    continue;
                }
                let mut kept = Vec::new();
                let mut changed = false;
                for line in s.lines() {
                    if urls_in(line).iter().any(|u| !self.contains(u)) {
                        out.urls_dropped += 1;
                        changed = true;
                        continue;
                    }
                    if quotes_in(line).iter().any(|q| {
                        q.split_whitespace().count() >= MIN_QUOTE_WORDS && !self.read_has(q)
                    }) {
                        out.quotes_dropped += 1;
                        changed = true;
                        continue;
                    }
                    kept.push(line);
                }
                if changed {
                    let t = kept.join("\n").trim().to_owned();
                    *v = if t.is_empty() { Value::Null } else { json!(t) };
                }
            }
        }
        out
    }

    /// Add every `http(s)` URL written in `text`.
    pub fn add_text(&self, text: &str) {
        for u in urls_in(text) {
            self.add(u);
        }
    }

    /// Whether `url` was seen (fragments and a trailing slash aside).
    pub fn contains(&self, url: &str) -> bool {
        seen_key(url).is_some_and(|k| {
            self.urls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&k)
        })
    }
}

/// A URL as [`Seen`] compares it: parsed (scheme and host lowercase), no
/// fragment, no trailing slash. The path and query stay exact: a changed
/// one is a new URL.
fn seen_key(url: &str) -> Option<String> {
    let mut u = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.set_fragment(None);
    Some(u.as_str().trim_end_matches('/').to_owned())
}

/// The `http(s)` URLs written in `text` (prose, JSON or markup).
pub fn urls_in(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for word in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '`'
            )
    }) {
        let Some(at) = word.find("http://").or_else(|| word.find("https://")) else {
            continue;
        };
        let u = word[at..].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if !u.is_empty() {
            out.push(u);
        }
    }
    out
}

/// Text as quotes are compared: lowercase letters and digits, every other
/// run as one space (curly quotes, dashes and spacing don't matter).
pub fn quote_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_owned()
}

/// The quoted passages of `line`: between straight or curly double quotes.
fn quotes_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for c in line.chars() {
        let open = matches!(c, '"' | '“');
        let close = matches!(c, '"' | '”');
        match &mut cur {
            Some(q) if close => {
                out.push(std::mem::take(q));
                cur = None;
            }
            Some(q) => q.push(c),
            None if open => cur = Some(String::new()),
            None => {}
        }
    }
    out
}

/// A page's main pictures, absolute against `base`: its preview image
/// (`og:image`, `twitter:image`, `image_src`) and the `image` of its JSON-LD
/// objects, each once, at most `max`.
pub fn page_images(html: &str, base: &str, max: usize) -> Vec<String> {
    let doc = scraper::Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    let push = |u: &str, out: &mut Vec<String>| {
        let u = crate::fetch::absolute(base, u.trim());
        if (u.starts_with("https://") || u.starts_with("http://"))
            && !out.contains(&u)
            && out.len() < max
        {
            out.push(u);
        }
    };
    for sel in [
        r#"meta[property="og:image"]"#,
        r#"meta[property="og:image:secure_url"]"#,
        r#"meta[name="twitter:image"]"#,
    ] {
        if let Ok(s) = scraper::Selector::parse(sel) {
            for m in doc.select(&s) {
                if let Some(c) = m.value().attr("content") {
                    push(c, &mut out);
                }
            }
        }
    }
    if let Ok(s) = scraper::Selector::parse(r#"link[rel="image_src"]"#) {
        for m in doc.select(&s) {
            if let Some(h) = m.value().attr("href") {
                push(h, &mut out);
            }
        }
    }
    fn images(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| images(x, out)),
            Value::Object(o) => {
                if let Some(u) = o.get("url").or(o.get("contentUrl")) {
                    images(u, out);
                }
            }
            _ => {}
        }
    }
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => {
                if let Some(i) = o.get("image") {
                    images(i, out);
                }
                if let Some(g) = o.get("@graph") {
                    walk(g, out);
                }
            }
            _ => {}
        }
    }
    if let Ok(s) = scraper::Selector::parse(r#"script[type="application/ld+json"]"#) {
        for m in doc.select(&s) {
            let text: String = m.text().collect();
            if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
                let mut found = Vec::new();
                walk(&v, &mut found);
                for u in found {
                    push(&u, &mut out);
                }
            }
        }
    }
    out
}

/// The page's links (`<a href>`), made absolute against `base`: `(text,
/// url)`, each URL once, at most `max`.
fn page_links(html: &str, base: &str, max: usize) -> Vec<(String, String)> {
    let doc = scraper::Html::parse_document(html);
    let Ok(sel) = scraper::Selector::parse("a[href]") else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in doc.select(&sel) {
        let url = crate::fetch::absolute(base, a.value().attr("href").unwrap_or(""));
        let Some(k) = seen_key(&url) else { continue };
        if k == seen_key(base).unwrap_or_default() || !seen.insert(k) {
            continue;
        }
        let text = a.text().collect::<Vec<_>>().join(" ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push((crate::html::truncate(&text, 80), url));
        if out.len() >= max {
            break;
        }
    }
    out
}

/// The longest `web_search` query, in characters: real queries are a few
/// words, and a long one can carry rows out to the search provider.
pub const MAX_QUERY_CHARS: usize = 300;

/// A query holding a run of this many characters of a row's text is
/// refused: it quotes the row rather than searching for it.
const ROW_RUN_CHARS: usize = 40;

/// `web_search {query}`: at most `max` uses.
pub struct SearchTool {
    web: Web,
    left: AtomicU32,
    /// The text of the request's current rows (decision
    /// `web-fetch-provenance`): a query can't repeat a long run of it.
    rows: Vec<String>,
}

impl SearchTool {
    pub fn new(web: Web, max: u32) -> Self {
        Self {
            web,
            left: AtomicU32::new(max),
            rows: Vec::new(),
        }
    }

    /// Refuse queries that repeat a long run of `rows`' text (every string
    /// value of each row).
    pub fn with_rows(mut self, rows: &[serde_json::Map<String, Value>]) -> Self {
        self.rows = row_texts(rows);
        self
    }
}

/// The long string values of `rows`, normalized.
fn row_texts(rows: &[serde_json::Map<String, Value>]) -> Vec<String> {
    rows.iter()
        .flat_map(|r| r.values())
        .filter_map(Value::as_str)
        .map(normalize)
        .filter(|t| t.chars().count() >= ROW_RUN_CHARS)
        .collect()
}

/// Lowercase text with its whitespace collapsed, to compare a query with
/// a row's values.
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Why `query` may not be sent to the search provider, if it may not: too
/// long, or it repeats a long run of a current row's text.
fn refuse_query(query: &str, rows: &[String]) -> Option<String> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Some(format!(
            "that query is too long (at most {MAX_QUERY_CHARS} characters): search with a few key words"
        ));
    }
    let q = normalize(query);
    for row in rows {
        let chars: Vec<char> = row.chars().collect();
        let hit = chars
            .windows(ROW_RUN_CHARS)
            .any(|w| q.contains(&w.iter().collect::<String>()));
        if hit {
            return Some(
                "that query repeats the text of a current row: search with a few key words, not the row's contents"
                    .into(),
            );
        }
    }
    None
}

/// Take one use, if any is left.
fn take(left: &AtomicU32) -> bool {
    let mut n = left.load(Ordering::SeqCst);
    while n > 0 {
        match left.compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => n = now,
        }
    }
    false
}

impl Tool for SearchTool {
    fn definition(&self) -> Value {
        json!({
            "name": "web_search",
            "description": "Search the web. Answers up to 8 results: title, URL and a snippet each.",
            "input_schema": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "What to search for" } },
                "required": ["query"],
                "additionalProperties": false,
            },
        })
    }

    fn call<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let q = input["query"].as_str().unwrap_or("");
            // A refused query doesn't use up one of the task's searches.
            if let Some(why) = refuse_query(q, &self.rows) {
                return Err(why);
            }
            if !take(&self.left) {
                return Err("no searches left for this task: answer with what you have".into());
            }
            let w = &self.web;
            let results =
                crate::search::search(&w.hosts, &w.cache, &w.client, &w.provider, q, 8, w.meter())
                    .await?;
            if results.is_empty() {
                return Ok("No results.".into());
            }
            for r in &results {
                w.seen.add(&r.url);
            }
            let text = crate::search::results_text(&results);
            w.seen.add_text(&text);
            w.seen.add_read(&text);
            Ok(text)
        })
    }
}

/// `web_fetch {url}`: a page's readable text; at most `max` uses.
pub struct FetchTool {
    web: Web,
    left: AtomicU32,
}

impl FetchTool {
    pub fn new(web: Web, max: u32) -> Self {
        Self {
            web,
            left: AtomicU32::new(max),
        }
    }
}

impl Tool for FetchTool {
    fn definition(&self) -> Value {
        json!({
            "name": "web_fetch",
            "description": "Read a web page: its readable text (long pages are cut) and its links. Only pages already seen can be read: a URL in the request, a search result, or a link or URL on a page read before.",
            "input_schema": {
                "type": "object",
                "properties": { "url": { "type": "string", "description": "The page's http(s) URL, exactly as it was seen" } },
                "required": ["url"],
                "additionalProperties": false,
            },
        })
    }

    fn call<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let url = crate::fetch::check_url(
                input["url"].as_str().unwrap_or(""),
                self.web.allow_private,
            )?;
            if !self.web.seen.contains(&url) {
                return Err(
                    "only pages already seen can be read: a URL in the request, a search \
                            result, or a link on a page read before; search for it instead"
                        .into(),
                );
            }
            if !take(&self.left) {
                return Err("no page reads left for this task: answer with what you have".into());
            }
            let fetched =
                crate::hosts::fetch(&self.web.hosts, &self.web.client, &url, None, None, &[])
                    .await
                    .map_err(|e| e.message)?;
            let crate::fetch::Fetched::Body { bytes, .. } = fetched else {
                return Err("the page answered nothing".into());
            };
            let html = String::from_utf8_lossy(&bytes);
            Ok(page_answer(&self.web.seen, &html, &url))
        })
    }
}

/// What `web_fetch` hands the model for the page `html` at `url`: its text
/// (cut at [`MAX_FETCH_CHARS`]), pictures and links (at most
/// [`MAX_FETCH_LINKS`]). The whole text is kept for checking quotes, and
/// what was shown marks its URLs seen.
fn page_answer(seen: &Seen, html: &str, url: &str) -> String {
    let text = crate::html::to_text(html);
    // Quotes may come from anywhere on the page, not only the part
    // the model was shown.
    seen.add_read(&text);
    let mut out = crate::html::truncate(&text, MAX_FETCH_CHARS);
    let images = page_images(html, url, MAX_FETCH_IMAGES);
    if !images.is_empty() {
        out.push_str("\n\nImages:");
        for i in &images {
            out.push_str(&format!("\n- {i}"));
            seen.add(i);
        }
    }
    let links = page_links(html, url, MAX_FETCH_LINKS);
    if !links.is_empty() {
        out.push_str("\n\nLinks:");
        for (text, link) in &links {
            if text.is_empty() {
                out.push_str(&format!("\n- {link}"));
            } else {
                out.push_str(&format!("\n- {text}: {link}"));
            }
            seen.add(link);
        }
    }
    seen.add_text(&out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_queries_and_row_text_are_refused() {
        let row = json!({
            "name": "Gig",
            "notes": "Door code 4417, ask for Dana at the side entrance after nine",
        });
        let tool_rows = row_texts(&[row.as_object().unwrap().clone()]);
        assert_eq!(tool_rows.len(), 1);
        // Real queries pass, even one naming a row.
        assert!(refuse_query("Gig tickets Venue Hall 2026 reviews", &tool_rows).is_none());
        let long = "concert listings ".repeat(20);
        assert!(
            refuse_query(&long, &tool_rows)
                .unwrap()
                .contains("too long")
        );
        assert!(refuse_query(&"a".repeat(MAX_QUERY_CHARS), &[]).is_none());
        // A run of a row's private field, in any case or spacing.
        let q = "site:x.example DOOR CODE 4417, ask for  Dana at the side entrance";
        assert!(refuse_query(q, &tool_rows).unwrap().contains("current row"));
    }

    #[test]
    fn only_seen_urls_can_be_fetched() {
        let seen = Seen::default();
        seen.add_text(
            "Request: concerts listed on https://Venue.example/shows. Rows:\n\
             {\"name\":\"Gig\",\"link\":\"https://tickets.example/e/1?ref=a\"}",
        );
        assert!(seen.contains("https://venue.example/shows"));
        assert!(seen.contains("https://venue.example/shows/#top"));
        assert!(seen.contains("https://tickets.example/e/1?ref=a"));
        // Data can't ride along in a query or a path the model writes.
        assert!(!seen.contains("https://tickets.example/e/1?ref=a&d=Gig"));
        assert!(!seen.contains("https://venue.example/shows?d=Gig"));
        assert!(!seen.contains("https://attacker.example/?d=Gig"));
        assert!(!seen.contains("https://venue.example/Gig"));
        assert!(!seen.contains("not a url"));
    }

    #[test]
    fn citations_are_checked_against_what_was_read() {
        let seen = Seen::default();
        seen.add("https://shop.example/tee");
        seen.add_read(
            "Reviews: “Soft, thick and it didn't shrink at all.” — Pat. Fits true to size.",
        );
        let mut rows = vec![
            json!({
                "url": "https://shop.example/tee",
                "image": "https://cdn.example/made-up.jpg",
                "reviews": "\"soft, thick and it didn't shrink at all\" (shop.example)\n\"Best tee I have ever owned\" (shop.example)\n\"Fits\" (shop.example)",
                "sources": "Shop page: https://shop.example/tee\nA blog: https://blog.example/never-read",
                "why": "A heavy cotton tee.",
            })
            .as_object()
            .unwrap()
            .clone(),
        ];
        let c = seen.check_rows(&mut rows);
        assert_eq!(
            c,
            Cited {
                urls_dropped: 2,
                quotes_dropped: 1
            }
        );
        let r = &rows[0];
        assert_eq!(r["url"], "https://shop.example/tee");
        assert_eq!(r["image"], Value::Null);
        assert_eq!(
            r["reviews"],
            "\"soft, thick and it didn't shrink at all\" (shop.example)\n\"Fits\" (shop.example)"
        );
        assert_eq!(r["sources"], "Shop page: https://shop.example/tee");
        assert_eq!(r["why"], "A heavy cotton tee.");
    }

    #[test]
    fn a_page_lists_its_pictures() {
        let html = r#"<head><meta property="og:image" content="/img/tee.jpg">
            <meta name="twitter:image" content="https://cdn.example/tee.jpg">
            <script type="application/ld+json">{"@graph":[{"@type":"Product","image":["https://cdn.example/tee.jpg",{"url":"https://cdn.example/back.jpg"}]}]}</script></head>"#;
        assert_eq!(
            page_images(html, "https://shop.example/p/1", 10),
            vec![
                "https://shop.example/img/tee.jpg".to_owned(),
                "https://cdn.example/tee.jpg".to_owned(),
                "https://cdn.example/back.jpg".to_owned()
            ]
        );
        assert_eq!(page_images(html, "https://shop.example/p/1", 1).len(), 1);
    }

    #[test]
    fn urls_in_prose_lose_trailing_punctuation() {
        assert_eq!(
            urls_in("see https://a.example/x, or (https://b.example/y). Then xhttps://c.example."),
            vec![
                "https://a.example/x",
                "https://b.example/y",
                "https://c.example"
            ]
        );
    }

    #[test]
    fn a_read_shows_part_of_a_long_page_and_keeps_all_of_it_for_quotes() {
        let mut html = format!(
            "<p>{}</p><p>Near the end: soft thick cotton</p>",
            "word ".repeat(5_000)
        );
        for i in 0..100 {
            html.push_str(&format!("<a href=\"/p/{i}\">Item {i}</a>"));
        }
        let seen = Seen::default();
        let out = page_answer(&seen, &html, "https://shop.example/");
        let (text, links) = out.split_once("\n\nLinks:").unwrap();
        assert_eq!(
            text.chars().count(),
            MAX_FETCH_CHARS + 1,
            "cut, with an ellipsis"
        );
        assert!(!text.contains("Near the end"));
        assert_eq!(
            links.lines().filter(|l| l.starts_with("- ")).count(),
            MAX_FETCH_LINKS
        );
        assert!(seen.contains("https://shop.example/p/39"));
        assert!(!seen.contains("https://shop.example/p/40"));
        // A quote from past the cut still checks out.
        seen.add("https://shop.example/");
        let mut rows = vec![
            json!({ "reviews": "\"Near the end: soft thick cotton\" (shop.example)" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        assert_eq!(seen.check_rows(&mut rows), Cited::default());
    }

    #[test]
    fn a_page_lists_its_links_absolute() {
        let html = r#"<a href="/shows/1"> Jazz
            night </a><a href="https://other.example/x#y">Other</a><a href="/shows/1">again</a>
            <a href="javascript:void(0)">js</a><a href="">self</a>"#;
        let links = page_links(html, "https://venue.example/shows", 10);
        assert_eq!(
            links,
            vec![
                (
                    "Jazz night".to_owned(),
                    "https://venue.example/shows/1".to_owned()
                ),
                ("Other".to_owned(), "https://other.example/x#y".to_owned()),
            ]
        );
        assert_eq!(page_links(html, "https://venue.example/shows", 1).len(), 1);
    }
}
