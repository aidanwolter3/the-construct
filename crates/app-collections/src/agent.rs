//! Model work for fillers (decisions `collection-fillers`,
//! `web-and-agent-modules`), behind the [`FillAgent`] trait so tests use a
//! fake:
//!
//! - **agent fillers** ([`FillAgent::fill`]): a research request answered as
//!   rows. With the pod's `web_search` jack plugged, the model searches and
//!   reads pages with the pod's own tools ([`web_extract::tools`]: the pod's
//!   host policy, cache and search provider); without it, with the API's
//!   server-side web tools, as before.
//! - **`llm` pages** ([`FillAgent::read_page`]): a page's text into rows.
//! - **`query_field` rows** ([`FillAgent::pick`]): one row's fields chosen
//!   from web search results.
//!
//! The prompts are the web module's ([`web_extract::tasks`]); the calls are
//! the agent module's ([`construct_agent::Agent`]: the `model` jack, daily
//! budgets, retries). Answers are checked against what was fetched
//! ([`web_extract::tasks::check_rows`]) before they are stored.

use std::sync::Arc;

use construct_agent::{Agent, AgentError};
use pod_core::{BoxFuture, Pod};
use serde_json::{Map, Value, json};
use web_extract::tasks::{self, RowField};

use crate::model::Kind;

/// One field the agent fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentField {
    pub name: String,
    pub kind: Kind,
    pub options: Vec<String>,
}

impl AgentField {
    pub fn row_field(&self) -> RowField {
        RowField {
            name: self.name.clone(),
            kind: self.kind.as_str().to_owned(),
            options: self.options.clone(),
        }
    }
}

fn row_fields(fields: &[AgentField]) -> Vec<RowField> {
    fields.iter().map(AgentField::row_field).collect()
}

/// What one agent fill asks.
#[derive(Clone)]
pub struct AgentRequest<'a> {
    pub pod: &'a Pod,
    /// The collection's title.
    pub title: &'a str,
    pub prompt: &'a str,
    pub fields: &'a [AgentField],
    /// Today's date (`YYYY-MM-DD`, the pod's zone).
    pub today: &'a str,
    pub max_searches: u32,
    pub max_fetches: u32,
    /// The key field's name (rows with the same key are the same row).
    pub key: Option<&'a str>,
    /// The rows it filled last time (their filled fields by name), so it
    /// can keep what still holds.
    pub current: &'a [Map<String, Value>],
}

/// What one agent fill answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentAnswer {
    /// Rows by field name.
    pub rows: Vec<Map<String, Value>>,
    pub searches: u32,
    pub fetches: u32,
}

/// What an `llm` fetch filler asks about one page.
#[derive(Clone)]
pub struct PageRequest<'a> {
    pub pod: &'a Pod,
    pub title: &'a str,
    pub fields: &'a [AgentField],
    /// The page's readable text.
    pub page_text: &'a str,
    pub url: &'a str,
    /// The filler's notes about the page.
    pub hint: &'a str,
    pub today: &'a str,
    /// The filler leaves out past items itself (`upcoming`): the model lists
    /// them too, so a page of only past ones doesn't read as an empty page.
    pub with_past: bool,
}

/// What a `query_field` filler asks about one row.
#[derive(Clone)]
pub struct PickRequest<'a> {
    pub pod: &'a Pod,
    pub title: &'a str,
    pub fields: &'a [AgentField],
    /// The row's item, as its query field (and notes) say it.
    pub item: &'a str,
    pub results: &'a [web_extract::search::SearchResult],
    pub hint: &'a str,
    pub today: &'a str,
}

/// Answers an [`AgentRequest`] (an `agent` filler), a [`PageRequest`] (an
/// `llm` fetch filler) and a [`PickRequest`] (a `query_field` filler).
pub trait FillAgent: Send + Sync + std::fmt::Debug {
    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>>;

    /// The rows one page holds, by field name.
    fn read_page<'a>(
        &'a self,
        req: PageRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        let _ = req;
        Box::pin(async { Err("this model can't read pages".to_owned()) })
    }

    /// One row's fields (by name) from search results; no rows when no
    /// result is about the item.
    fn pick<'a>(
        &'a self,
        req: PickRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        let _ = req;
        Box::pin(async { Err("this model can't pick from search results".to_owned()) })
    }
}

/// Requests per agent fill (the first, resumptions and tool results).
pub const MAX_TURNS: usize = 12;
/// Rows one fill may answer.
pub const MAX_ROWS: usize = tasks::MAX_ROWS;
/// Current rows shown to the model.
pub const MAX_CURRENT: usize = tasks::MAX_CURRENT;

/// The pod's [`FillAgent`]: the web module's tasks run by the agent module.
#[derive(Debug, Clone)]
pub struct PodAgent {
    agent: Arc<Agent>,
}

impl PodAgent {
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent }
    }

    /// Over [`Agent::from_env`].
    pub fn from_env() -> Option<Arc<dyn FillAgent>> {
        Agent::from_env().map(|a| Arc::new(Self::new(a)) as Arc<dyn FillAgent>)
    }
}

fn err(e: AgentError) -> String {
    e.to_string()
}

/// The pod's own web tools for a research task, when its `web_search` jack
/// is plugged: `(search, fetch)` sharing the pod's host policy and cache.
pub fn pod_web(pod: &Pod) -> Option<web_extract::tools::Web> {
    let config = pod_core::jacks::config(pod.store(), pod_core::jacks::WEB_SEARCH)?;
    let provider = web_extract::search::Provider::from_config(&config).ok()?;
    let adapter = pod.adapter::<crate::CollectionsAdapter>()?;
    Some(web_extract::tools::Web {
        hosts: pod.local::<web_extract::hosts::Hosts>(),
        cache: pod.local::<web_extract::search::SearchCache>(),
        client: adapter.fetch_client().clone(),
        provider,
        allow_private: adapter.allow_private_hosts(),
    })
}

impl FillAgent for PodAgent {
    fn read_page<'a>(
        &'a self,
        req: PageRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            let fields = row_fields(req.fields);
            let task = tasks::extract_rows(&tasks::PageInput {
                title: req.title,
                fields: &fields,
                page_text: req.page_text,
                url: req.url,
                hint: req.hint,
                today: req.today,
                with_past: req.with_past,
            });
            let out = self.agent.run(req.pod.store(), task).await.map_err(err)?;
            let mut rows = tasks::rows_of(&out)?;
            tasks::check_rows(&mut rows, &format!("{}\n{}", req.url, req.page_text));
            Ok(rows)
        })
    }

    fn pick<'a>(
        &'a self,
        req: PickRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            let fields = row_fields(req.fields);
            let task = tasks::pick_from_search(&tasks::PickInput {
                title: req.title,
                fields: &fields,
                item: req.item,
                results: req.results,
                hint: req.hint,
                today: req.today,
            });
            let out = self.agent.run(req.pod.store(), task).await.map_err(err)?;
            let mut rows = tasks::rows_of(&out)?;
            rows.truncate(1);
            tasks::check_rows(&mut rows, &web_extract::search::results_text(req.results));
            Ok(rows)
        })
    }

    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async move {
            let fields = row_fields(req.fields);
            let mut task = tasks::research(
                &tasks::ResearchInput {
                    title: req.title,
                    prompt: req.prompt,
                    fields: &fields,
                    today: req.today,
                    key: req.key,
                    current: req.current,
                },
                MAX_TURNS,
            );
            let mut own_tools = false;
            match pod_web(req.pod) {
                Some(web) => {
                    own_tools = true;
                    if req.max_searches > 0 {
                        task.tools
                            .push(Arc::new(web_extract::tools::SearchTool::new(
                                web.clone(),
                                req.max_searches,
                            )));
                    }
                    if req.max_fetches > 0 {
                        task.tools.push(Arc::new(web_extract::tools::FetchTool::new(
                            web,
                            req.max_fetches,
                        )));
                    }
                }
                None => {
                    if req.max_searches > 0 {
                        task.server_tools.push(json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": req.max_searches }));
                    }
                    if req.max_fetches > 0 {
                        task.server_tools.push(json!({ "type": "web_fetch_20260209", "name": "web_fetch", "max_uses": req.max_fetches }));
                    }
                }
            }
            let out = self.agent.run(req.pod.store(), task).await.map_err(err)?;
            let Some(input) = out.finished.as_ref() else {
                return Err("the model answered without saving rows".into());
            };
            let rows = tasks::rows_in(input);
            let (searches, fetches) = if own_tools {
                // The pod's tools: each call is a search or a fetch.
                (out.tool_calls, 0)
            } else {
                (out.server_searches, out.server_fetches)
            };
            Ok(AgentAnswer {
                rows,
                searches,
                fetches,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_carry_their_kind_names() {
        let f = AgentField {
            name: "stock".into(),
            kind: Kind::Choice,
            options: vec!["in".into(), "out".into()],
        };
        let r = f.row_field();
        assert_eq!(r.kind, "choice");
        assert_eq!(
            tasks::field_schema(&r)["anyOf"][0]["enum"],
            json!(["in", "out"])
        );
    }
}
