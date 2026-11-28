//! Claude Code run headless (`claude -p`): the newcomer's agent, the
//! simulated user and the reviewer. The binary is `$NEWCOMER_CLAUDE`, else
//! `claude` on the path.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

fn claude() -> Command {
    Command::new(std::env::var("NEWCOMER_CLAUDE").unwrap_or_else(|_| "claude".into()))
}

/// One tool call the agent made.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Call {
    pub name: String,
    pub input: Value,
    pub output: String,
    pub is_error: bool,
    /// From the event before the call (the agent writing it) to its answer.
    pub ms: u64,
}

/// One user message to the agent and everything it did before replying.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Turn {
    pub session_id: String,
    pub reply: String,
    pub is_error: bool,
    pub calls: Vec<Call>,
    pub wall_ms: u64,
    pub cost_usd: f64,
    pub model: String,
    pub version: String,
}

/// The MCP config naming only the pod, its token in a header.
pub fn mcp_config(base_url: &str, token: &str) -> String {
    json!({"mcpServers": {"pod": {
        "type": "http",
        "url": format!("{}/v1/mcp", base_url.trim_end_matches('/')),
        "headers": {"Authorization": format!("Bearer {token}")},
    }}})
    .to_string()
}

/// Sends `message` to the newcomer's agent: Claude Code whose only tools are
/// the pod's (`--tools ""`, `--strict-mcp-config`), with no settings, run in
/// `dir` (an empty directory, so no CLAUDE.md reaches it). `resume` continues
/// the conversation. Every stream line goes to `transcript`.
pub fn agent_turn(
    dir: &Path,
    mcp: &str,
    message: &str,
    resume: Option<&str>,
    budget_usd: Option<f64>,
    transcript: &mut impl Write,
) -> Result<Turn, String> {
    let mut cmd = claude();
    cmd.current_dir(dir)
        .arg("-p")
        .arg(message)
        .args(["--mcp-config", mcp, "--strict-mcp-config"])
        .args(["--tools", "", "--allowedTools", "mcp__pod"])
        .args(["--setting-sources", "", "--disable-slash-commands"])
        .args(["--output-format", "stream-json", "--verbose"]);
    if let Some(id) = resume {
        cmd.args(["--resume", id]);
    }
    if let Some(b) = budget_usd {
        cmd.args(["--max-budget-usd", &b.to_string()]);
    }
    let start = Instant::now();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start claude: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;

    let mut turn = Turn {
        session_id: resume.unwrap_or_default().to_owned(),
        reply: String::new(),
        is_error: false,
        calls: Vec::new(),
        wall_ms: 0,
        cost_usd: 0.0,
        model: String::new(),
        version: String::new(),
    };
    let mut open: HashMap<String, (usize, u64)> = HashMap::new();
    let mut last_ms = 0u64;
    let mut got_result = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let now = start.elapsed().as_millis() as u64;
        let _ = writeln!(transcript, "{line}");
        let Ok(ev) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        read_event(&ev, now, last_ms, &mut turn, &mut open, &mut got_result);
        last_ms = now;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    turn.wall_ms = start.elapsed().as_millis() as u64;
    if !got_result {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "claude ended without a result ({}): {}",
            out.status,
            tail(&err, 600)
        ));
    }
    Ok(turn)
}

/// Folds one stream-json event into the turn.
fn read_event(
    ev: &Value,
    now: u64,
    last_ms: u64,
    turn: &mut Turn,
    open: &mut HashMap<String, (usize, u64)>,
    got_result: &mut bool,
) {
    match ev["type"].as_str() {
        Some("system") if ev["subtype"] == "init" => {
            turn.model = ev["model"].as_str().unwrap_or_default().to_owned();
            turn.version = ev["claude_code_version"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if let Some(id) = ev["session_id"].as_str() {
                turn.session_id = id.to_owned();
            }
        }
        Some("assistant") => {
            for c in ev["message"]["content"].as_array().into_iter().flatten() {
                if c["type"] == "tool_use" {
                    let name = c["name"].as_str().unwrap_or_default();
                    turn.calls.push(Call {
                        name: name.strip_prefix("mcp__pod__").unwrap_or(name).to_owned(),
                        input: c["input"].clone(),
                        output: String::new(),
                        is_error: false,
                        ms: 0,
                    });
                    let id = c["id"].as_str().unwrap_or_default().to_owned();
                    open.insert(id, (turn.calls.len() - 1, last_ms));
                }
            }
        }
        Some("user") => {
            for c in ev["message"]["content"].as_array().into_iter().flatten() {
                if c["type"] != "tool_result" {
                    continue;
                }
                let id = c["tool_use_id"].as_str().unwrap_or_default();
                if let Some((i, started)) = open.remove(id) {
                    let call = &mut turn.calls[i];
                    call.output = content_text(&c["content"]);
                    call.is_error = c["is_error"].as_bool().unwrap_or(false)
                        || serde_json::from_str::<Value>(&call.output)
                            .is_ok_and(|v| v.get("error").is_some());
                    call.ms = now.saturating_sub(started);
                }
            }
        }
        Some("result") => {
            *got_result = true;
            turn.reply = ev["result"].as_str().unwrap_or_default().to_owned();
            turn.is_error = ev["is_error"].as_bool().unwrap_or(false);
            turn.cost_usd = ev["total_cost_usd"].as_f64().unwrap_or(0.0);
            if let Some(id) = ev["session_id"].as_str() {
                turn.session_id = id.to_owned();
            }
        }
        _ => {}
    }
}

/// A tool result's text: a string, or text blocks joined.
fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A one-shot question to Claude with no tools but `Read` (when `read` is
/// set, for the reviewer's screenshots): the answer's text.
pub fn ask(system: &str, prompt: &str, read: bool, dir: &Path) -> Result<String, String> {
    let mut cmd = claude();
    cmd.current_dir(dir)
        .arg("-p")
        .arg(prompt)
        .args(["--system-prompt", system])
        .args(["--strict-mcp-config", "--setting-sources", ""])
        .args(["--disable-slash-commands", "--no-session-persistence"])
        .args(["--output-format", "json"]);
    if read {
        cmd.args(["--tools", "Read", "--allowedTools", "Read"]);
    } else {
        cmd.args(["--tools", ""]);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start claude: {e}"))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        format!(
            "claude answered no JSON ({}): {}",
            out.status,
            tail(&String::from_utf8_lossy(&out.stderr), 600)
        )
    })?;
    if v["is_error"].as_bool().unwrap_or(false) {
        return Err(format!("claude failed: {}", v["result"]));
    }
    Ok(v["result"].as_str().unwrap_or_default().to_owned())
}

/// The first JSON object in a model's answer (it may wrap it in prose or a
/// code fence).
pub fn json_in(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

fn tail(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_a_stream_into_a_turn() {
        let events = [
            json!({"type":"system","subtype":"init","session_id":"s1","model":"m","claude_code_version":"2.1"}),
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"mcp__pod__use_kit","input":{"kit":"checklist"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"{\"error\":\"kit_not_found\"}"}]}]}}),
            json!({"type":"result","result":"Done.","is_error":false,"total_cost_usd":0.25,"session_id":"s1"}),
        ];
        let mut turn = Turn {
            session_id: String::new(),
            reply: String::new(),
            is_error: false,
            calls: Vec::new(),
            wall_ms: 0,
            cost_usd: 0.0,
            model: String::new(),
            version: String::new(),
        };
        let (mut open, mut got) = (HashMap::new(), false);
        for (i, ev) in events.iter().enumerate() {
            let now = 1000 * (i as u64 + 1);
            read_event(ev, now, now - 1000, &mut turn, &mut open, &mut got);
        }
        assert!(got);
        assert_eq!(turn.session_id, "s1");
        assert_eq!(turn.reply, "Done.");
        assert_eq!(turn.calls.len(), 1);
        let call = &turn.calls[0];
        assert_eq!(call.name, "use_kit");
        assert!(call.is_error, "an answer with an error counts as refused");
        assert_eq!(call.ms, 2000);
        assert_eq!(turn.cost_usd, 0.25);
    }

    #[test]
    fn finds_json_in_an_answer() {
        let v = json_in("Here:\n```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v["a"], 1);
        assert!(json_in("no json").is_none());
    }
}
