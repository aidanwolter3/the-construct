//! Layout previews for agents (decision `rust-render-engine`): the MCP tools
//! `render_layout_preview` and `dispatch_virtual_interaction`. A preview is
//! a layout as a client gets it (`GET /v1/workspaces/<id>`, run through the
//! pod's own router for the owner), laid out and drawn by `construct-render`
//! on the CPU, returned as a WebP image with the semantic tree (ids, bounds,
//! text, what a tap does). It is kept for a while so taps, scrolls and
//! typing can change its device state and draw it again.
//!
//! Nothing here writes: a tap that would reach the pod or the platform (a
//! mutation, a sign-in, a device command) is reported, not sent.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, header},
};
use base64::Engine as _;
use construct_render::interact::{Effect, Outcome, Session, Target};
use construct_render::scene::Tap;
use construct_render::{Scene, Viewport};
use protocol::{ActionIntent, DataBundle, UiNode, WorkspaceSpec};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

use crate::Pod;

/// Previews a pod keeps, oldest dropped first.
const MAX_PREVIEWS: usize = 4;
/// Layouts a preview loads besides its own (inline embeds, layouts a tap opens).
const MAX_LINKED_LAYOUTS: usize = 8;
/// Semantic nodes listed per answer.
const MAX_NODES: usize = 250;
/// Largest viewport side, in points, and scale.
const MAX_SIDE: f32 = 2400.0;
const MAX_SCALE: f32 = 3.0;
/// Most pixels a frame may have (about 1440 × 3120, a large phone at 3x).
const MAX_PIXELS: f32 = 4_500_000.0;

type Shared = Arc<Mutex<Session>>;

/// The pod's live previews.
#[derive(Default)]
pub struct Previews {
    list: Mutex<VecDeque<(String, Shared)>>,
}

impl Previews {
    fn insert(&self, id: String, s: Session) -> Shared {
        let shared = Arc::new(Mutex::new(s));
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        list.push_back((id, shared.clone()));
        while list.len() > MAX_PREVIEWS {
            list.pop_front();
        }
        shared
    }

    fn get(&self, id: &str) -> Option<Shared> {
        let list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        list.iter().find(|(i, _)| i == id).map(|(_, s)| s.clone())
    }
}

pub(crate) fn tools() -> Vec<Value> {
    let viewport = json!({ "type": "object", "description": "Logical size and pixels per point (default 390 × 844 @2, a phone)",
        "properties": { "width": { "type": "number" }, "height": { "type": "number" }, "scale": { "type": "number" } } });
    vec![
        json!({
            "name": "render_layout_preview",
            "description": "See a layout: it is drawn as the user's app draws it, with the user's own data, and returned as an image with every node's id, bounds (points), text and what a tap on it does. Pass id (a saved layout, default home) or layout (a draft node, drawn with the data of data_from, default home). form presets form values ($form.<field>), dark draws dark mode. Answers a preview_id for dispatch_virtual_interaction. Nothing is saved or sent.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "A layout id: home or a c-... id from list_layouts" },
                "layout": { "type": "object", "description": "A draft layout or node (components are expanded) to draw instead of a saved one" },
                "data_from": { "type": "string", "description": "With layout: whose data to draw it with (default home)" },
                "viewport": viewport,
                "form": { "type": "object", "description": "{field: value} form values" },
                "dark": { "type": "boolean" }
            }, "required": [] }
        }),
        json!({
            "name": "dispatch_virtual_interaction",
            "description": "Act on a preview from render_layout_preview and see it again: tap a node (node_id, its text, or x/y in points), scroll (dy points, positive moves the content up), type text into a field (node_id field:<name>), or back. Taps change only what a device would change by itself (form values, chips, local entity fields, opening a layout); what would reach the pod or the platform is reported in effects as not_sent.",
            "inputSchema": { "type": "object", "properties": {
                "preview_id": { "type": "string" },
                "action": { "type": "string", "enum": ["tap", "scroll", "type", "back"] },
                "node_id": { "type": "string" },
                "text": { "type": "string", "description": "tap: the text of the node to tap; type: the text to put in the field" },
                "x": { "type": "number" }, "y": { "type": "number" },
                "dy": { "type": "number" }
            }, "required": ["preview_id", "action"] }
        }),
    ]
}

fn error(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
            "isError": true, "structuredContent": v })
}

/// A layout's bundle as a client gets it: plumbing, entities, queries.
async fn fetch(pod: &Pod, headers: &HeaderMap, id: &str) -> Result<WorkspaceSpec, Value> {
    let mut forwarded = headers.clone();
    forwarded.remove(header::AUTHORIZATION);
    forwarded.remove(crate::CREDENTIAL_HEADER);
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/workspaces/{id}"))
        .body(Body::empty())
        .expect("a valid request");
    *req.headers_mut() = forwarded;
    req.headers_mut().insert(
        protocol::FEATURES_HEADER,
        HeaderValue::from_str(&protocol::features_header()).expect("ascii"),
    );
    let owner = crate::blocking(pod, |s| s.owner_session())
        .await
        .ok()
        .flatten();
    if let Some(hash) = owner
        && let Ok(v) = HeaderValue::from_str(&pod.internal_as(&hash))
    {
        req.headers_mut().insert(crate::session::INTERNAL_HEADER, v);
    }
    let resp = crate::app(pod.clone())
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {});
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        return Err(
            json!({ "error": "layout_not_loaded", "id": id, "status": status.as_u16(), "detail": detail }),
        );
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| json!({ "error": "layout_not_loaded", "id": id, "detail": e.to_string() }))
}

/// Every feed of home's (what any layout may bind), merged into `data`.
async fn home_feed_data(pod: &Pod, headers: &HeaderMap, data: &mut DataBundle) {
    let feeds = crate::blocking(pod, |s| s.with_conn(crate::custom::home_feeds_on))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let owner = crate::mcp::owner(pod).await;
    let mut forwarded = headers.clone();
    forwarded.remove(header::AUTHORIZATION);
    forwarded.remove(crate::CREDENTIAL_HEADER);
    for (source, feed) in feeds {
        let Some((adapter, _)) = pod.catalog().source(&source) else {
            continue;
        };
        let req = crate::RequestCtx {
            headers: forwarded.clone(),
            client: protocol::ClientCaps::this_build(),
            session: owner.clone(),
        };
        if let crate::Hydration::Data(d) = adapter.feed(pod, &feed, &req).await {
            merge(data, d.entities, d.queries);
        }
    }
}

/// Layouts a tree embeds inline or opens, by id.
fn linked_layouts(root: &UiNode) -> Vec<String> {
    let mut out = Vec::new();
    for n in root.walk() {
        if let UiNode::Layout { layout, .. } = n {
            out.push(layout.clone());
        }
        for i in n.own_intents() {
            if let ActionIntent::Navigate {
                layout: Some(l), ..
            } = i
            {
                out.push(l.clone());
            }
        }
    }
    out.retain(|l| !l.contains('$'));
    out.sort();
    out.dedup();
    out
}

fn merge(
    into: &mut DataBundle,
    entities: BTreeMap<String, protocol::EntityRecord>,
    queries: BTreeMap<String, Vec<String>>,
) {
    for (k, v) in entities {
        into.entities.entry(k).or_insert(v);
    }
    for (k, v) in queries {
        into.queries.entry(k).or_insert(v);
    }
}

fn viewport(args: &Value) -> Viewport {
    let v = &args["viewport"];
    let pick = |k: &str, d: f32, max: f32| {
        v[k].as_f64()
            .map(|n| n as f32)
            .filter(|n| n.is_finite() && *n > 0.0)
            .map_or(d, |n| n.min(max))
    };
    let width = pick("width", 390.0, MAX_SIDE).max(80.0);
    let height = pick("height", 844.0, MAX_SIDE).max(80.0);
    let mut scale = pick("scale", 2.0, MAX_SCALE).max(0.5);
    // A frame costs 4 bytes a pixel: lower the scale of a very large one.
    let pixels = width * height * scale * scale;
    if pixels > MAX_PIXELS {
        scale = (MAX_PIXELS / (width * height)).sqrt().max(0.25);
    }
    Viewport {
        width,
        height,
        scale,
    }
}

/// The nodes for the answer, compact: short types, whole-point bounds.
fn nodes(scene: &Scene) -> Vec<Value> {
    scene
        .semantics
        .iter()
        .filter(|n| n.text_content.is_some() || n.interactive || n.node_type != "core://Box")
        .take(MAX_NODES)
        .map(|n| {
            let mut v = json!({
                "id": n.id,
                "type": n.node_type.trim_start_matches("core://"),
                "bounds": n.bounds.map(|b| (b * 10.0).round() / 10.0),
            });
            if let Some(t) = &n.text_content {
                v["text"] = json!(t);
            }
            if let Some(tap) = &n.tap {
                v["tap"] = match tap {
                    Tap::SetForm { field, value } => json!(format!("sets {field} = {value:?}")),
                    Tap::Focus { field } => json!(format!("focuses {field}")),
                    Tap::Intent { intent } => intent.clone(),
                };
            }
            if n.selected {
                v["selected"] = json!(true);
            }
            v
        })
        .collect()
}

fn effect_line(e: &Effect) -> String {
    match e {
        Effect::FormSet { field, value } => format!("form {field} set to {value:?}"),
        Effect::Focused { field } => format!("field {field} focused"),
        Effect::LocalSet { entity_uri, fields } => format!(
            "{entity_uri} changed on the device: {}",
            serde_json::to_string(fields).unwrap_or_default()
        ),
        Effect::Opened { layout } => format!("opened layout {layout}"),
        Effect::OpenedScreen { screen } => format!("opened screen {screen}"),
        Effect::LoadFailed { what, error } => format!("couldn't load {what}: {error}"),
        Effect::Back => "went back".into(),
        Effect::Scrolled { id, offset } => format!("scrolled {id} to {offset}"),
        Effect::NotSent { intent } => format!(
            "not sent (a preview doesn't reach the pod): {}",
            serde_json::to_string(intent).unwrap_or_default()
        ),
        Effect::Nothing => "nothing happened".into(),
    }
}

/// Draws the session and answers image + text.
fn answer(id: &str, s: &mut Session, outcome: Option<&Outcome>, ms_load: f64) -> Value {
    let t = std::time::Instant::now();
    let scene = match s.scene() {
        Ok(sc) => sc.clone(),
        Err(e) => return error(json!({ "error": "layout_failed", "detail": e.to_string() })),
    };
    let laid_out = t.elapsed();
    let img = match s.render_cpu() {
        Ok(i) => i,
        Err(e) => return error(json!({ "error": "render_failed", "detail": e.to_string() })),
    };
    let webp = construct_render::encode::webp(&img);
    let took = t.elapsed();
    let mut summary = String::new();
    if let Some(o) = outcome {
        if let Some(n) = &o.node {
            summary.push_str(&format!(
                "Tapped {}{}.\n",
                n.id,
                n.text_content
                    .as_ref()
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default()
            ));
        }
        for e in &o.effects {
            summary.push_str(&format!("Effect: {}.\n", effect_line(e)));
        }
    }
    let form: Vec<String> = s.form.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    summary.push_str(&format!(
        "Layout {} at {}x{} @{}. Form: {}.\n",
        s.current(),
        scene.width,
        scene.height,
        scene.scale,
        if form.is_empty() {
            "(empty)".into()
        } else {
            form.join(", ")
        }
    ));
    let details = json!({
        "preview_id": id,
        "state": s.state(),
        "effects": outcome.map(|o| json!(o.effects)),
        "timing_ms": { "load": (ms_load * 10.0).round() / 10.0,
                       "layout": (laid_out.as_secs_f64() * 10000.0).round() / 10.0,
                       "layout_render_encode": (took.as_secs_f64() * 10000.0).round() / 10.0 },
        "image": { "width": img.width(), "height": img.height(), "format": "webp" },
        "nodes": nodes(&scene),
    });
    json!({
        "content": [
            { "type": "image", "mimeType": "image/webp",
              "data": base64::engine::general_purpose::STANDARD.encode(&webp) },
            { "type": "text", "text": format!("{summary}{}", serde_json::to_string(&details).unwrap_or_default()) },
        ],
        "isError": false,
    })
}

pub(crate) async fn render_layout_preview(pod: &Pod, headers: &HeaderMap, args: &Value) -> Value {
    let started = std::time::Instant::now();
    let (id, root, mut data) = if args["layout"].is_object() {
        let node = args["layout"].clone();
        let p = pod.clone();
        let expanded = crate::blocking(pod, move |s| {
            Ok(
                crate::parts::expand(s, p.catalog(), &node, None, crate::parts::Mode::Strict)
                    .map(|v| crate::porcelain::to_plumbing(s, p.catalog(), &v).unwrap_or(v)),
            )
        })
        .await;
        let expanded = match expanded {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return error(json!({ "error": "invalid_layout", "detail": e.to_json() }));
            }
            Err((_, axum::Json(b))) => return error(b),
        };
        let root: UiNode = match serde_json::from_value(expanded) {
            Ok(r) => r,
            Err(e) => return error(json!({ "error": "invalid_layout", "detail": e.to_string() })),
        };
        let from = args["data_from"].as_str().unwrap_or(crate::HOME_APP);
        let mut data = match fetch(pod, headers, from).await {
            Ok(spec) => DataBundle {
                entities: spec.entities,
                queries: spec.queries,
            },
            Err(e) => return error(e),
        };
        // A draft may bind any of home's feeds, loaded or not.
        if from == crate::HOME_APP {
            home_feed_data(pod, headers, &mut data).await;
        }
        ("draft".to_owned(), root, data)
    } else {
        let id = args["id"]
            .as_str()
            .or(args["app"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(crate::HOME_APP)
            .to_owned();
        match fetch(pod, headers, &id).await {
            Ok(spec) => {
                let data = DataBundle {
                    entities: spec.entities,
                    queries: spec.queries,
                };
                (id, spec.workspace, data)
            }
            Err(e) => return error(e),
        }
    };
    let mut layouts = BTreeMap::new();
    for linked in linked_layouts(&root).into_iter().take(MAX_LINKED_LAYOUTS) {
        if linked == id {
            continue;
        }
        if let Ok(spec) = fetch(pod, headers, &linked).await {
            merge(&mut data, spec.entities, spec.queries);
            layouts.insert(linked, spec.workspace);
        }
    }
    let ms_load = started.elapsed().as_secs_f64() * 1000.0;
    let mut session = Session::new(&id, root, data, viewport(args));
    session.layouts = layouts;
    session.dark = args["dark"].as_bool().unwrap_or(false);
    if let Some(form) = args["form"].as_object() {
        for (k, v) in form {
            session.set_form(k, &construct_render::bind::render(v));
        }
    }
    let preview_id = {
        let mut buf = [0u8; 6];
        getrandom::fill(&mut buf).expect("OS random number generator");
        format!("p-{}", crate::util::hex(&buf))
    };
    let shared = pod.previews.insert(preview_id.clone(), session);
    tokio::task::spawn_blocking(move || {
        let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
        answer(&preview_id, &mut s, None, ms_load)
    })
    .await
    .unwrap_or_else(|e| error(json!({ "error": "render_failed", "detail": e.to_string() })))
}

pub(crate) async fn dispatch_virtual_interaction(pod: &Pod, args: &Value) -> Value {
    let Some(id) = args["preview_id"].as_str().map(str::to_owned) else {
        return error(
            json!({ "error": "invalid_arguments", "detail": "\"preview_id\" is required" }),
        );
    };
    let Some(shared) = pod.previews.get(&id) else {
        return error(json!({ "error": "preview_not_found", "preview_id": id,
            "detail": "previews last for a few renders; call render_layout_preview again" }));
    };
    let args = args.clone();
    tokio::task::spawn_blocking(move || {
        let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
        let target = if let Some(n) = args["node_id"].as_str() {
            Some(Target::Id(n.to_owned()))
        } else if let (Some(x), Some(y)) = (args["x"].as_f64(), args["y"].as_f64()) {
            Some(Target::Point(x as f32, y as f32))
        } else {
            args["text"].as_str().map(|t| Target::Text(t.to_owned()))
        };
        let action = args["action"].as_str().unwrap_or("tap");
        let outcome = match action {
            "tap" => match &target {
                Some(t) => s.tap(t),
                None => Err("tap needs node_id, text, or x and y".into()),
            },
            "scroll" => s.scroll_by(target.as_ref(), args["dy"].as_f64().unwrap_or(400.0) as f32),
            "type" => match (args["node_id"].as_str(), args["text"].as_str()) {
                (Some(f), Some(t)) => Ok(s.type_text(f, t)),
                _ => Err("type needs node_id (field:<name>) and text".into()),
            },
            "back" => s.dispatch(&json!({ "intent": "action.navigation.back" })),
            other => Err(format!(
                "unknown action {other:?} (tap, scroll, type, back)"
            )),
        };
        match outcome {
            Ok(o) => answer(&id, &mut s, Some(&o), 0.0),
            Err(detail) => error(json!({ "error": "interaction_failed", "detail": detail })),
        }
    })
    .await
    .unwrap_or_else(|e| error(json!({ "error": "render_failed", "detail": e.to_string() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_linked_layouts() {
        let root: UiNode = serde_json::from_value(json!({
            "type": "core://Column", "children": [
                { "type": "core://Layout", "layout": "c-abc" },
                { "type": "core://Button", "label": "Inbox", "on_click": { "intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet" } },
                { "type": "core://Layout", "layout": "$item.id" }
            ] })).unwrap();
        assert_eq!(
            linked_layouts(&root),
            vec!["c-abc".to_owned(), "inbox".to_owned()]
        );
    }

    #[test]
    fn viewport_is_clamped() {
        let v = viewport(&json!({ "viewport": { "width": 500, "height": -3, "scale": 9 } }));
        assert_eq!((v.width, v.height, v.scale), (500.0, 844.0, MAX_SCALE));
        let huge =
            viewport(&json!({ "viewport": { "width": 99999, "height": 99999, "scale": 3 } }));
        assert_eq!((huge.width, huge.height), (MAX_SIDE, MAX_SIDE));
        assert!(huge.width * huge.height * huge.scale * huge.scale <= MAX_PIXELS * 1.001);
    }
}
