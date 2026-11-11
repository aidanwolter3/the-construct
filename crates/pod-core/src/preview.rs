//! Layout previews for agents (decision `rust-render-engine`): the MCP tools
//! `render_layout_preview` and `dispatch_virtual_interaction`. A preview is
//! a layout as the owner's device gets it (`GET /v1/workspaces/<id>`, run
//! through the pod's own router for the owner), laid out and drawn by
//! `construct-render` on the CPU, returned as a WebP image with the semantic
//! tree (ids, bounds, text, what a tap does). It is kept for a while so taps,
//! scrolls and typing can change its device state and draw it again.
//!
//! A preview behaves as a device with the user's data (decision
//! `previews-read-like-a-device`): the device data a client builds
//! (`local:layouts`, `local://inbox`, …) comes from the pod, and a screen's
//! data path, a layout a tap opens and a layout's versions are read through
//! [`Reader`]. Nothing here writes: a tap that would change the pod or reach
//! the platform (a mutation, a create, a sign-in, a device command) is
//! reported as not sent.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, header},
};
use base64::Engine as _;
use construct_render::device_data;
use construct_render::interact::{Effect, Loader, Outcome, Session, Target};
use construct_render::scene::Tap;
use construct_render::{Scene, Viewport};
use protocol::{ActionIntent, DataBundle, EntityRecord, UiNode, WorkspaceSpec};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
/// Tallest a `full_height` frame grows, in points (its scale drops to stay
/// within [`MAX_PIXELS`]).
const MAX_FULL_HEIGHT: f32 = 10_000.0;
/// Entities and queries a preview may be given.
const MAX_INJECTED: usize = 200;
/// How long one read of the pod may take.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// The URI a made-up `item` is stored at.
const ITEM_URI: &str = "preview://item";

/// How much of the semantic tree an answer lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodesMode {
    /// None: the image and the summary only.
    None,
    /// Ids, types, bounds, text and a short summary of each tap.
    Text,
    /// As `Text`, with each tap's whole intent.
    Full,
}

impl NodesMode {
    fn parse(v: &Value) -> Result<Option<NodesMode>, Value> {
        match v.as_str() {
            None if v.is_null() => Ok(None),
            Some("none") => Ok(Some(NodesMode::None)),
            Some("text") => Ok(Some(NodesMode::Text)),
            Some("full") => Ok(Some(NodesMode::Full)),
            _ => Err(json!({ "error": "invalid_arguments",
                "detail": "nodes is \"none\", \"text\" (the default) or \"full\"" })),
        }
    }
}

/// How a preview is drawn, kept for its interactions.
#[derive(Debug, Clone, Copy)]
struct Options {
    nodes: NodesMode,
    /// Draw the whole height of the top screen's content.
    full_height: bool,
    /// The viewport asked for (a `full_height` frame grows from it).
    viewport: Viewport,
}

struct Preview {
    session: Session,
    options: Options,
}

type Shared = Arc<Mutex<Preview>>;

/// The pod's live previews.
#[derive(Default)]
pub struct Previews {
    list: Mutex<VecDeque<(String, Shared)>>,
}

impl Previews {
    fn insert(&self, id: String, p: Preview) -> Shared {
        let shared = Arc::new(Mutex::new(p));
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
    let nodes = json!({ "type": "string", "enum": ["none", "text", "full"],
        "description": "How much of the node tree to list: none, text (default: ids, types, bounds, text and a short summary of each tap) or full (each tap's whole intent)" });
    let full_height = json!({ "type": "boolean",
        "description": "Draw the whole scrolling content of the screen in one tall image instead of the first viewport" });
    vec![
        json!({
            "name": "render_layout_preview",
            "description": "See a layout: it is drawn as the user's device draws it, with the user's own data (feeds, the device's list of layouts, the inbox, a screen's data path), and returned as an image with every node's id, bounds (points), text and what a tap on it does. Pass id (a saved layout, default home) or layout (a draft node, drawn with the data of data_from, default home). target_uri opens the layout's screen whose route matches it (e.g. <scheme>://<things>/<id>), loading its data; params sets $route.<name>. item binds $item (an entity URI, or an object of made-up fields) and entities / queries add entities and lists to the data, to try a row template or component. form presets form values ($form.<field>), dark draws dark mode, full_height draws the whole scrolling content, scroll_to scrolls a node into view, nodes picks how much of the node tree to list. Answers a preview_id for dispatch_virtual_interaction. Nothing is saved or sent.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "A layout id: home or a c-... id from list_layouts" },
                "layout": { "type": "object", "description": "A draft layout or node (components are expanded) to draw instead of a saved one" },
                "data_from": { "type": "string", "description": "With layout: whose data to draw it with (default home)" },
                "target_uri": { "type": "string", "description": "Open the screen of the layout whose route matches this URI, as a tap with this target_uri does" },
                "params": { "type": "object", "description": "{name: value} for $route.<name> of the layout itself" },
                "item": { "description": "What $item is: an entity URI (string), or an object of fields (a made-up entity at preview://item)" },
                "entities": { "type": "object", "description": "{uri: {field: value}} entities to add to the data, or to replace there" },
                "queries": { "type": "object", "description": "{key: [uri, ...]} lists to add to the data, or to replace there" },
                "viewport": viewport,
                "full_height": full_height,
                "scroll_to": { "type": "string", "description": "A node id to scroll into view before drawing" },
                "nodes": nodes,
                "form": { "type": "object", "description": "{field: value} form values" },
                "dark": { "type": "boolean" }
            }, "required": [] }
        }),
        json!({
            "name": "dispatch_virtual_interaction",
            "description": "Act on a preview from render_layout_preview and see it again: tap a node (node_id, its text, or x/y in points), scroll (dy points, positive moves the content up), type text into a field (node_id field:<name>), or back. Taps change only what a device would change by itself (form values, chips, local entity fields, opening a layout or screen, which loads its data); what would write to the pod or reach the platform is reported in effects as not_sent. nodes and full_height change how this preview is drawn from now on.",
            "inputSchema": { "type": "object", "properties": {
                "preview_id": { "type": "string" },
                "action": { "type": "string", "enum": ["tap", "scroll", "type", "back"] },
                "node_id": { "type": "string" },
                "text": { "type": "string", "description": "tap: the text of the node to tap; type: the text to put in the field" },
                "x": { "type": "number" }, "y": { "type": "number" },
                "dy": { "type": "number" },
                "full_height": full_height,
                "nodes": nodes
            }, "required": ["preview_id", "action"] }
        }),
    ]
}

fn error(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
            "isError": true, "structuredContent": v })
}

/// Whether `id` has the shape of a layout id (`home`, a `c-…` id, a
/// blueprint's name): 1 to 64 lowercase letters, digits, `_` or `-`.
/// Nothing else goes into the internal request path, where a `/`, `?` or
/// space would reach another route or make no valid request.
fn is_layout_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Whether `path` is a data path some data source of the pod declares
/// (`SourceManifest::data_paths`, e.g. `/v1/recipes/screens/recipe?id=x`):
/// the only paths a preview's screens and `action.data.load` may read.
fn is_data_path(pod: &Pod, path: &str) -> bool {
    let plain = path.len() <= 512
        && path.starts_with("/v1/")
        && !path.contains("..")
        && !path.contains("//")
        && path
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'#' && b != b'\\');
    plain
        && pod
            .catalog()
            .sources()
            .filter(|(_, m)| !m.dev || pod.show_dev_apps())
            .flat_map(|(_, m)| m.data_paths)
            .any(|p| crate::source::path_matches(p.pattern, path))
}

/// The JSON a pod path answers its owner (`GET`, through the pod's own
/// router, as the owner's device would ask).
async fn get(pod: &Pod, headers: &HeaderMap, path: &str) -> Result<Value, Value> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .map_err(|e| json!({ "error": "not_loaded", "path": path, "detail": e.to_string() }))?;
    *req.headers_mut() = headers.clone();
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
    let resp = tokio::time::timeout(READ_TIMEOUT, crate::app(pod.clone()).oneshot(req))
        .await
        .map_err(|_| json!({ "error": "not_loaded", "path": path, "detail": "timed out" }))?
        .unwrap_or_else(|e| match e {});
    let status = resp.status();
    let bytes = tokio::time::timeout(
        READ_TIMEOUT,
        axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        return Err(
            json!({ "error": "not_loaded", "path": path, "status": status.as_u16(), "detail": detail }),
        );
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| json!({ "error": "not_loaded", "path": path, "detail": e.to_string() }))
}

/// The headers a preview's reads carry: the caller's, without its
/// credentials (reads act as the owner, [`get`]).
fn forwarded(headers: &HeaderMap) -> HeaderMap {
    let mut h = headers.clone();
    h.remove(header::AUTHORIZATION);
    h.remove(crate::CREDENTIAL_HEADER);
    h
}

/// A layout's bundle as a client gets it: plumbing, entities, queries.
async fn fetch(pod: &Pod, headers: &HeaderMap, id: &str) -> Result<WorkspaceSpec, Value> {
    if !is_layout_id(id) {
        return Err(json!({ "error": "unknown_layout", "id": id,
            "detail": "not a layout id: home, a c-... id from list_layouts, or a blueprint" }));
    }
    let v = get(pod, &forwarded(headers), &format!("/v1/workspaces/{id}"))
        .await
        .map_err(|mut e| {
            e["error"] = json!("layout_not_loaded");
            e["id"] = json!(id);
            e
        })?;
    serde_json::from_value(v)
        .map_err(|e| json!({ "error": "layout_not_loaded", "id": id, "detail": e.to_string() }))
}

/// Reads the pod for a preview's session as the owner's device would
/// ([`Loader`]): layouts a tap opens, screens' data paths, device data and
/// a layout's versions. It never writes: every write a [`Loader`] could
/// take is left to its default, which the session reports as not sent.
struct Reader {
    pod: Pod,
    headers: HeaderMap,
    rt: tokio::runtime::Handle,
    /// What `local://servers/current` names.
    base: String,
    /// Layout titles by id, for device data rows.
    titles: Mutex<HashMap<String, String>>,
    /// Versions picked in this preview (`pin_version`), by layout.
    pins: Mutex<HashMap<String, i64>>,
}

impl Reader {
    fn new(pod: &Pod, headers: &HeaderMap, rt: tokio::runtime::Handle) -> Reader {
        let base = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map_or_else(|| "this pod".to_owned(), |h| format!("https://{h}"));
        Reader {
            pod: pod.clone(),
            headers: forwarded(headers),
            rt,
            base,
            titles: Mutex::default(),
            pins: Mutex::default(),
        }
    }

    /// Blocks on [`get`]: called off the async workers (`spawn_blocking`).
    fn get(&self, path: &str) -> Result<Value, String> {
        self.rt
            .block_on(get(&self.pod, &self.headers, path))
            .map_err(|e| {
                let why = e["detail"]["detail"]
                    .as_str()
                    .or(e["detail"]["error"].as_str())
                    .or(e["detail"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| e["detail"].to_string());
                match e["status"].as_u64() {
                    Some(s) => format!("{path}: HTTP {s} {why}"),
                    None => format!("{path}: {why}"),
                }
            })
    }

    /// The device's list of layouts (`local:layouts`, `local:all_layouts`,
    /// `local:apps` and their entities).
    fn layouts(&self) -> DataBundle {
        match self.get(device_data::LAYOUTS_PATH) {
            Ok(v) => {
                let mut titles = self.titles.lock().unwrap_or_else(|p| p.into_inner());
                device_data::layouts_bundle(&v, &mut titles)
            }
            Err(e) => {
                tracing::debug!(error = %e, "preview: layouts");
                DataBundle::default()
            }
        }
    }

    /// The rest of the device data for what `needs` (layouts' JSON) binds.
    fn device(&self, needs: &str) -> DataBundle {
        let titles = self.titles.lock().map(|t| t.clone()).unwrap_or_default();
        let mut d = device_data::device(&self.base, needs, &titles, |p| self.get(p));
        // A preview stands for a phone with notifications allowed.
        d.entities.insert(
            "local://device".into(),
            record(json!({ "platform": "android", "notification_permission": "granted" })),
        );
        d
    }

    fn versions(&self, app: &str) -> Result<DataBundle, String> {
        if !is_layout_id(app) {
            return Err(format!("{app:?} is not a layout id"));
        }
        let v = self.get(&device_data::versions_path(app))?;
        let pinned = self.pins.lock().ok().and_then(|p| p.get(app).copied());
        let title = self
            .titles
            .lock()
            .ok()
            .and_then(|t| t.get(app).cloned())
            .unwrap_or_else(|| app.to_owned());
        Ok(device_data::versions_bundle(
            app,
            &v,
            pinned,
            &title,
            device_data::now_secs(),
        ))
    }
}

impl Loader for Reader {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let spec = self
            .rt
            .block_on(fetch(&self.pod, &self.headers, id))
            .map_err(|e| format!("layout {id}: {}", e["detail"]))?;
        crate::validate::check_node_values(&spec.workspace)?;
        let mut data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        let needs = serde_json::to_string(&spec.workspace).unwrap_or_default();
        let device = self.device(&needs);
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        Ok((spec.workspace, data))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        if !is_data_path(&self.pod, path) {
            return Err(format!(
                "{path}: not a data path of the pod's data sources (list_data_sources lists them)"
            ));
        }
        serde_json::from_value(self.get(path)?).map_err(|e| format!("{path}: {e}"))
    }

    fn device(&self, command: &str, app: &str, id: &str) -> Option<Result<DataBundle, String>> {
        match command {
            "show_versions" => Some(self.versions(app)),
            "pin_version" => {
                let n = id.parse::<i64>().ok()?;
                if let Ok(mut p) = self.pins.lock() {
                    p.insert(app.to_owned(), n);
                }
                Some(self.versions(app))
            }
            "follow_latest" => {
                if let Ok(mut p) = self.pins.lock() {
                    p.remove(app);
                }
                Some(self.versions(app))
            }
            _ => None,
        }
    }
}

fn record(v: Value) -> EntityRecord {
    serde_json::from_value(v).unwrap_or_default()
}

/// Every feed of home's (what any layout may bind), merged into `data`.
async fn home_feed_data(pod: &Pod, headers: &HeaderMap, data: &mut DataBundle) {
    let feeds = crate::blocking(pod, |s| s.with_conn(crate::custom::home_feeds_on))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let owner = crate::mcp::owner(pod).await;
    let forwarded = forwarded(headers);
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

/// Adds what `into` lacks (what it has stays).
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

/// What the caller adds to a preview's data: `entities`, `queries` and a
/// made-up `item`; and the `$item` URI. Replaces what the data has.
#[derive(Debug, Default, PartialEq)]
struct Injected {
    data: DataBundle,
    item: Option<String>,
}

fn injected(args: &Value) -> Result<Injected, Value> {
    let bad = |detail: String| json!({ "error": "invalid_arguments", "detail": detail });
    let mut out = Injected::default();
    if let Some(m) = args["entities"].as_object() {
        if m.len() > MAX_INJECTED {
            return Err(bad(format!("at most {MAX_INJECTED} entities")));
        }
        for (uri, fields) in m {
            if !fields.is_object() {
                return Err(bad(format!("entities.{uri}: an object of fields")));
            }
            out.data
                .entities
                .insert(uri.clone(), record(fields.clone()));
        }
    } else if !args["entities"].is_null() {
        return Err(bad("entities: {uri: {field: value}}".into()));
    }
    if let Some(m) = args["queries"].as_object() {
        if m.len() > MAX_INJECTED {
            return Err(bad(format!("at most {MAX_INJECTED} queries")));
        }
        for (key, uris) in m {
            let list: Option<Vec<String>> = uris
                .as_array()
                .map(|a| a.iter().map(|u| u.as_str().map(str::to_owned)).collect())
                .and_then(|l| l);
            let Some(list) = list.filter(|l| l.len() <= 1000) else {
                return Err(bad(format!(
                    "queries.{key}: a list of at most 1000 entity URIs"
                )));
            };
            out.data.queries.insert(key.clone(), list);
        }
    } else if !args["queries"].is_null() {
        return Err(bad("queries: {key: [uri, ...]}".into()));
    }
    match &args["item"] {
        Value::Null => {}
        Value::String(uri) if !uri.is_empty() => out.item = Some(uri.clone()),
        Value::Object(_) => {
            out.data
                .entities
                .insert(ITEM_URI.into(), record(args["item"].clone()));
            out.item = Some(ITEM_URI.into());
        }
        _ => {
            return Err(bad(
                "item: an entity URI, or an object of fields (a made-up entity)".into(),
            ));
        }
    }
    Ok(out)
}

fn route_params(args: &Value) -> Result<BTreeMap<String, String>, Value> {
    match &args["params"] {
        Value::Null => Ok(BTreeMap::new()),
        Value::Object(m) => Ok(m
            .iter()
            .map(|(k, v)| (k.clone(), construct_render::bind::render(v)))
            .collect()),
        _ => Err(json!({ "error": "invalid_arguments", "detail": "params: {name: value}" })),
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
    let scale = pick("scale", 2.0, MAX_SCALE).max(0.5);
    fit(width, height, scale)
}

/// A viewport of this size, its scale lowered where the frame would have
/// more than [`MAX_PIXELS`] (a frame costs 4 bytes a pixel).
fn fit(width: f32, height: f32, scale: f32) -> Viewport {
    let mut scale = scale;
    if width * height * scale * scale > MAX_PIXELS {
        scale = (MAX_PIXELS / (width * height)).sqrt().max(0.25);
    }
    Viewport {
        width,
        height,
        scale,
    }
}

/// Grows the viewport until the top screen's scrolling content fits
/// (`full_height`), from `base`; at most [`MAX_FULL_HEIGHT`] tall.
fn grow_to_content(s: &mut Session, base: Viewport) {
    s.resize(base);
    // A few rounds: content may grow with the viewport (a `fill` child).
    for _ in 0..4 {
        let Ok(extents) = s.scroll_extents() else {
            return;
        };
        let extra = extents.values().copied().fold(0.0, f32::max);
        let height = (s.viewport.height + extra).min(MAX_FULL_HEIGHT);
        if extra < 1.0 || height <= s.viewport.height {
            return;
        }
        s.resize(fit(base.width, height, base.scale));
    }
}

/// Scrolls until node `id` is in view: true when it is.
fn scroll_into_view(s: &mut Session, id: &str) -> bool {
    for _ in 0..50 {
        let height = s.viewport.height;
        let Ok(scene) = s.scene() else {
            return false;
        };
        let found = scene
            .semantics
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.bounds);
        // The scrolling view around it, else the first one.
        let around = found.and_then(|b| {
            scene
                .semantics
                .iter()
                .rev()
                .filter(|n| n.node_type == "core://Scroll")
                .find(|n| {
                    let [_, sy, _, sh] = n.bounds;
                    b[1] >= sy - 1.0 && b[1] < sy + sh.max(1.0) * 50.0
                })
                .map(|n| (n.id.clone(), n.bounds))
        });
        let dy = match (found, &around) {
            (Some(b), Some((_, sv))) => {
                let (top, bottom) = (sv[1].max(0.0), (sv[1] + sv[3]).min(height));
                if b[1] >= top && b[1] + b[3].min(bottom - top) <= bottom + 0.5 {
                    return true;
                }
                b[1] - top
            }
            (Some(b), None) => {
                if b[1] >= 0.0 && b[1] < height {
                    return true;
                }
                b[1]
            }
            // Not laid out where it can be seen yet: further down.
            (None, _) => height * 0.8,
        };
        let target = around.map(|(i, _)| Target::Id(i));
        match s.scroll_by(target.as_ref(), dy) {
            Ok(o) if o.effects.iter().all(|e| matches!(e, Effect::Nothing)) => {
                return found.is_some_and(|b| b[1] < height);
            }
            Ok(_) => {}
            Err(_) => return found.is_some_and(|b| b[1] >= 0.0 && b[1] < height),
        }
    }
    false
}

/// A tap's intent in a few words: its name and what it is about.
fn tap_summary(intent: &Value) -> String {
    let name = intent["intent"].as_str().unwrap_or("intent");
    let about: Vec<String> = [
        "layout",
        "target_uri",
        "presentation",
        "command",
        "path",
        "endpoint",
        "entity_uri",
        "field",
        "url",
    ]
    .iter()
    .filter_map(|k| {
        let v = intent[*k].as_str()?;
        Some(format!("{k}={}", v.chars().take(80).collect::<String>()))
    })
    .collect();
    // A confirmation names what it runs.
    let then = intent["then"]
        .as_object()
        .map(|i| format!(" then {}", tap_summary(&Value::Object(i.clone()))))
        .unwrap_or_default();
    if about.is_empty() {
        format!("{name}{then}")
    } else {
        format!("{name} {}{then}", about.join(" "))
    }
}

/// The nodes for the answer, compact: short types, whole-point bounds.
fn nodes(scene: &Scene, mode: NodesMode) -> Vec<Value> {
    if mode == NodesMode::None {
        return Vec::new();
    }
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
                    Tap::Intent { intent } if mode == NodesMode::Full => intent.clone(),
                    Tap::Intent { intent } => json!(tap_summary(intent)),
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
        Effect::Device { command } => format!("ran {command} on the device"),
        Effect::LoadFailed { what, error } => format!("couldn't load {what}: {error}"),
        Effect::Back => "went back".into(),
        Effect::Scrolled { id, offset } => format!("scrolled {id} to {offset}"),
        Effect::NotSent { intent } => format!(
            "not sent (a preview doesn't write to the pod): {}",
            serde_json::to_string(intent).unwrap_or_default()
        ),
        Effect::Sent { path } => format!("sent to {path}"),
        Effect::OpenLink { url } => format!("would open {url} in the browser"),
        Effect::SignedIn { .. } => "signed in".into(),
        Effect::LeftPod => "would leave the pod on this device".into(),
        Effect::SwitchServer { url } => format!("would switch this device to the server {url}"),
        Effect::Paste { field } => format!("would paste the clipboard into {field}"),
        Effect::Copy { text } => format!("would copy {text:?}"),
        Effect::PickMedia { field, .. } => format!("would open the photo picker for {field}"),
        Effect::Nothing => "nothing happened".into(),
    }
}

/// Draws the preview and answers image + text.
fn answer(id: &str, p: &mut Preview, outcome: Option<&Outcome>, ms_load: f64) -> Value {
    let t = std::time::Instant::now();
    let Preview {
        session: s,
        options,
    } = p;
    if options.full_height {
        grow_to_content(s, options.viewport);
    } else if s.viewport != options.viewport {
        s.resize(options.viewport);
    }
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
    let mut details = json!({
        "preview_id": id,
        "state": s.state(),
        "effects": outcome.map(|o| json!(o.effects)),
        "timing_ms": { "load": (ms_load * 10.0).round() / 10.0,
                       "layout": (laid_out.as_secs_f64() * 10000.0).round() / 10.0,
                       "layout_render_encode": (took.as_secs_f64() * 10000.0).round() / 10.0 },
        "image": { "width": img.width(), "height": img.height(), "format": "webp" },
    });
    if options.nodes != NodesMode::None {
        details["nodes"] = json!(nodes(&scene, options.nodes));
    }
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
    let nodes_mode = match NodesMode::parse(&args["nodes"]) {
        Ok(m) => m.unwrap_or(NodesMode::Text),
        Err(e) => return error(e),
    };
    let injected = match injected(args) {
        Ok(i) => i,
        Err(e) => return error(e),
    };
    let route = match route_params(args) {
        Ok(r) => r,
        Err(e) => return error(e),
    };
    let target_uri = args["target_uri"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
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
    // What the renderer draws must be in the bounds a saved layout's values
    // are held to (an icon of 65535 dp would be a 4 GB mask on the server).
    if let Err(e) = crate::validate::check_node_values(&root) {
        return error(json!({ "error": "invalid_layout", "detail": e }));
    }
    let mut layouts = BTreeMap::new();
    for linked in linked_layouts(&root).into_iter().take(MAX_LINKED_LAYOUTS) {
        if linked == id {
            continue;
        }
        if let Ok(spec) = fetch(pod, headers, &linked).await
            && crate::validate::check_node_values(&spec.workspace).is_ok()
        {
            merge(&mut data, spec.entities, spec.queries);
            layouts.insert(linked, spec.workspace);
        }
    }
    let options = Options {
        nodes: nodes_mode,
        full_height: args["full_height"].as_bool().unwrap_or(false),
        viewport: viewport(args),
    };
    let reader = Reader::new(pod, headers, tokio::runtime::Handle::current());
    let pod = pod.clone();
    let args = args.clone();
    tokio::task::spawn_blocking(move || {
        // The device data, as a client builds it, for what these layouts bind.
        let mut needs = serde_json::to_string(&root).unwrap_or_default();
        for l in layouts.values() {
            needs.push_str(&serde_json::to_string(l).unwrap_or_default());
        }
        let list = reader.layouts();
        merge(&mut data, list.entities, list.queries);
        let device = reader.device(&needs);
        merge(&mut data, device.entities, device.queries);
        data.entities.extend(injected.data.entities);
        data.queries.extend(injected.data.queries);
        let ms_load = started.elapsed().as_secs_f64() * 1000.0;

        let routes = screen_routes(&root);
        let mut session = Session::new(&id, root, data, options.viewport);
        session.layouts = layouts;
        session.dark = args["dark"].as_bool().unwrap_or(false);
        session.loader = Some(Box::new(reader));
        if let Some(form) = args["form"].as_object() {
            for (k, v) in form {
                session.set_form(k, &construct_render::bind::render(v));
            }
        }
        if !route.is_empty() || injected.item.is_some() {
            session.set_root(route, injected.item);
        }
        let mut opened = None;
        if let Some(uri) = &target_uri {
            let o = match session
                .dispatch(&json!({ "intent": "action.navigation.open", "target_uri": uri }))
            {
                Ok(o) => o,
                Err(e) => return error(json!({ "error": "interaction_failed", "detail": e })),
            };
            if !o
                .effects
                .iter()
                .any(|e| matches!(e, Effect::OpenedScreen { .. }))
            {
                return error(json!({ "error": "no_screen", "target_uri": uri,
                    "detail": format!("no screen of layout {id} has a route matching it"),
                    "routes": routes }));
            }
            opened = Some(o);
        }
        if let Some(node) = args["scroll_to"].as_str()
            && !scroll_into_view(&mut session, node)
        {
            let o = opened.get_or_insert_with(|| Outcome {
                node: None,
                effects: Vec::new(),
            });
            o.effects.push(Effect::LoadFailed {
                what: format!("scroll_to {node}"),
                error: "no node with that id came into view".into(),
            });
        }
        let preview_id = {
            let mut buf = [0u8; 6];
            getrandom::fill(&mut buf).expect("OS random number generator");
            format!("p-{}", crate::util::hex(&buf))
        };
        let shared = pod
            .previews
            .insert(preview_id.clone(), Preview { session, options });
        let mut p = shared.lock().unwrap_or_else(|p| p.into_inner());
        answer(&preview_id, &mut p, opened.as_ref(), ms_load)
    })
    .await
    .unwrap_or_else(|e| error(json!({ "error": "render_failed", "detail": e.to_string() })))
}

/// The routes of a page's screens.
fn screen_routes(root: &UiNode) -> Vec<String> {
    match root {
        UiNode::Page { screens, .. } => screens.iter().map(|s| s.route.clone()).collect(),
        _ => Vec::new(),
    }
}

pub(crate) async fn dispatch_virtual_interaction(pod: &Pod, args: &Value) -> Value {
    let Some(id) = args["preview_id"].as_str().map(str::to_owned) else {
        return error(
            json!({ "error": "invalid_arguments", "detail": "\"preview_id\" is required" }),
        );
    };
    let nodes_mode = match NodesMode::parse(&args["nodes"]) {
        Ok(m) => m,
        Err(e) => return error(e),
    };
    let Some(shared) = pod.previews.get(&id) else {
        return error(json!({ "error": "preview_not_found", "preview_id": id,
            "detail": "previews last for a few renders; call render_layout_preview again" }));
    };
    let args = args.clone();
    tokio::task::spawn_blocking(move || {
        let mut p = shared.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = nodes_mode {
            p.options.nodes = m;
        }
        if let Some(f) = args["full_height"].as_bool() {
            p.options.full_height = f;
        }
        let s = &mut p.session;
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
            Ok(o) => answer(&id, &mut p, Some(&o), 0.0),
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
    fn only_layout_ids_reach_the_request_path() {
        for id in ["home", "c-abcdefgh23", "shell", "a_blueprint-2"] {
            assert!(is_layout_id(id), "{id}");
        }
        let long = "a".repeat(65);
        for id in [
            "",
            "home/events",
            "home?x=1",
            "home events",
            "../v1/apps",
            "Home",
            "home#x",
            "%2e%2e",
            long.as_str(),
        ] {
            assert!(!is_layout_id(id), "{id}");
        }
    }

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
        let tall = fit(390.0, MAX_FULL_HEIGHT, 2.0);
        assert!(tall.width * tall.height * tall.scale * tall.scale <= MAX_PIXELS * 1.001);
    }

    #[test]
    fn taps_are_summed_up() {
        assert_eq!(
            tap_summary(
                &json!({ "intent": "action.navigation.open", "layout": "inbox",
                "presentation": "Sheet", "payload": { "a": 1 } })
            ),
            "action.navigation.open layout=inbox presentation=Sheet"
        );
        assert_eq!(
            tap_summary(&json!({ "intent": "action.data.load" })),
            "action.data.load"
        );
    }

    #[test]
    fn injected_data_and_item() {
        let i = injected(&json!({
            "entities": { "test://rows/x": { "title": "Salad" } },
            "queries": { "all": ["test://rows/x"] },
            "item": { "title": "Made up" }
        }))
        .unwrap();
        assert_eq!(i.item.as_deref(), Some(ITEM_URI));
        assert_eq!(i.data.entities.len(), 2);
        assert_eq!(i.data.queries["all"], vec!["test://rows/x".to_owned()]);
        let by_uri = injected(&json!({ "item": "test://rows/x" })).unwrap();
        assert_eq!(by_uri.item.as_deref(), Some("test://rows/x"));
        assert!(by_uri.data.entities.is_empty());
        for bad in [
            json!({ "item": 3 }),
            json!({ "entities": { "a://b": 1 } }),
            json!({ "queries": { "k": "a://b" } }),
            json!({ "entities": [] }),
        ] {
            assert!(injected(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn nodes_modes() {
        assert_eq!(NodesMode::parse(&Value::Null).unwrap(), None);
        assert_eq!(
            NodesMode::parse(&json!("none")).unwrap(),
            Some(NodesMode::None)
        );
        assert!(NodesMode::parse(&json!("all")).is_err());
    }
}
