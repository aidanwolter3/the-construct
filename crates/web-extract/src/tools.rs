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

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use construct_agent::Tool;
use pod_core::BoxFuture;
use serde_json::{Value, json};

use crate::hosts::Hosts;
use crate::search::{Provider, SearchCache};

/// Most page text a fetch hands the model.
pub const MAX_FETCH_CHARS: usize = 30_000;

/// Most links a fetch lists after the page's text.
pub const MAX_FETCH_LINKS: usize = 150;

/// What the tools share: the pod's host policy and search cache, an HTTP
/// client, the search provider, and the URLs this task has seen.
#[derive(Clone)]
pub struct Web {
    pub hosts: Arc<Hosts>,
    pub cache: Arc<SearchCache>,
    pub client: reqwest::Client,
    pub provider: Provider,
    pub allow_private: bool,
    /// What `web_fetch` may read: seed it with the request's text
    /// ([`Seen::add_text`]); search results and fetched pages add theirs.
    pub seen: Seen,
}

/// The URLs one research task has seen (link provenance): only these can
/// be fetched. Shared by the task's tools.
#[derive(Clone, Default, Debug)]
pub struct Seen(Arc<Mutex<HashSet<String>>>);

impl Seen {
    /// Add one URL.
    pub fn add(&self, url: &str) {
        if let Some(k) = seen_key(url) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(k);
        }
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
            self.0
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
fn urls_in(text: &str) -> Vec<&str> {
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

/// `web_search {query}`: at most `max` uses.
pub struct SearchTool {
    web: Web,
    left: AtomicU32,
}

impl SearchTool {
    pub fn new(web: Web, max: u32) -> Self {
        Self {
            web,
            left: AtomicU32::new(max),
        }
    }
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
            if !take(&self.left) {
                return Err("no searches left for this task: answer with what you have".into());
            }
            let q = input["query"].as_str().unwrap_or("");
            let w = &self.web;
            let results =
                crate::search::search(&w.hosts, &w.cache, &w.client, &w.provider, q, 8).await?;
            if results.is_empty() {
                return Ok("No results.".into());
            }
            for r in &results {
                w.seen.add(&r.url);
            }
            let text = crate::search::results_text(&results);
            w.seen.add_text(&text);
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
            let mut out = crate::html::truncate(&crate::html::to_text(&html), MAX_FETCH_CHARS);
            let links = page_links(&html, &url, MAX_FETCH_LINKS);
            if !links.is_empty() {
                out.push_str("\n\nLinks:");
                for (text, link) in &links {
                    if text.is_empty() {
                        out.push_str(&format!("\n- {link}"));
                    } else {
                        out.push_str(&format!("\n- {text}: {link}"));
                    }
                    self.web.seen.add(link);
                }
            }
            self.web.seen.add_text(&out);
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
