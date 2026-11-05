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

const INSTRUCTIONS: &str = "This is the user's own pod in The Construct. It serves apps whose screens are \
JSON layouts (server-driven UI) over data sources. The server ships blueprints (the built-in layouts: list_apps \
lists them under blueprints; get_layout reads one); the pod shows only the user's own apps, which you build. \
The user's app opens on their home app (id home) and shows nothing else: home IS the user's whole interface. A \
new pod's home is a blank page (the 'shell' blueprint); build the user's navigation into home yourself, so every \
app you build is reachable from it (e.g. a core://Drawer or core://Dock over local:apps with a core://AppFrame \
per app, and pages for notifications and settings over the local: device data). Earlier shell versions are \
working examples: version 3 is a full drawer with Notifications and Settings pages, version 2 a dock \
(get_layout, or reference one with {\"type\": \"core://Ref\", \"app\": \"shell\", \"part\": \"layout\", \
\"version\": 3}). home can't be deleted. Call list_data_sources and describe_vocabulary first, read a blueprint \
with get_layout for working examples, sample_data to see real fields, then create_app or update_layout. Every \
write is validated (errors give a JSON pointer and what is allowed) and appears live on the user's devices; \
restore_version undoes an edit. Blueprints can't be edited. Decide per request: reference a blueprint \
({\"type\": \"core://Ref\", \"app\": <blueprint>, \"part\": \"layout\"} as the whole layout, with the \
blueprint's feeds; or a part, list_parts lists them) so it stays current, or copy it to change it and pass \
copied_from so the user hears when the blueprint changes (rebase_context then gives you what to merge).";

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
    let feeds = json!({ "type": "array", "description": "The app's root feeds: [source, feed] pairs (see list_data_sources)",
        "items": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2 } });
    let layout = json!({ "type": "object", "description": "A core://Workspace layout (see describe_vocabulary); its id is set by the pod" });
    let app = json!({ "type": "string", "description": "An app id" });
    let note = json!({ "type": "string", "description": "What changed, in the user's words (shown in the version history)" });
    let copied = json!({ "type": "object", "description": "When the layout copies parts of a built-in app: {app, version} of what was copied",
        "properties": { "app": { "type": "string" }, "version": { "type": "integer" } } });
    vec![
        tool(
            "list_apps",
            "The user's apps (apps, the home app among them) and the blueprints apps can start from (blueprints), with their feeds and latest version.",
            json!({}),
            &[],
        ),
        tool(
            "list_data_sources",
            "Every data source of the pod: its feeds (what an app's root can show), data paths (screen data), actions (endpoints buttons may call), entity types with their fields, and whether its sign-in is done.",
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
            "An app's layout (latest, or one version), its feeds when custom, and its version history.",
            json!({ "app": app, "version": { "type": "integer" } }),
            &["app"],
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
            "Check a layout against feeds without saving it.",
            json!({ "feeds": feeds, "layout": layout }),
            &["feeds", "layout"],
        ),
        tool(
            "create_app",
            "Create a custom app. It appears on the user's devices at once.",
            json!({ "title": { "type": "string" }, "icon": { "type": "string", "enum": custom::APP_ICONS },
                    "feeds": feeds, "layout": layout, "note": note, "copied_from": copied }),
            &["title", "feeds", "layout"],
        ),
        tool(
            "update_layout",
            "Save a new version of a custom app's layout: a whole layout, or a JSON Patch (RFC 6902) against the latest. Pass base_version (the version you read) so a concurrent edit isn't lost.",
            json!({ "app": app, "layout": layout,
                    "json_patch": { "type": "array", "items": { "type": "object" } },
                    "note": note, "base_version": { "type": "integer" }, "copied_from": copied }),
            &["app"],
        ),
        tool(
            "restore_version",
            "Make an old version of a custom app the latest again (appends a copy).",
            json!({ "app": app, "version": { "type": "integer" }, "note": note }),
            &["app", "version"],
        ),
        tool(
            "rename_app",
            "Change a custom app's title, icon or feeds (its layout must still fit new feeds).",
            json!({ "app": app, "title": { "type": "string" }, "icon": { "type": "string", "enum": custom::APP_ICONS }, "feeds": feeds }),
            &["app"],
        ),
        tool(
            "delete_app",
            "Delete a custom app on every device. Only when the user asked for it.",
            json!({ "app": app }),
            &["app"],
        ),
        tool(
            "list_parts",
            "The parts of built-in apps a custom layout can reference with core://Ref (screens, slots and named parts), with each part's layout.",
            json!({ "app": { "type": "string", "description": "A built-in app (all when left out)" }, "with_layouts": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "rebase_context",
            "For a custom app copied from a built-in (copied_from): the built-in layout it was copied from, the built-in's latest, and the custom app's current layout, to merge new built-in changes into the copy. Save the merge with update_layout and copied_from set to the new version; or dismiss_update if the user doesn't want it.",
            json!({ "app": app }),
            &["app"],
        ),
        tool(
            "dismiss_update",
            "Stop telling the user that the built-in a custom app was copied from has updates (up to a version).",
            json!({ "app": app, "version": { "type": "integer" } }),
            &["app", "version"],
        ),
        tool(
            "set_time_zone",
            "The user's time zone (an IANA name such as America/Denver), used where the pod turns a date or time without a zone into a moment (collection reminders). Ask the user when you don't know it. Without time_zone: answers the current one (UTC when unset).",
            json!({ "time_zone": { "type": "string", "description": "An IANA zone name, e.g. Europe/Berlin" } }),
            &[],
        ),
        tool(
            "set_hidden",
            "Hide an app from the user's app list (built-in apps too), or show it again.",
            json!({ "app": app, "hidden": { "type": "boolean" } }),
            &["app", "hidden"],
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
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let r = match name {
        "list_apps" => list_apps(pod).await,
        "list_data_sources" => list_data_sources(pod).await,
        "describe_vocabulary" => Ok(vocabulary()),
        "get_layout" => get_layout(pod, &args).await,
        "sample_data" => sample_data(pod, headers, &args).await,
        "validate_layout" => custom::validate(pod, &args).map_err(refused),
        "create_app" => custom::create(pod, &args, Writer::Claude)
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
        "rename_app" => match str_arg(&args, "app") {
            Ok(app) => custom::patch(pod, app, &args)
                .await
                .map(|w| json!({ "ok": true, "id": w.id, "version": w.version }))
                .map_err(refused),
            Err(e) => Err(e),
        },
        "delete_app" => match str_arg(&args, "app") {
            Ok(app) => custom::delete(pod, app)
                .await
                .map(|()| json!({ "ok": true }))
                .map_err(refused),
            Err(e) => Err(e),
        },
        "list_parts" => list_parts(pod, &args).await,
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
    let listed = crate::blocking(pod, move |s| {
        let hidden = custom::hidden_apps(s)?;
        let custom_apps = s.with_conn(custom::custom_apps_on)?;
        let row = |a: custom::AppRef| -> rusqlite::Result<Value> {
            let latest = s.app_latest_meta(a.id())?.map(|v| v.version);
            Ok(json!({
                "id": a.id(), "title": a.title(), "icon": a.icon(), "kind": a.kind(),
                "hidden": hidden.iter().any(|h| h == a.id()),
                "feeds": a.feeds(), "latest_version": latest,
            }))
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
    Ok(json!({ "apps": listed.0, "blueprints": listed.1 }))
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
        "note": "A custom app's root also gets core://sources/<id> entities {title, signed_in, error?}: show a hint such as \"Connect it in Settings\" with core://Conditional on ${core://sources/<id>#signed_in}. Sign-ins start from Settings; layouts can't hold sign-in buttons.",
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
    Ok(json!({
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
    }))
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

/// `(node type, fields, what it is)`: every [`protocol::NODE_TYPES`] entry
/// (a test checks it).
pub const NODE_DOCS: &[(&str, &str, &str)] = &[
    (
        "core://Workspace",
        "id, title, slots: [{slot_id, provider_uri, content}], screens: [{id, route, title, data?, content, footer?}]",
        "The root. Slots stack top to bottom; screens open on navigation (route like scheme://things/{id}; data is a data path loaded when it opens; $route.<param> in it).",
    ),
    (
        "core://Container",
        "orientation: Vertical|Horizontal, padding_dp, children",
        "A plain stack.",
    ),
    (
        "core://Text",
        "value_expr, style: Title|Body|Caption|Headline|Label|Secondary|Display, format?: {date: Relative|Short, past?}, count?: {query_key, where?: [{field, equals}], zero_text?}",
        "Text; value_expr may bind $item.<field>, ${<uri>#<field>}, $form.<field>. format draws the YYYY-MM-DD dates it binds on the device (Relative: today, tomorrow, in 3 days, 2 days ago, else Nov 5; Short: Nov 5); with past, a date before today shows that text instead (\"Due $item.due\" with past \"Overdue\"). count puts the number of a query's entities matching every where (as in core://List) in place of $count (\"$count left\"); zero_text replaces a count of 0.",
    ),
    (
        "core://Button",
        "label, on_click",
        "A button running an intent.",
    ),
    (
        "core://List",
        "query_key, item_template, empty_text?, columns?, spacing_dp?, padding_h_dp?, filter?: {value, fields}, group_by?: {field, header}, where?: {field, equals}, indent?",
        "The entities of a query (a feed's or a data path's query key); item_template binds $item.<field> ($item.uri, $item.id).",
    ),
    (
        "core://Image",
        "url, shape?: Rect|Circle|Rounded, size_dp?, aspect_ratio?, content_description?, blurhash?",
        "An image (url usually a binding).",
    ),
    (
        "core://TextField",
        "field, placeholder, max_length?, lines?, on_submit?",
        "A form field ($form.<field>).",
    ),
    (
        "core://Row",
        "children, spacing_dp?, padding_h_dp?, padding_v_dp?, align?",
        "Children side by side.",
    ),
    (
        "core://Column",
        "children, spacing_dp?, padding_h_dp?, padding_v_dp?, align?",
        "Children stacked, scrolling as a page.",
    ),
    ("core://Spacer", "size_dp?", "Space (or fill)."),
    ("core://Divider", "", "A hairline."),
    (
        "core://Conditional",
        "value, equals?, content",
        "Shows content when value (a binding) is truthy, or equals `equals`.",
    ),
    (
        "core://Carousel",
        "items, item_template, aspect_ratio, aspect_field?, min_aspect?, max_aspect?",
        "Horizontally paged items (e.g. $item.media_uris).",
    ),
    (
        "core://IconButton",
        "icon, content_description, on_click, active?, active_icon?, active_tint?, size_dp?",
        "An icon button; `active` binds a boolean (liked).",
    ),
    ("core://Chip", "text, icon?, tone?", "A small label."),
    ("core://Badge", "text, icon", "An icon with a count."),
    (
        "core://Tappable",
        "on_click, content, content_description?",
        "Makes content tappable (e.g. open a screen).",
    ),
    (
        "core://Box",
        "content, background?, content_color?, border_color?, corner_dp?, padding_h_dp?, padding_v_dp?, width_dp?, height_dp?, align?, fill?",
        "A coloured, rounded box (#RRGGBB or bindings). fill: true fills the space given with content placed by align (Center: the middle); as a workspace's one slot, the whole screen with no title bar.",
    ),
    (
        "core://ChoiceChips",
        "field, options: [{label, value?}], remember?",
        "Choice chips writing $form.<field>.",
    ),
    (
        "core://MediaPicker",
        "field, max_items, label?",
        "Picks photos for a create's attachments.",
    ),
    (
        "core://Sticky",
        "content",
        "Pins content to the top while its column scrolls (not inside a List or Carousel).",
    ),
    (
        "core://Progress",
        "size_dp?",
        "An indeterminate spinner for work in progress, e.g. inside a core://Conditional on a status field",
    ),
    (
        "core://Dock",
        "field, before?: [tab], query_key?, item?: tab, max_items?, after?: [tab]; tab = {value, label, icon?, badge?, content}",
        "An app's main navigation: tabs along the bottom (a side rail on wide windows) and the selected tab's content filling the screen above. Tabs: before, one per entity of query_key drawn from item ($item bound; at most max_items), then after. $form.<field> is the selected value (the first tab until one is picked). Only as the content of a workspace's one slot. The home blueprint docks local:apps with an AppFrame each.",
    ),
    (
        "core://AppFrame",
        "app",
        "Another app of the pod drawn whole (its layout, screens and Back) in the space it is given, e.g. a dock tab's content. app is an id or a binding ($item.id).",
    ),
    (
        "core://Drawer",
        "field, before?: [page], query_key?, item?: page, after?: [page], drawer, footer?, badge?; page = {value, content}",
        "An app's main navigation as a side drawer: the selected page fills the screen and a menu button at the top left (badge: a count or dot binding) opens drawer, a panel of any layout (footer stays pinned to its bottom). Pages: before, one per entity of query_key from item ($item bound), then after; $form.<field> is the selected value. Pick a page from the panel with action.device open_app (an app) or show (id = a page value); any action in the panel closes it. Only as the content of a workspace's one slot. The home blueprint draws local:apps this way.",
    ),
    (
        "core://Switch",
        "checked, on_click, content_description?",
        "An on/off switch, on while checked (a binding, e.g. $item.enabled) is truthy; a tap dispatches on_click (e.g. action.device toggle_channel).",
    ),
];

/// The pod-only node of a custom layout (never sent to clients).
pub const REF_DOC: (&str, &str, &str) = (
    "core://Ref",
    "app, part, version?",
    "A blueprint's part (list_parts): \"layout\" (the whole blueprint, only as a layout's root; the app keeps its own id), a screen:<id> part in screens, any other part as a node. The pod expands it when serving, from the blueprint's newest version (or the pinned one), so it stays current.",
);

fn vocabulary() -> Value {
    json!({
        "features": protocol::FEATURES,
        "nodes": NODE_DOCS.iter().chain(std::iter::once(&REF_DOC)).map(|(t, f, d)| json!({ "type": t, "fields": f, "doc": d })).collect::<Vec<_>>(),
        "icons": protocol::ICONS,
        "intents": [
            { "intent": "action.navigation.open", "fields": "target_uri, presentation: Push|Modal|Replace", "doc": "Open a screen whose route matches target_uri." },
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
            { "intent": "action.device", "fields": "command, app?, id?, value?",
              "doc": "Something the device does itself. Commands: open_app (app), open_notification (id), mark_all_read, toggle_channel (app, id of a notification channel), sign_in (id of a provider; value: another server, e.g. $form?.server from a core://TextField, for a provider whose local:providers row says custom_server), sign_out, set_server (id of a server option), show (id of a core://Drawer or core://Dock page), show_versions (app: load its versions into local:versions), pin_version (id: a version of that app, shown on this device only), follow_latest, leave_pod (forget the pod on this device), refresh, open_settings and open_inbox (the native panels). Any layout may use any command." },
        ],
        "local_data": {
            "doc": "Data the device provides, with no feed: bind ${local://<entity>#<field>} anywhere and list local:apps in a core://List or core://Dock.",
            "local://connection": "online (bool), pending (count of unsent changes), label (\"Online\", \"Offline · 2 pending\"), attention (true while offline or changes wait)",
            "local://inbox": "unread (count), label (\"3 unread\")",
            "local://apps": "count (the pod's apps, home excluded)",
            "local:apps": "query of the pod's apps but home: local://apps/<id> with id, title, icon, kind, detail, unread, version (\"v12 · latest\" or \"v10 · pinned\")",
            "local:notifications": "query of the newest notifications: id, app_id, app_title, title, body, when (\"5 min\"), day (\"Today\"), unread",
            "local:channels": "query of the notification channels: app_id, app_title, id, title, description, enabled",
            "local:sign_ins": "query of the pod's sign-ins: id, title, detail (\"@dana · photos.example\")",
            "local:providers": "query of the sign-ins the pod offers that aren't signed in: id, title, custom_server (another server may be typed), server_placeholder",
            "local:servers": "query of the servers this device can use: id, title, url, selected",
            "local:versions": "query of the versions of the app show_versions named, newest first: version, label (\"v12\"), detail (its note), when, compatible, showing",
            "local://versions": "app_id, app_title, pinned (the pinned version, or \"\" while following the latest)",
            "local://device": "platform (android|desktop|simulator)",
            "local://session": "open, server, error, message: the pod sign-in",
        },
        "bindings": {
            "$item.<field>": "a field of the list or carousel item",
            "${<entity uri>#<field>}": "a field of any entity, e.g. ${core://sources/<source>#signed_in}",
            "$route.<param>": "a parameter of the open screen's route",
            "$form.<field>": "a form field's value ($form?.<field>: may be blank)",
            "$nonce": "a fresh id per action",
            "$value": "the new value in a mutation's payload",
        },
        "rules": "Custom layouts may only use their feeds' sources: data paths, actions, query keys and entity schemes from list_data_sources. No sign-in intents and no other /v1/ paths. Data the device provides (local_data) needs no feed. Read get_layout of a blueprint for complete examples.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_node_type_is_documented() {
        for t in protocol::NODE_TYPES {
            assert!(
                NODE_DOCS.iter().any(|(d, _, _)| *d == t),
                "{t} has no entry in NODE_DOCS"
            );
        }
        assert_eq!(NODE_DOCS.len(), protocol::NODE_TYPES.len());
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
