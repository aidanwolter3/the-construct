//! The `agent` filler (decision `collection-fillers`): a model with web
//! search and web fetch answers a collection's rows for a prompt ("the five
//! best-reviewed 27-inch monitors under $400 and where each is cheapest").
//!
//! One fill is one conversation over the Messages API: the model searches
//! and reads pages with the server-side web tools (capped by the filler's
//! `max_searches` and `max_fetches`), then calls `save_rows`, a strict tool
//! whose schema is built from the collection's fields, so every row fits
//! them. A `pause_turn` (the server's tool loop paused) is resumed. Behind
//! the [`FillAgent`] trait, so tests use a fake.

use std::sync::Arc;
use std::time::Duration;

use pod_core::BoxFuture;
use serde_json::{Map, Value, json};

use crate::model::Kind;

/// One field the agent fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentField {
    pub name: String,
    pub kind: Kind,
    pub options: Vec<String>,
}

/// What one fill asks.
#[derive(Debug, Clone)]
pub struct AgentRequest<'a> {
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

/// What one fill answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentAnswer {
    /// Rows by field name.
    pub rows: Vec<Map<String, Value>>,
    pub searches: u32,
    pub fetches: u32,
}

/// What an `llm` fetch filler asks about one page.
#[derive(Debug, Clone)]
pub struct PageRequest<'a> {
    pub title: &'a str,
    pub fields: &'a [AgentField],
    /// The page's readable text.
    pub page_text: &'a str,
    pub url: &'a str,
    /// The filler's notes about the page.
    pub hint: &'a str,
    pub today: &'a str,
}

/// Answers an [`AgentRequest`] (an `agent` filler) and a [`PageRequest`]
/// (an `llm` fetch filler).
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
}

/// Most page text sent to the model.
pub const MAX_PAGE_CHARS: usize = 60_000;

const PAGE_SYSTEM: &str = "You read the text of a web page and list the items it holds as rows of \
a table in a personal app. Give every field of each row: the page's own words for text; null for \
anything the page doesn't say; never guess a number, a price or a date. Dates are YYYY-MM-DD; a date \
and time is YYYY-MM-DDTHH:MM, with the UTC offset only when the page states one. Skip navigation, ads \
and anything the notes exclude. Answer {\"rows\": []} when there are none.";

/// The request body of a [`PageRequest`].
pub fn page_request_body(model: &str, req: &PageRequest<'_>) -> Value {
    let schema = save_rows_tool(req.fields)["input_schema"].clone();
    let fields: Vec<String> = req
        .fields
        .iter()
        .map(|f| format!("- {} ({})", f.name, f.kind.as_str()))
        .collect();
    let mut prompt = format!(
        "Today is {}. The table is \"{}\", with columns:\n{}\n\nThe page is {}.\n\n<page>\n{}\n</page>",
        req.today,
        req.title,
        fields.join("\n"),
        req.url,
        web_extract::html::truncate(req.page_text, MAX_PAGE_CHARS)
    );
    if !req.hint.trim().is_empty() {
        prompt.push_str(&format!("\n\nNotes about this page: {}", req.hint.trim()));
    }
    json!({
        "model": model,
        "max_tokens": 16000,
        "system": PAGE_SYSTEM,
        "output_config": { "effort": "low", "format": { "type": "json_schema", "schema": schema } },
        "fallbacks": "default",
        "messages": [{ "role": "user", "content": prompt }],
    })
}

/// The rows of a [`PageRequest`]'s answer.
pub fn page_rows(answer: &Value) -> Result<Vec<Map<String, Value>>, String> {
    match answer["stop_reason"].as_str() {
        Some("refusal") => return Err("the model declined to read this page".into()),
        Some("max_tokens") => return Err("the model's answer was cut off (too many rows)".into()),
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
    Ok(parsed["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_object().cloned())
        .take(MAX_ROWS * 2)
        .collect())
}

/// The env var naming the model agent fillers use; unset:
/// [`DEFAULT_MODEL`]. The key and URL are the computed fields'
/// (`CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_URL`).
pub const MODEL_VAR: &str = "CONSTRUCT_FILL_MODEL";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Requests per fill (the first and `pause_turn` resumptions).
pub const MAX_TURNS: usize = 6;
/// Rows one fill may answer.
pub const MAX_ROWS: usize = 100;
/// Current rows shown to the model.
pub const MAX_CURRENT: usize = 50;

const SYSTEM: &str = "You keep a table in a personal app up to date. Research the request on the web \
with the tools you have, then call save_rows exactly once with every row the request asks for. Each row \
gives every field: use null for anything you could not find; never guess a number, a price or a date. \
Dates are YYYY-MM-DD. Prefer primary sources (the store's own page, the official results page) and \
current information. Keep a current row when it still holds, with the same key.";

/// A [`FillAgent`] over the Anthropic Messages API with the server-side
/// web tools.
pub struct MessagesAgent {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
}

impl std::fmt::Debug for MessagesAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagesAgent")
            .field("url", &self.url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl MessagesAgent {
    pub fn new(url: String, key: String, model: String) -> Self {
        let client = pod_core::net::builder(Duration::from_secs(600))
            .build()
            .expect("an HTTP client");
        Self {
            client,
            url,
            key,
            model,
        }
    }

    /// From `CONSTRUCT_INFER_API_KEY` (required), [`MODEL_VAR`] and
    /// `CONSTRUCT_INFER_URL`.
    pub fn from_env() -> Option<Arc<dyn FillAgent>> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let key = var(crate::compute::API_KEY_VAR)?;
        let model = var(MODEL_VAR).unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let url =
            var(crate::compute::URL_VAR).unwrap_or_else(|| crate::compute::DEFAULT_URL.to_owned());
        Some(Arc::new(Self::new(url, key, model)))
    }
}

/// The JSON schema of one value of `f` (null: not found).
fn field_schema(f: &AgentField) -> Value {
    let one = match f.kind {
        Kind::Text => json!({ "type": "string" }),
        Kind::Number => json!({ "type": "number" }),
        Kind::Bool => json!({ "type": "boolean" }),
        Kind::Date => json!({ "type": "string", "format": "date" }),
        Kind::Datetime => {
            json!({ "type": "string", "description": "YYYY-MM-DDTHH:MM with the UTC offset when the source gives one (e.g. 2026-11-06T19:30-07:00), or YYYY-MM-DD" })
        }
        Kind::Choice => json!({ "type": "string", "enum": f.options }),
    };
    json!({ "anyOf": [one, { "type": "null" }] })
}

/// The `save_rows` tool for `fields`.
pub fn save_rows_tool(fields: &[AgentField]) -> Value {
    let props: Map<String, Value> = fields
        .iter()
        .map(|f| (f.name.clone(), field_schema(f)))
        .collect();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    json!({
        "name": "save_rows",
        "description": "Save the rows of the table. Call it once, with every row.",
        "strict": true,
        "input_schema": {
            "type": "object",
            "properties": {
                "rows": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": props,
                        "required": names,
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["rows"],
            "additionalProperties": false,
        },
    })
}

/// The first user message of a fill.
pub fn user_prompt(req: &AgentRequest<'_>) -> String {
    let fields: Vec<String> = req
        .fields
        .iter()
        .map(|f| match f.kind {
            Kind::Choice => format!("- {} (one of: {})", f.name, f.options.join(", ")),
            k => format!("- {} ({})", f.name, k.as_str()),
        })
        .collect();
    let mut p = format!(
        "Today is {}. The table is \"{}\". Its columns:\n{}\n\nRequest: {}",
        req.today,
        req.title,
        fields.join("\n"),
        req.prompt
    );
    if let Some(k) = req.key {
        p.push_str(&format!(
            "\n\nA row is identified by its {k}: give the same {k} for the same thing."
        ));
    }
    if !req.current.is_empty() {
        let rows: Vec<String> = req
            .current
            .iter()
            .take(MAX_CURRENT)
            .map(|r| Value::Object(r.clone()).to_string())
            .collect();
        p.push_str(&format!(
            "\n\nThe table's current rows (check them; they may be out of date):\n{}",
            rows.join("\n")
        ));
    }
    p
}

/// The request body for `messages`.
pub fn request_body(model: &str, req: &AgentRequest<'_>, messages: &[Value]) -> Value {
    let mut tools = Vec::new();
    if req.max_searches > 0 {
        tools.push(json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": req.max_searches }));
    }
    if req.max_fetches > 0 {
        tools.push(json!({ "type": "web_fetch_20260209", "name": "web_fetch", "max_uses": req.max_fetches }));
    }
    tools.push(save_rows_tool(req.fields));
    json!({
        "model": model,
        "max_tokens": 16000,
        "system": SYSTEM,
        "output_config": { "effort": "medium" },
        "fallbacks": "default",
        "tools": tools,
        "tool_choice": { "type": "auto" },
        "messages": messages,
    })
}

/// What a response means for the fill.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    /// The server's tool loop paused: send the content back to resume.
    Resume(Value),
    /// `save_rows` was called with these rows.
    Rows(Vec<Map<String, Value>>),
    Failed(String),
}

/// Count the web tool calls in `answer` into `(searches, fetches)`.
pub fn count_tools(answer: &Value, out: &mut (u32, u32)) {
    for b in answer["content"].as_array().into_iter().flatten() {
        if b["type"] == "server_tool_use" {
            match b["name"].as_str() {
                Some("web_search") => out.0 += 1,
                Some("web_fetch") => out.1 += 1,
                _ => {}
            }
        }
    }
}

/// Read one response.
pub fn read_turn(answer: &Value) -> Turn {
    let content = answer["content"].clone();
    if let Some(input) = content
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["type"] == "tool_use" && b["name"] == "save_rows")
        .map(|b| b["input"].clone())
    {
        let rows = input["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_object().cloned())
            .take(MAX_ROWS)
            .collect();
        return Turn::Rows(rows);
    }
    match answer["stop_reason"].as_str() {
        Some("pause_turn") => Turn::Resume(content),
        Some("refusal") => Turn::Failed("the model declined this request".into()),
        Some("max_tokens") => Turn::Failed("the model's answer was cut off".into()),
        _ => Turn::Failed("the model answered without saving rows".into()),
    }
}

impl MessagesAgent {
    async fn post(&self, body: &Value) -> Result<Value, String> {
        let resp = self
            .client
            .post(&self.url)
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2026-10-21")
            .header("anthropic-beta", "server-side-fallback-2026-07-01")
            .json(body)
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
                web_extract::html::truncate(msg, 200)
            ));
        }
        Ok(answer)
    }
}

impl FillAgent for MessagesAgent {
    fn read_page<'a>(
        &'a self,
        req: PageRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            let answer = self.post(&page_request_body(&self.model, &req)).await?;
            page_rows(&answer)
        })
    }

    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async move {
            let mut messages = vec![json!({ "role": "user", "content": user_prompt(&req) })];
            let mut used = (0, 0);
            for _ in 0..MAX_TURNS {
                let body = request_body(&self.model, &req, &messages);
                let answer = self.post(&body).await?;
                count_tools(&answer, &mut used);
                match read_turn(&answer) {
                    Turn::Rows(rows) => {
                        return Ok(AgentAnswer {
                            rows,
                            searches: used.0,
                            fetches: used.1,
                        });
                    }
                    Turn::Resume(content) => {
                        messages.push(json!({ "role": "assistant", "content": content }));
                    }
                    Turn::Failed(e) => return Err(e),
                }
            }
            Err("the model didn't finish within its turns".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<AgentField> {
        vec![
            AgentField {
                name: "name".into(),
                kind: Kind::Text,
                options: Vec::new(),
            },
            AgentField {
                name: "price".into(),
                kind: Kind::Number,
                options: Vec::new(),
            },
            AgentField {
                name: "stock".into(),
                kind: Kind::Choice,
                options: vec!["in".into(), "out".into()],
            },
        ]
    }

    #[test]
    fn the_request_has_the_web_tools_and_a_strict_save_tool() {
        let f = fields();
        let current = vec![
            json!({"name": "Acme 27", "price": 329})
                .as_object()
                .unwrap()
                .clone(),
        ];
        let req = AgentRequest {
            title: "Monitors",
            prompt: "the five best-reviewed 27-inch monitors under $400",
            fields: &f,
            today: "2026-11-04",
            max_searches: 3,
            max_fetches: 0,
            key: Some("name"),
            current: &current,
        };
        let body = request_body(
            "m",
            &req,
            &[json!({"role": "user", "content": user_prompt(&req)})],
        );
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2, "no fetch tool with max_fetches 0");
        assert_eq!(tools[0]["type"], "web_search_20260209");
        assert_eq!(tools[0]["max_uses"], 3);
        let save = &tools[1];
        assert_eq!(save["strict"], true);
        let item = &save["input_schema"]["properties"]["rows"]["items"];
        assert_eq!(item["required"], json!(["name", "price", "stock"]));
        assert_eq!(
            item["properties"]["stock"]["anyOf"][0]["enum"],
            json!(["in", "out"])
        );
        assert_eq!(body["tool_choice"]["type"], "auto");
        let prompt = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            prompt.contains("Today is 2026-11-04") && prompt.contains("Acme 27"),
            "{prompt}"
        );
    }

    #[test]
    fn turns_are_read() {
        let saved = json!({"stop_reason": "tool_use", "content": [
            {"type": "server_tool_use", "name": "web_search", "id": "s1", "input": {}},
            {"type": "text", "text": "Found them."},
            {"type": "tool_use", "name": "save_rows", "id": "t1", "input": {"rows": [{"name": "A", "price": 1, "stock": null}]}}]});
        let mut used = (0, 0);
        count_tools(&saved, &mut used);
        assert_eq!(used, (1, 0));
        match read_turn(&saved) {
            Turn::Rows(r) => assert_eq!(r[0]["name"], "A"),
            other => panic!("{other:?}"),
        }
        let paused = json!({"stop_reason": "pause_turn", "content": [{"type": "server_tool_use", "name": "web_fetch"}]});
        assert!(matches!(read_turn(&paused), Turn::Resume(_)));
        assert!(matches!(
            read_turn(&json!({"stop_reason": "end_turn", "content": []})),
            Turn::Failed(_)
        ));
        assert!(matches!(
            read_turn(&json!({"stop_reason": "refusal", "content": []})),
            Turn::Failed(_)
        ));
    }
}
