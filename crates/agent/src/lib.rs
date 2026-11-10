//! The agent module (decision `web-and-agent-modules`): every model call the
//! pod makes goes through [`Agent::run`].
//!
//! The agent knows models, money and the Messages API. It knows nothing
//! about web pages, collections or fields: a caller (the web module,
//! computed fields) writes the prompt and the output it wants, picks a
//! [`Tier`], and hands it a [`Task`]. The agent then:
//!
//! - refuses when the pod's `model` jack is unplugged
//!   ([`pod_core::jacks::MODEL`]);
//! - counts the call toward the pod's daily [`Budgets`] (per feature and per
//!   pod, in `agent_usage`) and refuses once one is spent;
//! - picks the tier's model, sends the request, and retries a busy or
//!   failing API (429, 5xx, 529, a timeout) with backoff;
//! - runs the tool loop: a `pause_turn` is resumed, a call to one of the
//!   task's client [`Tool`]s is run and its result sent back, and a call to
//!   the task's finish tool ends the task with that tool's input;
//! - records the call in the metrics (`construct_llm_*`, by feature).

use std::sync::Arc;
use std::time::Duration;

use pod_core::BoxFuture;
use pod_core::metrics;
use pod_core::store::Store;
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

/// The env var with the model API key; unset: no agent (no model features).
pub const API_KEY_VAR: &str = "CONSTRUCT_INFER_API_KEY";
/// The env var naming the [`Tier::Cheap`] model (computed fields); unset:
/// cheap tasks are refused, as computed fields were without it.
pub const CHEAP_MODEL_VAR: &str = "CONSTRUCT_INFER_MODEL";
/// The env var naming the [`Tier::Capable`] model (reading pages, agent
/// fills, picking from search results); unset: [`DEFAULT_CAPABLE_MODEL`].
pub const CAPABLE_MODEL_VAR: &str = "CONSTRUCT_FILL_MODEL";
/// The env var with the Messages API URL.
pub const URL_VAR: &str = "CONSTRUCT_INFER_URL";
/// [`URL_VAR`] when unset.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
/// [`CAPABLE_MODEL_VAR`] when unset.
pub const DEFAULT_CAPABLE_MODEL: &str = "claude-opus-5-5";

/// Which model a task needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Short answers in bulk: computed fields.
    Cheap,
    /// Reading pages, research loops, picking from search results.
    Capable,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Capable => "capable",
        }
    }
}

/// A tool the agent runs itself when the model calls it (a client tool),
/// such as the web module's search and fetch.
pub trait Tool: Send + Sync {
    /// The tool's definition: `{name, description, input_schema}`.
    fn definition(&self) -> Value;
    /// Run it with the model's input: the text sent back as its result, or
    /// an error the model sees (`is_error`).
    fn call<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Result<String, String>>;
}

impl std::fmt::Debug for dyn Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tool({})", self.definition()["name"])
    }
}

/// One piece of model work.
#[derive(Clone)]
pub struct Task {
    /// What asked (metrics and budgets): `collections`, `fill_page`,
    /// `fill_agent`, `web_pick`, ...
    pub feature: &'static str,
    pub tier: Tier,
    pub system: Option<String>,
    /// The conversation so far (usually one user message).
    pub messages: Vec<Value>,
    pub max_tokens: u32,
    /// `output_config.effort` (low, medium, high), when set.
    pub effort: Option<&'static str>,
    /// A JSON schema the answer's text must follow (structured output).
    pub schema: Option<Value>,
    /// Server-side tools (run by the API, e.g. its web search), as sent.
    pub server_tools: Vec<Value>,
    /// Client tools the agent runs ([`Tool`]).
    pub tools: Vec<Arc<dyn Tool>>,
    /// A tool whose call ends the task with its input (e.g. `save_rows`).
    pub finish_tool: Option<Value>,
    /// Requests at most (the first, resumptions and tool results).
    pub max_turns: usize,
}

impl Task {
    /// A one-message task.
    pub fn new(feature: &'static str, tier: Tier, prompt: impl Into<String>) -> Self {
        Self {
            feature,
            tier,
            system: None,
            messages: vec![json!({ "role": "user", "content": prompt.into() })],
            max_tokens: 1024,
            effort: None,
            schema: None,
            server_tools: Vec::new(),
            tools: Vec::new(),
            finish_tool: None,
            max_turns: 1,
        }
    }

    pub fn system(mut self, s: impl Into<String>) -> Self {
        self.system = Some(s.into());
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn effort(mut self, e: &'static str) -> Self {
        self.effort = Some(e);
        self
    }

    pub fn schema(mut self, s: Value) -> Self {
        self.schema = Some(s);
        self
    }

    fn finish_name(&self) -> Option<&str> {
        self.finish_tool.as_ref().and_then(|t| t["name"].as_str())
    }

    /// The request body for `model`.
    pub fn body(&self, model: &str) -> Value {
        let mut body = json!({
            "model": model,
            "max_tokens": self.max_tokens,
            "messages": self.messages,
            "fallbacks": "default",
        });
        if let Some(s) = &self.system {
            body["system"] = json!(s);
        }
        let mut output = Map::new();
        if let Some(e) = self.effort {
            output.insert("effort".into(), json!(e));
        }
        if let Some(s) = &self.schema {
            output.insert(
                "format".into(),
                json!({ "type": "json_schema", "schema": s }),
            );
        }
        if !output.is_empty() {
            body["output_config"] = Value::Object(output);
        }
        let mut tools: Vec<Value> = self.server_tools.clone();
        tools.extend(self.tools.iter().map(|t| t.definition()));
        if let Some(f) = &self.finish_tool {
            tools.push(f.clone());
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!({ "type": "auto" });
        }
        body
    }
}

/// What a task answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// The last response.
    pub answer: Value,
    /// The finish tool's input, when the model called it.
    pub finished: Option<Value>,
    /// Requests made.
    pub turns: usize,
    /// Server-side web searches and fetches the API ran.
    pub server_searches: u32,
    pub server_fetches: u32,
    /// Client tool calls run.
    pub tool_calls: u32,
}

impl Output {
    /// The answer's text blocks, joined.
    pub fn text(&self) -> String {
        text_of(&self.answer)
    }
}

/// The text blocks of a response, joined.
pub fn text_of(answer: &Value) -> String {
    answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect()
}

/// Why a task didn't run or didn't finish.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentError {
    /// The pod's `model` jack is unplugged (or locked).
    Unplugged,
    /// A daily budget is spent.
    Budget(String),
    /// No model is configured for the tier.
    NoModel(Tier),
    /// The model declined.
    Refused,
    /// The answer was cut off at `max_tokens`.
    CutOff,
    Failed(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unplugged => write!(
                f,
                "the pod's model jack is unplugged (plug_jack model, if the server allows it)"
            ),
            Self::Budget(m) => write!(f, "{m}"),
            Self::NoModel(t) => write!(f, "no {} model is configured on this server", t.as_str()),
            Self::Refused => write!(f, "the model declined this request"),
            Self::CutOff => write!(f, "the model's answer was cut off"),
            Self::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// Model calls a pod may make per day: per feature (unlisted features share
/// only the pod's total) and in all.
#[derive(Debug, Clone, PartialEq)]
pub struct Budgets {
    pub per_feature: Vec<(&'static str, u32)>,
    pub per_pod: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            per_feature: vec![
                ("collections", 2_000),
                ("fill_page", 500),
                ("fill_agent", 150),
                ("web_pick", 300),
            ],
            per_pod: 3_000,
        }
    }
}

/// Today's date in UTC (`YYYY-MM-DD`), the budgets' day.
pub fn utc_day(now_secs: i64) -> String {
    let days = now_secs.div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Count one call of `feature` today, unless a budget is spent.
pub fn admit(store: &Store, budgets: &Budgets, feature: &str, now: i64) -> Result<(), AgentError> {
    let day = utc_day(now);
    let cap = budgets
        .per_feature
        .iter()
        .find(|(f, _)| *f == feature)
        .map(|(_, n)| *n);
    let r = store.transaction(|tx| {
        let mine: i64 = tx
            .query_row(
                "SELECT calls FROM agent_usage WHERE day = ?1 AND feature = ?2",
                params![day, feature],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let all: i64 = tx.query_row(
            "SELECT COALESCE(SUM(calls), 0) FROM agent_usage WHERE day = ?1",
            [&day],
            |r| r.get(0),
        )?;
        if let Some(cap) = cap
            && mine >= i64::from(cap)
        {
            return Ok(Err(AgentError::Budget(format!(
                "this pod already made {mine} {feature} model calls today (its daily budget)"
            ))));
        }
        if all >= i64::from(budgets.per_pod) {
            return Ok(Err(AgentError::Budget(format!(
                "this pod already made {all} model calls today (its daily budget)"
            ))));
        }
        tx.execute(
            "INSERT INTO agent_usage (day, feature, calls) VALUES (?1, ?2, 1)
             ON CONFLICT (day, feature) DO UPDATE SET calls = calls + 1",
            params![day, feature],
        )?;
        tx.execute(
            "DELETE FROM agent_usage WHERE day < ?1",
            [&utc_day(now - 7 * 86_400)],
        )?;
        Ok(Ok(()))
    });
    r.map_err(|e| AgentError::Failed(format!("counting the model call: {e}")))?
}

/// Model calls of `feature` today (all features: `None`).
pub fn used_today(store: &Store, feature: Option<&str>) -> i64 {
    let day = utc_day(now_secs());
    store
        .with_conn(|c| match feature {
            Some(f) => c
                .query_row(
                    "SELECT calls FROM agent_usage WHERE day = ?1 AND feature = ?2",
                    params![day, f],
                    |r| r.get(0),
                )
                .optional()
                .map(Option::unwrap_or_default),
            None => c.query_row(
                "SELECT COALESCE(SUM(calls), 0) FROM agent_usage WHERE day = ?1",
                [&day],
                |r| r.get(0),
            ),
        })
        .unwrap_or(0)
}

/// Sends one request body: the HTTP status and the JSON answer.
pub trait Transport: Send + Sync {
    fn post<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<(u16, Value), String>>;
}

/// [`Transport`] over HTTPS to the Messages API.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    key: String,
}

impl HttpTransport {
    pub fn new(url: String, key: String) -> Self {
        let client = pod_core::net::builder(Duration::from_secs(600))
            .build()
            .expect("an HTTP client");
        Self { client, url, key }
    }
}

impl Transport for HttpTransport {
    fn post<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<(u16, Value), String>> {
        Box::pin(async move {
            let resp = self
                .client
                .post(&self.url)
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2026-10-21")
                .header("anthropic-beta", "server-side-fallback-2026-07-01")
                .json(body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        "the model timed out".to_owned()
                    } else {
                        format!("the model could not be reached: {e}")
                    }
                })?;
            let status = resp.status().as_u16();
            let answer: Value = resp
                .json()
                .await
                .map_err(|e| format!("the model's answer could not be read: {e}"))?;
            Ok((status, answer))
        })
    }
}

/// Retries after a busy or failing API (the first attempt not counted).
pub const RETRIES: u32 = 2;

fn retryable(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// The pod's one model client. Cheap to share (`Arc`).
pub struct Agent {
    transport: Arc<dyn Transport>,
    cheap: Option<String>,
    capable: String,
    budgets: Budgets,
    /// The first retry's wait (doubling, plus up to as much again).
    retry_base: Duration,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("cheap", &self.cheap)
            .field("capable", &self.capable)
            .finish_non_exhaustive()
    }
}

impl Agent {
    pub fn new(transport: Arc<dyn Transport>, cheap: Option<String>, capable: String) -> Self {
        Self {
            transport,
            cheap,
            capable,
            budgets: Budgets::default(),
            retry_base: Duration::from_secs(2),
        }
    }

    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }

    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    /// From [`API_KEY_VAR`] (required), [`CHEAP_MODEL_VAR`],
    /// [`CAPABLE_MODEL_VAR`] and [`URL_VAR`].
    pub fn from_env() -> Option<Arc<Self>> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let key = var(API_KEY_VAR)?;
        let url = var(URL_VAR).unwrap_or_else(|| DEFAULT_URL.to_owned());
        let capable = var(CAPABLE_MODEL_VAR).unwrap_or_else(|| DEFAULT_CAPABLE_MODEL.to_owned());
        Some(Arc::new(Self::new(
            Arc::new(HttpTransport::new(url, key)),
            var(CHEAP_MODEL_VAR),
            capable,
        )))
    }

    /// The model of `tier`, if one is configured.
    pub fn model(&self, tier: Tier) -> Option<&str> {
        match tier {
            Tier::Cheap => self.cheap.as_deref(),
            Tier::Capable => Some(&self.capable),
        }
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// One request with retries, counted and recorded.
    async fn post(&self, store: &Store, task: &Task, body: &Value) -> Result<Value, AgentError> {
        admit(store, &self.budgets, task.feature, now_secs())?;
        let mut attempt = 0;
        loop {
            let r = self.transport.post(body).await;
            let retry = match &r {
                Ok((status, _)) => retryable(*status),
                Err(_) => true,
            };
            if retry && attempt < RETRIES {
                let base = self.retry_base * 2u32.pow(attempt);
                let mut b = [0u8; 2];
                let _ = getrandom::fill(&mut b);
                let jitter = base.mul_f64(f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX));
                tokio::time::sleep(base + jitter).await;
                attempt += 1;
                continue;
            }
            return match r {
                Ok((status, answer)) => {
                    let ok = (200..300).contains(&status);
                    metrics::record_llm(task.feature, ok, Some(&answer));
                    if ok {
                        Ok(answer)
                    } else {
                        let msg = answer["error"]["message"].as_str().unwrap_or("");
                        let msg: String = msg.chars().take(200).collect();
                        Err(AgentError::Failed(format!(
                            "the model answered {status}: {msg}"
                        )))
                    }
                }
                Err(e) => {
                    metrics::record_llm(task.feature, false, None);
                    Err(AgentError::Failed(e))
                }
            };
        }
    }

    /// Run `task` for the pod whose store is `store`.
    pub async fn run(&self, store: &Store, mut task: Task) -> Result<Output, AgentError> {
        if !pod_core::jacks::core_plugged(store, pod_core::jacks::MODEL) {
            return Err(AgentError::Unplugged);
        }
        let model = self
            .model(task.tier)
            .ok_or(AgentError::NoModel(task.tier))?
            .to_owned();
        let mut out = Output::default();
        let turns = task.max_turns.max(1);
        for _ in 0..turns {
            let body = task.body(&model);
            let answer = self.post(store, &task, &body).await?;
            out.turns += 1;
            count_server_tools(&answer, &mut out, task.feature);
            let content = answer["content"].clone();
            let blocks: Vec<Value> = content.as_array().cloned().unwrap_or_default();
            if let Some(name) = task.finish_name()
                && let Some(input) = blocks
                    .iter()
                    .find(|b| b["type"] == "tool_use" && b["name"] == name)
                    .map(|b| b["input"].clone())
            {
                out.finished = Some(input);
                out.answer = answer;
                return Ok(out);
            }
            let calls: Vec<&Value> = blocks
                .iter()
                .filter(|b| {
                    b["type"] == "tool_use"
                        && task
                            .tools
                            .iter()
                            .any(|t| t.definition()["name"] == b["name"])
                })
                .collect();
            let stop = answer["stop_reason"].as_str().unwrap_or("");
            if !calls.is_empty() {
                let mut results = Vec::new();
                for c in calls {
                    let tool = task
                        .tools
                        .iter()
                        .find(|t| t.definition()["name"] == c["name"])
                        .cloned();
                    let r = match tool {
                        Some(t) => t.call(&c["input"]).await,
                        None => Err("no such tool".to_owned()),
                    };
                    out.tool_calls += 1;
                    let (text, is_error) = match r {
                        Ok(t) => (t, false),
                        Err(e) => (e, true),
                    };
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": c["id"],
                        "content": text,
                        "is_error": is_error,
                    }));
                }
                task.messages
                    .push(json!({ "role": "assistant", "content": content }));
                task.messages
                    .push(json!({ "role": "user", "content": results }));
                out.answer = answer;
                continue;
            }
            match stop {
                "pause_turn" => {
                    task.messages
                        .push(json!({ "role": "assistant", "content": content }));
                    out.answer = answer;
                    continue;
                }
                "refusal" => return Err(AgentError::Refused),
                "max_tokens" => return Err(AgentError::CutOff),
                _ => {
                    out.answer = answer;
                    return Ok(out);
                }
            }
        }
        if task.finish_tool.is_some() {
            return Err(AgentError::Failed(
                "the model didn't finish within its turns".into(),
            ));
        }
        Ok(out)
    }
}

fn count_server_tools(answer: &Value, out: &mut Output, feature: &str) {
    let mut n = (0u32, 0u32);
    for b in answer["content"].as_array().into_iter().flatten() {
        if b["type"] == "server_tool_use" {
            match b["name"].as_str() {
                Some("web_search") => n.0 += 1,
                Some("web_fetch") => n.1 += 1,
                _ => {}
            }
        }
    }
    out.server_searches += n.0;
    out.server_fetches += n.1;
    for (tool, k) in [("web_search", n.0), ("web_fetch", n.1)] {
        if k > 0 {
            metrics::add(
                metrics::LLM_WEB_TOOLS,
                &[("feature", feature), ("tool", tool)],
                u64::from(k),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers in order; records the bodies.
    struct Script {
        answers: Mutex<Vec<Result<(u16, Value), String>>>,
        bodies: Mutex<Vec<Value>>,
    }

    impl Script {
        fn new(answers: Vec<Result<(u16, Value), String>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into_iter().rev().collect()),
                bodies: Mutex::new(Vec::new()),
            })
        }
    }

    impl Transport for Script {
        fn post<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<(u16, Value), String>> {
            self.bodies.lock().unwrap().push(body.clone());
            let a = self
                .answers
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Err("no more answers".into()));
            Box::pin(async move { a })
        }
    }

    fn agent(s: &Arc<Script>) -> Agent {
        Agent::new(s.clone(), Some("cheap-m".into()), "capable-m".into())
            .with_retry_base(Duration::from_millis(1))
    }

    fn text(t: &str) -> Value {
        json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": t }] })
    }

    struct Echo;
    impl Tool for Echo {
        fn definition(&self) -> Value {
            json!({ "name": "echo", "description": "echo", "input_schema": { "type": "object" } })
        }
        fn call<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move { Ok(format!("echo {}", input["q"])) })
        }
    }

    #[tokio::test]
    async fn a_cheap_task_uses_the_cheap_model_and_answers_text() {
        let s = Script::new(vec![Ok((200, text("Produce")))]);
        let store = Store::open_in_memory().unwrap();
        let out = agent(&s)
            .run(
                &store,
                Task::new("collections", Tier::Cheap, "which aisle?"),
            )
            .await
            .unwrap();
        assert_eq!(out.text(), "Produce");
        assert_eq!(s.bodies.lock().unwrap()[0]["model"], "cheap-m");
        assert_eq!(used_today(&store, Some("collections")), 1);
    }

    #[tokio::test]
    async fn an_unplugged_model_jack_refuses_before_any_request() {
        let s = Script::new(vec![Ok((200, text("x")))]);
        let store = Store::open_in_memory().unwrap();
        let spec = pod_core::jacks::core_jacks()
            .into_iter()
            .find(|j| j.id == pod_core::jacks::MODEL)
            .unwrap();
        pod_core::jacks::unplug(&store, &spec, 1).unwrap();
        let e = agent(&s)
            .run(&store, Task::new("collections", Tier::Cheap, "q"))
            .await
            .unwrap_err();
        assert_eq!(e, AgentError::Unplugged);
        assert!(s.bodies.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_spent_budget_refuses() {
        let s = Script::new(vec![Ok((200, text("a"))), Ok((200, text("b")))]);
        let store = Store::open_in_memory().unwrap();
        let a = agent(&s).with_budgets(Budgets {
            per_feature: vec![("collections", 1)],
            per_pod: 10,
        });
        a.run(&store, Task::new("collections", Tier::Cheap, "q"))
            .await
            .unwrap();
        let e = a
            .run(&store, Task::new("collections", Tier::Cheap, "q"))
            .await
            .unwrap_err();
        assert!(matches!(e, AgentError::Budget(_)), "{e:?}");
    }

    #[tokio::test]
    async fn a_busy_api_is_retried() {
        let s = Script::new(vec![
            Ok((529, json!({"error": {"message": "overloaded"}}))),
            Err("timed out".into()),
            Ok((200, text("ok"))),
        ]);
        let store = Store::open_in_memory().unwrap();
        let out = agent(&s)
            .run(&store, Task::new("fill_page", Tier::Capable, "q"))
            .await
            .unwrap();
        assert_eq!(out.text(), "ok");
        assert_eq!(s.bodies.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn client_tools_run_and_the_finish_tool_ends_the_task() {
        let s = Script::new(vec![
            Ok((
                200,
                json!({ "stop_reason": "tool_use", "content": [
                    { "type": "tool_use", "id": "t1", "name": "echo", "input": { "q": "hi" } }
                ]}),
            )),
            Ok((
                200,
                json!({ "stop_reason": "tool_use", "content": [
                    { "type": "tool_use", "id": "t2", "name": "save_rows", "input": { "rows": [] } }
                ]}),
            )),
        ]);
        let store = Store::open_in_memory().unwrap();
        let mut task = Task::new("fill_agent", Tier::Capable, "research");
        task.tools = vec![Arc::new(Echo)];
        task.finish_tool =
            Some(json!({ "name": "save_rows", "input_schema": { "type": "object" } }));
        task.max_turns = 4;
        let out = agent(&s).run(&store, task).await.unwrap();
        assert_eq!(out.finished, Some(json!({ "rows": [] })));
        assert_eq!(out.tool_calls, 1);
        let bodies = s.bodies.lock().unwrap();
        let second = &bodies[1]["messages"];
        assert_eq!(second[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(second[2]["content"][0]["content"], "echo \"hi\"");
    }

    #[test]
    fn utc_days() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_794_268_800), "2026-11-10");
    }
}
