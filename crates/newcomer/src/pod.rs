//! A pod on the server under test: a temporary one made for a persona and
//! deleted when dropped, and calls to its MCP tools (plain JSON-RPC, as the
//! endpoint is stateless).

use serde_json::{Value, json};
use std::time::Duration;

pub struct Pod {
    pub base_url: String,
    pub name: String,
    token: String,
    http: reqwest::blocking::Client,
}

/// Why a pod couldn't be made: the server is full or busy, not a failure of
/// the newcomer's flow (a broken run).
#[derive(Debug)]
pub struct Unavailable(pub String);

impl Pod {
    /// `POST /v1/pods/temporary`, naming the caller in `X-Construct-Client`.
    pub fn temporary(base_url: &str, label: &str) -> Result<Pod, Unavailable> {
        let http = client();
        let base_url = base_url.trim_end_matches('/').to_owned();
        let r = http
            .post(format!("{base_url}/v1/pods/temporary"))
            .header("X-Construct-Client", label)
            .send()
            .map_err(|e| Unavailable(format!("cannot reach {base_url}: {e}")))?;
        let status = r.status();
        let body: Value = r.json().unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(Unavailable(format!(
                "POST /v1/pods/temporary answered {status} {}",
                body["error"].as_str().unwrap_or("")
            )));
        }
        let token = body["session_token"]
            .as_str()
            .ok_or_else(|| Unavailable("no session_token in the answer".into()))?
            .to_owned();
        Ok(Pod {
            base_url,
            name: body["pod"].as_str().unwrap_or_default().to_owned(),
            token,
            http,
        })
    }

    /// The pod's token, for the agent's MCP config and the simulator. Never
    /// written to a file.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Calls one of the pod's MCP tools: its answer's JSON (or its text, as
    /// a string), `Err` with the tool's error.
    pub fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let r = self
            .http
            .post(format!("{}/v1/mcp", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": tool, "arguments": args}}))
            .send()
            .map_err(|e| format!("{tool}: {e}"))?;
        let v: Value = r.json().map_err(|e| format!("{tool}: {e}"))?;
        if let Some(e) = v.get("error") {
            return Err(format!("{tool}: {e}"));
        }
        let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
        let answer = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
        if v["result"]["isError"].as_bool().unwrap_or(false) || answer.get("error").is_some() {
            return Err(format!("{tool}: {text}"));
        }
        Ok(answer)
    }
}

impl Drop for Pod {
    /// Gives the slot back now instead of holding it for its whole lifetime.
    fn drop(&mut self) {
        let _ = self
            .http
            .delete(format!("{}/v1/pods/temporary", self.base_url))
            .bearer_auth(&self.token)
            .send();
    }
}

/// A client for the server under test: a fixed host the caller chose, not
/// one a pod's user picked.
#[allow(clippy::disallowed_methods)]
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("an HTTP client")
}
