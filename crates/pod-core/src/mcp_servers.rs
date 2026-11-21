//! MCP servers the pod is plugged into (decision `mcp-jack`): a client of
//! other people's Model Context Protocol servers, so pod features (a
//! Collections filler) can call their tools.
//!
//! The `mcp` jack ([`crate::jacks::MCP`]) switches the whole thing on or off;
//! the servers themselves are named entries in `mcp_servers`, each with an
//! https URL and its auth:
//!
//! - **none**: the server needs nothing.
//! - **token**: a bearer token the user gave once (sealed, never shown back).
//! - **oauth**: the MCP authorization flow. [`connect`] finds the server's
//!   authorization server (RFC 9728 protected resource metadata, then RFC
//!   8414 metadata), registers the pod as a client (RFC 7591, public client)
//!   and answers a sign-in URL (PKCE S256, `resource` set to the server). The
//!   user opens it in a browser and signs in; the authorization server sends
//!   them back to the pod's public callback ([`CALLBACK_PATH`] under the
//!   pod's public prefix), which trades the code for tokens. Access tokens
//!   refresh on their own (one refresh at a time per pod, since refresh
//!   tokens rotate); when a refresh is refused the server reads
//!   `needs_sign_in` and [`connect`] gives a new sign-in URL.
//!
//! What the pod may call (issue #436): the tools a server marks read-only
//! (`readOnlyHint: true`, recorded when its tools are listed). A filler
//! calls nothing else; `mcp_call_tool` calls another only when it is on the
//! server's allow list (`mcp_connect {allow_tools}`), which the user sets.
//! Sign-in asks for no scopes unless the server's `WWW-Authenticate` names
//! some or `mcp_connect {scopes}` gives them.
//!
//! Calls use the Streamable HTTP transport: each call opens its own session
//! (`initialize`, `notifications/initialized`, then the request), answered as
//! JSON or as an event stream. Only https URLs on public hosts are called
//! (private hosts only when the server allows them, for tests), redirects are
//! not followed, and answers are capped in size and time.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::adapter::Pod;
use crate::net::{OutboundClient, OutboundResponse};
use crate::store::Store;
use crate::util::now_secs;

/// The pod's OAuth callback, under its public prefix (a GET without a
/// bearer: the browser lands here after signing in).
pub const CALLBACK_PATH: &str = "/v1/mcp-servers/callback";
/// The MCP protocol version the pod speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// One HTTP request's time limit (a server waking from sleep can be slow).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// The largest answer read from a server.
pub const MAX_BODY: usize = 8 * 1024 * 1024;
/// Servers per pod.
pub const MAX_SERVERS: usize = 20;
/// How long a sign-in URL stays good.
pub const PENDING_TTL_SECS: i64 = 30 * 60;
/// Tools listed per server.
const MAX_TOOLS: usize = 200;
/// A tool description's length in listings.
const MAX_DESCRIPTION: usize = 600;
/// The longest tool name kept (a longer one is skipped: a cut name can't be
/// called).
const MAX_TOOL_NAME: usize = 128;
/// A tool's input schema, serialized, at most; a larger one is stored as a
/// bare object schema.
const MAX_SCHEMA: usize = 16 * 1024;
/// All of a server's tools, serialized, at most; tools past it are dropped.
const MAX_TOOLS_JSON: usize = 1024 * 1024;
/// A server's error text kept in `last_error`.
const MAX_ERROR: usize = 300;

/// The state of a server.
pub const CONNECTED: &str = "connected";
pub const NEEDS_SIGN_IN: &str = "needs_sign_in";
pub const FAILING: &str = "failing";

/// A pod's lock around token refreshes (refresh tokens rotate: two at once
/// would spend the same one).
#[derive(Default)]
pub struct RefreshLock(tokio::sync::Mutex<()>);

/// Why a call failed. `sign_in`: the server needs the user to sign in again
/// (`mcp_connect` gives the URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub message: String,
    pub sign_in: bool,
}

impl CallError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            sign_in: false,
        }
    }

    fn sign_in(name: &str) -> Self {
        Self {
            message: format!(
                "the MCP server '{name}' needs you to sign in again: ask Claude to run mcp_connect {{name: \"{name}\"}} and open the link it gives"
            ),
            sign_in: true,
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

// ---------- storage ----------

/// One stored server.
#[derive(Debug, Clone, PartialEq)]
struct Server {
    name: String,
    url: String,
    /// `none`, `token` or `oauth`.
    auth: String,
    /// OAuth endpoints, client id, redirect URI, expiry, pending sign-in.
    config: Map<String, Value>,
    /// Sealed values by field (`token`, `access_token`, `refresh_token`,
    /// `client_secret`, `verifier`).
    secrets: Map<String, Value>,
    state: String,
    last_error: Option<String>,
    tools: Value,
    created_at: i64,
}

impl Server {
    fn text(&self, k: &str) -> Option<&str> {
        self.config
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }
}

fn secret_context(name: &str, field: &str) -> String {
    format!("mcp_servers:{name}:{field}")
}

fn parse_map(s: String) -> Map<String, Value> {
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn row_to_server(r: &rusqlite::Row<'_>) -> rusqlite::Result<Server> {
    Ok(Server {
        name: r.get(0)?,
        url: r.get(1)?,
        auth: r.get(2)?,
        config: parse_map(r.get(3)?),
        secrets: parse_map(r.get(4)?),
        state: r.get(5)?,
        last_error: r.get(6)?,
        tools: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_else(|_| json!([])),
        created_at: r.get(8)?,
    })
}

const COLUMNS: &str = "name, url, auth, config, secrets, state, last_error, tools, created_at";

fn load_on(c: &Connection, name: &str) -> rusqlite::Result<Option<Server>> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM mcp_servers WHERE name = ?1"),
        [name],
        row_to_server,
    )
    .optional()
}

fn all_on(c: &Connection) -> rusqlite::Result<Vec<Server>> {
    let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM mcp_servers ORDER BY name"))?;
    let rows = st.query_map([], row_to_server)?;
    rows.collect()
}

/// The server whose sign-in `state` hashes to `hash`, still good at `now`:
/// looked up in SQL, so the public callback reads one row whatever it is sent.
fn pending_on(c: &Connection, hash: &str, now: i64) -> rusqlite::Result<Option<Server>> {
    c.query_row(
        &format!(
            "SELECT {COLUMNS} FROM mcp_servers
             WHERE json_extract(config, '$.pending_state') = ?1
               AND json_extract(config, '$.pending_until') >= ?2
             LIMIT 1"
        ),
        params![hash, now],
        row_to_server,
    )
    .optional()
}

fn save_on(c: &Connection, s: &Server) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO mcp_servers (name, url, auth, config, secrets, state, last_error, tools, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (name) DO UPDATE SET url = excluded.url, auth = excluded.auth,
             config = excluded.config, secrets = excluded.secrets, state = excluded.state,
             last_error = excluded.last_error, tools = excluded.tools,
             updated_at = excluded.updated_at",
        params![
            s.name,
            s.url,
            s.auth,
            Value::Object(s.config.clone()).to_string(),
            Value::Object(s.secrets.clone()).to_string(),
            s.state,
            s.last_error,
            s.tools.to_string(),
            s.created_at,
            now_secs(),
        ],
    )?;
    Ok(())
}

async fn load(pod: &Pod, name: &str) -> Option<Server> {
    let n = name.to_owned();
    pod.blocking(move |s| s.with_conn(|c| load_on(c, &n)))
        .await
        .ok()
        .flatten()
}

async fn save(pod: &Pod, server: &Server) -> Result<(), CallError> {
    let s = server.clone();
    pod.blocking(move |st| st.with_conn(|c| save_on(c, &s)))
        .await
        .map_err(|_| CallError::new("the pod could not save the MCP server"))
}

fn open_secret(store: &Store, s: &Server, field: &str) -> Option<String> {
    let sealed = s.secrets.get(field)?.as_str()?;
    store
        .secrets()
        .open(&secret_context(&s.name, field), sealed)
        .ok()
        .filter(|v| !v.is_empty())
}

fn set_secret(store: &Store, s: &mut Server, field: &str, value: Option<&str>) {
    match value {
        Some(v) => {
            let sealed = store.secrets().seal(&secret_context(&s.name, field), v);
            s.secrets.insert(field.to_owned(), json!(sealed));
        }
        None => {
            s.secrets.remove(field);
        }
    }
}

/// The servers as `list_jacks` shows them under the `mcp` jack: name, URL
/// and state.
pub fn summaries(c: &Connection) -> rusqlite::Result<Vec<Value>> {
    Ok(all_on(c)?
        .iter()
        .map(|s| json!({ "name": s.name, "url": s.url, "state": s.state }))
        .collect())
}

/// Where server `s`'s own text comes from, for [`crate::mcp::untrusted`]:
/// its URL's host.
fn origin(s: &Server) -> String {
    host_of(&s.url)
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// A server as Claude sees it. `schemas`: each tool's input schema too.
/// The server's own text (each tool's description and schema, its last
/// error) is marked untrusted: another party wrote it (issue #439).
fn describe(s: &Server, schemas: bool) -> Value {
    let from = origin(s);
    let allowed = allow_list(s);
    let tools: Vec<Value> = s
        .tools
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    let tool = t["name"].as_str().unwrap_or("");
                    let mut v = json!({
                        "name": t["name"],
                        "description": crate::mcp::untrusted(&from, t["description"].clone()),
                        "read_only": read_only(t),
                        "callable": read_only(t) || allowed.iter().any(|a| a == tool),
                    });
                    if destructive(t) {
                        v["destructive"] = json!(true);
                    }
                    if schemas {
                        v["input_schema"] = crate::mcp::untrusted(&from, t["inputSchema"].clone());
                    }
                    v
                })
                .collect()
        })
        .unwrap_or_default();
    let mut v = json!({
        "name": s.name,
        "url": s.url,
        "auth": s.auth,
        "state": s.state,
        "allow_tools": allowed,
        "tools": tools,
    });
    if let Some(e) = &s.last_error {
        v["last_error"] = crate::mcp::untrusted(&from, json!(e));
    }
    if s.auth == "token" {
        v["token_set"] = json!(s.secrets.contains_key("token"));
    }
    v
}

// ---------- HTTP ----------

fn client(pod: &Pod) -> OutboundClient {
    // A redirected POST would turn into a GET, and a token must not follow a
    // redirect to another host: neither is followed.
    OutboundClient::builder(REQUEST_TIMEOUT)
        .private(pod.mcp_private_hosts())
        .no_redirects()
        .body_cap(MAX_BODY)
        .build()
}

/// `s` as a URL the pod may call: https on a public host name, or any
/// http(s) URL when the server allows private hosts (tests).
fn check_url(pod: &Pod, s: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(s.trim()).map_err(|_| format!("'{s}' is not a URL"))?;
    if pod.mcp_private_hosts() && matches!(u.scheme(), "http" | "https") {
        return Ok(u);
    }
    if crate::net::is_public_https(&u) {
        return Ok(u);
    }
    Err(format!(
        "'{s}' is not an https:// address on a public host name (default port)"
    ))
}

/// Read a response's body, at most [`MAX_BODY`].
async fn body_of(resp: OutboundResponse) -> Result<Vec<u8>, String> {
    resp.body_cap(MAX_BODY).bytes().await.map_err(|e| {
        if e.is_too_large() {
            "the answer is too large".into()
        } else {
            format!("reading the answer failed: {e}")
        }
    })
}

fn short(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

// ---------- the transport ----------

/// Why a JSON-RPC exchange failed.
#[derive(Debug)]
enum RpcError {
    /// 401: the `WWW-Authenticate` header, if any.
    Unauthorized(Option<String>),
    Failed(String),
}

impl RpcError {
    fn message(self) -> String {
        match self {
            RpcError::Unauthorized(_) => "the server answered 401 Unauthorized".into(),
            RpcError::Failed(m) => m,
        }
    }
}

/// One MCP session over Streamable HTTP.
struct Conn {
    client: OutboundClient,
    url: reqwest::Url,
    bearer: Option<String>,
    session: Option<String>,
    protocol: Option<String>,
    next_id: i64,
}

/// The JSON-RPC message answering `id` in an event stream's `data:` lines.
fn from_event_stream(text: &str, id: i64) -> Option<Value> {
    let mut data = String::new();
    let mut found = None;
    let mut flush = |data: &mut String| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data)
                && v["id"].as_i64() == Some(id)
                && found.is_none()
            {
                found = Some(v);
            }
            data.clear();
        }
    };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            flush(&mut data);
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    flush(&mut data);
    found
}

impl Conn {
    async fn open(
        client: OutboundClient,
        url: reqwest::Url,
        bearer: Option<String>,
    ) -> Result<Self, RpcError> {
        let mut c = Conn {
            client,
            url,
            bearer,
            session: None,
            protocol: None,
            next_id: 0,
        };
        let init = c
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "the-construct", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        c.protocol = Some(
            init["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION)
                .to_owned(),
        );
        c.send(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            None,
        )
        .await?;
        Ok(c)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let answer = self
            .send(&msg, Some(id))
            .await?
            .ok_or_else(|| RpcError::Failed(format!("the server didn't answer {method}")))?;
        if let Some(e) = answer.get("error").filter(|e| !e.is_null()) {
            return Err(RpcError::Failed(format!(
                "{method} failed: {}",
                short(e["message"].as_str().unwrap_or("an error"), 300)
            )));
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    /// POST one message; with `id`, read the answer to it.
    async fn send(&mut self, msg: &Value, id: Option<i64>) -> Result<Option<Value>, RpcError> {
        let mut req = self
            .client
            .post(self.url.clone())
            .header(header::ACCEPT, "application/json, text/event-stream")
            .json(msg);
        if let Some(b) = &self.bearer {
            req = req.bearer_auth(b);
        }
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(p) = &self.protocol {
            req = req.header("MCP-Protocol-Version", p);
        }
        let resp = req.send().await.map_err(|e| {
            RpcError::Failed(if e.is_timeout() {
                "the server took too long to answer".into()
            } else if e.is_connect() {
                "the server could not be reached".into()
            } else {
                format!("the request failed: {e}")
            })
        })?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            let www = resp
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            return Err(RpcError::Unauthorized(www));
        }
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(s.to_owned());
        }
        let stream = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.contains("text/event-stream"));
        if !status.is_success() {
            let body = body_of(resp).await.unwrap_or_default();
            return Err(RpcError::Failed(format!(
                "the server answered {}: {}",
                status.as_u16(),
                short(&String::from_utf8_lossy(&body), 200)
            )));
        }
        let Some(id) = id else {
            return Ok(None);
        };
        let body = body_of(resp).await.map_err(RpcError::Failed)?;
        let text = String::from_utf8_lossy(&body);
        if stream {
            return Ok(from_event_stream(&text, id));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|_| RpcError::Failed("the server's answer is not JSON".into()))?;
        Ok(match v {
            Value::Array(a) => a.into_iter().find(|m| m["id"].as_i64() == Some(id)),
            m => Some(m),
        })
    }

    /// Every tool (`tools/list`, following cursors).
    async fn tools(&mut self) -> Result<Vec<Value>, RpcError> {
        let mut out = Vec::new();
        let mut size = 0;
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params).await?;
            for t in r["tools"].as_array().into_iter().flatten() {
                if out.len() >= MAX_TOOLS || size >= MAX_TOOLS_JSON {
                    break;
                }
                let Some(tool) = kept_tool(t) else { continue };
                size += tool.to_string().len();
                if size > MAX_TOOLS_JSON {
                    break;
                }
                out.push(tool);
            }
            cursor = r["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() || out.len() >= MAX_TOOLS || size >= MAX_TOOLS_JSON {
                break;
            }
        }
        Ok(out)
    }
}

/// A listed tool as the pod stores it, capped: `None` for one without a
/// usable name; a description cut to [`MAX_DESCRIPTION`]; a schema over
/// [`MAX_SCHEMA`] replaced by a bare object schema.
fn kept_tool(t: &Value) -> Option<Value> {
    let name = t["name"].as_str().map(str::trim)?;
    if name.is_empty() || name.chars().count() > MAX_TOOL_NAME {
        return None;
    }
    let schema = &t["inputSchema"];
    let schema = if schema.to_string().len() > MAX_SCHEMA {
        json!({ "type": "object" })
    } else {
        schema.clone()
    };
    // Only the hints the pod acts on, and only as booleans. Always present
    // (maybe empty): a stored tool without it was listed before the pod
    // recorded hints, and is listed again before it is called.
    let mut hints = Map::new();
    for k in ["readOnlyHint", "destructiveHint"] {
        if let Some(b) = t["annotations"][k].as_bool() {
            hints.insert(k.into(), json!(b));
        }
    }
    Some(json!({
        "name": name,
        "description": short(t["description"].as_str().unwrap_or(""), MAX_DESCRIPTION),
        "inputSchema": schema,
        "annotations": hints,
    }))
}

/// Whether a stored tool is marked read-only (`readOnlyHint: true`; the
/// MCP default is false).
fn read_only(t: &Value) -> bool {
    t["annotations"]["readOnlyHint"] == true
}

/// Whether a stored tool is marked destructive (`destructiveHint: true`).
fn destructive(t: &Value) -> bool {
    t["annotations"]["destructiveHint"] == true
}

/// The stored tool named `tool`.
fn find_tool<'a>(tools: &'a Value, tool: &str) -> Option<&'a Value> {
    tools.as_array()?.iter().find(|t| t["name"] == tool)
}

/// A server's allow list: the tools not marked read-only that
/// `mcp_call_tool` may call all the same.
fn allow_list(s: &Server) -> Vec<String> {
    s.config
        .get("allow_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Who calls a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// Claude, through `mcp_call_tool`: read-only tools and the allow list.
    Claude,
    /// A collection filler, unattended: read-only tools only.
    Filler,
}

/// Whether `caller` may call `tool` of server `name`, given its listed
/// `tools` and `allow` list: `Err` with why not.
fn permitted(
    name: &str,
    tools: &Value,
    allow: &[String],
    tool: &str,
    caller: Caller,
) -> Result<(), String> {
    let Some(t) = find_tool(tools, tool) else {
        return Err(format!(
            "the MCP server '{name}' lists no tool named '{tool}' (mcp_servers {{name: \"{name}\"}} shows its tools)"
        ));
    };
    if read_only(t) {
        return Ok(());
    }
    let marked = if destructive(t) {
        "is marked destructive"
    } else {
        "isn't marked read-only (readOnlyHint)"
    };
    match caller {
        Caller::Filler => Err(format!(
            "a filler only calls tools the server marks read-only, and '{tool}' of '{name}' {marked}"
        )),
        Caller::Claude if allow.iter().any(|a| a == tool) => Ok(()),
        Caller::Claude => Err(format!(
            "'{tool}' of '{name}' {marked}, so the pod calls it only when it is on the server's allow list. Only if the user asks for it: mcp_connect {{name: \"{name}\", allow_tools: [\"{tool}\"]}}"
        )),
    }
}

/// A `tools/call` result as a value: its structured content, else its text
/// (parsed as JSON when it is JSON; several texts become a list). An error
/// result is an `Err` with its text.
pub fn result_value(result: &Value) -> Result<Value, String> {
    let texts: Vec<&str> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect();
    if result["isError"] == true {
        let t = texts.join("\n");
        return Err(if t.trim().is_empty() {
            "the tool failed".into()
        } else {
            short(&t, 500)
        });
    }
    if let Some(s) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(s.clone());
    }
    let parse = |t: &str| serde_json::from_str::<Value>(t).unwrap_or_else(|_| json!(t));
    Ok(match texts.as_slice() {
        [] => Value::Null,
        [one] => parse(one),
        many => Value::Array(many.iter().map(|t| parse(t)).collect()),
    })
}

// ---------- OAuth ----------

/// Where a server's users sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthServer {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

/// A parameter of a `WWW-Authenticate: Bearer k="v", ...` header.
fn auth_param(www: &str, key: &str) -> Option<String> {
    let rest = www.trim();
    let rest = rest
        .strip_prefix("Bearer")
        .or_else(|| rest.strip_prefix("bearer"))
        .unwrap_or(rest);
    for part in rest.split(',') {
        if let Some((k, v)) = part.split_once('=')
            && k.trim().eq_ignore_ascii_case(key)
        {
            return Some(v.trim().trim_matches('"').to_owned());
        }
    }
    None
}

async fn get_json(pod: &Pod, client: &OutboundClient, url: &str) -> Result<Value, String> {
    let u = check_url(pod, url)?;
    let resp = client
        .get(u)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status().as_u16()));
    }
    let body = body_of(resp).await?;
    serde_json::from_slice(&body).map_err(|_| format!("{url} is not JSON"))
}

/// The well-known URLs of `base` for `suffix`: path-aware first (RFC 8414
/// §3.1, RFC 9728 §3.1), then the root.
fn well_known(base: &reqwest::Url, suffix: &str) -> Vec<String> {
    let origin = base.origin().ascii_serialization();
    let path = base.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.push(format!("{origin}/.well-known/{suffix}{path}"));
    }
    out.push(format!("{origin}/.well-known/{suffix}"));
    out
}

/// Find where users of the MCP server at `url` sign in.
async fn discover(
    pod: &Pod,
    client: &OutboundClient,
    url: &reqwest::Url,
    www: Option<&str>,
) -> Result<AuthServer, String> {
    let mut resource_urls = Vec::new();
    if let Some(m) = www.and_then(|w| auth_param(w, "resource_metadata")) {
        resource_urls.push(m);
    }
    resource_urls.extend(well_known(url, "oauth-protected-resource"));
    let mut issuer = None;
    for r in &resource_urls {
        if let Ok(meta) = get_json(pod, client, r).await
            && let Some(a) = meta["authorization_servers"][0].as_str()
        {
            issuer = Some(a.to_owned());
            break;
        }
    }
    // Servers from before protected resource metadata are their own
    // authorization server.
    let issuer = issuer.unwrap_or_else(|| url.origin().ascii_serialization());
    let issuer_url = check_url(pod, &issuer)?;
    let mut candidates = well_known(&issuer_url, "oauth-authorization-server");
    candidates.extend(well_known(&issuer_url, "openid-configuration"));
    let mut meta = None;
    for c in &candidates {
        if let Ok(m) = get_json(pod, client, c).await
            && m["authorization_endpoint"].is_string()
            && m["token_endpoint"].is_string()
        {
            meta = Some(m);
            break;
        }
    }
    let Some(meta) = meta else {
        return Err(format!(
            "the server asks for a sign-in, but its authorization server ({issuer}) publishes no OAuth metadata"
        ));
    };
    if let Some(methods) = meta["code_challenge_methods_supported"].as_array()
        && !methods.iter().any(|m| m == "S256")
    {
        return Err("the authorization server doesn't support PKCE (S256)".into());
    }
    let text = |k: &str| meta[k].as_str().map(str::to_owned);
    let a = AuthServer {
        issuer,
        authorization_endpoint: text("authorization_endpoint").unwrap_or_default(),
        token_endpoint: text("token_endpoint").unwrap_or_default(),
        registration_endpoint: text("registration_endpoint"),
    };
    check_url(pod, &a.authorization_endpoint)?;
    check_url(pod, &a.token_endpoint)?;
    if let Some(r) = &a.registration_endpoint {
        check_url(pod, r)?;
    }
    Ok(a)
}

/// A random URL-safe token of 32 bytes.
fn random_token() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("the OS random number generator");
    URL_SAFE_NO_PAD.encode(b)
}

fn sha256_hex(s: &str) -> String {
    crate::util::hex(&Sha256::digest(s.as_bytes()))
}

/// The PKCE S256 challenge of `verifier`.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Register the pod as a client of `a` (RFC 7591): `(client_id,
/// client_secret, how the secret is sent)`.
async fn register(
    client: &OutboundClient,
    a: &AuthServer,
    redirect_uri: &str,
) -> Result<(String, Option<String>, String), String> {
    let Some(endpoint) = &a.registration_endpoint else {
        return Err(
            "the authorization server has no dynamic client registration, so the pod can't sign in to it"
                .into(),
        );
    };
    let resp = client
        .post(endpoint)
        .json(&json!({
            "client_name": "The Construct",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|e| format!("registering with the authorization server failed: {e}"))?;
    let status = resp.status();
    let body = body_of(resp).await?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let why = v["error_description"]
            .as_str()
            .or(v["error"].as_str())
            .map(|e| short(e, MAX_ERROR))
            .unwrap_or_else(|| short(&String::from_utf8_lossy(&body), 200));
        return Err(format!(
            "the authorization server refused to register the pod ({}): {why}",
            status.as_u16()
        ));
    }
    let id = v["client_id"]
        .as_str()
        .ok_or("the authorization server's registration has no client_id")?
        .to_owned();
    let secret = v["client_secret"].as_str().map(str::to_owned);
    let method = v["token_endpoint_auth_method"]
        .as_str()
        .unwrap_or(if secret.is_some() {
            "client_secret_post"
        } else {
            "none"
        })
        .to_owned();
    Ok((id, secret, method))
}

/// Tokens from a token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tokens {
    access: String,
    refresh: Option<String>,
    expires_in: Option<i64>,
}

/// Why a token request failed. `refused`: the grant is no good (sign in
/// again); otherwise a passing failure.
struct TokenError {
    message: String,
    refused: bool,
}

async fn token_request(
    client: &OutboundClient,
    s: &Server,
    store: &Store,
    form: Vec<(&str, String)>,
) -> Result<Tokens, TokenError> {
    let endpoint = s.text("token_endpoint").unwrap_or_default().to_owned();
    let client_id = s.text("client_id").unwrap_or_default().to_owned();
    let mut form = form;
    form.push(("client_id", client_id.clone()));
    form.push(("resource", s.url.clone()));
    let secret = open_secret(store, s, "client_secret");
    let mut req = client
        .post(&endpoint)
        .header(header::ACCEPT, "application/json");
    match (s.text("token_auth"), &secret) {
        (Some("client_secret_basic"), Some(sec)) => req = req.basic_auth(&client_id, Some(sec)),
        (_, Some(sec)) => form.push(("client_secret", sec.clone())),
        _ => {}
    }
    let resp = req.form(&form).send().await.map_err(|e| TokenError {
        message: format!("the token request failed: {e}"),
        refused: false,
    })?;
    let status = resp.status();
    let body = body_of(resp).await.map_err(|m| TokenError {
        message: m,
        refused: false,
    })?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let err = v["error"].as_str().unwrap_or("");
        return Err(TokenError {
            message: format!(
                "the authorization server answered {}: {}",
                status.as_u16(),
                short(v["error_description"].as_str().unwrap_or(err), MAX_ERROR)
            ),
            refused: status.is_client_error(),
        });
    }
    let access = v["access_token"].as_str().ok_or(TokenError {
        message: "the token answer has no access_token".into(),
        refused: false,
    })?;
    Ok(Tokens {
        access: access.to_owned(),
        refresh: v["refresh_token"].as_str().map(str::to_owned),
        expires_in: v["expires_in"].as_i64(),
    })
}

fn store_tokens(store: &Store, s: &mut Server, t: &Tokens, now: i64) {
    set_secret(store, s, "access_token", Some(&t.access));
    if let Some(r) = &t.refresh {
        set_secret(store, s, "refresh_token", Some(r));
    }
    match t.expires_in {
        Some(secs) => s.config.insert("expires_at".into(), json!(now + secs)),
        None => s.config.remove("expires_at"),
    };
}

/// The pod's public base URL: the host's (`CONSTRUCT_PUBLIC_URL`), else the
/// request's `Host` (https behind a proxy that says so).
fn base_url(pod: &Pod, headers: &HeaderMap) -> String {
    if let Some(b) = pod.public_base() {
        return b.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"))
        || headers.contains_key("fly-forwarded-port");
    format!("{}://{host}", if https { "https" } else { "http" })
}

/// The scopes a sign-in asks for: the ones `mcp_connect` gave (stored as
/// `scopes`), else the ones the server's `WWW-Authenticate` names, else
/// none. Never every scope the authorization server supports.
fn sign_in_scope(s: &Server, www: Option<&str>) -> Option<String> {
    if let Some(given) = s.config.get("scopes").and_then(Value::as_array) {
        let given: Vec<&str> = given.iter().filter_map(Value::as_str).collect();
        return (!given.is_empty()).then(|| given.join(" "));
    }
    www.and_then(|w| auth_param(w, "scope"))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// A sign-in: the URL to open and the host it signs in at.
struct SignIn {
    url: String,
    host: String,
}

/// Start signing in to `s`: find its authorization server, register (once
/// per redirect URI), and answer the URL to open.
async fn start_sign_in(
    pod: &Pod,
    client: &OutboundClient,
    s: &mut Server,
    www: Option<&str>,
    redirect_uri: &str,
) -> Result<SignIn, String> {
    let url = check_url(pod, &s.url)?;
    let a = discover(pod, client, &url, www).await?;
    let same_client = s.text("issuer") == Some(a.issuer.as_str())
        && s.text("redirect_uri") == Some(redirect_uri)
        && s.text("client_id").is_some();
    if !same_client {
        let (id, secret, method) = register(client, &a, redirect_uri).await?;
        s.config.insert("client_id".into(), json!(id));
        s.config.insert("token_auth".into(), json!(method));
        set_secret(pod.store(), s, "client_secret", secret.as_deref());
    }
    s.config.insert("issuer".into(), json!(a.issuer));
    s.config
        .insert("token_endpoint".into(), json!(a.token_endpoint));
    s.config.insert("redirect_uri".into(), json!(redirect_uri));
    let verifier = random_token();
    let state = random_token();
    set_secret(pod.store(), s, "verifier", Some(&verifier));
    s.config
        .insert("pending_state".into(), json!(sha256_hex(&state)));
    s.config
        .insert("pending_until".into(), json!(now_secs() + PENDING_TTL_SECS));
    let mut auth = check_url(pod, &a.authorization_endpoint)?;
    {
        let mut q = auth.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", s.text("client_id").unwrap_or_default())
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &s.url);
        if let Some(scope) = sign_in_scope(s, www) {
            q.append_pair("scope", &scope);
        }
    }
    s.auth = "oauth".into();
    s.state = NEEDS_SIGN_IN.into();
    Ok(SignIn {
        host: auth.host_str().unwrap_or_default().to_owned(),
        url: auth.to_string(),
    })
}

/// The bearer to call `s` with: its token, or (OAuth) an access token,
/// refreshed first when it is about to expire or `force`. Reloads `s` (a
/// refresh elsewhere may have rotated its tokens).
async fn bearer(pod: &Pod, s: &mut Server, force: bool) -> Result<Option<String>, CallError> {
    match s.auth.as_str() {
        "token" => return Ok(open_secret(pod.store(), s, "token")),
        "oauth" => {}
        _ => return Ok(None),
    }
    let lock = pod.local::<RefreshLock>();
    let _held = lock.0.lock().await;
    if let Some(fresh) = load(pod, &s.name).await {
        *s = fresh;
    }
    let access = open_secret(pod.store(), s, "access_token");
    let expires = s.config.get("expires_at").and_then(Value::as_i64);
    let now = now_secs();
    if !force
        && let Some(a) = &access
        && expires.is_none_or(|e| e > now + 60)
    {
        return Ok(Some(a.clone()));
    }
    let Some(refresh) = open_secret(pod.store(), s, "refresh_token") else {
        if access.is_none() || force {
            s.state = NEEDS_SIGN_IN.into();
            s.last_error = Some("signed out: no refresh token".into());
            save(pod, s).await?;
            return Err(CallError::sign_in(&s.name));
        }
        return Ok(access);
    };
    let client = client(pod);
    match token_request(
        &client,
        s,
        pod.store(),
        vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh),
        ],
    )
    .await
    {
        Ok(t) => {
            store_tokens(pod.store(), s, &t, now);
            save(pod, s).await?;
            Ok(Some(t.access))
        }
        Err(e) if e.refused => {
            s.state = NEEDS_SIGN_IN.into();
            s.last_error = Some(e.message);
            set_secret(pod.store(), s, "access_token", None);
            set_secret(pod.store(), s, "refresh_token", None);
            save(pod, s).await?;
            Err(CallError::sign_in(&s.name))
        }
        Err(e) => Err(CallError::new(e.message)),
    }
}

// ---------- the API ----------

/// Whether the `mcp` jack is plugged.
fn plugged(pod: &Pod) -> bool {
    crate::jacks::core_plugged(pod.store(), crate::jacks::MCP)
}

const UNPLUGGED: &str = "the pod's mcp jack is unplugged, so it can't call MCP servers (plug_jack mcp, if the server allows it)";

/// Whether `name` is a server name: `[a-z][a-z0-9_-]*`, at most 40.
pub fn valid_name(name: &str) -> bool {
    let mut b = name.bytes();
    name.len() <= 40
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Call tool `tool` of server `name` with `args` for `caller`; answers the
/// tool's value ([`result_value`]). Refused unless the tool is one `caller`
/// may call ([`Caller`]); a tool stored before the pod recorded hints is
/// listed again first.
pub async fn call_tool(
    pod: &Pod,
    name: &str,
    tool: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CallError> {
    if !plugged(pod) {
        return Err(CallError::new(UNPLUGGED));
    }
    let Some(mut s) = load(pod, name).await else {
        return Err(CallError::new(format!(
            "the pod has no MCP server named '{name}' (mcp_servers lists them)"
        )));
    };
    let allow = allow_list(&s);
    let known = find_tool(&s.tools, tool).is_some_and(|t| t.get("annotations").is_some());
    if known {
        permitted(name, &s.tools, &allow, tool, caller).map_err(CallError::new)?;
    }
    let url = check_url(pod, &s.url).map_err(CallError::new)?;
    let client = client(pod);
    let mut force = false;
    for attempt in 0..2 {
        let token = bearer(pod, &mut s, force).await?;
        if s.auth == "oauth" && token.is_none() {
            return Err(CallError::sign_in(name));
        }
        let outcome = async {
            let mut c = Conn::open(client.clone(), url.clone(), token).await?;
            // Not listed with its hints yet: list the tools again first.
            let fresh = if known {
                None
            } else {
                Some(json!(c.tools().await?))
            };
            if let Some(f) = &fresh
                && let Err(why) = permitted(name, f, &allow, tool, caller)
            {
                return Ok((fresh, Err(why)));
            }
            let r = c
                .request("tools/call", json!({ "name": tool, "arguments": args }))
                .await?;
            Ok((fresh, Ok(r)))
        }
        .await;
        match outcome {
            Ok((fresh, r)) => {
                let listed = fresh.is_some();
                if let Some(f) = fresh {
                    s.tools = f;
                }
                if listed || s.state != CONNECTED || s.last_error.is_some() {
                    s.state = CONNECTED.into();
                    s.last_error = None;
                    save(pod, &s).await?;
                }
                let r = r.map_err(CallError::new)?;
                return result_value(&r).map_err(CallError::new);
            }
            Err(RpcError::Unauthorized(_)) if s.auth == "oauth" && attempt == 0 => {
                force = true;
            }
            Err(RpcError::Unauthorized(_)) => {
                s.state = NEEDS_SIGN_IN.into();
                s.last_error = Some("the server refused the pod's credentials".into());
                save(pod, &s).await?;
                return Err(if s.auth == "oauth" {
                    CallError::sign_in(name)
                } else {
                    CallError::new(format!(
                        "the MCP server '{name}' refused the pod's credentials: give it a token with mcp_connect"
                    ))
                });
            }
            Err(RpcError::Failed(m)) => {
                s.state = FAILING.into();
                s.last_error = Some(m.clone());
                save(pod, &s).await?;
                return Err(CallError::new(m));
            }
        }
    }
    Err(CallError::sign_in(name))
}

/// Connect (or reconnect) server `name`.
async fn connect(
    pod: &Pod,
    headers: &HeaderMap,
    name: &str,
    url: Option<&str>,
    token: Option<&str>,
    allow_tools: Option<Vec<String>>,
    scopes: Option<Vec<String>>,
) -> Result<Value, Value> {
    let refuse = |e: &str, d: String| json!({ "error": e, "detail": d });
    if !plugged(pod) {
        return Err(refuse("jack_unplugged", UNPLUGGED.into()));
    }
    if !valid_name(name) {
        return Err(refuse(
            "invalid_arguments",
            "name is [a-z][a-z0-9_-]*, at most 40 characters".into(),
        ));
    }
    let existing = load(pod, name).await;
    let url = match (url.map(str::trim).filter(|u| !u.is_empty()), &existing) {
        (Some(u), _) => u.to_owned(),
        (None, Some(s)) => s.url.clone(),
        (None, None) => {
            return Err(refuse(
                "invalid_arguments",
                format!("url is required for a new server ('{name}' isn't connected yet)"),
            ));
        }
    };
    check_url(pod, &url).map_err(|e| refuse("invalid_url", e))?;
    if existing.is_none() {
        let count = pod
            .blocking(|s| s.with_conn(|c| all_on(c).map(|v| v.len())))
            .await
            .unwrap_or(0);
        if count >= MAX_SERVERS {
            return Err(refuse(
                "too_many_servers",
                format!(
                    "a pod connects at most {MAX_SERVERS} MCP servers: mcp_disconnect one first"
                ),
            ));
        }
    }
    let now = now_secs();
    let mut s = match existing {
        // Same URL: keep the client registration and tokens (a reconnect
        // after the sign-in expired). A new URL starts over.
        Some(s) if s.url == url => s,
        Some(s) => Server {
            created_at: s.created_at,
            ..blank(name, &url, now)
        },
        None => blank(name, &url, now),
    };
    if let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) {
        if t.len() > 4_000 {
            return Err(refuse("invalid_arguments", "token is too long".into()));
        }
        s.auth = "token".into();
        set_secret(pod.store(), &mut s, "token", Some(t));
    }
    let client = client(pod);
    let bearer_now = match bearer(pod, &mut s, false).await {
        Ok(b) => b,
        // Signed out: start a new sign-in below.
        Err(e) if e.sign_in => None,
        Err(e) => return Err(refuse("mcp_failed", e.message)),
    };
    // Replaced when given ([] clears them); kept on a reconnect otherwise.
    // After `bearer`, which reloads the stored server.
    if let Some(a) = allow_tools {
        s.config.insert("allow_tools".into(), json!(a));
    }
    if let Some(sc) = scopes {
        s.config.insert("scopes".into(), json!(sc));
    }
    let url_ok = check_url(pod, &s.url).map_err(|e| refuse("invalid_url", e))?;
    let listed = async {
        let mut c = Conn::open(client.clone(), url_ok.clone(), bearer_now).await?;
        c.tools().await
    }
    .await;
    match listed {
        Ok(tools) => {
            s.tools = json!(tools);
            s.state = CONNECTED.into();
            s.last_error = None;
            if s.auth.is_empty() {
                s.auth = "none".into();
            }
            save(pod, &s)
                .await
                .map_err(|e| refuse("store_error", e.message))?;
            Ok(json!({ "ok": true, "server": describe(&s, false),
                "next": "mcp_servers {name} shows each tool's input schema; mcp_call_tool tries one; a filler {kind: \"mcp\", server, tool, arguments, items, map} fills a collection from it" }))
        }
        Err(RpcError::Unauthorized(_)) if s.auth == "token" => {
            s.state = NEEDS_SIGN_IN.into();
            s.last_error = Some("the server refused the token".into());
            save(pod, &s)
                .await
                .map_err(|e| refuse("store_error", e.message))?;
            Err(refuse(
                "unauthorized",
                format!("the MCP server '{name}' refused the token"),
            ))
        }
        Err(RpcError::Unauthorized(www)) => {
            let redirect = format!(
                "{}{}{CALLBACK_PATH}",
                base_url(pod, headers),
                pod.public_prefix()
            );
            match start_sign_in(pod, &client, &mut s, www.as_deref(), &redirect).await {
                Ok(link) => {
                    save(pod, &s)
                        .await
                        .map_err(|e| refuse("store_error", e.message))?;
                    let host = link.host;
                    Ok(json!({
                        "ok": true,
                        "state": NEEDS_SIGN_IN,
                        "sign_in_url": link.url,
                        "sign_in_host": host,
                        "scope": sign_in_scope(&s, www.as_deref()),
                        "redirect_uri": redirect,
                        "detail": format!("'{name}' needs the user to sign in. Give them sign_in_url to open in a browser (it works for {} minutes) and tell them it signs in at {host}, so they can check that is the service they expect (the MCP server chose it); after they sign in, mcp_servers shows it connected. If the server refuses the redirect, it has to allow redirect_uri (e.g. its list of allowed redirect hosts).", PENDING_TTL_SECS / 60),
                    }))
                }
                Err(e) => {
                    s.state = FAILING.into();
                    s.last_error = Some(e.clone());
                    let _ = save(pod, &s).await;
                    Err(refuse("sign_in_unavailable", e))
                }
            }
        }
        Err(e) => {
            let m = e.message();
            Err(refuse(
                "mcp_failed",
                format!("connecting to '{name}' failed: {m}"),
            ))
        }
    }
}

fn blank(name: &str, url: &str, now: i64) -> Server {
    Server {
        name: name.to_owned(),
        url: url.to_owned(),
        auth: String::new(),
        config: Map::new(),
        secrets: Map::new(),
        state: NEEDS_SIGN_IN.into(),
        last_error: None,
        tools: json!([]),
        created_at: now,
    }
}

/// `GET /v1/mcp-servers/callback?code&state` (public): finish a sign-in.
pub async fn callback(State(pod): State<Pod>, Query(q): Query<Map<String, Value>>) -> Response {
    let text = |k: &str| q.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let (code, state, error) = (text("code"), text("state"), text("error"));
    if state.is_empty() {
        return page(StatusCode::BAD_REQUEST, "This sign-in link is incomplete.");
    }
    let hash = sha256_hex(&state);
    let now = now_secs();
    let found = pod
        .blocking(move |st| st.with_conn(|c| pending_on(c, &hash, now)))
        .await
        .ok()
        .flatten();
    let Some(mut s) = found else {
        return page(
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired or was already used. Ask Claude to connect the server again.",
        );
    };
    s.config.remove("pending_state");
    s.config.remove("pending_until");
    let verifier = open_secret(pod.store(), &s, "verifier");
    set_secret(pod.store(), &mut s, "verifier", None);
    if !error.is_empty() || code.is_empty() || verifier.is_none() {
        s.last_error = Some(format!(
            "sign-in didn't finish: {}",
            if error.is_empty() {
                "no code".to_owned()
            } else {
                short(&error, MAX_ERROR)
            }
        ));
        let _ = save(&pod, &s).await;
        return page(
            StatusCode::BAD_REQUEST,
            &format!(
                "Signing in to {} didn't finish. Ask Claude to try again.",
                s.name
            ),
        );
    }
    let client = client(&pod);
    let redirect = s.text("redirect_uri").unwrap_or_default().to_owned();
    let tokens = token_request(
        &client,
        &s,
        pod.store(),
        vec![
            ("grant_type", "authorization_code".into()),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier.unwrap_or_default()),
        ],
    )
    .await;
    let t = match tokens {
        Ok(t) => t,
        Err(e) => {
            s.last_error = Some(e.message);
            let _ = save(&pod, &s).await;
            return page(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "{} didn't accept the sign-in. Ask Claude to try again.",
                    s.name
                ),
            );
        }
    };
    store_tokens(pod.store(), &mut s, &t, now);
    s.state = CONNECTED.into();
    s.last_error = None;
    if let Ok(url) = check_url(&pod, &s.url) {
        let listed = async {
            let mut c = Conn::open(client.clone(), url, Some(t.access.clone())).await?;
            c.tools().await
        }
        .await;
        match listed {
            Ok(tools) => s.tools = json!(tools),
            Err(e) => {
                s.state = FAILING.into();
                s.last_error = Some(e.message());
            }
        }
    }
    if save(&pod, &s).await.is_err() {
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The pod could not save the sign-in.",
        );
    }
    page(
        StatusCode::OK,
        &format!(
            "{} is connected to your pod. You can close this tab.",
            s.name
        ),
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(status: StatusCode, message: &str) -> Response {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>The Construct</title></head><body><p>{}</p></body></html>",
        escape(message)
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// Forget server `name` (its tokens and registration).
async fn disconnect(pod: &Pod, name: &str) -> Result<bool, Value> {
    let n = name.to_owned();
    pod.blocking(move |s| {
        s.with_conn(|c| c.execute("DELETE FROM mcp_servers WHERE name = ?1", [&n]))
    })
    .await
    .map(|n| n > 0)
    .map_err(|_| json!({ "error": "store_error" }))
}

/// `v` with long text cut and long lists trimmed, for showing Claude a
/// tool's answer.
fn shorten(v: Value, depth: usize) -> Value {
    match v {
        Value::String(s) => Value::String(short(&s, 400)),
        Value::Array(a) => {
            let n = a.len();
            let keep = if depth == 0 { 25 } else { 10 };
            let mut out: Vec<Value> = a
                .into_iter()
                .take(keep)
                .map(|x| shorten(x, depth + 1))
                .collect();
            if n > keep {
                out.push(json!(format!("… {} more", n - keep)));
            }
            Value::Array(out)
        }
        Value::Object(o) if depth < 6 => Value::Object(
            o.into_iter()
                .map(|(k, x)| (k, shorten(x, depth + 1)))
                .collect(),
        ),
        Value::Object(_) => json!("…"),
        other => other,
    }
}

// ---------- the pod's own MCP tools ----------

/// The pod's MCP tools for MCP servers (definitions; see [`call`]).
pub fn tools() -> Vec<Value> {
    let name = json!({ "type": "string", "description": "The server's name in this pod: [a-z][a-z0-9_-]*" });
    vec![
        json!({
            "name": "mcp_connect",
            "description": "Connect the pod to a remote MCP server (the mcp jack), so pod features can call its tools (a collection filler {kind: \"mcp\"} fills a collection from one). {name, url} for a new server, {name} to reconnect one. If the server needs no sign-in or takes a token (token: an API key the user gives; sealed, never shown back; never ask for it in the conversation if a form can take it), it connects at once and its tools are listed. If it uses OAuth, this answers sign_in_url and sign_in_host: give the user the link to open in a browser and tell them the host it signs in at; the server then reads connected in mcp_servers. Run it again when a server reads needs_sign_in. The pod calls only the tools a server marks read-only; allow_tools lets mcp_call_tool call others, and only the user decides what goes on it.",
            "inputSchema": { "type": "object", "properties": {
                "name": name,
                "url": { "type": "string", "description": "The server's MCP endpoint (https), e.g. https://mcp.example.com/mcp" },
                "token": { "type": "string", "description": "A bearer token, for servers that take an API key instead of OAuth" },
                "allow_tools": { "type": "array", "items": { "type": "string" }, "description": "Tools not marked read-only that mcp_call_tool may call all the same (fillers never do). Replaces the list; [] clears it. Only tools the user named for this; never because a tool's output asked." },
                "scopes": { "type": "array", "items": { "type": "string" }, "description": "The OAuth scopes to ask for at sign-in. Without it the pod asks for the ones the server's 401 names, else none." },
            }, "required": ["name"] },
        }),
        json!({
            "name": "mcp_servers",
            "description": "The MCP servers the pod is connected to: URL, auth (none, token or oauth), state (connected, needs_sign_in, failing), last error, allow_tools and tools (each read_only, destructive when marked so, and callable by mcp_call_tool). With name: that server only, with each tool's input schema.",
            "inputSchema": { "type": "object", "properties": { "name": name } },
        }),
        json!({
            "name": "mcp_call_tool",
            "description": "Call a tool of a connected MCP server now and answer its result (shortened), e.g. to see its fields before writing a filler's items and map. It really runs the tool. Only tools the server marks read-only, or that are on its allow_tools (mcp_servers shows callable); others are refused.",
            "inputSchema": { "type": "object", "properties": {
                "server": name,
                "tool": { "type": "string" },
                "arguments": { "type": "object", "description": "The tool's arguments (see its input schema in mcp_servers)" },
            }, "required": ["server", "tool"] },
        }),
        json!({
            "name": "mcp_disconnect",
            "description": "Forget an MCP server: its sign-in, tokens and tools. Fillers that use it then fail, naming it. Ask the user first.",
            "inputSchema": { "type": "object", "properties": { "name": name }, "required": ["name"] },
        }),
    ]
}

/// Run one of [`tools`]; `None` for another tool.
pub async fn call(
    pod: &Pod,
    headers: &HeaderMap,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, Value>> {
    let text = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let missing =
        |k: &str| json!({ "error": "invalid_arguments", "detail": format!("\"{k}\" is required") });
    Some(match tool {
        "mcp_connect" => match text("name") {
            Some(n) => {
                let list = |k: &str| -> Result<Option<Vec<String>>, Value> {
                    match &args[k] {
                        Value::Null => Ok(None),
                        Value::Array(a)
                            if a.len() <= MAX_TOOLS
                                && a.iter().all(|v| {
                                    v.as_str().is_some_and(|s| {
                                        !s.trim().is_empty() && s.chars().count() <= MAX_TOOL_NAME
                                    })
                                }) =>
                        {
                            Ok(Some(
                                a.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.trim().to_owned()))
                                    .collect(),
                            ))
                        }
                        _ => Err(
                            json!({ "error": "invalid_arguments", "detail": format!("{k} is a list of names (at most {MAX_TOOLS}, each 1..={MAX_TOOL_NAME} characters)") }),
                        ),
                    }
                };
                match (list("allow_tools"), list("scopes")) {
                    (Ok(a), Ok(sc)) => {
                        connect(pod, headers, n, text("url"), text("token"), a, sc).await
                    }
                    (Err(e), _) | (_, Err(e)) => Err(e),
                }
            }
            None => Err(missing("name")),
        },
        "mcp_servers" => {
            let servers = pod
                .blocking(|s| s.with_conn(all_on))
                .await
                .unwrap_or_default();
            match text("name") {
                Some(n) => match servers.iter().find(|s| s.name == n) {
                    Some(s) => Ok(json!({ "server": describe(s, true) })),
                    None => Err(
                        json!({ "error": "not_found", "detail": format!("no MCP server named '{n}'") }),
                    ),
                },
                None => Ok(json!({
                    "jack": if plugged(pod) { "plugged" } else { "unplugged" },
                    "servers": servers.iter().map(|s| describe(s, false)).collect::<Vec<_>>(),
                })),
            }
        }
        "mcp_call_tool" => match (text("server"), text("tool")) {
            (Some(s), Some(t)) => {
                let a = match &args["arguments"] {
                    Value::Null => json!({}),
                    v @ Value::Object(_) => v.clone(),
                    _ => {
                        return Some(Err(
                            json!({ "error": "invalid_arguments", "detail": "arguments is an object" }),
                        ));
                    }
                };
                // The server's answer is its own text (issue #439); the
                // pod's own refusals (jack unplugged, no such server or
                // tool, a tool not allowed) stay plain.
                let from = if plugged(pod) {
                    load(pod, s)
                        .await
                        .filter(|x| {
                            permitted(&x.name, &x.tools, &allow_list(x), t, Caller::Claude).is_ok()
                        })
                        .map(|x| origin(&x))
                } else {
                    None
                };
                call_tool(pod, s, t, &a, Caller::Claude)
                    .await
                    .map(|v| {
                        let from = from.clone().unwrap_or_else(|| s.to_owned());
                        json!({ "ok": true, "result": crate::mcp::untrusted(&from, shorten(v, 0)) })
                    })
                    .map_err(|e| {
                        if e.sign_in {
                            json!({ "error": "needs_sign_in", "detail": e.message })
                        } else if let Some(from) = &from {
                            json!({ "error": "mcp_failed", "detail": crate::mcp::untrusted(from, json!(e.message)) })
                        } else {
                            json!({ "error": "mcp_failed", "detail": e.message })
                        }
                    })
            }
            (None, _) => Err(missing("server")),
            (_, None) => Err(missing("tool")),
        },
        "mcp_disconnect" => match text("name") {
            Some(n) => match disconnect(pod, n).await {
                Ok(true) => Ok(json!({ "ok": true })),
                Ok(false) => Err(
                    json!({ "error": "not_found", "detail": format!("no MCP server named '{n}'") }),
                ),
                Err(e) => Err(e),
            },
            None => Err(missing("name")),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_stream_finds_the_answer() {
        let s = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\n data: \"result\":{\"ok\":true}}\n\n";
        // A data field split over two lines is joined with a newline.
        let s = s.replace("\n data:", "\ndata:");
        assert_eq!(
            from_event_stream(&s, 2).map(|v| v["result"]["ok"].clone()),
            Some(json!(true))
        );
        assert_eq!(from_event_stream(&s, 3), None);
    }

    #[test]
    fn results_read_structured_then_text() {
        assert_eq!(
            result_value(&json!({"content": [{"type": "text", "text": "{\"a\":1}"}]})),
            Ok(json!({"a": 1}))
        );
        assert_eq!(
            result_value(
                &json!({"content": [{"type": "text", "text": "hi"}], "structuredContent": {"b": 2}})
            ),
            Ok(json!({"b": 2}))
        );
        assert_eq!(
            result_value(&json!({"content": [{"type": "text", "text": "plain"}]})),
            Ok(json!("plain"))
        );
        assert_eq!(
            result_value(&json!({"isError": true, "content": [{"type": "text", "text": "nope"}]})),
            Err("nope".into())
        );
    }

    #[test]
    fn www_authenticate_params() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp""#;
        assert_eq!(
            auth_param(h, "resource_metadata").as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(auth_param(h, "scope"), None);
    }

    #[test]
    fn well_known_urls_are_path_aware_first() {
        let u = reqwest::Url::parse("https://mcp.example.com/mcp").unwrap();
        assert_eq!(
            well_known(&u, "oauth-protected-resource"),
            vec![
                "https://mcp.example.com/.well-known/oauth-protected-resource/mcp".to_owned(),
                "https://mcp.example.com/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
        let root = reqwest::Url::parse("https://auth.example.com").unwrap();
        assert_eq!(
            well_known(&root, "oauth-authorization-server"),
            vec!["https://auth.example.com/.well-known/oauth-authorization-server".to_owned()]
        );
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn tools_keep_their_hints() {
        let t = kept_tool(&json!({"name": "send", "annotations":
            {"readOnlyHint": false, "destructiveHint": true, "title": "Send"}}))
        .unwrap();
        assert_eq!(
            t["annotations"],
            json!({"readOnlyHint": false, "destructiveHint": true})
        );
        assert!(!read_only(&t) && destructive(&t));
        let bare = kept_tool(&json!({"name": "note"})).unwrap();
        assert_eq!(bare["annotations"], json!({}), "present, empty");
        assert!(!read_only(&bare));
    }

    #[test]
    fn only_read_only_or_allowed_tools_are_called() {
        let tools = json!([
            {"name": "list", "annotations": {"readOnlyHint": true}},
            {"name": "send", "annotations": {"destructiveHint": true}},
            {"name": "note", "annotations": {}},
        ]);
        let none: Vec<String> = vec![];
        let allow = vec!["send".to_owned(), "note".to_owned()];
        for c in [Caller::Claude, Caller::Filler] {
            assert_eq!(permitted("s", &tools, &none, "list", c), Ok(()));
            assert!(permitted("s", &tools, &allow, "gone", c).is_err());
        }
        let e = permitted("s", &tools, &none, "send", Caller::Claude).unwrap_err();
        assert!(
            e.contains("destructive") && e.contains("allow_tools"),
            "{e}"
        );
        assert!(permitted("s", &tools, &none, "note", Caller::Claude).is_err());
        assert_eq!(
            permitted("s", &tools, &allow, "send", Caller::Claude),
            Ok(())
        );
        assert_eq!(
            permitted("s", &tools, &allow, "note", Caller::Claude),
            Ok(())
        );
        // A filler never calls past the hint, allow list or not.
        let e = permitted("s", &tools, &allow, "note", Caller::Filler).unwrap_err();
        assert!(e.contains("read-only"), "{e}");
        assert!(permitted("s", &tools, &allow, "send", Caller::Filler).is_err());
    }

    #[test]
    fn sign_in_asks_for_given_or_named_scopes_only() {
        let mut s = blank("s", "https://mcp.example.com/mcp", 0);
        let www = r#"Bearer error="insufficient_scope", scope="calendar.read mail.read""#;
        assert_eq!(sign_in_scope(&s, None), None, "none by default");
        assert_eq!(
            sign_in_scope(&s, Some(www)).as_deref(),
            Some("calendar.read mail.read")
        );
        s.config.insert("scopes".into(), json!(["calendar.read"]));
        assert_eq!(
            sign_in_scope(&s, Some(www)).as_deref(),
            Some("calendar.read")
        );
        s.config.insert("scopes".into(), json!([]));
        assert_eq!(sign_in_scope(&s, Some(www)), None, "[] asks for none");
    }

    #[test]
    fn names() {
        assert!(valid_name("proton"));
        assert!(valid_name("proton-mcp_2"));
        assert!(!valid_name("Proton"));
        assert!(!valid_name("2proton"));
        assert!(!valid_name(""));
    }
}
