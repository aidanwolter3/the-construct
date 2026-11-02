//! Anthropic Messages API provider (raw HTTPS over reqwest; Rust has no
//! official SDK). See the M7 contract "Anthropic provider" and D25.
//!
//! One non-streaming `POST /v1/messages` per prompt with structured outputs
//! (`output_config.format` = [`patch_schema`]): the model answers
//! `{"explanation", "operations": [{op, path, from, value_json}]}` and the ops
//! are converted to RFC 6902 here (`value_json` is parsed as JSON).

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{GenerateFuture, Generated, LayoutProvider, ProviderError};

pub const NAME: &str = "anthropic";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const API_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const MAX_TOKENS: u32 = 16_000;
pub const EFFORT: &str = "medium";
/// Retries after the first attempt for 429 / 5xx / overloaded.
pub const MAX_RETRIES: u32 = 2;
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Upper bound on one honored `retry-after`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// The structured-output schema (root must be an object; patch values are
/// arbitrary JSON, so they travel as strings in `value_json`).
pub fn patch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": { "type": "string" },
            "operations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["add", "remove", "replace", "move", "copy", "test"] },
                        "path": { "type": "string" },
                        "from": { "type": "string" },
                        "value_json": { "type": "string" }
                    },
                    "required": ["op", "path", "from", "value_json"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["explanation", "operations"],
        "additionalProperties": false
    })
}

const INSTRUCTIONS: &str = "\
You edit the layout of a server-driven UI app. The layout is a JSON tree of components; \
the component registry below is the JSON Schema of every node type and action intent the \
clients can render. The user describes a change; you answer with RFC 6902 JSON Patch \
operations that make it.

Rules:
- Output only patch operations (plus a one-sentence explanation) in the required JSON format.
- Paths are JSON Pointers relative to the workspace root node shown below (the \
\"core://Workspace\" object), e.g. /slots/0/content/item_template/caption_slot.
- Never invent component types, fields or enum values: every node must validate against the \
registry. Keep every required field.
- Action intents may only be copies of intents that already exist in the layout; never add \
new endpoints or URLs.
- Keep ids (the workspace id, slot_id values) stable. The root must stay a core://Workspace \
with at least one slot.
- A PostCard renders its slots in the fixed order author_slot, media_slot, action_slot, \
caption_slot, whatever node each holds; reorder content by replacing slot values.
- Text value_expr is a CEL-like expression over `entity` (e.g. entity.caption) or a literal \
string; an empty string renders nothing.
- Encoding: `value_json` is the JSON text of the value for add/replace/test (\"\" for \
remove/move/copy); `from` is the source pointer for move/copy (\"\" otherwise).
- If the request cannot be expressed with the registry, return no operations and say why in \
the explanation.";

/// Configuration (from the environment in production, explicit in tests).
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// `fallbacks: "default"` + the server-side fallback beta header.
    pub fallbacks: bool,
    /// First retry delay when the response has no `retry-after` (doubles).
    pub retry_base: Duration,
    pub timeout: Duration,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            fallbacks: true,
            retry_base: Duration::from_secs(1),
            timeout: TIMEOUT,
        }
    }
}

pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("reqwest client");
        Self { config, client }
    }

    pub fn config(&self) -> &AnthropicConfig {
        &self.config
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// The request body (public for tests and debugging).
    pub fn request_body(&self, prompt: &str, layout: &Value, registry: &Value) -> Value {
        // Stable parts first (instructions + registry, cacheable), then the
        // current layout. Value maps are BTreeMaps: keys print sorted.
        let stable = format!(
            "{INSTRUCTIONS}\n\nComponent registry (JSON Schema):\n{}",
            serde_json::to_string_pretty(registry).unwrap_or_default()
        );
        let current = format!(
            "Current layout (the workspace root node):\n{}",
            serde_json::to_string_pretty(layout).unwrap_or_default()
        );
        let mut body = json!({
            "model": self.config.model,
            "max_tokens": MAX_TOKENS,
            "system": [
                { "type": "text", "text": stable, "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": current }
            ],
            "messages": [ { "role": "user", "content": prompt } ],
            "output_config": {
                "effort": EFFORT,
                "format": { "type": "json_schema", "schema": patch_schema() }
            }
        });
        if self.config.fallbacks {
            body["fallbacks"] = json!("default");
        }
        body
    }

    async fn call(&self, body: &Value) -> Result<Value, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut req = self
                .client
                .post(self.endpoint())
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .body(body.to_string());
            if self.config.fallbacks {
                req = req.header("anthropic-beta", FALLBACK_BETA);
            }
            let (status, retry_after, text) = match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .filter(|s| s.is_finite() && *s >= 0.0)
                        .map(|s| Duration::from_secs_f64(s).min(MAX_RETRY_AFTER));
                    let text = resp.text().await.map_err(|e| {
                        ProviderError::Failed(format!("reading the response failed: {e}"))
                    })?;
                    (status, retry_after, text)
                }
                Err(e) if e.is_connect() && attempt < MAX_RETRIES => {
                    tracing::warn!(error = %e, attempt, "anthropic: connect failed; retrying");
                    tokio::time::sleep(self.config.retry_base * 2u32.pow(attempt)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) if e.is_timeout() => {
                    return Err(ProviderError::Failed(format!(
                        "request timed out after {} s",
                        self.config.timeout.as_secs()
                    )));
                }
                Err(e) => return Err(ProviderError::Failed(format!("request failed: {e}"))),
            };
            let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if status.is_success() {
                return Ok(parsed);
            }
            let err_type = parsed["error"]["type"].as_str().unwrap_or("");
            let message = parsed["error"]["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| text.chars().take(300).collect());
            let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529)
                || err_type == "overloaded_error";
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(ProviderError::NotConfigured(format!(
                    "the Anthropic API rejected ANTHROPIC_API_KEY (HTTP {}): {message}",
                    status.as_u16()
                )));
            }
            if retryable && attempt < MAX_RETRIES {
                let delay = retry_after.unwrap_or(self.config.retry_base * 2u32.pow(attempt));
                tracing::warn!(
                    status = status.as_u16(),
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "anthropic: retryable error; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            let suffix = if retryable {
                format!(" (after {} attempts)", attempt + 1)
            } else {
                String::new()
            };
            return Err(ProviderError::Failed(format!(
                "HTTP {}{suffix}: {message}",
                status.as_u16()
            )));
        }
    }
}

/// Turns a Messages API response into RFC 6902 ops.
pub fn parse_response(resp: &Value) -> Result<Generated, ProviderError> {
    match resp["stop_reason"].as_str() {
        Some("refusal") => {
            let category = resp["stop_details"]["category"]
                .as_str()
                .unwrap_or("unspecified");
            return Err(ProviderError::Failed(format!(
                "the model declined the request (refusal, category: {category})"
            )));
        }
        Some("max_tokens") => {
            return Err(ProviderError::Failed(
                "truncated: the response hit max_tokens".to_owned(),
            ));
        }
        _ => {}
    }
    let text = resp["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| ProviderError::Failed("the response has no text block".to_owned()))?;
    let out: Value = serde_json::from_str(text)
        .map_err(|e| ProviderError::Failed(format!("the response text is not valid JSON: {e}")))?;
    let operations = out["operations"].as_array().ok_or_else(|| {
        ProviderError::Failed("the response JSON has no `operations` array".to_owned())
    })?;
    let mut ops = Vec::with_capacity(operations.len());
    for (i, o) in operations.iter().enumerate() {
        let op = o["op"].as_str().unwrap_or_default();
        let mut rfc = json!({ "op": op, "path": o["path"].as_str().unwrap_or_default() });
        if matches!(op, "move" | "copy") {
            rfc["from"] = o["from"].clone();
        }
        if matches!(op, "add" | "replace" | "test") {
            let raw = o["value_json"].as_str().unwrap_or_default();
            rfc["value"] = serde_json::from_str(raw).map_err(|e| {
                ProviderError::Failed(format!("operation {i}: value_json is not valid JSON: {e}"))
            })?;
        }
        ops.push(rfc);
    }
    Ok(Generated {
        ops,
        explanation: out["explanation"].as_str().map(str::to_owned),
    })
}

impl LayoutProvider for AnthropicProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn model(&self) -> Option<&str> {
        Some(&self.config.model)
    }

    fn generate<'a>(
        &'a self,
        prompt: &'a str,
        layout: &'a Value,
        registry: &'a Value,
    ) -> GenerateFuture<'a> {
        Box::pin(async move {
            let body = self.request_body(prompt, layout, registry);
            let resp = self.call(&body).await?;
            parse_response(&resp)
        })
    }
}
