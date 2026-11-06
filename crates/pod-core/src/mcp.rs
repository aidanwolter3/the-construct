//! `POST /v1/mcp`: the pod's MCP endpoint for Claude (decision
//! `mcp-connector`), streamable HTTP with one JSON response per request.
//!
//! Claude reaches it with an MCP credential the pod host issued (OAuth, or
//! `server pods mcp`): the host forwards such a request with
//! [`crate::CREDENTIAL_HEADER`]` = mcp` and no bearer, and the credential
//! reaches nothing else. The pod token itself works too (served alone, in
//! tests). The tools read the pod's apps, data sources and layouts and write
//! custom apps through the same validated path as `/v1/custom-apps`
//! ([`crate::custom`]); writes are recorded with author `claude`. Nothing
//! here can call an app's routes or act for the user upstream, except
//! `sample_data`, which reads (truncated) data the user's own pod already
//! serves them.

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::custom::{self, Writer};
use crate::session::Caller;
use crate::{Pod, RequestCtx, json_error, parse_json_body};

/// MCP protocol versions this endpoint speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Most items per query and characters per string `sample_data` returns.
pub const SAMPLE_MAX_ITEMS: usize = 20;
const SAMPLE_MAX_CHARS: usize = 300;

pub(crate) fn router() -> Router<Pod> {
    Router::new().route("/v1/mcp", post(handle).get(no_stream).delete(no_stream))
}

/// `GET` / `DELETE /v1/mcp`: no server-initiated stream and no sessions.
async fn no_stream() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        Json(json!({ "error": "method_not_allowed" })),
    )
        .into_response()
}

/// Whether the caller may use the MCP endpoint: an MCP credential vouched
/// for by the host, or the pod token's session.
async fn authorized(pod: &Pod, headers: &HeaderMap) -> Result<(), Box<Response>> {
    if pod.host_credential(headers).as_deref() == Some("mcp") {
        return Ok(());
    }
    match pod.caller(headers).await {
        Ok(Caller::Signed(s)) if s.pinned => Ok(()),
        Ok(_) => {
            let mut r = json_error(
                StatusCode::UNAUTHORIZED,
                json!({ "error": "unauthorized",
                        "detail": "connect Claude to your pod (OAuth), or use an MCP credential from `server pods mcp`" }),
            );
            r.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            Err(Box::new(r))
        }
        Err(e) => Err(Box::new(e.into_response())),
    }
}

/// A browser page of another site must not drive the endpoint (DNS
/// rebinding): an `Origin` must name this host.
fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    origin
        .split_once("://")
        .is_some_and(|(_, rest)| !host.is_empty() && rest.trim_end_matches('/') == host)
}

async fn handle(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if !origin_ok(&headers) {
        return json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "origin_not_allowed" }),
        );
    }
    if let Err(r) = authorized(&pod, &headers).await {
        return *r;
    }
    let msg = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_error(Value::Null, -32700, "Parse error")).into_response(),
    };
    let Some(obj) = msg.as_object() else {
        return Json(rpc_error(Value::Null, -32600, "Invalid Request")).into_response();
    };
    let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = obj.get("id").cloned() else {
        // A notification (or a response to us): accepted, nothing to say.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let answer = match method {
        "initialize" => Ok(initialize(&pod, &params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": all_tools(&pod) })),
        "tools/call" => Ok(call_tool(&pod, &headers, &params).await),
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    let reply = match answer {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => rpc_error(id, code, &message),
    };
    Json(reply).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(pod: &Pod, params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "the-construct-pod", "title": "The Construct", "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions(pod),
    })
}

/// [`INSTRUCTIONS`] and every adapter's paragraph.
fn instructions(pod: &Pod) -> String {
    let mut out = INSTRUCTIONS.to_owned();
    for a in pod.catalog().adapters() {
        if let Some(more) = a.mcp_instructions() {
            out.push_str("\n\n");
            out.push_str(more);
        }
    }
    out
}

const INSTRUCTIONS: &str = "This is the user's own pod in The Construct. It serves layouts: JSON screens \
(server-driven UI) over data sources. The user's app opens on the home layout (id home) and shows nothing else: \
home IS the user's whole interface, and it embeds or presents every other layout. A new pod's home is a blank page \
with Notifications and Settings buttons. Build only what the user asks for, as small as it can be: \"show my photo \
feed\" is one layout (create_layout referencing the blueprint that fits: {\"type\": \"core://Ref\", \"app\": \
<blueprint>, \"part\": \"layout\"}, with its feeds) and a home that shows it (update_layout on home: a core://Layout \
of that layout as the one slot). With more than one layout, home lists them and each row pushes its layout \
(action.navigation.open {\"layout\": <id>, \"presentation\": \"Push\"}); system Back comes back. A core://Layout \
can also show a layout in part of the screen, or as a Sheet or Panel while a condition holds. Home owns every \
feed: feeds you name when creating a layout are added to home's, and every layout may bind any of home's feeds. \
The inbox and Settings are the layouts inbox and settings (open them with presentation Sheet and Panel); \
describe_vocabulary has small snippets. home can't be deleted. Blueprints are the built-in layouts \
(list_layouts lists them under blueprints, get_layout reads one, list_parts lists their parts); reference a \
blueprint so it stays current, or copy it to change it and pass copied_from so the user hears when it changes \
(rebase_context then gives you what to merge). Call list_data_sources and describe_vocabulary first and \
sample_data to see real fields. Every write is validated (errors give a JSON pointer and what is allowed) and \
appears live on the user's devices; restore_version undoes an edit. The vocabulary is plumbing (what the app draws) \
and porcelain: components the pod expands, used with {\"type\": \"core://Use\", \"component\": <name>, \"args\", \
\"children\"} (list_components; core: ones ship with the server, pod: ones are the user's own). Prefer a component \
over repeating nodes, and define a pod: component (define_component, or copy_component to change a core: one) when a \
piece repeats across layouts. expand shows what any layout or component becomes. A layout whose root is a \
core://Page gets no chrome from the app: it draws its own header (a title with actions on one line) and Back \
(core:back_button); core://Workspace is drawn as core:workspace. Use ${local://theme#<token>} for colours \
(set_theme). A button whose step can't be undone (action.device leave_pod or sign_out, a delete) asks first: wrap \
its intent in action.confirm.";

// ---------- tools ----------

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

/// The names of the core's own tools (adapters may not reuse them).
pub fn core_tool_names() -> Vec<String> {
    tools()
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect()
}

/// The core's tools, then every adapter's ([`crate::AppAdapter::mcp_tools`]).
fn all_tools(pod: &Pod) -> Vec<Value> {
    let mut out = tools();
    for a in pod.catalog().adapters() {
        out.extend(a.mcp_tools());
    }
    out
}

fn tools() -> Vec<Value> {
    let feeds = json!({ "type": "array", "description": "Feeds to add to home's (every layout binds home's feeds): [source, feed] pairs (see list_data_sources)",
        "items": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2 } });
    let layout = json!({ "type": "object", "description": "A core://Workspace layout (see describe_vocabulary); its id is set by the pod" });
    let id = json!({ "type": "string", "description": "A layout id: home, a c-... id from list_layouts, or a blueprint" });
    let note = json!({ "type": "string", "description": "What changed, in the user's words (shown in the version history)" });
    let copied = json!({ "type": "object", "description": "When the layout copies parts of a blueprint: {app, version} of what was copied",
        "properties": { "app": { "type": "string" }, "version": { "type": "integer" } } });
    vec![
        tool(
            "list_layouts",
            "The user's layouts (home first, with the feeds every layout may bind) and the blueprints layouts can start from, with their latest version.",
            json!({}),
            &[],
        ),
        tool(
            "list_data_sources",
            "Every data source of the pod: its feeds (what a layout can show), data paths (screen data), actions (endpoints buttons may call), entity types with their fields, and whether its sign-in is done.",
            json!({}),
            &[],
        ),
        tool(
            "describe_vocabulary",
            "The layout vocabulary: node types with their fields, action intents and binding syntax.",
            json!({}),
            &[],
        ),
        tool(
            "get_layout",
            "A layout (latest, or one version), the feeds it may bind, and its version history. expanded: true adds what clients get (components expanded to plumbing).",
            json!({ "id": id, "version": { "type": "integer" }, "expanded": { "type": "boolean" } }),
            &["id"],
        ),
        tool(
            "sample_data",
            "A truncated sample of what a feed or a data path returns for the user (entities and query keys), to see the real fields.",
            json!({ "source": { "type": "string" }, "feed": { "type": "string" },
                    "path": { "type": "string", "description": "A data path with real values, e.g. one from a feed's entity" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": SAMPLE_MAX_ITEMS } }),
            &["source"],
        ),
        tool(
            "validate_layout",
            "Check a layout against home's feeds (plus feeds, when given) without saving it: its features and, when it uses components, what it expands to.",
            json!({ "feeds": feeds, "layout": layout }),
            &["layout"],
        ),
        tool(
            "create_layout",
            "Create a layout. It can be shown on the user's devices at once (home embeds or presents it). feeds are added to home's.",
            json!({ "title": { "type": "string" }, "icon": { "type": "string", "enum": custom::APP_ICONS },
                    "feeds": feeds, "layout": layout, "note": note, "copied_from": copied }),
            &["title", "layout"],
        ),
        tool(
            "update_layout",
            "Save a new version of a layout: a whole layout, or a JSON Patch (RFC 6902) against the latest. Pass base_version (the version you read) so a concurrent edit isn't lost.",
            json!({ "id": id, "layout": layout,
                    "json_patch": { "type": "array", "items": { "type": "object" } },
                    "note": note, "base_version": { "type": "integer" }, "copied_from": copied }),
            &["id"],
        ),
        tool(
            "restore_version",
            "Make an old version of a layout the latest again (appends a copy).",
            json!({ "id": id, "version": { "type": "integer" }, "note": note }),
            &["id", "version"],
        ),
        tool(
            "rename_layout",
            "Change a layout's title or icon. feeds: on home, home's whole feed list (refused while another layout binds a feed it drops); on another layout, feeds to add to home's.",
            json!({ "id": id, "title": { "type": "string" }, "icon": { "type": "string", "enum": custom::APP_ICONS }, "feeds": feeds }),
            &["id"],
        ),
        tool(
            "delete_layout",
            "Delete a layout on every device. Only when the user asked for it.",
            json!({ "id": id }),
            &["id"],
        ),
        tool(
            "list_parts",
            "The parts of blueprints a layout can reference with core://Ref (screens, slots and named parts), with each part's layout.",
            json!({ "app": { "type": "string", "description": "A blueprint (all when left out)" }, "with_layouts": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "list_components",
            "Porcelain the layouts can use: every component (core: ones ship with the server, pod: ones are the user's own) with its doc and parameters, and the blueprints (core:<blueprint>). Use one with {\"type\": \"core://Use\", \"component\": <name>, \"args\": {...}, \"children\": [...]}.",
            json!({}),
            &[],
        ),
        tool(
            "get_component",
            "A component's definition (doc, params, template, examples), each example expanded to the plumbing it becomes, and its version history.",
            json!({ "name": { "type": "string", "description": "core:<name> or pod:<name>" }, "version": { "type": "integer" } }),
            &["name"],
        ),
        tool(
            "expand",
            "What clients get: a layout (id, version?), a component use (component, args?, children?, version?) or any node (node), with every component expanded, as plumbing only, and the features it needs.",
            json!({ "id": id, "version": { "type": "integer" }, "component": { "type": "string" },
                    "args": { "type": "object" }, "children": { "type": "array", "items": { "type": "object" } },
                    "node": { "type": "object" } }),
            &[],
        ),
        tool(
            "define_component",
            "Define a pod: component any layout on this pod can use: {doc, params: [{name, type: text|number|bool|uri|binding, default?, required?, doc}], children?: true, template, examples: [{args, children?, note}]}. In the template, $arg.<name> is an argument (a whole \"$arg.n\" keeps its type), {\"type\": \"core://Children\"} is where the caller's children go (\"at\": i for one), and {\"type\": \"core://IfArg\", \"arg\", \"equals\"?, \"then\", \"else\"?} picks a branch. Every example must expand.",
            json!({ "name": { "type": "string", "description": "pod:<name>" }, "component": { "type": "object" }, "note": note }),
            &["name", "component"],
        ),
        tool(
            "update_component",
            "Save a new version of a pod: component. Layouts that use it without a pinned version get a new version that says so.",
            json!({ "name": { "type": "string" }, "component": { "type": "object" }, "note": note, "base_version": { "type": "integer" } }),
            &["name", "component"],
        ),
        tool(
            "copy_component",
            "Copy a component (e.g. core:workspace) into a pod: component you can change; it records copied_from.",
            json!({ "from": { "type": "string" }, "version": { "type": "integer" }, "name": { "type": "string", "description": "pod:<name> (default: the same name)" }, "note": note }),
            &["from"],
        ),
        tool(
            "restore_component",
            "Make an old version of a pod: component the latest again.",
            json!({ "name": { "type": "string" }, "version": { "type": "integer" } }),
            &["name", "version"],
        ),
        tool(
            "delete_component",
            "Delete a pod: component. Refused while a layout uses it (the error lists them). Only when the user asked for it.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            "rebase_context",
            "For a layout copied from a blueprint (copied_from): the blueprint layout it was copied from, the blueprint's latest, and the layout's current version, to merge new blueprint changes into the copy. Save the merge with update_layout and copied_from set to the new version; or dismiss_update if the user doesn't want it.",
            json!({ "id": id }),
            &["id"],
        ),
        tool(
            "dismiss_update",
            "Stop telling the user that the blueprint a layout was copied from has updates (up to a version).",
            json!({ "id": id, "version": { "type": "integer" } }),
            &["id", "version"],
        ),
        tool(
            "set_time_zone",
            "The user's time zone (an IANA name such as America/Denver), used where the pod turns a date or time without a zone into a moment (collection reminders). Ask the user when you don't know it. Without time_zone: answers the current one (UTC when unset).",
            json!({ "time_zone": { "type": "string", "description": "An IANA zone name, e.g. Europe/Berlin" } }),
            &[],
        ),
        tool(
            "set_theme",
            "The pod's theme colours, each with a light and a dark value; layouts bind them as ${local://theme#<token>} and the app picks the value for the device's mode. Without theme: answers the current theme and its tokens.",
            json!({ "theme": { "type": "object", "description": "{token: {light?: #RRGGBB, dark?: #RRGGBB}}" } }),
            &[],
        ),
        tool(
            "set_hidden",
            "Hide a layout from the device's list of layouts (local:layouts), or show it again.",
            json!({ "id": id, "hidden": { "type": "boolean" } }),
            &["id", "hidden"],
        ),
    ]
}

/// A tool result: the value as text (and structured), or an error the model
/// can act on.
fn tool_result(r: Result<Value, Value>) -> Value {
    let (value, is_error) = match r {
        Ok(v) => (v, false),
        Err(e) => (e, true),
    };
    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
    let mut out = json!({ "content": [{ "type": "text", "text": text }], "isError": is_error });
    if value.is_object() {
        out["structuredContent"] = value;
    }
    out
}

fn refused((status, body): (StatusCode, Value)) -> Value {
    let mut b = body;
    b["status"] = json!(status.as_u16());
    b
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Result<&'a str, Value> {
    args[k].as_str().filter(|s| !s.is_empty()).ok_or_else(
        || json!({ "error": "invalid_arguments", "detail": format!("\"{k}\" is required") }),
    )
}

async fn call_tool(pod: &Pod, headers: &HeaderMap, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    // Layout tools name the layout `id` (decision `layouts-not-apps`); the
    // code below reads `app`, which older callers still send.
    if args["app"].is_null()
        && let Some(id) = args["id"].as_str().map(str::to_owned)
        && name != "validate_layout"
        && name != "create_layout"
    {
        args["app"] = json!(id);
    }
    let r = match name {
        "list_layouts" | "list_apps" => list_apps(pod).await,
        "list_data_sources" => list_data_sources(pod).await,
        "describe_vocabulary" => vocabulary(pod).await,
        "get_layout" => get_layout(pod, &args).await,
        "sample_data" => sample_data(pod, headers, &args).await,
        "validate_layout" => custom::validate(pod, &args).map_err(refused),
        "create_layout" | "create_app" => custom::create(pod, &args, Writer::Claude)
            .await
            .map(|w| json!({ "ok": true, "id": w.id, "version": w.version }))
            .map_err(refused),
        "update_layout" => update_layout(pod, &args).await,
        "restore_version" => match str_arg(&args, "app") {
            Ok(app) => custom::restore(pod, app, &args, Writer::Claude)
                .await
                .map(|w| json!({ "ok": true, "id": w.id, "version": w.version }))
                .map_err(refused),
            Err(e) => Err(e),
        },
        "rename_layout" | "rename_app" => match str_arg(&args, "app") {
            Ok(app) => custom::patch(pod, app, &args)
                .await
                .map(|w| json!({ "ok": true, "id": w.id, "version": w.version }))
                .map_err(refused),
            Err(e) => Err(e),
        },
        "delete_layout" | "delete_app" => match str_arg(&args, "app") {
            Ok(app) => custom::delete(pod, app)
                .await
                .map(|()| json!({ "ok": true }))
                .map_err(refused),
            Err(e) => Err(e),
        },
        "list_parts" => list_parts(pod, &args).await,
        "list_components" => list_components(pod).await,
        "get_component" => get_component(pod, &args).await,
        "expand" => expand_tool(pod, &args).await,
        "define_component" => write_component(pod, &args, false, None).await,
        "update_component" => write_component(pod, &args, true, None).await,
        "copy_component" => copy_component(pod, &args).await,
        "restore_component" => restore_component(pod, &args).await,
        "delete_component" => delete_component(pod, &args).await,
        "rebase_context" => rebase_context(pod, &args).await,
        "dismiss_update" => match (str_arg(&args, "app"), args["version"].as_i64()) {
            (Ok(app), Some(v)) => custom::dismiss_update(pod, app, v)
                .await
                .map(|()| json!({ "ok": true }))
                .map_err(refused),
            (Err(e), _) => Err(e),
            (_, None) => {
                Err(json!({ "error": "invalid_arguments", "detail": "\"version\" is required" }))
            }
        },
        "set_time_zone" => set_time_zone(pod, &args).await,
        "set_theme" => set_theme(pod, &args).await,
        "set_hidden" => match (str_arg(&args, "app"), args["hidden"].as_bool()) {
            (Ok(app), Some(hidden)) => custom::set_hidden(pod, app, hidden)
                .await
                .map(|()| json!({ "ok": true, "hidden": hidden }))
                .map_err(refused),
            (Err(e), _) => Err(e),
            (_, None) => {
                Err(json!({ "error": "invalid_arguments", "detail": "\"hidden\" is a boolean" }))
            }
        },
        _ => {
            let mut found = None;
            for a in pod.catalog().adapters() {
                if let Some(f) = a.mcp_call(pod, name, &args) {
                    found = Some(f.await);
                    break;
                }
            }
            found.unwrap_or_else(|| Err(json!({ "error": "unknown_tool", "tool": name })))
        }
    };
    tool_result(r)
}

async fn set_theme(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let changes = args["theme"].clone();
    let r = crate::blocking(pod, move |s| {
        Ok(if changes.is_null() {
            crate::porcelain::theme(s).map_err(|e| e.to_string())
        } else {
            crate::porcelain::set_theme(s, &changes)
        })
    })
    .await
    .map_err(store_err)?;
    match r {
        Ok(t) => Ok(json!({ "ok": true, "theme": t,
            "use": "bind ${local://theme#<token>} where a colour goes (core://Box background, border_color, content_color; core://IconButton active_tint); the app picks the light or dark value. Devices pick a change up the next time they load the layout." })),
        Err(e) => Err(json!({ "error": "invalid_theme", "detail": e })),
    }
}

async fn set_time_zone(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = args["time_zone"].as_str().map(str::to_owned);
    let r = crate::blocking(pod, move |s| {
        Ok(match name {
            Some(n) => crate::tz::set_time_zone(s, &n).map(Some),
            None => crate::tz::time_zone_name(s).map_err(|e| e.to_string()),
        })
    })
    .await
    .map_err(|(s, Json(b))| refused((s, b)))?;
    match r {
        Ok(name) => Ok(json!({ "ok": true, "time_zone": name.unwrap_or_else(|| "UTC".into()) })),
        Err(e) => Err(json!({ "error": "invalid_time_zone", "detail": e })),
    }
}

async fn list_apps(pod: &Pod) -> Result<Value, Value> {
    let builtin: Vec<crate::AppInfo> = pod
        .catalog()
        .apps()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .copied()
        .collect();
    // Only home lists the feeds: every layout binds home's.
    let listed = crate::blocking(pod, move |s| {
        let hidden = custom::hidden_apps(s)?;
        let custom_apps = s.with_conn(custom::custom_apps_on)?;
        let row = |a: custom::AppRef| -> rusqlite::Result<Value> {
            let latest = s.app_latest_meta(a.id())?.map(|v| v.version);
            let v: rusqlite::Result<Value> = Ok(json!({
                "id": a.id(), "title": a.title(), "icon": a.icon(), "kind": a.kind(),
                "hidden": hidden.iter().any(|h| h == a.id()),
                "latest_version": latest,
            }));
            let mut v = v?;
            if a.id() == crate::HOME_APP || !a.is_custom() {
                v["feeds"] = json!(a.feeds());
            }
            Ok(v)
        };
        let apps = custom_apps
            .into_iter()
            .map(|a| row(custom::AppRef::Custom(a)))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let blueprints = builtin
            .into_iter()
            .map(|a| row(custom::AppRef::Builtin(a)))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((apps, blueprints))
    })
    .await
    .map_err(|(s, Json(b))| refused((s, b)))?;
    Ok(json!({ "layouts": listed.0, "blueprints": listed.1 }))
}

/// The owner's session (the pod token's), for what is signed in and for
/// samples; `None` on a pod without one.
async fn owner(pod: &Pod) -> Option<crate::Session> {
    let hash = crate::blocking(pod, |s| s.owner_session()).await.ok()??;
    pod.caller_for_hash(hash).await.ok()?.into_session()
}

async fn list_data_sources(pod: &Pod) -> Result<Value, Value> {
    let owner = owner(pod).await;
    let sources: Vec<Value> = pod
        .catalog()
        .sources()
        .filter(|(_, m)| !m.dev || pod.show_dev_apps())
        .map(|(id, m)| {
            let signed_in = m
                .sign_in
                .is_none_or(|s| owner.as_ref().is_some_and(|o| o.has(s)));
            json!({
                "id": id,
                "title": m.title,
                "sign_in": m.sign_in,
                "signed_in": signed_in,
                "entity_schemes": m.entity_schemes,
                "feeds": m.feeds.iter().map(|f| json!({ "name": f.name, "query_keys": f.query_keys, "doc": f.doc })).collect::<Vec<_>>(),
                "data_paths": m.data_paths.iter().map(|p| json!({ "pattern": p.pattern, "query_keys": p.query_keys, "doc": p.doc })).collect::<Vec<_>>(),
                "actions": m.actions.iter().map(|p| json!({ "pattern": p.pattern, "doc": p.doc })).collect::<Vec<_>>(),
                "entities": m.entities.iter().map(|e| json!({
                    "uri": e.uri, "doc": e.doc,
                    "fields": e.fields.iter().map(|(f, d)| json!({ "name": f, "doc": d })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({
        "sources": sources,
        "note": "A custom app's root also gets core://sources/<id> entities {title, signed_in, error?}: show a hint such as \"Connect it in Settings\" with core://Conditional on ${core://sources/<id>#signed_in}. To sign in, use action.device sign_in (id: the provider from local:providers; value: another server, e.g. $form?.server from a core://TextField), anywhere in a layout, e.g. under that hint; see the sign_in_row snippet. The raw action.auth.* intents are not for custom layouts.",
    }))
}

async fn get_layout(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let app_id = str_arg(args, "app")?.to_owned();
    let app = pod
        .app(&app_id)
        .await
        .map_err(|(s, Json(b))| refused((s, b)))?
        .ok_or_else(|| json!({ "error": "app_not_found", "id": app_id }))?;
    let version = args["version"].as_i64();
    let lookup = app_id.clone();
    let (found, history) = crate::blocking(pod, move |s| {
        let found = match version {
            Some(n) => s.app_version(&lookup, n)?,
            None => s.app_latest(&lookup)?,
        };
        Ok((found, s.app_versions(&lookup, None, 20)?))
    })
    .await
    .map_err(|(s, Json(b))| refused((s, b)))?;
    let Some((v, json_text)) = found else {
        return Err(json!({ "error": "version_not_found", "id": app_id }));
    };
    let layout: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
    let expanded = if args["expanded"].as_bool() == Some(true) {
        let p = pod.clone();
        let text = json_text.clone();
        Some(
            crate::blocking(pod, move |s| Ok(layout_as_plumbing(s, p.catalog(), &text)))
                .await
                .map_err(store_err)?,
        )
    } else {
        None
    };
    let mut out = json!({
        "id": app_id,
        "kind": app.kind(),
        "title": app.title(),
        "feeds": app.feeds(),
        "version": v.version,
        "layout": layout,
        "history": history.iter().map(|h| json!({
            "version": h.version, "note": h.note, "source": h.source, "author": h.author,
            "created_at": crate::layouts::rfc3339(h.created_at),
        })).collect::<Vec<_>>(),
    });
    if let Some(e) = expanded {
        out["expanded"] = e;
    }
    Ok(out)
}

async fn list_parts(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let only = args["app"].as_str().map(str::to_owned);
    let with_layouts = args["with_layouts"].as_bool().unwrap_or(false);
    let apps: Vec<(String, String)> = pod
        .catalog()
        .apps()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .filter(|a| only.as_deref().is_none_or(|o| o == a.id))
        .map(|a| (a.id.to_owned(), a.title.to_owned()))
        .collect();
    if apps.is_empty() {
        return Err(json!({ "error": "app_not_found", "detail": "parts come from built-in apps" }));
    }
    let listed = crate::blocking(pod, move |s| {
        apps.into_iter()
            .map(|(id, title)| {
                let parts = crate::parts::load(s, &id, None, None)?.unwrap_or_default();
                Ok(json!({
                    "app": id, "title": title, "version": parts.version,
                    "parts": parts.parts.iter().map(|(name, v)| {
                        let mut row = json!({ "part": name,
                            "where": if name.starts_with("screen:") { "screens" } else { "any node" } });
                        if with_layouts {
                            row["layout"] = v.clone();
                        }
                        row
                    }).collect::<Vec<_>>(),
                }))
            })
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .await
    .map_err(|(s, Json(b))| refused((s, b)))?;
    Ok(json!({ "apps": listed,
        "use": "{\"type\": \"core://Ref\", \"app\": <app>, \"part\": <part>} (add \"version\" to pin one); the pod expands it when serving and keeps it current" }))
}

async fn rebase_context(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let app = str_arg(args, "app")?.to_owned();
    let lookup = app.clone();
    let found = crate::blocking(pod, move |s| {
        let Some((from_app, from)) = custom::copied_from(s, &lookup)? else {
            return Ok(None);
        };
        let base = s.app_version(&from_app, from)?;
        let latest = s.app_latest(&from_app)?;
        let current = s.app_latest(&lookup)?;
        Ok(Some((from_app, from, base, latest, current)))
    })
    .await
    .map_err(|(s, Json(b))| refused((s, b)))?;
    let Some((from_app, from, base, latest, current)) = found else {
        return Err(
            json!({ "error": "not_a_copy", "detail": "this app has no copied_from; references stay current by themselves" }),
        );
    };
    let layout = |v: Option<(crate::store::AppVersion, String)>| {
        v.map(|(m, j)| json!({ "version": m.version, "layout": serde_json::from_str::<Value>(&j).unwrap_or(Value::Null) }))
    };
    Ok(json!({
        "app": app,
        "copied_from": { "app": from_app, "version": from },
        "base": layout(base),
        "builtin_latest": layout(latest),
        "custom": layout(current),
        "how": "Merge what changed from base to builtin_latest into custom, keep the user's changes, then update_layout with copied_from {app, version: builtin_latest.version} and base_version custom.version. If the user doesn't want it, dismiss_update.",
    }))
}

async fn update_layout(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let app = str_arg(args, "app")?.to_owned();
    let mut body = args.clone();
    if let Some(patch) = args.get("json_patch").filter(|p| !p.is_null()) {
        let lookup = app.clone();
        let latest = crate::blocking(pod, move |s| s.app_latest(&lookup))
            .await
            .map_err(|(s, Json(b))| refused((s, b)))?
            .ok_or_else(|| json!({ "error": "app_not_found", "id": app }))?;
        let mut doc: Value = serde_json::from_str(&latest.1).unwrap_or(Value::Null);
        apply_patch(&mut doc, patch).map_err(
            |e| json!({ "error": "invalid_patch", "detail": e, "latest_version": latest.0.version }),
        )?;
        body["layout"] = doc;
        if body.get("base_version").is_none_or(Value::is_null) {
            body["base_version"] = json!(latest.0.version);
        }
    } else if args.get("layout").is_none_or(|l| !l.is_object()) {
        return Err(
            json!({ "error": "invalid_arguments", "detail": "pass \"layout\" or \"json_patch\"" }),
        );
    }
    custom::update_layout(pod, &app, &body, Writer::Claude)
        .await
        .map(|w| json!({ "ok": true, "id": w.id, "version": w.version }))
        .map_err(refused)
}

async fn sample_data(pod: &Pod, headers: &HeaderMap, args: &Value) -> Result<Value, Value> {
    let source = str_arg(args, "source")?;
    let (adapter, manifest) = pod
        .catalog()
        .source(source)
        .filter(|(_, m)| !m.dev || pod.show_dev_apps())
        .ok_or_else(|| json!({ "error": "unknown_source", "source": source }))?;
    let limit = args["limit"]
        .as_u64()
        .map_or(5, |n| (n as usize).clamp(1, SAMPLE_MAX_ITEMS));
    let owner = owner(pod).await;
    if let Some(sign_in) = manifest.sign_in
        && !owner.as_ref().is_some_and(|o| o.has(sign_in))
    {
        return Err(json!({ "error": "signed_out", "source": source,
            "detail": "the user hasn't signed in to this source (they can in Settings)" }));
    }
    let mut forwarded = headers.clone();
    forwarded.remove(header::AUTHORIZATION);
    forwarded.remove(crate::CREDENTIAL_HEADER);
    let bundle: Value = if let Some(path) = args["path"].as_str() {
        if !manifest
            .data_paths
            .iter()
            .any(|p| crate::source::path_matches(p.pattern, path))
            || path.contains('$')
        {
            return Err(
                json!({ "error": "unknown_path", "detail": "not a data path of this source (with real values)",
                "allowed": manifest.data_paths.iter().map(|p| p.pattern).collect::<Vec<_>>() }),
            );
        }
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .expect("a valid request");
        *req.headers_mut() = forwarded;
        if let Some(o) = &owner
            && let Ok(v) = HeaderValue::from_str(&pod.internal_as(&o.token_hash))
        {
            req.headers_mut().insert(crate::session::INTERNAL_HEADER, v);
        }
        let resp = crate::app(pod.clone())
            .oneshot(req)
            .await
            .unwrap_or_else(|e| match e {});
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap_or_default();
        if !status.is_success() {
            return Err(json!({ "error": "data_path_failed", "status": status.as_u16() }));
        }
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        let feed = args["feed"]
            .as_str()
            .or_else(|| {
                manifest
                    .feeds
                    .iter()
                    .find(|f| !f.is_pattern())
                    .map(|f| f.name)
            })
            .unwrap_or("");
        if manifest.feed(feed).is_none() {
            return Err(json!({ "error": "unknown_feed", "feed": feed,
                "allowed": manifest.feeds.iter().map(|f| f.name).collect::<Vec<_>>() }));
        }
        let req = RequestCtx {
            headers: forwarded,
            client: protocol::ClientCaps::this_build(),
            session: owner,
        };
        match adapter.feed(pod, feed, &req).await {
            crate::Hydration::Data(d) => serde_json::to_value(d).unwrap_or(Value::Null),
            // The source's own error (e.g. `monitoring_off` with how to
            // turn it on), so Claude can tell the user what to do.
            crate::Hydration::Error(resp) => {
                let status = resp.status().as_u16();
                let detail = axum::body::to_bytes(resp.into_body(), 16 * 1024)
                    .await
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                return Err(json!({ "error": "feed_failed", "feed": feed,
                    "status": status, "detail": detail }));
            }
            _ => return Err(json!({ "error": "feed_failed", "feed": feed })),
        }
    };
    Ok(truncate_bundle(&bundle, limit))
}

/// The first `limit` items of every query, their entities (and the
/// entities those name), long strings cut.
fn truncate_bundle(bundle: &Value, limit: usize) -> Value {
    let empty = Map::new();
    let entities = bundle["entities"].as_object().unwrap_or(&empty);
    let mut queries = Map::new();
    let mut keep: Vec<String> = Vec::new();
    for (k, list) in bundle["queries"].as_object().unwrap_or(&empty) {
        let items: Vec<Value> = list
            .as_array()
            .map(|a| a.iter().take(limit).cloned().collect())
            .unwrap_or_default();
        keep.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
        queries.insert(k.clone(), json!(items));
    }
    // Entities the kept ones name (authors, media), one level deep.
    let named: Vec<String> = keep
        .iter()
        .filter_map(|u| entities.get(u))
        .flat_map(uris_in)
        .filter(|u| entities.contains_key(u))
        .collect();
    keep.extend(named);
    if keep.is_empty() {
        keep = entities.keys().take(limit).cloned().collect();
    }
    let mut out = Map::new();
    for u in keep {
        if let Some(e) = entities.get(&u)
            && out.len() < limit * 4
        {
            out.insert(u, cut_strings(e));
        }
    }
    json!({ "entities": out, "queries": queries,
            "truncated": format!("at most {limit} items per query, strings cut at {SAMPLE_MAX_CHARS} characters") })
}

fn uris_in(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) if s.contains("://") => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(uris_in).collect(),
        Value::Object(m) => m.values().flat_map(uris_in).collect(),
        _ => Vec::new(),
    }
}

fn cut_strings(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > SAMPLE_MAX_CHARS => {
            json!(format!(
                "{}…",
                s.chars().take(SAMPLE_MAX_CHARS).collect::<String>()
            ))
        }
        Value::Array(a) => Value::Array(a.iter().take(SAMPLE_MAX_ITEMS).map(cut_strings).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), cut_strings(v))).collect())
        }
        other => other.clone(),
    }
}

// ---------- components (decision `plumbing-and-porcelain`) ----------

fn invalid_args(detail: impl Into<String>) -> Value {
    json!({ "error": "invalid_arguments", "detail": detail.into() })
}

/// `pod:<name>` from `name` or `pod:<name>`.
fn pod_name(raw: &str) -> Result<String, Value> {
    let bare = raw.strip_prefix(crate::components::POD).unwrap_or(raw);
    if raw.starts_with(crate::components::CORE) {
        return Err(
            json!({ "error": "read_only", "detail": "core: components ship with the server and never change; copy_component makes a pod: copy you can change" }),
        );
    }
    if !crate::components::valid_bare_name(bare) {
        return Err(invalid_args(format!(
            "a component name is pod:<name>, <name> being [a-z][a-z0-9_]* (at most {} characters)",
            crate::components::MAX_NAME_CHARS
        )));
    }
    Ok(format!("{}{bare}", crate::components::POD))
}

fn store_err((s, Json(b)): (StatusCode, Json<Value>)) -> Value {
    refused((s, b))
}

/// A stored layout as clients get it: uses expanded (leniently) and, for a
/// workspace, the plumbing its porcelain stands for.
fn layout_as_plumbing(s: &crate::store::Store, catalog: &crate::Catalog, json_text: &str) -> Value {
    let doc: Value = serde_json::from_str(json_text).unwrap_or(Value::Null);
    let expanded =
        crate::parts::expand(s, catalog, &doc, None, crate::parts::Mode::Lenient).unwrap_or(doc);
    crate::porcelain::to_plumbing(s, catalog, &expanded).unwrap_or(expanded)
}

fn with_features(v: Value) -> Value {
    let features = serde_json::from_value::<protocol::UiNode>(v.clone())
        .map(|n| protocol::required_features(&n))
        .unwrap_or_default();
    json!({ "expanded": v, "features": features })
}

async fn list_components(pod: &Pod) -> Result<Value, Value> {
    let blueprints: Vec<String> = pod
        .catalog()
        .apps()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .map(|a| format!("{}{}", crate::components::CORE, a.id))
        .collect();
    let rows = crate::blocking(pod, crate::components::list)
        .await
        .map_err(store_err)?;
    let components: Vec<Value> = rows
        .iter()
        .map(|(m, d)| crate::components::summary(m, d))
        .collect();
    Ok(json!({
        "components": components,
        "blueprints": blueprints,
        "use": "{\"type\": \"core://Use\", \"component\": <name>, \"args\": {...}, \"children\": [nodes]} anywhere a node goes (add \"version\" to pin one). core:<blueprint> is a whole blueprint layout (only as a layout's root) and core:<blueprint>/<part> one of its parts (list_parts). The pod expands uses before serving; expand shows the result.",
    }))
}

async fn get_component(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "name")?.to_owned();
    let version = args["version"].as_i64();
    let lookup = name.clone();
    let p = pod.clone();
    let found = crate::blocking(pod, move |s| {
        let def = match version {
            Some(n) => crate::components::version(s, &lookup, n)?.map(|d| (n, d)),
            None => crate::components::latest(s, &lookup)?,
        };
        let Some((v, def)) = def else { return Ok(None) };
        let history = crate::components::history(s, &lookup)?;
        let examples: Vec<Value> = crate::components::Def::parse(&def)
            .map(|d| d.examples)
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                let node = crate::parts::use_node(
                    &lookup,
                    Some(v),
                    &Value::Object(e.args.clone()),
                    &Value::Array(e.children.clone()),
                );
                let expanded = crate::parts::expand(s, p.catalog(), &node, None, crate::parts::Mode::Strict)
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                json!({ "args": e.args, "children": e.children, "note": e.note, "expanded": expanded })
            })
            .collect();
        Ok(Some((v, def, history, examples)))
    })
    .await
    .map_err(store_err)?;
    let Some((v, def, history, examples)) = found else {
        return Err(json!({ "error": "component_not_found", "name": name,
            "detail": "list_components lists them; blueprints are core:<blueprint> (get_layout reads them)" }));
    };
    Ok(json!({
        "name": name,
        "version": v,
        "definition": def,
        "examples": examples,
        "history": history.iter().map(|h| json!({
            "version": h.version, "note": h.note, "author": h.author,
            "created_at": crate::layouts::rfc3339(h.created_at),
        })).collect::<Vec<_>>(),
    }))
}

async fn expand_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let p = pod.clone();
    if let Some(id) = args["app"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    {
        let version = args["version"].as_i64();
        let lookup = id.clone();
        let found = crate::blocking(pod, move |s| {
            let found = match version {
                Some(n) => s.app_version(&lookup, n)?,
                None => s.app_latest(&lookup)?,
            };
            Ok(found.map(|(v, j)| (v.version, layout_as_plumbing(s, p.catalog(), &j))))
        })
        .await
        .map_err(store_err)?;
        let Some((v, expanded)) = found else {
            return Err(json!({ "error": "layout_not_found", "id": id }));
        };
        let mut out = with_features(expanded);
        out["id"] = json!(id);
        out["version"] = json!(v);
        return Ok(out);
    }
    let node = if let Some(name) = args["component"].as_str() {
        crate::parts::use_node(
            name,
            args["version"].as_i64(),
            &args["args"],
            &args["children"],
        )
    } else if args["node"].is_object() {
        args["node"].clone()
    } else {
        return Err(invalid_args(
            "pass id (a layout, with version?), component (with args?, children?, version?) or node",
        ));
    };
    let expanded = crate::blocking(pod, move |s| {
        Ok(
            crate::parts::expand(s, p.catalog(), &node, None, crate::parts::Mode::Strict)
                .map(|v| crate::porcelain::to_plumbing(s, p.catalog(), &v).unwrap_or(v)),
        )
    })
    .await
    .map_err(store_err)?
    .map_err(|e| json!({ "error": "invalid_layout", "detail": e.to_json() }))?;
    Ok(with_features(expanded))
}

/// define_component (`update`: false) and update_component.
async fn write_component(
    pod: &Pod,
    args: &Value,
    update: bool,
    copied_from: Option<Value>,
) -> Result<Value, Value> {
    let name = pod_name(str_arg(args, "name")?)?;
    let def = args["component"].clone();
    if !def.is_object() {
        return Err(invalid_args(
            "\"component\" is {doc, params, children?, template, examples}",
        ));
    }
    let note = args["note"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(if update { "Edited" } else { "Defined" })
        .chars()
        .take(custom::MAX_NOTE_CHARS)
        .collect::<String>();
    custom::allow_write(pod).map_err(refused)?;
    let p = pod.clone();
    let base = args["base_version"].as_i64();
    let r = crate::blocking(pod, move |s| {
        let latest = crate::components::latest(s, &name)?.map(|(v, _)| v);
        match (update, latest) {
            (false, Some(_)) => {
                return Ok(Err(json!({ "error": "component_exists", "name": name,
                    "detail": "update_component changes it" })));
            }
            (true, None) => {
                return Ok(Err(json!({ "error": "component_not_found", "name": name,
                    "detail": "define_component makes it" })));
            }
            _ => {}
        }
        if let (Some(b), Some(l)) = (base, latest)
            && b != l
        {
            return Ok(Err(json!({ "error": "stale_base", "latest_version": l,
                "detail": "the component changed since that version; read it again" })));
        }
        if latest.is_none() && crate::components::pod_count(s)? >= crate::components::MAX_POD_COMPONENTS {
            return Ok(Err(json!({ "error": "too_many_components",
                "detail": format!("at most {} pod components", crate::components::MAX_POD_COMPONENTS) })));
        }
        let next = latest.unwrap_or(0) + 1;
        if let Err(e) = crate::parts::check_definition(s, p.catalog(), &name, next, &def, &crate::parts::Pending::default()) {
            return Ok(Err(json!({ "error": "invalid_component", "detail": e })));
        }
        let meta = copied_from.map(|c| json!({ "copied_from": c }));
        let v = crate::components::put_pod(s, &name, &def, &note, custom::AUTHOR_CLAUDE, meta.as_ref())?;
        let updated = custom::component_updates(s, p.catalog(), &[(name.clone(), v)])?;
        Ok(Ok((name, v, updated)))
    })
    .await
    .map_err(store_err)??;
    let (name, v, updated) = r;
    for (id, lv) in &updated {
        pod.publish_layout(id, *lv);
    }
    Ok(json!({ "ok": true, "name": name, "version": v,
        "layouts_updated": updated.iter().map(|(id, v)| json!({ "id": id, "version": v })).collect::<Vec<_>>() }))
}

async fn copy_component(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let from = str_arg(args, "from")?.to_owned();
    let version = args["version"].as_i64();
    let lookup = from.clone();
    let found = crate::blocking(pod, move |s| match version {
        Some(n) => Ok(crate::components::version(s, &lookup, n)?.map(|d| (n, d))),
        None => crate::components::latest(s, &lookup),
    })
    .await
    .map_err(store_err)?;
    let Some((v, def)) = found else {
        return Err(json!({ "error": "component_not_found", "name": from }));
    };
    let bare = from.split_once(':').map_or(from.as_str(), |(_, b)| b);
    let name = args["name"].as_str().unwrap_or(bare).to_owned();
    let body = json!({ "name": name, "component": def,
        "note": args["note"].as_str().map_or_else(|| format!("Copied from {from} v{v}"), str::to_owned) });
    write_component(
        pod,
        &body,
        false,
        Some(json!({ "component": from, "version": v })),
    )
    .await
}

async fn restore_component(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = pod_name(str_arg(args, "name")?)?;
    let version = args["version"]
        .as_i64()
        .ok_or_else(|| invalid_args("\"version\" is required"))?;
    let lookup = name.clone();
    let def = crate::blocking(pod, move |s| {
        crate::components::version(s, &lookup, version)
    })
    .await
    .map_err(store_err)?
    .ok_or_else(|| json!({ "error": "version_not_found", "name": name, "version": version }))?;
    let body = json!({ "name": name, "component": def, "note": format!("Restored v{version}") });
    write_component(pod, &body, true, None).await
}

async fn delete_component(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = pod_name(str_arg(args, "name")?)?;
    custom::allow_write(pod).map_err(refused)?;
    crate::blocking(pod, move |s| {
        let using = custom::layouts_using_component(s, &name)?;
        if !using.is_empty() {
            return Ok(Err(json!({ "error": "component_in_use", "name": name, "layouts": using,
                "detail": "these layouts use it; change them first (update_layout), then delete it" })));
        }
        if !crate::components::delete_pod(s, &name)? {
            return Ok(Err(json!({ "error": "component_not_found", "name": name })));
        }
        Ok(Ok(json!({ "ok": true, "name": name })))
    })
    .await
    .map_err(store_err)?
}

// ---------- JSON Patch (RFC 6902) ----------

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// `(parent pointer tokens, last token)` of `pointer`.
fn split_pointer(pointer: &str) -> Result<(Vec<String>, String), String> {
    if pointer.is_empty() {
        return Err("the root can't be the target of this operation".into());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(format!("'{pointer}' is not a JSON pointer"));
    };
    let mut tokens: Vec<String> = rest.split('/').map(unescape).collect();
    let last = tokens.pop().unwrap_or_default();
    Ok((tokens, last))
}

fn walk_mut<'a>(doc: &'a mut Value, tokens: &[String]) -> Result<&'a mut Value, String> {
    let mut cur = doc;
    for t in tokens {
        cur = match cur {
            Value::Object(m) => m.get_mut(t).ok_or_else(|| format!("no '{t}'"))?,
            Value::Array(a) => {
                let i: usize = t.parse().map_err(|_| format!("'{t}' is not an index"))?;
                a.get_mut(i).ok_or_else(|| format!("no index {i}"))?
            }
            _ => return Err(format!("'{t}' is under a scalar")),
        };
    }
    Ok(cur)
}

fn get_at(doc: &Value, pointer: &str) -> Result<Value, String> {
    doc.pointer(pointer)
        .cloned()
        .ok_or_else(|| format!("nothing at '{pointer}'"))
}

fn remove_at(doc: &mut Value, pointer: &str) -> Result<Value, String> {
    let (parent, last) = split_pointer(pointer)?;
    match walk_mut(doc, &parent)? {
        Value::Object(m) => m
            .remove(&last)
            .ok_or_else(|| format!("nothing at '{pointer}'")),
        Value::Array(a) => {
            let i: usize = last
                .parse()
                .map_err(|_| format!("'{last}' is not an index"))?;
            if i >= a.len() {
                return Err(format!("nothing at '{pointer}'"));
            }
            Ok(a.remove(i))
        }
        _ => Err(format!("'{pointer}' is under a scalar")),
    }
}

fn add_at(doc: &mut Value, pointer: &str, value: Value, replace: bool) -> Result<(), String> {
    let (parent, last) = split_pointer(pointer)?;
    match walk_mut(doc, &parent)? {
        Value::Object(m) => {
            if replace && !m.contains_key(&last) {
                return Err(format!("nothing to replace at '{pointer}'"));
            }
            m.insert(last, value);
            Ok(())
        }
        Value::Array(a) => {
            if last == "-" && !replace {
                a.push(value);
                return Ok(());
            }
            let i: usize = last
                .parse()
                .map_err(|_| format!("'{last}' is not an index"))?;
            if replace {
                let slot = a
                    .get_mut(i)
                    .ok_or_else(|| format!("nothing to replace at '{pointer}'"))?;
                *slot = value;
            } else {
                if i > a.len() {
                    return Err(format!("index {i} is past the end at '{pointer}'"));
                }
                a.insert(i, value);
            }
            Ok(())
        }
        _ => Err(format!("'{pointer}' is under a scalar")),
    }
}

/// Apply an RFC 6902 patch to `doc` (all or nothing).
pub fn apply_patch(doc: &mut Value, patch: &Value) -> Result<(), String> {
    let ops = patch
        .as_array()
        .ok_or("a JSON Patch is an array of operations")?;
    let mut work = doc.clone();
    for (n, op) in ops.iter().enumerate() {
        let fail = |e: String| format!("operation {n}: {e}");
        let kind = op["op"].as_str().unwrap_or("");
        let path = op["path"]
            .as_str()
            .ok_or_else(|| fail("\"path\" is required".into()))?;
        match kind {
            "add" => add_at(&mut work, path, op["value"].clone(), false).map_err(fail)?,
            "replace" => add_at(&mut work, path, op["value"].clone(), true).map_err(fail)?,
            "remove" => {
                remove_at(&mut work, path).map_err(fail)?;
            }
            "move" | "copy" => {
                let from = op["from"]
                    .as_str()
                    .ok_or_else(|| fail("\"from\" is required".into()))?;
                let v = if kind == "move" {
                    remove_at(&mut work, from).map_err(fail)?
                } else {
                    get_at(&work, from).map_err(fail)?
                };
                add_at(&mut work, path, v, false).map_err(fail)?;
            }
            "test" => {
                if get_at(&work, path).map_err(fail)? != op["value"] {
                    return Err(fail(format!("test failed at '{path}'")));
                }
            }
            other => return Err(fail(format!("unknown op '{other}'"))),
        }
    }
    *doc = work;
    Ok(())
}

// ---------- vocabulary ----------

/// Node types retired by decision `layouts-not-apps`: never written again.
const RETIRED_NODES: [&str; 3] = ["core://Dock", "core://Drawer", "core://AppFrame"];

/// The feature that names node type `t` (`core://IconButton` ->
/// `icon_button`).
pub fn node_feature(t: &str) -> String {
    let n = t.strip_prefix("core://").unwrap_or(t);
    if n == "Layout" {
        return "layout_embed".into();
    }
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn feature_doc(name: &str) -> &'static str {
    protocol::FEATURE_DOCS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or("", |(_, d)| d)
}

/// Plumbing nodes, generated from the feature files
/// (`crates/protocol/features/*.md`).
fn plumbing_nodes() -> Vec<Value> {
    protocol::NODE_TYPES
        .iter()
        .filter(|t| !RETIRED_NODES.contains(t))
        .map(|t| {
            let f = node_feature(t);
            json!({ "type": t, "feature": f, "doc": feature_doc(&f) })
        })
        .collect()
}

/// The porcelain nodes (expanded by the pod, never seen by clients).
fn porcelain_nodes() -> Value {
    json!([
        { "type": crate::components::USE_TYPE, "fields": "component, args?, children?, version?",
          "doc": "A component (list_components): core:<name> ships with the server, pod:<name> is the user's own, core:<blueprint> is a whole blueprint layout (only as a layout's root; the layout keeps its own id) and core:<blueprint>/<part> a part of one (a screen:<id> part in screens). Without version it follows the component: the layout gets a new version when the component changes." },
        { "type": crate::parts::REF_TYPE, "fields": "app, part, version?",
          "doc": "The older name of {core://Use, component: core:<app>/<part>} (part \"layout\": core:<app>)." },
        { "type": crate::components::CHILDREN_TYPE, "fields": "at?",
          "doc": "Only in a component's template: where the caller's children go (at: just one of them)." },
        { "type": crate::components::IF_ARG_TYPE, "fields": "arg, equals?, then, else?",
          "doc": "Only in a component's template: then when the argument is set and truthy (or equals equals), else otherwise." },
    ])
}

/// Small, single-purpose layout pieces for describe_vocabulary: building
/// blocks, never whole pages (the agent builds only what was asked).
fn snippets() -> Value {
    json!({
        "home_one_layout": {
            "doc": "home showing one layout full screen: home's layout with this as its one slot (replace <layout id>)",
            "layout": {"type": "core://Workspace", "title": "Home", "slots": [{"slot_id": "main", "provider_uri": "local://home",
                "content": {"type": "core://Layout", "layout": "<layout id>"}}]}
        },
        "home_list_of_layouts": {
            "doc": "a home slot listing the pod's layouts; a tap pushes the layout, and system Back comes back",
            "node": {"type": "core://List", "query_key": "local:layouts", "item_template": {
                "type": "core://Tappable", "on_click": {"intent": "action.navigation.open", "layout": "$item.id", "presentation": "Push"},
                "content": {"type": "core://Row", "padding_h_dp": 16, "padding_v_dp": 12,
                    "children": [{"type": "core://Text", "style": "Body", "value_expr": "$item.title"}]}}}
        },
        "settings_and_inbox_buttons": {
            "doc": "buttons that present the inbox as a sheet and Settings as a side panel",
            "node": {"type": "core://Row", "children": [{"type": "core://Spacer"},
                {"type": "core://IconButton", "icon": "notifications", "content_description": "Notifications",
                 "on_click": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet"}},
                {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
                 "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}]}
        },
        "card_of_layout": {
            "doc": "another layout in a card-sized region of the page",
            "node": {"type": "core://Box", "corner_dp": 12, "border_color": "#DDDDDD",
                "content": {"type": "core://Layout", "layout": "<layout id>", "height_dp": 240}}
        },
        "sign_in_row": {
            "doc": "a row per sign-in the pod offers, with a server box where the provider takes one",
            "node": {"type": "core://List", "query_key": "local:providers", "item_template": {"type": "core://Row",
                "padding_h_dp": 16, "spacing_dp": 8, "children": [
                    {"type": "core://TextField", "field": "server", "placeholder": "Server (optional)"},
                    {"type": "core://Button", "label": "Sign in to $item.title", "on_click": {"intent": "action.device",
                        "command": "sign_in", "id": "$item.id", "value": "$form?.server"}}]}}
        },
        "notification_list": {
            "doc": "the inbox, newest first, by day; a tap opens what the notification is about",
            "node": {"type": "core://List", "query_key": "local:notifications", "empty_text": "No notifications yet",
                "group_by": {"field": "day", "header": {"type": "core://Text", "style": "Label", "value_expr": "$item.day"}},
                "item_template": {"type": "core://Tappable",
                    "on_click": {"intent": "action.device", "command": "open_notification", "id": "$item.id"},
                    "content": {"type": "core://Column", "padding_h_dp": 16, "padding_v_dp": 8, "children": [
                        {"type": "core://Text", "style": "Body", "value_expr": "$item.title"},
                        {"type": "core://Text", "style": "Caption", "value_expr": "$item.app_title · $item.when"}]}}}
        },
        "load_older_notifications": {
            "doc": "under a local:notifications list: a button that loads the next page, shown only while there is more",
            "node": {"type": "core://Conditional", "value": "${local://inbox#has_older}", "content": {"type": "core://Button",
                "label": "Load older", "on_click": {"intent": "action.device", "command": "load_older_notifications"}}}
        },
        "channel_switch": {
            "doc": "a switch per notification channel",
            "node": {"type": "core://List", "query_key": "local:channels", "item_template": {"type": "core://Row",
                "padding_h_dp": 16, "children": [
                    {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}, {"type": "core://Spacer"},
                    {"type": "core://Switch", "checked": "$item.enabled", "on_click": {"intent": "action.device",
                        "command": "toggle_channel", "app": "$item.app_id", "id": "$item.id"}}]}}
        },
        "version_list": {
            "doc": "a layout's versions to pick from on this device; send show_versions (app: the layout) first, e.g. in an action.sequence with action.navigation.open",
            "node": {"type": "core://List", "query_key": "local:versions", "item_template": {"type": "core://Tappable",
                "on_click": {"intent": "action.device", "command": "pin_version", "id": "$item.version"},
                "content": {"type": "core://Row", "padding_h_dp": 16, "padding_v_dp": 8, "children": [
                    {"type": "core://Text", "style": "Body", "value_expr": "$item.label"},
                    {"type": "core://Text", "style": "Caption", "value_expr": "$item.detail"}]}}}
        },
        "connection_dot": {
            "doc": "a dot that is green online and grey offline",
            "node": {"type": "core://Box", "background": "${local://connection#color}", "corner_dp": 5,
                "width_dp": 10, "height_dp": 10, "content": {"type": "core://Spacer", "size_dp": 0}}
        }
    })
}

async fn vocabulary(pod: &Pod) -> Result<Value, Value> {
    let rows = crate::blocking(pod, crate::components::list)
        .await
        .map_err(store_err)?;
    let components: Vec<Value> = rows
        .iter()
        .map(|(m, d)| json!({ "name": m.name, "doc": d["doc"],
            "params": d["params"].as_array().map(|ps| ps.iter().map(|p| p["name"].clone()).collect::<Vec<_>>()) }))
        .collect();
    Ok(vocabulary_with(components))
}

fn vocabulary_with(components: Vec<Value>) -> Value {
    let features: Vec<Value> = protocol::FEATURE_DOCS
        .iter()
        .map(|(n, d)| json!({ "name": n, "doc": d }))
        .collect();
    json!({
        "plumbing": {
            "doc": "What the app draws. Adding to it needs an app build; a client gets the newest layout version whose features it draws.",
            "nodes": plumbing_nodes(),
            "features": features,
        },
        "porcelain": {
            "doc": "Defined as data in terms of plumbing and expanded by the pod before serving, so it needs no app build. get_component reads a definition; expand shows what anything becomes; copy_component and define_component make the user's own.",
            "nodes": porcelain_nodes(),
            "components": components,
        },
        "icons": protocol::ICONS,
        "intents": [
            { "intent": "action.navigation.open", "fields": "target_uri?, layout?, params?: {name: value}, presentation: Push|Modal|Replace|Sheet|Panel, animation?: none|fade|slide|sheet_up",
              "doc": "With layout: present that layout on the back stack (Push: whole screen; Sheet; Panel), params bound as $route.<name> in it, then its screen matching target_uri if any. Without: open the screen of the layout on top whose route matches target_uri. System Back pops either." },
            { "intent": "action.navigation.back", "fields": "", "doc": "Close the screen." },
            { "intent": "action.mutation.optimistic", "fields": "entity_uri, field, value_expr, rollback_value, endpoint, payload, also?: [{field, value_expr}]",
              "doc": "Change a field at once and POST payload to an action endpoint ($value is the new value); rolled back on failure." },
            { "intent": "action.mutation.create", "fields": "query_key, entity_uri (with $nonce), entity, endpoint, payload, position?, attachments?",
              "doc": "Add an item at once and POST it to an action endpoint. To close the screen after it (a compose screen's Send), put it in an action.sequence followed by action.navigation.back." },
            { "intent": "action.data.load", "fields": "path", "doc": "Load a data path (more data into the store)." },
            { "intent": "action.sequence", "fields": "steps: [intent, ...]",
              "doc": "Run several intents in order as one tap, with one $nonce for all of them (e.g. a create, then an action.local.set, then action.navigation.back). Prefer composing steps over asking for a new intent." },
            { "intent": "action.local.set", "fields": "entity_uri, field, value_expr, also?: [{field, value_expr}]",
              "doc": "Set fields of a loaded entity on this device only (no request), e.g. view state like added/expanded shown with core://Conditional. The next load of the entity from the pod replaces it." },
            { "intent": "action.link.open", "fields": "url",
              "doc": "Open an http(s) link in the device's browser (e.g. a Get tickets button with url $item.tickets_url). Nothing is sent to the pod." },
            { "intent": "action.confirm", "fields": "title, message?, confirm_label, then: intent",
              "doc": "Ask \"Are you sure?\" in the app's own dialog and run then only if the user confirms (Cancel does nothing). Wrap every step that can't be undone in it: action.device leave_pod and sign_out, and anything that deletes. In an action.sequence the later steps wait for the answer too." },
            { "intent": "action.device", "fields": "command, app?, id?, value?",
              "doc": "Something the device does itself. Commands: open_notification (id: presents its layout), mark_all_read, toggle_channel (app, id of a notification channel), sign_in (id of a provider; value: another server, e.g. $form?.server from a core://TextField, for a provider whose local:providers row says custom_server), sign_out, set_server (id of a server option), show_versions (app: a layout; loads its versions into local:versions), pin_version (id: a version of that layout, shown on this device only; remembered across restarts), follow_latest, load_older_notifications (appends the next page to local:notifications), mark_read (id: one notification), hide_layout and unhide_layout (app: a layout; hidden from every device's local:layouts, listed in local:all_layouts), delete_layout (app: a layout; always inside an action.confirm's then), leave_pod (forget the pod on this device), refresh, open_settings and open_inbox (present the settings and inbox layouts). Any layout may use any command." },
        ],
        "local_data": {
            "doc": "Data the device provides, with no feed: bind ${local://<entity>#<field>} anywhere and list local:layouts in a core://List.",
            "local://connection": "online (bool), pending (count of unsent changes), label (\"Online\", \"Offline · 2 pending\"), attention (true while offline or changes wait)",
            "local://inbox": "unread (count), label (\"3 unread\"), has_older (more notifications than local:notifications holds: show a Load older button sending load_older_notifications), loading (a page is loading)",
            "local://layouts": "count (the pod's layouts, home excluded)",
            "local:layouts": "query of the pod's layouts but home (hidden ones left out): local://layouts/<id> with id, title, icon, kind, detail, unread, version (\"v12 · latest\" or \"v10 · pinned\"), hidden (always false here)",
            "local:all_layouts": "the same rows as local:layouts plus the hidden layouts (hidden: true), for a \"show hidden\" list with unhide_layout buttons",
            "local:notifications": "query of the newest notifications: id, app_id, app_title, title, body, when (\"5 min\"), day (\"Today\"), unread",
            "local:channels": "query of the notification channels: app_id, app_title, id, title, description, enabled",
            "local:sign_ins": "query of the pod's sign-ins: id, title, detail (\"@dana · photos.example\")",
            "local:providers": "query of the sign-ins the pod offers that aren't signed in: id, title, custom_server (another server may be typed), server_placeholder",
            "local:servers": "query of the servers this device can use: id, title, url, selected",
            "local:versions": "query of the versions of the layout show_versions named, newest first: version, label (\"v12\"), detail (its note), when, compatible, showing",
            "local://versions": "app_id, app_title, pinned (the pinned version, or \"\" while following the latest)",
            "local://device": "platform (android|desktop|simulator)",
            "local://session": "open, server, error, message: the pod sign-in",
        },
        "snippets": snippets(),
        "bindings": {
            "$item.<field>": "a field of the list or carousel item",
            "${<entity uri>#<field>}": "a field of any entity, e.g. ${core://sources/<source>#signed_in}",
            "$route.<param>": "a parameter of the open screen's route",
            "$form.<field>": "a form field's value ($form?.<field>: may be blank)",
            "$nonce": "a fresh id per action",
            "$value": "the new value in a mutation's payload",
        },
        "rules": "Layouts may only use the sources of home's feeds: data paths, actions, query keys and entity schemes from list_data_sources. No action.auth.* intents (sign in with action.device sign_in instead) and no other /v1/ paths. Data the device provides (local_data) needs no feed. Read get_layout of a blueprint for complete examples.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_snippet_is_a_valid_custom_layout() {
        let all = snippets();
        for (name, snippet) in all.as_object().unwrap() {
            let doc = match snippet.get("layout") {
                Some(l) => {
                    let mut l = l.clone();
                    l["id"] = json!("home");
                    l
                }
                None => json!({"type": "core://Workspace", "id": "home", "title": "Home",
                    "slots": [{"slot_id": "s", "provider_uri": "local://x", "content": snippet["node"]}]}),
            };
            // Placeholders stand for a real layout id.
            let doc: Value =
                serde_json::from_str(&doc.to_string().replace("<layout id>", "c-abcdefghij"))
                    .unwrap();
            crate::validate::custom_layout("home", &doc, &[])
                .unwrap_or_else(|e| panic!("snippet {name}: {} at {}", e.message, e.pointer));
        }
    }

    #[test]
    fn every_node_type_is_documented() {
        for n in plumbing_nodes() {
            let t = n["type"].as_str().unwrap();
            let f = n["feature"].as_str().unwrap();
            assert!(
                protocol::FEATURES.contains(&f),
                "{t}: no feature file {f}.md"
            );
            assert!(
                n["doc"].as_str().unwrap().contains("Fields:") || t == "core://Divider",
                "{t}: features/{f}.md lists no fields"
            );
        }
        let v = vocabulary_with(Vec::new());
        assert!(v["porcelain"]["nodes"].as_array().unwrap().len() >= 4);
    }

    #[test]
    fn json_patch_ops() {
        let mut doc = json!({"a": [1, 2], "b": {"c": "x"}});
        apply_patch(
            &mut doc,
            &json!([
                {"op": "add", "path": "/a/-", "value": 3},
                {"op": "replace", "path": "/b/c", "value": "y"},
                {"op": "copy", "from": "/b", "path": "/d"},
                {"op": "move", "from": "/a/0", "path": "/e"},
                {"op": "remove", "path": "/a/0"},
                {"op": "test", "path": "/d/c", "value": "y"},
            ]),
        )
        .unwrap();
        assert_eq!(
            doc,
            json!({"a": [3], "b": {"c": "y"}, "d": {"c": "y"}, "e": 1})
        );
        // All or nothing.
        let before = doc.clone();
        let err = apply_patch(
            &mut doc,
            &json!([{"op": "remove", "path": "/a"}, {"op": "test", "path": "/b/c", "value": "z"}]),
        )
        .unwrap_err();
        assert!(err.contains("operation 1"), "{err}");
        assert_eq!(doc, before);
        assert!(
            apply_patch(
                &mut doc,
                &json!([{"op": "replace", "path": "/nope", "value": 1}])
            )
            .is_err()
        );
    }

    #[test]
    fn origins_must_be_this_host() {
        let mut h = HeaderMap::new();
        assert!(origin_ok(&h), "no Origin: not a browser");
        h.insert(header::HOST, HeaderValue::from_static("pod.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://pod.example"),
        );
        assert!(origin_ok(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!origin_ok(&h));
    }
}
