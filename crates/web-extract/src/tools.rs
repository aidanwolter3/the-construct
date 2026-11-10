//! The pod's own web tools for research tasks (decision
//! `web-and-agent-modules`): `web_search` and `web_fetch` as client tools the
//! agent module runs ([`construct_agent::Tool`]), so every page an agent
//! reads goes through the pod's host policy, cache and block detection, and
//! searches use the pod's own `web_search` jack.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use construct_agent::Tool;
use pod_core::BoxFuture;
use serde_json::{Value, json};

use crate::hosts::Hosts;
use crate::search::{Provider, SearchCache};

/// Most page text a fetch hands the model.
pub const MAX_FETCH_CHARS: usize = 30_000;

/// What the tools share: the pod's host policy and search cache, an HTTP
/// client and the search provider.
#[derive(Clone)]
pub struct Web {
    pub hosts: Arc<Hosts>,
    pub cache: Arc<SearchCache>,
    pub client: reqwest::Client,
    pub provider: Provider,
    pub allow_private: bool,
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

fn take(left: &AtomicU32) -> bool {
    left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
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
            Ok(crate::search::results_text(&results))
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
            "description": "Read a web page: its readable text (long pages are cut).",
            "input_schema": {
                "type": "object",
                "properties": { "url": { "type": "string", "description": "The page's http(s) URL" } },
                "required": ["url"],
                "additionalProperties": false,
            },
        })
    }

    fn call<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if !take(&self.left) {
                return Err("no page reads left for this task: answer with what you have".into());
            }
            let url = crate::fetch::check_url(
                input["url"].as_str().unwrap_or(""),
                self.web.allow_private,
            )?;
            let fetched =
                crate::hosts::fetch(&self.web.hosts, &self.web.client, &url, None, None, &[])
                    .await
                    .map_err(|e| e.message)?;
            let crate::fetch::Fetched::Body { bytes, .. } = fetched else {
                return Err("the page answered nothing".into());
            };
            let text = crate::html::to_text(&String::from_utf8_lossy(&bytes));
            Ok(crate::html::truncate(&text, MAX_FETCH_CHARS))
        })
    }
}
