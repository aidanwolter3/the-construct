//! The app builder: + on the home screen asks for an app in a sentence, and
//! Claude builds it as a custom app with the pod's own MCP tools (decision
//! `mcp-connector`), called in-process: the same validated writes as from
//! the Claude app, recorded as Claude's.
//!
//! One task per request: a Messages API tool-use loop of at most
//! [`MAX_TURNS`] turns. Each tool call becomes a line of progress on the
//! request (`status_text`), and every change is published (`event: data`,
//! D50) so the request screen follows it live and opens the app once it is
//! `ready`.

use std::time::Duration;

use pod_core::{Pod, util::now_secs};
use serde_json::{Value, json};

use crate::{HomeAdapter, NAME, db};

/// How the pod calls the model (`CONSTRUCT_INFER_*`, shared with the
/// watcher and Collections; README).
#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub api_key: String,
    pub model: String,
    /// The Messages API endpoint (tests point it at a local fake).
    pub url: String,
}

/// The Messages API endpoint when `CONSTRUCT_INFER_URL` is unset.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
/// The builder's model when neither `CONSTRUCT_HOME_BUILDER_MODEL` nor
/// `CONSTRUCT_INFER_MODEL` is set.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Most model turns per build.
pub const MAX_TURNS: usize = 25;
/// Most tokens per model answer (a whole layout fits).
pub const MAX_TOKENS: u32 = 16_000;
/// One model call's timeout.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(240);

/// Pod setting: `0` turns building off (temporary pods).
pub const BUILD_SETTING: &str = "home.build";

/// The MCP tools the builder may use. Nothing that deletes, hides, restores
/// or fetches other hosts; `update_layout` only for the app it created.
pub const TOOLS: &[&str] = &[
    "list_apps",
    "list_data_sources",
    "describe_vocabulary",
    "get_layout",
    "sample_data",
    "validate_layout",
    "create_app",
    "update_layout",
    "list_parts",
    "list_collections",
    "describe_collection",
    "define_collection",
    "apply_change",
    "add_row",
];

pub const NO_MODEL: &str = "App building isn't set up on this pod yet.";
pub const TEMPORARY_POD: &str = "Building apps isn't available on temporary pods.";
const UNREACHABLE: &str = "Claude couldn't be reached. Try again in a minute.";
const NO_APP: &str = "Claude didn't create an app.";

const TASK: &str = "The user tapped + on their home screen and described an app they want. \
Build ONE new custom app that does what the user asked; make it good-looking and useful; create any \
collections it needs; call create_app exactly once; then reply with one short sentence. Use \
describe_vocabulary before writing the layout, and validate_layout when unsure. Keep the title short \
(one to three words). Don't ask questions: decide sensibly and build.";

impl BuilderConfig {
    /// From the environment: `CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_URL`
    /// and the model from `CONSTRUCT_HOME_BUILDER_MODEL`, else
    /// `CONSTRUCT_INFER_MODEL`. `None` without a key.
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Some(Self {
            api_key: var("CONSTRUCT_INFER_API_KEY")?,
            model: var("CONSTRUCT_HOME_BUILDER_MODEL")
                .or_else(|| var("CONSTRUCT_INFER_MODEL"))
                .unwrap_or_else(|| DEFAULT_MODEL.into()),
            url: var("CONSTRUCT_INFER_URL").unwrap_or_else(|| DEFAULT_URL.into()),
        })
    }
}

/// A line of progress for a tool call.
pub fn status_for(tool: &str) -> &'static str {
    match tool {
        "describe_vocabulary" => "Reading the layout vocabulary",
        "list_apps" | "get_layout" | "list_parts" => "Looking at how your apps are built",
        "list_data_sources" | "sample_data" => "Looking at your data",
        "list_collections" | "describe_collection" | "define_collection" | "apply_change" => {
            "Designing the data"
        }
        "add_row" => "Adding the first entries",
        "validate_layout" => "Checking the layout",
        "create_app" => "Creating the app",
        "update_layout" => "Polishing the layout",
        _ => "Working on it",
    }
}

/// Start building request `id` in the background.
pub fn spawn(pod: &Pod, id: String) {
    let pod = pod.clone();
    tokio::spawn(async move {
        let outcome = build(&pod, &id).await;
        let now = now_secs();
        let stored = match &outcome {
            Ok((app_id, title)) => {
                let (i, a, t) = (id.clone(), app_id.clone(), title.clone());
                pod.blocking(move |s| s.with_conn(|c| db::set_ready(c, &i, &a, &t, now)))
                    .await
            }
            Err(e) => {
                tracing::info!(request = %id, error = %e, "app request failed");
                let (i, e) = (id.clone(), e.clone());
                pod.blocking(move |s| s.with_conn(|c| db::set_failed(c, &i, &e, now)))
                    .await
            }
        };
        if stored.is_err() {
            tracing::error!(request = %id, "could not record the app request's outcome");
        }
        pod.publish_data_all(NAME);
    });
}

async fn set_status(pod: &Pod, id: &str, status: &'static str, text: &'static str) {
    let id = id.to_owned();
    let now = now_secs();
    let _ = pod
        .blocking(move |s| s.with_conn(|c| db::set_status(c, &id, status, text, now)))
        .await;
    pod.publish_data_all(NAME);
}

/// Build request `id`: the new app's `(id, title)`, or a sentence for
/// people saying why not.
async fn build(pod: &Pod, id: &str) -> Result<(String, String), String> {
    let off = pod
        .blocking(|s| s.pod_setting(BUILD_SETTING))
        .await
        .ok()
        .flatten();
    if off.as_deref() == Some("0") {
        return Err(TEMPORARY_POD.into());
    }
    let adapter = pod
        .adapter::<HomeAdapter>()
        .ok_or_else(|| NO_MODEL.to_owned())?;
    let cfg = adapter
        .builder()
        .ok_or_else(|| NO_MODEL.to_owned())?
        .clone();
    let client = adapter.client().clone();
    let key = id.to_owned();
    let request = pod
        .blocking(move |s| s.with_conn(|c| db::request(c, &key)))
        .await
        .map_err(|_| "The request could not be read.".to_owned())?
        .ok_or_else(|| "The request is gone.".to_owned())?;
    set_status(pod, id, "building", "Reading your request").await;

    let system = format!("{}\n\n{TASK}", pod_core::mcp::instructions_text(pod));
    let tools: Vec<Value> = pod_core::mcp::tool_definitions(pod)
        .into_iter()
        .filter(|t| t["name"].as_str().is_some_and(|n| TOOLS.contains(&n)))
        .map(|t| {
            json!({
                "name": t["name"],
                "description": t["description"],
                "input_schema": t["inputSchema"],
            })
        })
        .collect();
    let mut messages = vec![json!({
        "role": "user",
        "content": format!("What I want:\n\n{}", request.prompt),
    })];
    let mut created: Option<(String, String)> = None;
    let mut last_text = String::new();
    for _ in 0..MAX_TURNS {
        let body = json!({
            "model": cfg.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "tools": tools,
            "messages": messages,
        });
        let answer = call(&client, &cfg, &body).await?;
        let content = answer["content"].as_array().cloned().unwrap_or_default();
        for block in &content {
            if block["type"] == "text"
                && let Some(t) = block["text"].as_str()
                && !t.trim().is_empty()
            {
                last_text = t.trim().to_owned();
            }
        }
        messages.push(json!({ "role": "assistant", "content": content }));
        if answer["stop_reason"] != "tool_use" {
            break;
        }
        let mut results = Vec::new();
        for block in &content {
            if block["type"] != "tool_use" {
                continue;
            }
            let name = block["name"].as_str().unwrap_or("");
            let input = block["input"].clone();
            set_status(pod, id, "building", status_for(name)).await;
            let (text, is_error) = run(pod, name, &input, &mut created).await;
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": block["id"],
                "content": text,
                "is_error": is_error,
            }));
        }
        messages.push(json!({ "role": "user", "content": results }));
    }
    match created {
        Some(app) => Ok(app),
        None if last_text.is_empty() => Err(NO_APP.into()),
        None => Err(format!("{NO_APP} It said: {}", shorten(&last_text, 200))),
    }
}

/// Run one tool for the model: `(result text, is_error)`. Remembers the app
/// `create_app` made in `created`.
async fn run(
    pod: &Pod,
    name: &str,
    input: &Value,
    created: &mut Option<(String, String)>,
) -> (String, bool) {
    let refuse = |why: &str| {
        (
            json!({ "error": "refused", "detail": why }).to_string(),
            true,
        )
    };
    if !TOOLS.contains(&name) {
        return refuse("this tool isn't available while building an app from the home screen");
    }
    match name {
        "create_app" if created.is_some() => {
            return refuse("an app was already created in this build; use update_layout on it");
        }
        "update_layout" => {
            let mine = created.as_ref().map(|(id, _)| id.as_str());
            if input["app"].as_str() != mine || mine.is_none() {
                return refuse("only the app created in this build may be updated");
            }
        }
        _ => {}
    }
    let out = pod_core::mcp::run_tool(pod, name, input.clone()).await;
    let is_error = out["isError"] == true;
    if name == "create_app" && !is_error {
        let id = out["structuredContent"]["id"].as_str().unwrap_or("");
        if !id.is_empty() {
            let title = input["title"].as_str().unwrap_or("Your app").trim();
            *created = Some((id.to_owned(), shorten(title, 60)));
        }
    }
    let text = out["content"][0]["text"]
        .as_str()
        .unwrap_or("null")
        .to_owned();
    (text, is_error)
}

/// One Messages API call: the answer, or a sentence for people.
async fn call(
    client: &reqwest::Client,
    cfg: &BuilderConfig,
    body: &Value,
) -> Result<Value, String> {
    let resp = client
        .post(&cfg.url)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2026-10-21")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(body)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "the builder's model could not be reached");
            UNREACHABLE.to_owned()
        })?;
    let status = resp.status();
    let answer: Value = resp.json().await.map_err(|_| UNREACHABLE.to_owned())?;
    if !status.is_success() {
        tracing::warn!(%status, error = %answer["error"]["message"], "the builder's model refused");
        return Err(UNREACHABLE.into());
    }
    Ok(answer)
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut.trim_end())
    }
}
