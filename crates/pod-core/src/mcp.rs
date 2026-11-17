//! `POST /v1/mcp`: the pod's MCP endpoint for Claude (decision
//! `mcp-connector`), streamable HTTP with one JSON response per request.
//!
//! Claude reaches it with an MCP credential the pod host issued (OAuth, or
//! `server pods mcp`): the host forwards such a request with
//! [`crate::CREDENTIAL_HEADER`]` = mcp` and no bearer, and the credential
//! reaches nothing else. The pod token itself works too (served alone, in
//! tests). The tools read the pod's layouts, blueprints and data sources and
//! write the construct ([`crate::construct::write`], decision
//! `content-addressed-construct`); writes are recorded with author `claude`. Nothing
//! here can call a source's routes or act for the user upstream, except
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
feed\" is one layout (create_layout using the blueprint that fits whole: {\"type\": \"core://Use\", \"ref\": \
\"@core/<blueprint>\"}, with its feeds) and a home that shows it (update_layout on home: a core://Layout with \
\"ref\": \"<layout id>\" as the one slot). With more than one layout, home lists them and each row pushes its \
layout (action.navigation.open {\"ref\": \"<layout id>\", \"presentation\": \"Push\"}); system Back comes back. \
Layouts only go forward: a link back to a layout that leads here is action.navigation.pop_to {\"edge\": <id>} \
(pop_to_root for home). A core://Layout can also show a layout in part of the screen, or as a Sheet, Panel or \
Overlay (from any edge) while a condition holds; core://Gesture runs an intent on a tap, long press or swipe, and \
core://SwipeCard is a card that follows the finger and runs an intent when swiped off (a core://List with stack: \
true is a deck of them). Home owns every feed: feeds you name when creating a layout are added to home's, and \
every layout may bind any of home's feeds; a core://Layout or action.navigation.open binds a layout's inputs \
($in.<name>) with \"bind\" (only home binds the pod's data). The inbox and Settings are the blueprints inbox and \
settings (\"ref\": \"@core/inbox\" with presentation Sheet, \"@core/settings\" with Panel); describe_vocabulary \
has small snippets. home can't be deleted. Everything is a name that points to an immutable element: the pod's \
layouts are named by their id, blueprints (the built-in layouts) @core/<blueprint>, their parts \
@core/<blueprint>/<part>, components @core/<name> (ship with the server) and @me/<name> (the user's own). A \
reference follows its name (a deploy moves @core/ names, an edit moves the user's), so use a blueprint, a part or \
a component by reference to keep it current, or read it (get_layout, get_component) and write your own to change \
it. list_layouts lists layouts and blueprints, list_parts blueprints' parts, list_components components. Call \
list_data_sources and describe_vocabulary first and sample_data to see real fields. Every write is validated \
(errors give a JSON pointer and what is allowed) and appears live on the user's devices; each name keeps its \
history, and restore_version moves a layout back to an earlier one. Before changing anything the user sees (\"my \
notifications\", \"the shopping screen\"), find what is drawn there: start at home and follow its references \
(embeds, components, links and gestures) to the element on that screen; list_layouts and get_layout give each \
layout's path from home (reached) and flag a layout copied from a blueprint (copy_of). Change that element, not \
the blueprint it may have started from: a copy doesn't follow its blueprint, and a blueprint changes only with the \
server. Then look at it with render_layout_preview. The vocabulary is plumbing (what the app \
draws) and porcelain: components, used with {\"type\": \"core://Use\", \"ref\": \"@core/<name>\" or \
\"@me/<name>\", \"args\", \"children\"} and expanded by the app. Prefer a component over repeating nodes, and \
define one (define_component, or copy_component to change a @core/ one) when a piece repeats across layouts. \
expand shows what any layout or component becomes; render_layout_preview shows what it looks like (an image and \
every node's bounds, with the user's data, as their device shows it; target_uri opens a screen, item tries a row \
with one entity, full_height draws a long screen whole), and dispatch_virtual_interaction taps or scrolls that \
preview, so look at a layout after changing it. A layout whose root is a core://Page gets no chrome from the app: \
it draws its own header (a title with actions on one line) and Back (@core/back_button); core://Workspace is \
written as the page @core/workspace draws. Use ${local://theme#<token>} for colours (set_theme). A button whose \
step can't be undone (action.device leave_pod or sign_out, a delete) asks first: wrap its intent in \
action.confirm. Jacks are the abilities the pod is plugged into (a model, fetching websites, web search with the \
pod's own key, accounts such as an agent runner): list_jacks shows each and its state, plug_jack and unplug_jack \
change the switch and config ones (a search key is given once and never shown back); account jacks are plugged \
through their own sign-in. A feature whose jack is unplugged says so: tell the user which jack it needs rather \
than working around it. The user can share with other users of this server: publish \
answers a review first (show it: elements, inputs, and flagged text and URLs) and shares only with confirm; \
adopt binds a share link to the user's own data (propose, confirm with the user, then put its \"use\" in \
home); a revoked share keeps drawing where it was adopted.";

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

/// The core's tools, then every adapter's ([`crate::Source::mcp_tools`]).
fn all_tools(pod: &Pod) -> Vec<Value> {
    let mut out = tools();
    // Public links, where the host serves them (decision `public-web-links`).
    if pod.shelf().is_some_and(|(_, s)| s.public_links_enabled()) {
        out.extend(crate::public_copy::tool_specs());
    }
    for a in pod.catalog().adapters() {
        out.extend(a.mcp_tools());
    }
    out
}

fn tools() -> Vec<Value> {
    let mut out = base_tools();
    out.extend(crate::construct::share::tool_specs());
    out.extend(crate::preview::tools());
    out.extend(crate::mcp_servers::tools());
    out
}

fn base_tools() -> Vec<Value> {
    let feeds = json!({ "type": "array", "description": "Feeds to add to home's (every layout binds home's feeds): [source, feed] pairs (see list_data_sources)",
        "items": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2 } });
    let layout = json!({ "type": "object", "description": "A layout's root: a core://Page (or a core://Workspace, written as its page; see describe_vocabulary); its id is set by the pod" });
    let id = json!({ "type": "string", "description": "A layout id: home, a c-... id from list_layouts, or a blueprint" });
    let note = json!({ "type": "string", "description": "What changed, in the user's words (shown in the name's history)" });
    vec![
        tool(
            "list_jacks",
            "The pod's jacks (the abilities it is plugged into: model, web, web_search, and the accounts adapters declare): each one's kind (switch, config or account), state (plugged, unplugged or locked by the server), what it enables, and for config jacks their fields (secrets show only whether they are set).",
            json!({}),
            &[],
        ),
        tool(
            "plug_jack",
            "Plug a switch or config jack: {id} for a switch; {id, config: {field: value}} for a config jack (web_search: {provider: brave, api_key} or {provider: searxng, base_url} or {provider: duckduckgo}). A secret left out keeps its stored value. Account jacks are plugged by their own sign-in; this answers where. A locked jack can't be plugged.",
            json!({
                "id": { "type": "string", "description": "A jack id from list_jacks" },
                "config": { "type": "object", "description": "A config jack's values by field name" },
            }),
            &["id"],
        ),
        tool(
            "unplug_jack",
            "Unplug a jack: a switch turns off, a config jack forgets its values and secrets, the agent_runner account signs out of its runner (forgetting what it sent). Features that need it stop (fillers that need web or web_search fail with a message naming the jack). Ask the user first.",
            json!({ "id": { "type": "string", "description": "A jack id from list_jacks" } }),
            &["id"],
        ),
        tool(
            "list_layouts",
            "The user's layouts (home first, with the feeds every layout may bind; each with its element, its history's newest version, reached: how the user gets to it from home (e.g. home → (on_swipe_down, Overlay from top) → c-...), or that home doesn't reach it, and copy_of: the blueprint it looks copied from, which it doesn't follow) and the blueprints layouts can use (@core/<id>, with their feeds, reached, used_by: the layouts that use them, and copies).",
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
            "A layout or blueprint (now, or one entry of its name's history), its element and needs, its history, reached (how the user gets to it from home, or that home doesn't reach it) and copy_of (the blueprint it looks copied from, which it doesn't follow). expanded: true adds what clients get (components expanded to plumbing).",
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
            "Check a layout against home's feeds (plus feeds, when given) without saving it: what it is written as (references sealed).",
            json!({ "feeds": feeds, "layout": layout }),
            &["layout"],
        ),
        tool(
            "create_layout",
            "Create a layout. It can be shown on the user's devices at once (home embeds or presents it). feeds are added to home's.",
            json!({ "title": { "type": "string" }, "icon": { "type": "string", "enum": crate::layouts::LAYOUT_ICONS },
                    "feeds": feeds, "layout": layout, "note": note }),
            &["title", "layout"],
        ),
        tool(
            "update_layout",
            "Change a layout: a whole layout, or a JSON Patch (RFC 6902) against its current node (as get_layout shows it). Pass base_version (the version you read) so a concurrent edit isn't lost.",
            json!({ "id": id, "layout": layout,
                    "json_patch": { "type": "array", "items": { "type": "object" } },
                    "note": note, "base_version": { "type": "integer" }, "feeds": feeds }),
            &["id"],
        ),
        tool(
            "restore_version",
            "Move a layout back to an earlier entry of its name's history (get_layout lists it); the move is recorded too.",
            json!({ "id": id, "version": { "type": "integer" }, "note": note }),
            &["id", "version"],
        ),
        tool(
            "rename_layout",
            "Change a layout's title or icon. feeds: feeds to add to home's; on home itself, home's whole feeds list (a feed may go once no layout shows it).",
            json!({ "id": id, "title": { "type": "string" }, "icon": { "type": "string", "enum": crate::layouts::LAYOUT_ICONS }, "feeds": feeds }),
            &["id"],
        ),
        tool(
            "delete_layout",
            "Delete a layout on every device (refused while another layout links to or embeds it). Only when the user asked for it.",
            json!({ "id": id }),
            &["id"],
        ),
        tool(
            "list_parts",
            "The parts of blueprints a layout can use whole, by reference: @core/<blueprint>/<part> (each slot of a blueprint, slot:<id>, and the parts it declares), with each part's node when with_layouts.",
            json!({ "layout": { "type": "string", "description": "A blueprint (all when left out)" }, "with_layouts": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "list_components",
            "Porcelain the layouts can use: every component (@core/<name> ones ship with the server, @me/<name> ones are the user's own) with its doc and parameters, and the blueprints and their parts. Use one with {\"type\": \"core://Use\", \"ref\": <name>, \"args\": {...}, \"children\": [...]}.",
            json!({}),
            &[],
        ),
        tool(
            "get_component",
            "A component's element (doc, params, node: its template, examples) and its name's history.",
            json!({ "name": { "type": "string", "description": "@core/<name> or @me/<name>" } }),
            &["name"],
        ),
        tool(
            "expand",
            "What clients get: a layout or any name (id or name), or any node (node), with every component expanded, as plumbing only, the features it needs and its edges.",
            json!({ "id": id, "name": { "type": "string" }, "node": { "type": "object" } }),
            &[],
        ),
        tool(
            "define_component",
            "Define a component of the user's own (@me/<name>) any layout on this pod can use: {doc, params: [{name, type: text|number|bool|uri|binding, default?, required?, doc}], children?: true, template, examples: [{args, children?, note}]}. In the template, $arg.<name> is an argument (a whole \"$arg.n\" keeps its type), {\"type\": \"core://Children\"} is where the caller's children go (\"at\": i for one), and {\"type\": \"core://IfArg\", \"arg\", \"equals\"?, \"then\", \"else\"?} picks a branch. Every example must expand.",
            json!({ "name": { "type": "string", "description": "<name> (@me/<name>)" }, "component": { "type": "object" }, "note": note }),
            &["name", "component"],
        ),
        tool(
            "update_component",
            "Change a component of the user's own (@me/<name>): its name moves, and every layout that uses it follows.",
            json!({ "name": { "type": "string" }, "component": { "type": "object" }, "note": note, "base_version": { "type": "integer" } }),
            &["name", "component"],
        ),
        tool(
            "copy_component",
            "Copy a component (e.g. @core/workspace) into one of the user's own (@me/<name>) you can change.",
            json!({ "from": { "type": "string" }, "name": { "type": "string", "description": "<name> (@me/<name>; default: the copied one's)" }, "note": note }),
            &["from"],
        ),
        tool(
            "restore_component",
            "Move a component of the user's own back to an earlier entry of its name's history (get_component lists it).",
            json!({ "name": { "type": "string" }, "version": { "type": "integer" } }),
            &["name", "version"],
        ),
        tool(
            "delete_component",
            "Delete a component of the user's own. Refused while a layout or component uses it (the error lists them). Only when the user asked for it.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            "set_time_zone",
            "The user's time zone (an IANA name such as America/Denver), used where the pod turns a date or time without a zone into a moment (collection reminders). Ask the user when you don't know it. Without time_zone: answers the current one (UTC when unset).",
            json!({ "time_zone": { "type": "string", "description": "An IANA zone name, e.g. Europe/Berlin" } }),
            &[],
        ),
        tool(
            "set_theme",
            "The pod's theme colours, each with a light and a dark value; layouts bind them as ${local://theme#<token>} and the app picks the value for the device's mode. Besides the built-in tokens, a pod may name up to 40 of its own (e.g. a colour per category, a toolbar's fill): any new name [a-z][a-z0-9_]{0,31}, not ending in _dark; give it a light and a dark value (one alone is used for both). Without theme: answers the current theme and its tokens.",
            json!({ "theme": { "type": "object", "description": "{token: {light?: #RRGGBB, dark?: #RRGGBB}}, or flat: {token: #RRGGBB (light), token_dark: #RRGGBB}. A built-in token left unset follows its default for each mode; {token: null} puts a built-in back to its defaults or removes a token of the pod's own." } }),
            &[],
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
    // Previews answer an image as well as text (crate::preview).
    match name {
        "render_layout_preview" => {
            return crate::preview::render_layout_preview(pod, headers, &args).await;
        }
        "dispatch_virtual_interaction" => {
            return crate::preview::dispatch_virtual_interaction(pod, &args).await;
        }
        _ => {}
    }
    if let Some(r) = crate::mcp_servers::call(pod, headers, name, &args).await {
        return tool_result(r);
    }
    // Public links (decision `public-web-links`).
    if let Some(r) = crate::public_copy::tool(pod, headers, name, &args).await {
        return tool_result(r);
    }
    // Sharing between the server's pods (decision `sharing`).
    if let Some(r) = crate::construct::share::tool(pod, name, &args).await {
        return tool_result(r);
    }
    // The layout and component tools write the construct (decision
    // `content-addressed-construct`).
    if let Some(r) = crate::construct::write::mcp(pod, name, &args).await {
        let r = match name {
            "create_layout" | "update_layout" | "validate_layout" => {
                r.map(|v| with_colour_advice(v, &args["layout"]))
            }
            _ => r,
        };
        return tool_result(r);
    }
    let r = match name {
        "list_jacks" => Ok(json!({ "jacks": crate::jacks::list(pod).await })),
        "plug_jack" => crate::jacks::plug_tool(pod, &args).await,
        "unplug_jack" => crate::jacks::unplug_tool(pod, &args).await,
        "list_data_sources" => list_data_sources(pod).await,
        "describe_vocabulary" => vocabulary(pod).await,
        "sample_data" => sample_data(pod, headers, &args).await,
        "set_time_zone" => set_time_zone(pod, &args).await,
        "set_theme" => set_theme(pod, &args).await,
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
        Ok(t) => {
            let mut tokens: serde_json::Map<String, Value> = crate::porcelain::THEME_TOKENS
                .iter()
                .map(|(t, _, _, what)| ((*t).to_owned(), json!(what)))
                .collect();
            for own in t
                .keys()
                .filter(|k| !tokens.contains_key(*k))
                .cloned()
                .collect::<Vec<_>>()
            {
                tokens.insert(own, json!("the pod's own token"));
            }
            Ok(json!({ "ok": true, "theme": t, "tokens": tokens,
            "use": "bind ${local://theme#<token>} where a colour goes (core://Box background, border_color, content_color; core://IconButton active_tint; core://List focus colours) instead of a fixed #RRGGBB, so the layout follows the device's light or dark mode: page for a page's background, card for cards and rows on it, card_border for their borders, muted_text for secondary text. Devices pick a change up the next time they load the layout." }))
        }
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

/// The owner's session (the pod token's), for what is signed in and for
/// samples; `None` on a pod without one.
pub(crate) async fn owner(pod: &Pod) -> Option<crate::Session> {
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

/// A layout write's answer with `warnings`: the fixed colours it uses,
/// which stay the same in dark mode (none: the answer as it is).
fn with_colour_advice(mut answer: Value, layout: &Value) -> Value {
    let warnings = crate::validate::fixed_colours(layout);
    if !warnings.is_empty() {
        answer["warnings"] = json!(warnings);
    }
    answer
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

fn store_err((s, Json(b)): (StatusCode, Json<Value>)) -> Value {
    refused((s, b))
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
        .filter(|t| !RETIRED_NODES.contains(t) && **t != "core://Workspace")
        .map(|t| {
            let f = node_feature(t);
            json!({ "type": t, "feature": f, "doc": feature_doc(&f) })
        })
        .collect()
}

/// The porcelain nodes (expanded by the app from the elements they name).
fn porcelain_nodes() -> Value {
    json!([
        { "type": crate::components::USE_TYPE, "fields": "ref, args?, children?",
          "doc": "An element where it stands, by reference (\"ref\": a name): a component (list_components: @core/<name> ships with the server, @me/<name> is the user's own) with its args and children, a whole blueprint (@core/<blueprint>, as a layout's root; the layout keeps its own id) or a part of one (@core/<blueprint>/<part>, list_parts). It follows its name: when the name moves, the layout draws the new element. \"component\": \"core:<name>\" or \"pod:<name>\" is accepted for ref when written." },
        { "type": "core://Workspace", "fields": "id, title, slots: [{slot_id, content}], screens?: [{id, route, title, data?, content, footer?}], title_scrolls?",
          "doc": "A page the older way: its slots stacked under a title, and screens with a bar (Back, title, a spinner while data loads) over their scrolling content and footer. Written as a core://Page through @core/workspace and @core/screen; prefer a core://Page for a new layout." },
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
        "home_rows_of_layouts": {
            "doc": "a home slot with a row per layout; a tap pushes the layout, and system Back comes back (one row per layout home links to; replace <layout id>)",
            "node": {"type": "core://Column", "children": [{"type": "core://Tappable",
                "on_click": {"intent": "action.navigation.open", "layout": "<layout id>", "presentation": "Push"},
                "content": {"type": "core://Row", "padding_h_dp": 16, "padding_v_dp": 12,
                    "children": [{"type": "core://Text", "style": "Body", "value_expr": "Title of the layout"}]}}]}
        },
        "settings_and_inbox_buttons": {
            "doc": "buttons that present the inbox as a sheet and Settings as a side panel",
            "node": {"type": "core://Row", "children": [{"type": "core://Spacer"},
                {"type": "core://IconButton", "icon": "notifications", "content_description": "Notifications",
                 "on_click": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet"}},
                {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
                 "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}]}
        },
        "inbox_from_the_top_on_swipe_down": {
            "doc": "home's content wrapped so pulling it down (at its top) slides the inbox down from the top",
            "node": {"type": "core://Gesture",
                "on_swipe_down": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Overlay", "edge": "top"},
                "content": {"type": "core://Layout", "layout": "<layout id>"}}
        },
        "swipe_card_of_a_deck": {
            "doc": "a card of a deck: the item_template of a core://List with stack: true (one card at a time, the next peeking behind) and where [{field: verdict, equals: Unseen}]; dragged right it likes, left it passes, and the card leaves the list. To save the verdict, make each intent the data source's update action (action.mutation.optimistic on $item.uri, field verdict) instead",
            "node": {"type": "core://SwipeCard",
                "on_swipe_right": {"intent": "action.local.set", "entity_uri": "$item.uri", "field": "verdict", "value_expr": "\"Like\""},
                "on_swipe_left": {"intent": "action.local.set", "entity_uri": "$item.uri", "field": "verdict", "value_expr": "\"Pass\""},
                "content": {"type": "core://Box", "corner_dp": 18, "padding_h_dp": 16, "padding_v_dp": 16,
                    "background": "${local://theme#card}", "border_color": "${local://theme#card_border}",
                    "content": {"type": "core://Text", "style": "Headline", "value_expr": "$item.title"}}}
        },
        "centred_tiles": {
            "doc": "fixed-size tiles centred in a row on any screen width (justify: center, no spacers; space_evenly spreads them); tiles too wide for the row start at its padding",
            "node": {"type": "core://Row", "justify": "center", "spacing_dp": 8, "padding_h_dp": 16, "children": [
                {"type": "core://Box", "width_dp": 80, "height_dp": 80, "corner_dp": 14, "align": "Center",
                    "background": "${local://theme#card}", "content": {"type": "core://Text", "style": "Caption", "value_expr": "One"}},
                {"type": "core://Box", "width_dp": 80, "height_dp": 80, "corner_dp": 14, "align": "Center",
                    "background": "${local://theme#card}", "content": {"type": "core://Text", "style": "Caption", "value_expr": "Two"}}]}
        },
        "card_of_layout": {
            "doc": "another layout in a card-sized region of the page",
            "node": {"type": "core://Box", "corner_dp": 12, "background": "${local://theme#card}", "border_color": "${local://theme#card_border}",
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
        "once_it_has_happened": {
            "doc": "in a row's template: one thing once its session is over (date + time on the device's clock), another before then; the device redraws it at that minute by itself, so no scheduled job has to flip a flag. Put the row's check-off button in the Past branch. date: Past (the moment has come; a bare date: the day is over), Today or Future; time binds a wall-clock time (15:45, 9:00 AM) to a bare date, or the value may be a datetime",
            "node": {"type": "core://Column", "children": [
                {"type": "core://Conditional", "value": "$item.date", "time": "$item.ends", "date": "Past",
                 "content": {"type": "core://Text", "style": "Label", "value_expr": "Over: tap to check off"}},
                {"type": "core://Conditional", "value": "$item.date", "time": "$item.ends", "date": "Future",
                 "content": {"type": "core://Text", "style": "Secondary", "value_expr": "Opens at $item.ends"}}]}
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
                        "command": "toggle_channel", "layout": "$item.layout", "id": "$item.id"}}]}}
        },
        "connection_dot": {
            "doc": "a dot that is green online and grey offline",
            "node": {"type": "core://Box", "background": "${local://connection#color}", "corner_dp": 5,
                "width_dp": 10, "height_dp": 10, "content": {"type": "core://Spacer", "size_dp": 0}}
        }
    })
}

async fn vocabulary(pod: &Pod) -> Result<Value, Value> {
    let rows = crate::blocking(pod, |s| {
        s.with_conn(|c| {
            let mut out = Vec::new();
            for (n, h) in crate::construct::store::names(c)? {
                if crate::construct::is_layout_name(&n) {
                    continue;
                }
                let Some((el, _)) = crate::construct::store::object(c, &h)? else {
                    continue;
                };
                if el.get("params").is_some() || el.get("doc").is_some() {
                    out.push(json!({ "name": n, "doc": el["doc"],
                        "params": el["params"].as_array().map(|ps| ps.iter().map(|p| p["name"].clone()).collect::<Vec<_>>()) }));
                }
            }
            Ok(out)
        })
    })
    .await
    .map_err(store_err)?;
    Ok(vocabulary_with(rows))
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
            "doc": "Defined as data in terms of plumbing and expanded by the app from the elements it names, so it needs no app build. get_component reads a definition; expand shows what anything becomes; copy_component and define_component make the user's own.",
            "nodes": porcelain_nodes(),
            "components": components,
        },
        "icons": protocol::ICONS,
        "intents": [
            { "intent": "action.navigation.open", "fields": "target_uri?, ref?, bind?, params?: {name: value}, presentation: Push|Modal|Replace|Sheet|Panel|Overlay, animation?: none|fade|slide|sheet_up, edge?: top|bottom|start|end, size_dp?",
              "doc": "With ref (a layout's name: its id, or @core/<blueprint>; \"layout\": <id> is accepted when written): present that layout on the back stack (Push: whole screen; Sheet: from the bottom; Panel: from the start; Overlay: from edge, bottom when unset, size_dp tall or wide; drag it back toward its edge to dismiss), bind filling its inputs, params bound as $route.<name> in it, then its screen matching target_uri if any. Without: open the screen of the layout on top whose route matches target_uri. System Back pops either. A link may not lead back to a layout that leads here (layouts only go forward): use action.navigation.pop_to." },
            { "intent": "action.navigation.pop_to", "fields": "edge", "doc": "Back to the nearest open layout with that key (a layout's id, a blueprint's id)." },
            { "intent": "action.navigation.pop_to_root", "fields": "", "doc": "Back to home." },
            { "intent": "action.navigation.back", "fields": "", "doc": "Close the screen." },
            { "intent": "action.mutation.optimistic", "fields": "entity_uri, field, value_expr, endpoint, payload, also?: [{field, value_expr}], rollback_value?",
              "doc": "Change a field at once and POST payload to an action endpoint ($value is the new value); a refusal restores each field's value from before (rollback_value only when the field had none; leave it out)." },
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
            { "intent": "action.device", "fields": "command, layout?, id?, value?",
              "doc": "Something the device does itself. Commands: open_notification (id: a notification's id, or the uri of a row of the notifications collection ($item.uri); marks it read and presents its layout and target_uri), mark_all_read, toggle_channel (layout, id of a notification channel), sign_in (id of a provider; value: another server, e.g. $form?.server from a core://TextField, for a provider whose local:providers row says custom_server), sign_out, set_server (id of a server option), load_older_notifications (appends the next page to local:notifications), mark_read (id: one notification), request_notification_permission (the phone's prompt to allow notifications, or the app's notification settings once the prompt can't show again; nothing on desktop), leave_pod (forget the pod on this device), refresh. Any layout may use any command. To present Settings or the inbox, use action.navigation.open {ref: @core/settings, presentation: Panel} or {ref: @core/inbox, presentation: Sheet}." },
        ],
        "local_data": {
            "doc": "Data the device provides, with no feed: bind ${local://<entity>#<field>} anywhere and list local:notifications in a core://List.",
            "local://connection": "online (bool), pending (count of unsent changes), label (\"Online\", \"Offline · 2 pending\"), attention (true while offline or changes wait)",
            "local://inbox": "unread (count), label (\"3 unread\"), has_older (more notifications than local:notifications holds: show a Load older button sending load_older_notifications), loading (a page is loading)",
            "local:notifications": "query of the newest notifications: id, channel_layout, layout, app_title (its layout's title), title, body, when (\"5 min\"), day (\"Today\"), unread",
            "local:channels": "query of the notification channels: layout, app_title (its layout's title), id, title, description, enabled",
            "local:sign_ins": "query of the pod's sign-ins: id, title, detail (\"@dana · photos.example\")",
            "local:providers": "query of the sign-ins the pod offers that aren't signed in: id, title, custom_server (another server may be typed), server_placeholder",
            "local:servers": "query of the servers this device can use: id, title, url, selected",
            "local://device": "platform (android|desktop|simulator), notification_permission (granted|denied on Android; \"\" where it doesn't apply): pair \"denied\" with an action.device request_notification_permission button",
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
        "rules": "Layouts may only use the sources of home's feeds: data paths, actions, query keys and entity schemes from list_data_sources. No action.auth.* intents (sign in with action.device sign_in instead) and no other /v1/ paths. Data the device provides (local_data) needs no feed. Layouts only go forward (no link or embed back to a layout that reaches this one). Read get_layout of a blueprint for complete examples.",
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
