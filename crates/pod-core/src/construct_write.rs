//! Writes of the construct (decision `content-addressed-construct`): the
//! MCP layout and component tools, over elements and names.
//!
//! A write normalizes what the author wrote ([`normalize`]: the other
//! spellings `"component": "core:<name>"` / `"pod:<name>"` and `"layout":
//! <id>` become references, and every reference is sealed with where its
//! name points now), refuses a cycle, checks a layout as clients will draw
//! it, stores the element, moves the name and re-seals every name that
//! follows it, transitively.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use construct_elements::refs::{OPEN_INTENT, Ref, Site};
use construct_elements::{Objects, object_id};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use super::store as cstore;
use super::{CORE_PREFIX, ME_PREFIX, component_name, core_layout_name};
use crate::Pod;
use crate::adapter::Catalog;
pub use crate::layouts::home_feeds;

/// The objects of a store connection, with ones not stored yet on top.
pub struct ConnObjects<'a> {
    pub conn: &'a Connection,
    pub extra: RefCell<BTreeMap<String, Arc<Value>>>,
}

impl<'a> ConnObjects<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            extra: RefCell::new(BTreeMap::new()),
        }
    }
}

impl Objects for ConnObjects<'_> {
    /// A reference without a hash follows its name to what it points at
    /// now.
    fn resolve(&self, name: &str) -> Option<String> {
        cstore::name(self.conn, name).ok().flatten()
    }

    /// The pod's own theme scale.
    fn scale(&self, name: &str) -> Option<Value> {
        crate::porcelain::scale_on(self.conn).get(name).cloned()
    }

    fn object(&self, id: &str) -> Option<Arc<Value>> {
        if let Some(v) = self.extra.borrow().get(id) {
            return Some(v.clone());
        }
        // Objects are immutable (an id is their hash), so one read serves
        // every later lookup: expanding a layout looks up a component once
        // per use, and parsing it each time made large layouts slow.
        let v = cstore::object(self.conn, id)
            .ok()
            .flatten()
            .map(|(v, _)| v)?;
        self.extra.borrow_mut().insert(id.to_owned(), v.clone());
        Some(v)
    }
}

/// A refused write's answer.
fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

/// What the author wrote, with references sealed (see the module docs).
pub fn normalize(conn: &Connection, catalog: &Catalog, v: &Value) -> Result<Value, Value> {
    let mut v = v.clone();
    if v["type"].as_str() == Some("core://Workspace") {
        let slots = v["slots"].as_array().cloned().unwrap_or_default();
        let fills = crate::porcelain::fills_screen(&slots);
        if let Some(p) = crate::porcelain::page_of_with(&v, fills) {
            v = p;
        }
    }
    walk(conn, catalog, &v)
}

fn sealed_ref(conn: &Connection, r: &Ref) -> Result<Ref, Value> {
    let mut r = r.clone();
    if r.el.is_none() || !r.pin {
        let Some(h) = cstore::name(conn, &r.name).map_err(store_err)? else {
            return Err(refuse(
                "unknown_name",
                format!(
                    "nothing is named '{}' (list_layouts and list_components list the names)",
                    r.name
                ),
            ));
        };
        if r.el.is_none() || !r.pin {
            r.el = Some(h);
        }
    }
    Ok(r)
}

fn store_err(e: rusqlite::Error) -> Value {
    refuse("store_error", e.to_string())
}

/// The history entry `seq` of `name` (a pinned use's `version`).
fn at_seq(conn: &Connection, name: &str, seq: i64) -> Result<Option<String>, Value> {
    Ok(cstore::history(conn, name)
        .map_err(store_err)?
        .into_iter()
        .find(|e| e.seq == seq && !e.hash.is_empty())
        .map(|e| e.hash))
}

fn walk(conn: &Connection, catalog: &Catalog, v: &Value) -> Result<Value, Value> {
    match v {
        Value::Array(a) => a.iter().map(|c| walk(conn, catalog, c)).collect(),
        Value::Object(o) => walk_object(conn, catalog, o),
        other => Ok(other.clone()),
    }
}

fn walk_object(
    conn: &Connection,
    catalog: &Catalog,
    o: &Map<String, Value>,
) -> Result<Value, Value> {
    let ty = o.get("type").and_then(Value::as_str);
    let mut out = Map::new();
    let is_use = ty == Some(crate::components::USE_TYPE);
    let site = if is_use {
        Some(Site::Use)
    } else if ty == Some("core://Layout") {
        Some(Site::Embed)
    } else if o.get("intent").and_then(Value::as_str) == Some(OPEN_INTENT) {
        Some(Site::Link)
    } else {
        None
    };
    for (k, c) in o {
        if site == Some(Site::Use) && matches!(k.as_str(), "component" | "version") {
            continue;
        }
        if k == "ref" && site.is_some() {
            continue;
        }
        if k == "layout" && matches!(site, Some(Site::Embed | Site::Link)) {
            continue;
        }
        out.insert(k.clone(), walk(conn, catalog, c)?);
    }
    let Some(site) = site else {
        return Ok(Value::Object(out));
    };
    // The reference, from `ref` or an old spelling.
    let r = if let Some(raw) = o.get("ref") {
        let Some(r) = Ref::parse(raw) else {
            return Err(refuse(
                "invalid_ref",
                format!("\"ref\" is a name or {{el, name, pin}}: {raw}"),
            ));
        };
        Some(r)
    } else if site == Site::Use {
        let version = o.get("version").and_then(Value::as_i64);
        let c = o.get("component").and_then(Value::as_str).unwrap_or("");
        let name = component_name(c).ok_or_else(|| {
            refuse("invalid_use", format!("core://Use needs \"ref\": a name (@core/<component>, @me/<component>, @core/<layout>, @core/<layout>/<part>), not {c:?}"))
        })?;
        let el = match version {
            Some(n) => at_seq(conn, &name, n)?,
            None => None,
        };
        Some(Ref {
            name,
            pin: el.is_some(),
            el,
            as_: None,
        })
    } else {
        match o.get("layout").and_then(Value::as_str) {
            Some(l) if !l.contains('$') => {
                if cstore::name(conn, l).map_err(store_err)?.is_some() {
                    Some(Ref {
                        name: l.to_owned(),
                        el: None,
                        pin: false,
                        as_: None,
                    })
                } else if catalog.is_core_layout(l) {
                    Some(Ref {
                        name: core_layout_name(l),
                        el: None,
                        pin: false,
                        as_: Some(l.to_owned()),
                    })
                } else {
                    return Err(refuse(
                        "unknown_layout",
                        format!("no layout '{l}' (list_layouts lists them)"),
                    ));
                }
            }
            Some(l) => {
                out.insert("layout".into(), json!(l));
                None
            }
            None => None,
        }
    };
    if let Some(r) = r {
        let mut r = sealed_ref(conn, &r)?;
        if site == Site::Use {
            check_args(conn, &r, o)?;
        }
        // A core layout's whole layout keeps its key as the edge.
        if site != Site::Use && r.as_.is_none() && r.name.starts_with(CORE_PREFIX) {
            r.as_ = Some(r.name.trim_start_matches(CORE_PREFIX).to_owned());
        }
        out.insert("ref".into(), r.to_value());
    }
    if site == Site::Use {
        out.insert("type".into(), json!(crate::components::USE_TYPE));
    }
    Ok(Value::Object(out))
}

/// Element `id`'s component definition, if it is one. An id is its
/// content's hash, so a parsed definition never goes stale: it is kept
/// (a bounded few per thread), since a layout often uses one large
/// component many times and each use is checked.
fn use_def(
    conn: &Connection,
    id: &str,
) -> Result<Option<Arc<construct_elements::def::Def>>, Value> {
    type Defs = BTreeMap<String, Option<Arc<construct_elements::def::Def>>>;
    thread_local! {
        static DEFS: RefCell<Defs> = const { RefCell::new(BTreeMap::new()) };
    }
    if let Some(d) = DEFS.with(|m| m.borrow().get(id).cloned()) {
        return Ok(d);
    }
    let Some((el, _)) = cstore::object(conn, id).map_err(store_err)? else {
        return Ok(None);
    };
    let def = construct_elements::materialize::element_def(&el)
        .ok()
        .flatten()
        .map(Arc::new);
    DEFS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= 256 {
            m.clear();
        }
        m.insert(id.to_owned(), def.clone());
    });
    Ok(def)
}

/// A use's arguments against its component's parameters: none it doesn't
/// have, every required one given (their values may be bindings, so their
/// types are checked where they are drawn).
fn check_args(conn: &Connection, r: &Ref, use_: &Map<String, Value>) -> Result<(), Value> {
    let Some(id) = &r.el else {
        return Ok(());
    };
    let Some(def) = use_def(conn, id)? else {
        return Ok(());
    };
    let args = match use_.get("args") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(other) => {
            return Err(refuse(
                "invalid_use",
                format!("{}: \"args\" is an object, not {other}", r.name),
            ));
        }
    };
    let names: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
    if let Some(k) = args.keys().find(|k| !names.contains(&k.as_str())) {
        return Err(refuse(
            "invalid_use",
            format!(
                "{}: no parameter '{k}' (parameters: {})",
                r.name,
                names.join(", ")
            ),
        ));
    }
    if let Some(p) = def
        .params
        .iter()
        .find(|p| p.required && args.get(&p.name).is_none_or(Value::is_null))
    {
        return Err(refuse(
            "invalid_use",
            format!("{}: '{}' is required", r.name, p.name),
        ));
    }
    Ok(())
}

/// The names `v`'s unpinned references follow.
pub fn followed(v: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    construct_elements::refs::visit(&v["node"], &mut |r, _| {
        if !r.pin {
            out.insert(r.name.clone());
        }
    });
    if let Some(ex) = v.get("examples") {
        construct_elements::refs::visit(ex, &mut |r, _| {
            if !r.pin {
                out.insert(r.name.clone());
            }
        });
    }
    out.into_iter().collect()
}

/// Whether element `v` (to be `name`) reaches `name` again.
fn closes_cycle(conn: &Connection, name: &str, v: &Value) -> rusqlite::Result<bool> {
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut seen = BTreeSet::new();
    let push = |v: &Value, q: &mut VecDeque<String>| {
        construct_elements::refs::visit(&v["node"], &mut |r, _| {
            if let Some(e) = &r.el {
                q.push_back(e.clone());
            }
            q.push_back(format!("name:{}", r.name));
        });
    };
    push(v, &mut queue);
    while let Some(x) = queue.pop_front() {
        if !seen.insert(x.clone()) {
            continue;
        }
        if let Some(n) = x.strip_prefix("name:") {
            if n == name {
                return Ok(true);
            }
            continue;
        }
        if let Some((el, _)) = cstore::object(conn, &x)? {
            push(&el, &mut queue);
        }
    }
    Ok(false)
}

/// Store `el` as `name`'s element and re-seal what follows it. Answers each
/// name moved and its new `seq`. In a transaction.
pub fn put(
    conn: &Connection,
    catalog: &Catalog,
    name: &str,
    mut el: Value,
    layout: bool,
    author: &str,
    note: &str,
) -> Result<Vec<(String, i64)>, Value> {
    if closes_cycle(conn, name, &el).map_err(store_err)? {
        return Err(refuse(
            "cycle",
            format!(
                "this would make '{name}' reach itself: elements only go forward (a link back is action.navigation.pop_to)"
            ),
        ));
    }
    check_bindings(conn, name, &el)?;
    let feeds = if name == crate::HOME_APP {
        serde_json::from_value(el["bind"]["feeds"].clone()).unwrap_or_default()
    } else {
        home_feeds(conn)
    };
    let objects = ConnObjects::new(conn);
    let probe = object_id(&el);
    objects
        .extra
        .borrow_mut()
        .insert(probe.clone(), Arc::new(el.clone()));
    let (needs, features) = super::core::derive(catalog, &feeds, &objects, &probe, layout);
    // An author may add to the needs, never take away.
    el["needs"] = union(&el["needs"], &needs);
    let id = object_id(&el);
    cstore::put_object(conn, &id, &el, &features).map_err(store_err)?;
    let mut moved = Vec::new();
    if let Some(seq) = cstore::move_name(conn, name, &id, author, Some(note)).map_err(store_err)? {
        moved.push((name.to_owned(), seq));
    }
    cstore::set_follows(conn, name, &followed(&el)).map_err(store_err)?;
    moved.extend(reseal_followers(conn, catalog, name).map_err(store_err)?);
    cstore::bump_generation(conn).map_err(store_err)?;
    cstore::collect_garbage(conn).map_err(store_err)?;
    Ok(moved)
}

/// Every edge (`core://Layout`, `action.navigation.open`) in `v`: its
/// reference and its `bind`.
fn edges_with_bind(v: &Value, out: &mut Vec<(Ref, Map<String, Value>)>) {
    match v {
        Value::Object(o) => {
            if let Some(site) = construct_elements::refs::site_of(o)
                && site != Site::Use
                && let Some(r) = Ref::parse(&o["ref"])
            {
                let bind = o
                    .get(construct_elements::refs::BIND_FIELD)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                out.push((r, bind));
            }
            o.values().for_each(|c| edges_with_bind(c, out));
        }
        Value::Array(a) => a.iter().for_each(|c| edges_with_bind(c, out)),
        _ => {}
    }
}

/// The inputs one use site leaves unbound, as a refusal.
fn unbound(from: &str, r: &Ref, bind: &Map<String, Value>, target: &Value) -> Result<(), Value> {
    let missing: Vec<String> = construct_elements::template::inputs_of(target)
        .into_iter()
        .filter(|n| !bind.contains_key(n))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let example: Map<String, Value> = missing
        .iter()
        .map(|n| {
            (
                n.clone(),
                json!("<a query key of home's feeds, or $in.<name>>"),
            )
        })
        .collect();
    Err(refuse(
        "unbound_input",
        format!(
            "'{from}' uses '{}' without binding its input(s) {}: add \"bind\": {} to that \
             core://Layout or action.navigation.open (decision content-addressed-construct)",
            r.name,
            missing.join(", "),
            Value::Object(example)
        ),
    ))
}

/// Bindings between layouts (decision `content-addressed-construct`): each
/// edge of `el` binds every input its target reads; only home binds the
/// pod's data, so another layout's `bind` values pass its own inputs on
/// (`"$in.<name>"`); and a new element for `name` reads no input that a use
/// site of it leaves unbound.
fn check_bindings(conn: &Connection, name: &str, el: &Value) -> Result<(), Value> {
    let mut edges = Vec::new();
    edges_with_bind(&el["node"], &mut edges);
    for (r, bind) in &edges {
        if name != crate::HOME_APP {
            for (k, v) in bind {
                let passes = v
                    .as_str()
                    .and_then(|s| s.strip_prefix("$in."))
                    .is_some_and(|n| {
                        !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    });
                if !passes {
                    return Err(refuse(
                        "bind_outside_home",
                        format!(
                            "'{name}' binds '{k}' of '{}' to {v}: only home binds the pod's data; \
                             another layout passes its own inputs on (\"{k}\": \"$in.{k}\") and \
                             home binds them",
                            r.name
                        ),
                    ));
                }
            }
        }
        let Some(target) = r.el.as_deref() else {
            continue;
        };
        if let Some((t, _)) = cstore::object(conn, target).map_err(store_err)? {
            unbound(name, r, bind, &t)?;
        }
    }
    if construct_elements::template::inputs_of(el).is_empty() {
        return Ok(());
    }
    for f in cstore::followers(conn, name).map_err(store_err)? {
        let Some(cur) = cstore::name(conn, &f).map_err(store_err)? else {
            continue;
        };
        let Some((fv, _)) = cstore::object(conn, &cur).map_err(store_err)? else {
            continue;
        };
        let mut theirs = Vec::new();
        edges_with_bind(&fv["node"], &mut theirs);
        for (r, bind) in theirs.iter().filter(|(r, _)| r.name == name && !r.pin) {
            unbound(&f, r, bind, el)?;
        }
    }
    Ok(())
}

fn union(given: &Value, derived: &Value) -> Value {
    let mut out = derived.clone();
    if let (Some(g), Some(o)) = (given.as_object(), out.as_object_mut()) {
        for (k, v) in g {
            let mut items: Vec<Value> = o
                .get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for x in v.as_array().into_iter().flatten() {
                if !items.contains(x) {
                    items.push(x.clone());
                }
            }
            o.insert(k.clone(), Value::Array(items));
        }
    }
    out
}

/// Re-seal every name that follows `moved` (transitively): their unpinned
/// references to a moved name take its new element.
pub fn reseal_followers(
    conn: &Connection,
    catalog: &Catalog,
    moved: &str,
) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([moved.to_owned()]);
    let mut rounds = 0;
    while let Some(n) = queue.pop_front() {
        rounds += 1;
        if rounds > 10_000 {
            break;
        }
        let Some(to) = cstore::name(conn, &n)? else {
            continue;
        };
        for f in cstore::followers(conn, &n)? {
            let Some(cur) = cstore::name(conn, &f)? else {
                continue;
            };
            let Some((el, _)) = cstore::object(conn, &cur)? else {
                continue;
            };
            let mut v = (*el).clone();
            let mut changed = false;
            let mut fix = |r: &mut Ref, _: Site| {
                if !r.pin && r.name == n && r.el.as_deref() != Some(to.as_str()) {
                    r.el = Some(to.clone());
                    changed = true;
                }
            };
            construct_elements::refs::map(&mut v["node"], &mut fix);
            if let Some(ex) = v.get_mut("examples") {
                construct_elements::refs::map(ex, &mut fix);
            }
            if !changed {
                continue;
            }
            let layout = super::is_layout_name(&f);
            let feeds = if f == crate::HOME_APP {
                serde_json::from_value(v["bind"]["feeds"].clone()).unwrap_or_default()
            } else {
                home_feeds(conn)
            };
            let objects = ConnObjects::new(conn);
            let probe = object_id(&v);
            objects
                .extra
                .borrow_mut()
                .insert(probe.clone(), Arc::new(v.clone()));
            let (needs, features) = super::core::derive(catalog, &feeds, &objects, &probe, layout);
            v["needs"] = union(&v["needs"], &needs);
            let id = object_id(&v);
            cstore::put_object(conn, &id, &v, &features)?;
            if let Some(seq) = cstore::move_name_resealed(
                conn,
                &f,
                &id,
                &format!("{} {n} moved", cstore::RESEAL_NOTE),
            )? {
                out.push((f.clone(), seq));
                queue.push_back(f);
            }
        }
    }
    Ok(out)
}

/// Announce moves to the event streams (`event: construct`).
pub fn publish(pod: &Pod, moved: &[(String, i64)]) {
    super::state(pod)
        .roots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    if !moved.is_empty() {
        let g = pod
            .store()
            .with_conn(cstore::generation)
            .unwrap_or_default();
        pod.publish_construct(g);
    }
}

/// A layout as clients draw it: what checks of the pod's data read on a
/// switched pod ([`live_layouts`]).
#[derive(Debug, Clone)]
pub struct LiveLayout {
    /// Its name (a pod layout's id, or `@core/<layout>`).
    pub name: String,
    pub title: String,
    pub icon: String,
    /// Its name's history seq (what `get_layout` calls its version).
    pub version: i64,
    /// Its element materialized: components expanded, its inputs filled in
    /// with what the first use site reaching it from home binds.
    pub node: Value,
}

/// Every layout of a switched pod as clients draw it: each one home
/// reaches (breadth first, bound as the engine binds it), then each of the
/// pod's layouts home doesn't reach, unbound. A layout bound two ways is
/// listed for each. Elements that can't be materialized are left out.
pub fn live_layouts(conn: &Connection) -> rusqlite::Result<Vec<LiveLayout>> {
    let objects = ConnObjects::new(conn);
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut queue = VecDeque::new();
    if let Some(h) = cstore::name(conn, crate::HOME_APP)? {
        queue.push_back((crate::HOME_APP.to_owned(), h, Map::new()));
    }
    // Then the layouts home doesn't reach (opened by name), unbound.
    let mut unreached = false;
    loop {
        while let Some((name, el, inputs)) = queue.pop_front() {
            if seen.len() > 10_000 {
                break;
            }
            if !seen.insert(construct_elements::instance_id(&el, &inputs)) {
                continue;
            }
            let Some(v) = objects.object(&el) else {
                continue;
            };
            let Ok(m) = construct_elements::materialize_with(&objects, &el, &inputs) else {
                continue;
            };
            for e in &m.edges {
                if let Some(t) = &e.el {
                    queue.push_back((e.name.clone(), t.clone(), e.bind.clone()));
                }
            }
            named.insert(name.clone());
            out.push(LiveLayout {
                version: cstore::last_seq(conn, &name)?,
                title: v["title"].as_str().unwrap_or("").to_owned(),
                icon: v["icon"].as_str().unwrap_or("").to_owned(),
                name,
                node: m.node,
            });
        }
        if unreached {
            break;
        }
        unreached = true;
        for (name, h) in cstore::names(conn)? {
            if super::is_layout_name(&name) && !named.contains(&name) {
                queue.push_back((name, h, Map::new()));
            }
        }
    }
    Ok(out)
}

/// A layout element as written: its node normalized, its title and icon.
fn layout_element(
    conn: &Connection,
    catalog: &Catalog,
    id: &str,
    layout: &Value,
    title: &str,
    icon: &str,
    bind: Option<Value>,
) -> Result<Value, Value> {
    let mut layout = layout.clone();
    if let Some(o) = layout.as_object_mut() {
        o.insert("id".into(), json!(id));
    }
    let node = normalize(conn, catalog, &layout)?;
    let mut el = json!({ "kind": "element", "title": title, "icon": icon, "node": node });
    if let Some(b) = bind {
        el["bind"] = b;
    }
    Ok(el)
}

/// Check a layout element as clients draw it (materialized): structure,
/// vocabulary, and only what home's feeds' sources offer.
fn check_layout(
    pod: &Pod,
    conn: &Connection,
    id: &str,
    el: &Value,
    feeds: &[(String, String)],
) -> Result<(), Value> {
    let objects = ConnObjects::new(conn);
    let probe = object_id(el);
    objects
        .extra
        .borrow_mut()
        .insert(probe.clone(), Arc::new(el.clone()));
    // Its inputs are bound at its use sites (checked there): here each
    // stands for data the device provides, so a list may read one.
    let inputs: Map<String, Value> = construct_elements::template::inputs_of(el)
        .into_iter()
        .map(|n| {
            let key = format!("{}in.{n}", crate::validate::LOCAL_QUERY_PREFIX);
            (n, json!(key))
        })
        .collect();
    let m = construct_elements::materialize_with(&objects, &probe, &inputs)
        .map_err(|e| refuse("invalid_layout", e.to_string()))?;
    if let Some(x) = m.missing.iter().next() {
        return Err(refuse(
            "invalid_layout",
            format!("it uses {x}, which isn't stored"),
        ));
    }
    let sources = crate::layouts::feed_sources(pod, feeds);
    let invalid = |e: crate::validate::LayoutError| {
        json!({ "error": "invalid_layout", "detail": e.to_string(), "at": e.pointer,
                "allowed": e.allowed })
    };
    crate::validate::custom_layout(id, &m.node, &sources).map_err(invalid)?;
    // Each source's own checks (decision `collections`): e.g. that the
    // bindings name fields its stored definitions have.
    for (name, _) in &sources {
        let Some((adapter, _)) = pod.catalog().source(name) else {
            continue;
        };
        let names: Vec<String> = feeds
            .iter()
            .filter(|(s, _)| s == name)
            .map(|(_, f)| f.clone())
            .collect();
        adapter
            .check_custom_layout(conn, &names, &m.node)
            .map_err(invalid)?;
    }
    Ok(())
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Result<&'a str, Value> {
    args[k]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| refuse("invalid_arguments", format!("\"{k}\" is required")))
}

fn note_arg(args: &Value, default: &str) -> String {
    args["note"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(default)
        .chars()
        .take(crate::layouts::MAX_NOTE_CHARS)
        .collect()
}

/// The MCP layout and component tools; `None`: not one of them.
pub async fn mcp(pod: &Pod, name: &str, args: &Value) -> Option<Result<Value, Value>> {
    match name {
        "list_layouts" => Some(run(pod, args, list_layouts).await),
        "get_layout" => Some(run(pod, args, get_layout).await),
        "create_layout" => Some(run_write(pod, args, create_layout).await),
        "update_layout" => Some(run_write(pod, args, update_layout).await),
        "validate_layout" => Some(run(pod, args, validate_layout).await),
        "restore_version" => Some(run_write(pod, args, restore_version).await),
        "rename_layout" => Some(run_write(pod, args, rename_layout).await),
        "delete_layout" => Some(run_write(pod, args, delete_layout).await),
        "list_components" => Some(run(pod, args, list_components).await),
        "find_elements" => Some(run(pod, args, find_elements).await),
        "list_parts" => Some(run(pod, args, list_parts).await),
        "get_component" => Some(run(pod, args, get_component).await),
        "define_component" => {
            Some(run_write(pod, args, |p, c, a| put_component(p, c, a, false)).await)
        }
        "update_component" => {
            Some(run_write(pod, args, |p, c, a| put_component(p, c, a, true)).await)
        }
        "copy_component" => Some(run_write(pod, args, copy_component).await),
        "restore_component" => Some(run_write(pod, args, restore_component).await),
        "delete_component" => Some(run_write(pod, args, delete_component).await),
        "expand" => Some(run(pod, args, expand).await),
        // Edit groups and checkpoints (decision `edit-groups`): a scoped
        // credential's grant holds inside them too.
        "stage_edit_group" => {
            let g = crate::grant::current();
            Some(run_write(pod, args, move |p, c, a| super::edit_groups::stage(p, c, a, g.as_ref())).await)
        }
        "apply_edit_group" => {
            let g = crate::grant::current();
            Some(run_write(pod, args, move |p, c, a| super::edit_groups::apply(p, c, a, g.as_ref())).await)
        }
        "revert_edit_group" => {
            let g = crate::grant::current();
            Some(
                run_write(pod, args, move |p, c, a| {
                    if a["since"].is_i64() {
                        super::edit_groups::revert_since(p, c, a, g.as_ref())
                    } else {
                        super::edit_groups::revert(p, c, a, g.as_ref())
                    }
                })
                .await,
            )
        }
        "list_edit_groups" => Some(
            run(pod, args, |_, c, _| {
                Ok(json!({ "groups": super::edit_groups::list(c).map_err(store_err)? }))
            })
            .await,
        ),
        "list_checkpoints" => Some(
            run(pod, args, |_, c, _| {
                Ok(json!({ "checkpoints": super::edit_groups::list_checkpoints(c).map_err(store_err)? }))
            })
            .await,
        ),
        "restore_checkpoint" => {
            let g = crate::grant::current();
            if args["dry_run"] == true {
                Some(
                    // A dry run writes nothing, so no grant is needed.
                    run(pod, args, |p, c, a| {
                        super::edit_groups::restore_checkpoint(p, c, a, None).map(|(v, _)| v)
                    })
                    .await,
                )
            } else {
                Some(
                    run_write(pod, args, move |p, c, a| {
                        super::edit_groups::restore_checkpoint(p, c, a, g.as_ref())
                    })
                    .await,
                )
            }
        }
        _ => None,
    }
}

type Tool = fn(&Pod, &Connection, &Value) -> Result<Value, Value>;

/// What a write makes a device fetch: `{objects, bytes}` for a client on a
/// manifest (the elements written and the new manifest: an element sealed
/// again because something it references moved is the same element to it),
/// and `sealed: {objects, bytes}` for a client sent home sealed (every
/// element that moved, written or sealed again, whole).
fn refetch(pod: &Pod, moved: &[(String, i64)]) -> Value {
    let names: BTreeSet<&str> = moved.iter().map(|(n, _)| n.as_str()).collect();
    let (sealed, written, bytes) = pod
        .store()
        .with_conn(|c| {
            let (mut sealed, mut written, mut bytes) = (0, 0, 0);
            for n in &names {
                let size = cstore::name_bytes(c, n)?;
                sealed += size;
                if !cstore::newest_is_reseal(c, n)? {
                    written += 1;
                    bytes += size;
                }
            }
            Ok::<_, rusqlite::Error>((sealed, written, bytes))
        })
        .unwrap_or((0, 0, 0));
    // The manifest this build's clients are sent now.
    let manifest = super::seal::manifest_for(pod, &protocol::ClientCaps::this_build())
        .ok()
        .flatten()
        .and_then(|id| pod.store().with_conn(|c| cstore::object_bytes(c, &id)).ok())
        .unwrap_or(0);
    json!({ "objects": written + 1, "bytes": bytes + manifest,
        "sealed": { "objects": names.len(), "bytes": sealed } })
}

async fn run(pod: &Pod, args: &Value, f: Tool) -> Result<Value, Value> {
    let (p, a) = (pod.clone(), args.clone());
    match tokio::task::spawn_blocking(move || p.store().with_conn(|c| f(&p, c, &a))).await {
        Ok(r) => r,
        Err(e) => Err(refuse("internal", e.to_string())),
    }
}

pub(crate) async fn run_write(
    pod: &Pod,
    args: &Value,
    f: impl FnOnce(&Pod, &Connection, &Value) -> Result<(Value, Vec<(String, i64)>), Value>
    + Send
    + 'static,
) -> Result<Value, Value> {
    crate::layouts::allow_write(pod).map_err(|(s, b)| {
        let mut b = b;
        b["status"] = json!(s.as_u16());
        b
    })?;
    let (p, a) = (pod.clone(), args.clone());
    let r = tokio::task::spawn_blocking(move || {
        let st = super::state(&p);
        let _guard = st.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = None;
        let tx = p.store().transaction(|tx| {
            let r = f(&p, tx, &a);
            let ok = r.is_ok();
            out = Some(r);
            if ok {
                Ok(())
            } else {
                Err(rusqlite::Error::QueryReturnedNoRows)
            }
        });
        match (tx, out) {
            (_, Some(Err(e))) => Err(e),
            (Ok(()), Some(Ok(r))) => Ok((r, p)),
            (Err(e), _) => Err(store_err(e)),
            (Ok(()), None) => Err(refuse("internal", "no answer")),
        }
    })
    .await
    .map_err(|e| refuse("internal", e.to_string()))?;
    let ((mut answer, moved), p) = r?;
    // What the write costs a device that had the construct before it: every
    // name that moved is a new object, fetched whole (decision
    // `element-roles-and-reuse`).
    if answer.is_object() && !moved.is_empty() {
        answer["refetch"] = refetch(&p, &moved);
    }
    // Cached objects stay valid (they are immutable); roots don't.
    publish(&p, &moved);
    // Publications kept shared follow their names (decision `sharing`).
    if p.shelf().is_some() && !moved.is_empty() {
        let q = p.clone();
        let _ = tokio::task::spawn_blocking(move || super::share::after_write(&q, &moved)).await;
    }
    Ok(answer)
}

fn list_layouts(pod: &Pod, conn: &Connection, _: &Value) -> Result<Value, Value> {
    let mut layouts = Vec::new();
    let names = cstore::names(conn).map_err(store_err)?;
    let reach = super::trace::from_home(conn).map_err(store_err)?;
    let shown_core_layouts: Vec<(&str, &str)> = pod
        .catalog()
        .core_layouts()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .map(|a| (a.id, a.title))
        .collect();
    // Core layout name -> the pod's layouts that use it.
    let mut used_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (n, h) in &names {
        if !super::is_layout_name(n) {
            continue;
        }
        let el = cstore::object(conn, h).map_err(store_err)?.map(|(v, _)| v);
        let seq = cstore::last_seq(conn, n).map_err(store_err)?;
        let mut row = json!({
            "id": n, "name": n, "element": h, "version": seq,
            "title": el.as_ref().map(|e| e["title"].clone()),
            "icon": el.as_ref().map(|e| e["icon"].clone()),
            "reached": super::trace::reached(&reach, n),
            "bytes": cstore::name_bytes(conn, n).map_err(store_err)?,
        });
        let uses = el
            .as_ref()
            .map(|e| super::trace::names_used(e))
            .unwrap_or_default();
        for u in &uses {
            used_by.entry(u.clone()).or_default().push(n.clone());
        }
        let title = el.as_ref().and_then(|e| e["title"].as_str()).unwrap_or("");
        if let Some(bp) = super::trace::copy_of(title, &uses, shown_core_layouts.iter().copied()) {
            row["copy_of"] = json!(bp);
        }
        if n == crate::HOME_APP {
            row["feeds"] = json!(home_feeds(conn));
        }
        layouts.push(row);
    }
    layouts.sort_by_key(|r| r["id"] != json!(crate::HOME_APP));
    let core_layouts: Vec<Value> = pod
        .catalog()
        .core_layouts()
        .filter(|a| !a.dev || pod.show_dev_apps())
        .map(|a| {
            let name = core_layout_name(a.id);
            let mut row = json!({ "id": a.id, "name": name, "title": a.title, "icon": a.icon,
            "feeds": a.feeds.iter().map(|(s, f)| json!([s, f])).collect::<Vec<_>>() });
            if let Some(p) = reach.get(&name) {
                row["reached"] = json!(super::trace::describe(p));
            }
            let users: Vec<&String> = used_by
                .iter()
                .filter(|(u, _)| **u == name || u.starts_with(&format!("{name}/")))
                .flat_map(|(_, l)| l)
                .collect();
            if !users.is_empty() {
                row["used_by"] = json!(users);
            }
            let copies: Vec<&Value> = layouts
                .iter()
                .filter(|l| l["copy_of"] == json!(name))
                .map(|l| &l["id"])
                .collect();
            if !copies.is_empty() {
                row["copies"] = json!(copies);
            }
            row
        })
        .collect();
    Ok(json!({
        "construct": cstore::name(conn, crate::HOME_APP).map_err(store_err)?,
        "layouts": layouts,
        "core_layouts": core_layouts,
        // The word before decision `element-roles-and-reuse`, for agents
        // whose instructions still say it; goes with the next change to
        // this answer.
        "blueprints": core_layouts,
        "note": "Layouts are names that point to elements; reference others with \"ref\": \
                 \"<name>\" (layouts by id, @core/<layout>, @core/<layout>/<part>, \
                 @core/<component>, @me/<component>). reached is how the user gets to each from \
                 home (the shortest path, each step's field, presentation and label); change what \
                 the user sees there. copy_of names the core layout a layout looks copied from: it \
                 doesn't follow that core layout, so a change to the core layout doesn't reach it.",
    }))
}

fn get_layout(pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let id = str_arg(args, "app")?;
    let name = super::seal::name_of_key(conn, id)
        .map_err(store_err)?
        .ok_or_else(|| refuse("layout_not_found", format!("no layout '{id}'")))?;
    let hist = cstore::history(conn, &name).map_err(store_err)?;
    let entry = match args["version"].as_i64() {
        Some(n) => hist.iter().find(|e| e.seq == n),
        None => hist.first(),
    }
    .filter(|e| !e.hash.is_empty())
    .ok_or_else(|| {
        refuse(
            "version_not_found",
            format!("no version of '{id}' like that"),
        )
    })?;
    let (el, _) = cstore::object(conn, &entry.hash)
        .map_err(store_err)?
        .ok_or_else(|| refuse("not_found", "its element is gone"))?;
    let mut out = json!({
        "id": id, "name": name, "element": entry.hash, "version": entry.seq,
        "title": el["title"], "icon": el["icon"], "layout": el["node"], "needs": el["needs"],
        "history": hist.iter().take(20).map(|e| json!({
            "version": e.seq, "element": e.hash, "author": e.author, "note": e.note,
            "created_at": crate::layouts::rfc3339(e.at) })).collect::<Vec<_>>(),
    });
    if name == crate::HOME_APP {
        out["feeds"] = json!(home_feeds(conn));
    }
    let reach = super::trace::from_home(conn).map_err(store_err)?;
    out["reached"] = super::trace::reached(&reach, &name);
    let uses = super::trace::names_used(&el);
    let title = el["title"].as_str().unwrap_or("");
    if let Some(bp) = super::trace::copy_of(
        title,
        &uses,
        pod.catalog().core_layouts().map(|a| (a.id, a.title)),
    ) {
        out["copy_of"] = json!(bp);
    }
    if args["expanded"].as_bool() == Some(true) {
        let objects = ConnObjects::new(conn);
        if let Ok(m) = construct_elements::materialize(&objects, &entry.hash) {
            out["expanded"] = m.node;
        }
    }
    let _ = pod;
    Ok(out)
}

/// Home's feeds with `more` added (at most [`crate::layouts::MAX_HOME_FEEDS`]).
fn grown_feeds(
    pod: &Pod,
    conn: &Connection,
    more: &Value,
) -> Result<Option<Vec<(String, String)>>, Value> {
    if more.is_null() {
        return Ok(None);
    }
    let asked = crate::layouts::check_feeds(pod, more, crate::layouts::MAX_FEEDS)?;
    let mut feeds = home_feeds(conn);
    let before = feeds.len();
    for f in asked {
        if !feeds.contains(&f) {
            feeds.push(f);
        }
    }
    if feeds.len() > crate::layouts::MAX_HOME_FEEDS {
        return Err(refuse(
            "too_many_feeds",
            format!(
                "home binds at most {} feeds",
                crate::layouts::MAX_HOME_FEEDS
            ),
        ));
    }
    Ok((feeds.len() != before).then_some(feeds))
}

/// Drop home's feeds of sources this build doesn't have (decision
/// `remove-recipes-source`), and feeds their source no longer has
/// ([`crate::adapter::Source::has_feed`]: a deleted collection, issue
/// #358): such a feed would otherwise stay in home's `bind` for good,
/// since the tools only grow it, and refuse every layout write. Answers the
/// feeds dropped. Blocking; run when the pod opens, on a switched pod.
pub fn drop_unknown_feeds(
    store: &crate::store::Store,
    catalog: &Catalog,
) -> Result<Vec<(String, String)>, String> {
    let mut refused = None;
    let dropped = store
        .transaction(|tx| {
            let feeds = home_feeds(tx);
            let (keep, gone): (Vec<_>, Vec<_>) = feeds
                .into_iter()
                .partition(|(s, f)| catalog.source(s).is_some_and(|(a, _)| a.has_feed(tx, f)));
            if gone.is_empty() {
                return Ok(gone);
            }
            let Some(h) = cstore::name(tx, crate::HOME_APP)? else {
                return Ok(Vec::new());
            };
            let Some((el, _)) = cstore::object(tx, &h)? else {
                return Ok(Vec::new());
            };
            let mut el = (*el).clone();
            el["bind"]["feeds"] = json!(keep);
            let note = format!(
                "Home no longer binds {} (no such source or feed)",
                gone.iter()
                    .map(|(s, f)| format!("{s}/{f}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if let Err(e) = put(
                tx,
                catalog,
                crate::HOME_APP,
                el,
                true,
                crate::layouts::AUTHOR_POD,
                &note,
            ) {
                refused = Some(e);
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(gone)
        })
        .map_err(|e| match refused.take() {
            Some(r) => r.to_string(),
            None => e.to_string(),
        })?;
    Ok(dropped)
}

/// Give home these feeds (a new home element).
pub(crate) fn set_home_feeds(
    pod: &Pod,
    conn: &Connection,
    feeds: &[(String, String)],
) -> Result<Vec<(String, i64)>, Value> {
    let Some(h) = cstore::name(conn, crate::HOME_APP).map_err(store_err)? else {
        return Ok(Vec::new());
    };
    let Some((el, _)) = cstore::object(conn, &h).map_err(store_err)? else {
        return Ok(Vec::new());
    };
    let mut el = (*el).clone();
    el["bind"] = json!({ "feeds": feeds });
    put(
        conn,
        pod.catalog(),
        crate::HOME_APP,
        el,
        true,
        crate::layouts::AUTHOR_CLAUDE,
        "Home binds more feeds",
    )
}

fn create_layout(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Layout")
        .chars()
        .take(crate::layouts::MAX_TITLE_CHARS)
        .collect::<String>();
    let icon = args["icon"].as_str().unwrap_or("dashboard").to_owned();
    if !args["layout"].is_object() {
        return Err(refuse(
            "invalid_arguments",
            "\"layout\" is the layout's root node",
        ));
    }
    let count = cstore::names(conn)
        .map_err(store_err)?
        .iter()
        .filter(|(n, _)| super::is_layout_name(n))
        .count();
    if count >= pod.writes.caps.layouts {
        return Err(refuse(
            "too_many_layouts",
            format!(
                "a pod has at most {} layouts; delete one it no longer needs",
                pod.writes.caps.layouts
            ),
        ));
    }
    let icon = if crate::layouts::LAYOUT_ICONS.contains(&icon.as_str()) {
        icon
    } else {
        "dashboard".to_owned()
    };
    let mut moved = Vec::new();
    if let Some(f) = grown_feeds(pod, conn, &args["feeds"])? {
        moved.extend(set_home_feeds(pod, conn, &f)?);
    }
    let id = crate::layouts::new_layout_id();
    let el = layout_element(
        conn,
        pod.catalog(),
        &id,
        &args["layout"],
        &title,
        &icon,
        None,
    )?;
    check_layout(pod, conn, &id, &el, &home_feeds(conn))?;
    let note = note_arg(args, "Created");
    moved.extend(put(
        conn,
        pod.catalog(),
        &id,
        el,
        true,
        crate::layouts::AUTHOR_CLAUDE,
        &note,
    )?);
    let seq = cstore::last_seq(conn, &id).map_err(store_err)?;
    Ok((
        json!({ "ok": true, "id": id, "version": seq,
        "detail": "Link it from home (or a layout home reaches) with {\"ref\": \"<id>\"} on a core://Layout or action.navigation.open." }),
        moved,
    ))
}

fn current_layout(conn: &Connection, id: &str) -> Result<(String, Arc<Value>), Value> {
    if !super::is_layout_name(id) {
        return Err(refuse(
            "not_a_layout",
            format!("'{id}' isn't one of the pod's layouts (core layouts are changed by deploys)"),
        ));
    }
    let h = cstore::name(conn, id)
        .map_err(store_err)?
        .ok_or_else(|| refuse("layout_not_found", format!("no layout '{id}'")))?;
    let (el, _) = cstore::object(conn, &h)
        .map_err(store_err)?
        .ok_or_else(|| refuse("not_found", "its element is gone"))?;
    Ok((h, el))
}

/// `update_layout` on a switched pod, in the caller's transaction: also how
/// Collections' `apply_change` edits layouts along with a collection. The
/// caller announces the moves with [`publish`] after committing.
pub fn update_layout(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = str_arg(args, "app")?.to_owned();
    let (_, el) = current_layout(conn, &id)?;
    let seq = cstore::last_seq(conn, &id).map_err(store_err)?;
    if let Some(b) = args["base_version"].as_i64()
        && b != seq
    {
        return Err(json!({ "error": "stale_base", "latest_version": seq,
            "detail": "the layout changed since that version; read it again" }));
    }
    let layout = if let Some(patch) = args.get("json_patch").filter(|p| !p.is_null()) {
        let mut doc = el["node"].clone();
        crate::mcp::apply_patch(&mut doc, patch)
            .map_err(|e| json!({ "error": "invalid_patch", "detail": e, "latest_version": seq }))?;
        doc
    } else if args["layout"].is_object() {
        args["layout"].clone()
    } else {
        return Err(refuse(
            "invalid_arguments",
            "pass \"layout\" or \"json_patch\"",
        ));
    };
    let mut moved = Vec::new();
    if let Some(f) = grown_feeds(pod, conn, &args["feeds"])? {
        moved.extend(set_home_feeds(pod, conn, &f)?);
    }
    let bind = if id == crate::HOME_APP {
        Some(json!({ "feeds": home_feeds(conn) }))
    } else {
        None
    };
    let title = el["title"].as_str().unwrap_or("").to_owned();
    let icon = el["icon"].as_str().unwrap_or("dashboard").to_owned();
    let new = layout_element(conn, pod.catalog(), &id, &layout, &title, &icon, bind)?;
    check_layout(pod, conn, &id, &new, &home_feeds(conn))?;
    let note = note_arg(args, "Edited");
    moved.extend(put(
        conn,
        pod.catalog(),
        &id,
        new,
        true,
        crate::layouts::AUTHOR_CLAUDE,
        &note,
    )?);
    let seq = cstore::last_seq(conn, &id).map_err(store_err)?;
    Ok((json!({ "ok": true, "id": id, "version": seq }), moved))
}

fn validate_layout(pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let id = args["id"].as_str().unwrap_or("c-draft00000");
    let el = layout_element(conn, pod.catalog(), id, &args["layout"], "", "", None)?;
    let feeds = match grown_feeds(pod, conn, &args["feeds"])? {
        Some(f) => f,
        None => home_feeds(conn),
    };
    check_layout(pod, conn, id, &el, &feeds)?;
    // What clients would draw.
    let objects = ConnObjects::new(conn);
    let eid = object_id(&el);
    objects
        .extra
        .borrow_mut()
        .insert(eid.clone(), Arc::new(el.clone()));
    let expanded = construct_elements::materialize(&objects, &eid)
        .map(|m| m.node)
        .unwrap_or(Value::Null);
    Ok(json!({ "ok": true, "layout": el["node"], "expanded": expanded }))
}

fn restore_version(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = str_arg(args, "app")?.to_owned();
    let n = args["version"]
        .as_i64()
        .ok_or_else(|| refuse("invalid_arguments", "\"version\" is required"))?;
    let hash = at_seq(conn, &id, n)?
        .ok_or_else(|| refuse("version_not_found", format!("'{id}' has no version {n}")))?;
    let mut moved = Vec::new();
    if let Some(seq) = cstore::move_name(
        conn,
        &id,
        &hash,
        crate::layouts::AUTHOR_CLAUDE,
        Some(&format!("Restored version {n}")),
    )
    .map_err(store_err)?
    {
        moved.push((id.clone(), seq));
    }
    let (el, _) = cstore::object(conn, &hash)
        .map_err(store_err)?
        .ok_or_else(|| refuse("not_found", "that version's element is gone"))?;
    cstore::set_follows(conn, &id, &followed(&el)).map_err(store_err)?;
    moved.extend(reseal_followers(conn, pod.catalog(), &id).map_err(store_err)?);
    cstore::bump_generation(conn).map_err(store_err)?;
    let seq = cstore::last_seq(conn, &id).map_err(store_err)?;
    Ok((json!({ "ok": true, "id": id, "version": seq }), moved))
}

fn rename_layout(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = str_arg(args, "app")?.to_owned();
    let (_, el) = current_layout(conn, &id)?;
    let mut el = (*el).clone();
    if let Some(t) = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        el["title"] = json!(
            t.chars()
                .take(crate::layouts::MAX_TITLE_CHARS)
                .collect::<String>()
        );
    }
    if let Some(i) = args["icon"].as_str() {
        el["icon"] = json!(i);
    }
    let mut moved = Vec::new();
    if id == crate::HOME_APP && !args["feeds"].is_null() {
        // Home's feeds are replaced: one may go once no layout binds it.
        let feeds =
            crate::layouts::check_feeds(pod, &args["feeds"], crate::layouts::MAX_HOME_FEEDS)?;
        let dropped: Vec<(String, String)> = home_feeds(conn)
            .into_iter()
            .filter(|f| !feeds.contains(f))
            .collect();
        if !dropped.is_empty() {
            for l in crate::layouts::pod_layouts(conn).map_err(store_err)? {
                let Some(node) = crate::layouts::materialized(conn, &l.element) else {
                    continue;
                };
                if let Some((s, f)) =
                    crate::layouts::bound_feeds(pod.catalog(), &dropped, &node).first()
                {
                    return Err(
                        json!({ "error": "feed_in_use", "feed": [s, f], "layout": l.id,
                        "detail": format!("layout '{}' ({}) shows {s}/{f}", l.title, l.id) }),
                    );
                }
            }
        }
        el["bind"] = json!({ "feeds": feeds });
    } else if let Some(f) = grown_feeds(pod, conn, &args["feeds"])? {
        moved.extend(set_home_feeds(pod, conn, &f)?);
    }
    moved.extend(put(
        conn,
        pod.catalog(),
        &id,
        el,
        true,
        crate::layouts::AUTHOR_CLAUDE,
        "Renamed",
    )?);
    let seq = cstore::last_seq(conn, &id).map_err(store_err)?;
    Ok((json!({ "ok": true, "id": id, "version": seq }), moved))
}

fn delete_layout(
    _pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = str_arg(args, "app")?.to_owned();
    if id == crate::HOME_APP {
        return Err(refuse(
            "home_cannot_be_deleted",
            "home is the construct's root",
        ));
    }
    current_layout(conn, &id)?;
    let users = cstore::followers(conn, &id).map_err(store_err)?;
    if !users.is_empty() {
        return Err(json!({ "error": "in_use", "used_by": users,
            "detail": "take the links and embeds to it out of these first" }));
    }
    let mut moved = Vec::new();
    if let Some(seq) = cstore::move_name(
        conn,
        &id,
        "",
        crate::layouts::AUTHOR_CLAUDE,
        Some("Deleted"),
    )
    .map_err(store_err)?
    {
        moved.push((id.clone(), seq));
    }
    cstore::bump_generation(conn).map_err(store_err)?;
    Ok((json!({ "ok": true }), moved))
}

/// A component's name: `@me/<name>` from `name`, `pod:name` or `@me/name`.
fn me_name(raw: &str) -> Result<String, Value> {
    if raw.starts_with(CORE_PREFIX) || raw.starts_with(crate::components::CORE) {
        return Err(refuse(
            "read_only",
            "@core/ components ship with the server; copy_component makes one of your own",
        ));
    }
    let bare = raw
        .strip_prefix(ME_PREFIX)
        .or_else(|| raw.strip_prefix(crate::components::POD))
        .unwrap_or(raw);
    if !crate::components::valid_bare_name(bare) {
        return Err(refuse(
            "invalid_name",
            "a component's name is [a-z][a-z0-9_]*",
        ));
    }
    Ok(format!("{ME_PREFIX}{bare}"))
}

/// Any component's name (`@core/x`, `core:x`, `@me/x`, `pod:x`).
fn any_component_name(raw: &str) -> Result<String, Value> {
    if raw.starts_with(CORE_PREFIX) || raw.starts_with(ME_PREFIX) {
        return Ok(raw.to_owned());
    }
    component_name(raw).map_or_else(|| me_name(raw), Ok)
}

fn component_element(conn: &Connection, catalog: &Catalog, def: &Value) -> Result<Value, Value> {
    let parsed = crate::components::Def::parse(def).map_err(|e| refuse("invalid_component", e))?;
    let _ = parsed;
    let node = walk(conn, catalog, &def["template"])?;
    let examples = walk(conn, catalog, def.get("examples").unwrap_or(&json!([])))?;
    Ok(json!({
        "kind": "element",
        "doc": def["doc"],
        "params": def.get("params").cloned().unwrap_or(json!([])),
        "children": def["children"].as_bool().unwrap_or(false),
        "node": node,
        "examples": examples,
    }))
}

/// Every example of a component element expands to plumbing this build draws.
fn check_examples(conn: &Connection, el: &Value) -> Result<(), Value> {
    let objects = ConnObjects::new(conn);
    let id = object_id(el);
    objects
        .extra
        .borrow_mut()
        .insert(id.clone(), Arc::new(el.clone()));
    for (i, ex) in el["examples"].as_array().into_iter().flatten().enumerate() {
        let use_ = json!({ "kind": "element", "node": { "type": "core://Use",
            "ref": { "name": "@me/x", "el": id, "pin": true },
            "args": ex.get("args").cloned().unwrap_or(json!({})),
            "children": ex.get("children").cloned().unwrap_or(json!([])),
            "slots": ex.get("slots").cloned().unwrap_or(json!({})) } });
        let uid = object_id(&use_);
        objects
            .extra
            .borrow_mut()
            .insert(uid.clone(), Arc::new(use_));
        let m = construct_elements::materialize(&objects, &uid)
            .map_err(|e| refuse("invalid_component", format!("examples/{i}: {e}")))?;
        let node: protocol::UiNode = serde_json::from_value(m.node).map_err(|e| {
            refuse(
                "invalid_component",
                format!("examples/{i}: expands to an invalid node: {e}"),
            )
        })?;
        if let Some(t) = crate::validate::unknown_node_types(&node).first() {
            return Err(refuse(
                "invalid_component",
                format!("examples/{i}: unknown node type '{t}'"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn put_component(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    update: bool,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let name = me_name(str_arg(args, "name")?)?;
    let exists = cstore::name(conn, &name).map_err(store_err)?.is_some();
    match (update, exists) {
        (false, true) => return Err(refuse("component_exists", "update_component changes it")),
        (true, false) => return Err(refuse("component_not_found", "define_component makes it")),
        _ => {}
    }
    let seq = cstore::last_seq(conn, &name).map_err(store_err)?;
    if let Some(b) = args["base_version"].as_i64()
        && b != seq
    {
        return Err(json!({ "error": "stale_base", "latest_version": seq }));
    }
    let el = component_element(conn, pod.catalog(), &args["component"])?;
    check_examples(conn, &el)?;
    let note = note_arg(args, if update { "Edited" } else { "Defined" });
    let moved = put(
        conn,
        pod.catalog(),
        &name,
        el,
        false,
        crate::layouts::AUTHOR_CLAUDE,
        &note,
    )?;
    let seq = cstore::last_seq(conn, &name).map_err(store_err)?;
    Ok((
        json!({ "ok": true, "name": name, "version": seq, "re_sealed": moved.iter().filter(|(n, _)| *n != name).map(|(n, _)| n).collect::<Vec<_>>() }),
        moved,
    ))
}

fn copy_component(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let from = any_component_name(str_arg(args, "from").or_else(|_| str_arg(args, "component"))?)?;
    // Without a name, the copy takes the bare name of what it copies.
    let to = match args["name"].as_str().filter(|n| !n.is_empty()) {
        Some(n) => me_name(n)?,
        None => me_name(from.rsplit('/').next().unwrap_or(&from))?,
    };
    let h = cstore::name(conn, &from)
        .map_err(store_err)?
        .ok_or_else(|| refuse("component_not_found", format!("no component '{from}'")))?;
    if cstore::name(conn, &to).map_err(store_err)?.is_some() {
        return Err(refuse("component_exists", format!("'{to}' exists")));
    }
    let (el, _) = cstore::object(conn, &h)
        .map_err(store_err)?
        .ok_or_else(|| refuse("not_found", "its element is gone"))?;
    let mut el = (*el).clone();
    if let Some(o) = el.as_object_mut() {
        o.remove("needs");
    }
    let moved = put(
        conn,
        pod.catalog(),
        &to,
        el,
        false,
        crate::layouts::AUTHOR_CLAUDE,
        &format!("Copied from {from}"),
    )?;
    Ok((json!({ "ok": true, "name": to, "from": from }), moved))
}

fn restore_component(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let name = me_name(str_arg(args, "name")?)?;
    let mut a = args.clone();
    a["app"] = json!(name);
    // Same as a layout's: move the name back.
    let n = args["version"]
        .as_i64()
        .ok_or_else(|| refuse("invalid_arguments", "\"version\" is required"))?;
    let hash = at_seq(conn, &name, n)?
        .ok_or_else(|| refuse("version_not_found", format!("'{name}' has no version {n}")))?;
    let mut moved = Vec::new();
    if let Some(seq) = cstore::move_name(
        conn,
        &name,
        &hash,
        crate::layouts::AUTHOR_CLAUDE,
        Some(&format!("Restored version {n}")),
    )
    .map_err(store_err)?
    {
        moved.push((name.clone(), seq));
    }
    moved.extend(reseal_followers(conn, pod.catalog(), &name).map_err(store_err)?);
    cstore::bump_generation(conn).map_err(store_err)?;
    Ok((json!({ "ok": true, "name": name }), moved))
}

fn delete_component(
    _pod: &Pod,
    conn: &Connection,
    args: &Value,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let name = me_name(str_arg(args, "name")?)?;
    if cstore::name(conn, &name).map_err(store_err)?.is_none() {
        return Err(refuse(
            "component_not_found",
            format!("no component '{name}'"),
        ));
    }
    let users = cstore::followers(conn, &name).map_err(store_err)?;
    if !users.is_empty() {
        return Err(json!({ "error": "in_use", "used_by": users }));
    }
    let mut moved = Vec::new();
    if let Some(seq) = cstore::move_name(
        conn,
        &name,
        "",
        crate::layouts::AUTHOR_CLAUDE,
        Some("Deleted"),
    )
    .map_err(store_err)?
    {
        moved.push((name, seq));
    }
    cstore::bump_generation(conn).map_err(store_err)?;
    Ok((json!({ "ok": true }), moved))
}

fn list_components(_pod: &Pod, conn: &Connection, _: &Value) -> Result<Value, Value> {
    let mut out = Vec::new();
    for (n, h) in cstore::names(conn).map_err(store_err)? {
        if super::is_layout_name(&n) {
            continue;
        }
        let Some((el, _)) = cstore::object(conn, &h).map_err(store_err)? else {
            continue;
        };
        // What it is used as, and whose it is.
        let role = if el.get("params").is_some() || el.get("doc").is_some() {
            "component"
        } else if el.get("title").is_some() {
            "layout"
        } else {
            "part"
        };
        let scope = if n.starts_with(super::CORE_PREFIX) {
            "core"
        } else if n.starts_with("@me/") {
            "own"
        } else {
            "adopted"
        };
        out.push(
            json!({ "name": n, "role": role, "scope": scope, "element": h, "doc": el["doc"],
            "params": el["params"], "children": el["children"], "title": el["title"] }),
        );
    }
    Ok(json!({ "components": out,
        "use": "{\"type\": \"core://Use\", \"ref\": \"<name>\", \"args\": {...}, \"children\": [...]}" }))
}

/// MCP `find_elements {query?, role?, scope?, category?, limit?}`: what
/// already exists to build with, by what it is for (decision
/// `element-roles-and-reuse`). Every layout and component a layout may
/// reference: the pod's own, the core ones that ship with the server, and
/// adopted ones. Each row is small (name, role, scope, category, the line
/// that says when to use it, its parameters by name), so an agent reads the
/// few that fit a query instead of every component's whole definition.
fn find_elements(pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let want = |k: &str| args[k].as_str().filter(|v| !v.is_empty());
    let (role, scope, category) = (want("role"), want("scope"), want("category"));
    let words: Vec<String> = want("query")
        .unwrap_or("")
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_owned)
        .collect();
    let limit = args["limit"].as_u64().map_or(12, |n| n.clamp(1, 60)) as usize;
    let mut rows: Vec<(i64, Value)> = Vec::new();
    for (n, h) in cstore::names(conn).map_err(store_err)? {
        let Some((el, _)) = cstore::object(conn, &h).map_err(store_err)? else {
            continue;
        };
        let (el_scope, bare) = if let Some(b) = n.strip_prefix(super::CORE_PREFIX) {
            ("core", b)
        } else if let Some(b) = n.strip_prefix("@me/") {
            ("own", b)
        } else if super::is_layout_name(&n) {
            ("own", n.as_str())
        } else {
            ("adopted", n.as_str())
        };
        // A part of a core layout is listed by list_parts.
        if el_scope == "core" && bare.contains('/') {
            continue;
        }
        if el_scope == "core"
            && pod
                .catalog()
                .core_layout(bare)
                .is_some_and(|b| b.dev && !pod.show_dev_apps())
        {
            continue;
        }
        let is_component = el.get("params").is_some() || el.get("doc").is_some();
        let el_role = if is_component { "component" } else { "layout" };
        let note = (el_scope == "core")
            .then(|| pod.catalog().element_note(bare))
            .flatten();
        let doc = el["doc"].as_str().unwrap_or("");
        let title = el["title"].as_str().unwrap_or("");
        let use_when = match note {
            Some(n) => n.use_when.clone(),
            // Its doc's first sentence, or a layout's title.
            None if is_component => doc.split(". ").next().unwrap_or("").to_owned(),
            None => title.to_owned(),
        };
        let el_category = note
            .map(|n| n.category.as_str())
            .or((!is_component).then_some("layout"));
        if role.is_some_and(|r| r != el_role)
            || scope.is_some_and(|s| s != el_scope)
            || category.is_some_and(|c| Some(c) != el_category)
        {
            continue;
        }
        let params: Vec<Value> = el["params"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .map(|p| {
                        let mut o = json!({ "name": p["name"], "type": p["type"] });
                        if p["required"] == json!(true) {
                            o["required"] = json!(true);
                        }
                        o
                    })
                    .collect()
            })
            .unwrap_or_default();
        let score = if words.is_empty() {
            1
        } else {
            let hits = |text: &str, weight: i64| -> i64 {
                let text = text.to_lowercase();
                words.iter().filter(|w| text.contains(w.as_str())).count() as i64 * weight
            };
            let param_names: Vec<&str> = params.iter().filter_map(|p| p["name"].as_str()).collect();
            hits(bare, 4)
                + hits(title, 3)
                + hits(&use_when, 2)
                + hits(doc, 1)
                + hits(&param_names.join(" "), 1)
        };
        if score == 0 {
            continue;
        }
        let mut row =
            json!({ "name": n, "role": el_role, "scope": el_scope, "use_when": use_when });
        if let Some(c) = el_category {
            row["category"] = json!(c);
        }
        if is_component {
            row["params"] = json!(params);
            if el["children"] == json!(true) {
                row["children"] = json!(true);
            }
        } else if !title.is_empty() {
            row["title"] = json!(title);
        }
        rows.push((score, row));
    }
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["category"].as_str().cmp(&b.1["category"].as_str()))
            .then_with(|| a.1["name"].as_str().cmp(&b.1["name"].as_str()))
    });
    let total = rows.len();
    let elements: Vec<Value> = rows.into_iter().take(limit).map(|(_, r)| r).collect();
    Ok(json!({ "elements": elements, "total": total,
        "categories": crate::adapter::ELEMENT_CATEGORIES,
        "use": "A component: {\"type\": \"core://Use\", \"ref\": \"<name>\", \"args\": {...}, \"children\": [...]}; get_component shows its template and examples. A layout: embed it with core://Layout or open it with action.navigation.open, \"ref\": \"<name>\"; a core layout is also a layout's whole root with core://Use." }))
}

/// MCP `list_parts {layout?}`: the parts of core layouts a layout can use
/// whole (`@core/<layout>/<part>`: each slot of its newest file and the
/// parts it declares), with each part's node when `with_layouts`.
fn list_parts(pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let only = args["layout"].as_str().or(args["app"].as_str());
    let with = args["with_layouts"].as_bool() == Some(true);
    let mut out = Vec::new();
    for b in pod.catalog().core_layouts() {
        if only.is_some_and(|o| o != b.id) || (b.dev && !pod.show_dev_apps()) {
            continue;
        }
        let prefix = format!("{}/", core_layout_name(b.id));
        let mut parts = Vec::new();
        for (n, h) in cstore::names(conn).map_err(store_err)? {
            let Some(part) = n.strip_prefix(&prefix) else {
                continue;
            };
            let mut row = json!({ "name": n, "part": part, "element": h });
            if with && let Some((el, _)) = cstore::object(conn, &h).map_err(store_err)? {
                row["node"] = el["node"].clone();
            }
            parts.push(row);
        }
        out.push(json!({ "layout": b.id, "name": core_layout_name(b.id), "title": b.title, "parts": parts }));
    }
    if let Some(o) = only
        && out.is_empty()
    {
        return Err(refuse(
            "unknown_core_layout",
            format!("no core layout '{o}' (list_layouts lists them)"),
        ));
    }
    Ok(json!({ "layouts": out,
        "use": "{\"type\": \"core://Use\", \"ref\": \"@core/<layout>/<part>\"} draws a part where it stands, and follows its core layout" }))
}

fn get_component(_pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let name = any_component_name(str_arg(args, "name")?)?;
    let h = cstore::name(conn, &name)
        .map_err(store_err)?
        .ok_or_else(|| refuse("component_not_found", format!("no component '{name}'")))?;
    let (el, _) = cstore::object(conn, &h)
        .map_err(store_err)?
        .ok_or_else(|| refuse("not_found", "its element is gone"))?;
    let hist: Vec<Value> = cstore::history(conn, &name)
        .map_err(store_err)?
        .iter()
        .take(20)
        .map(|e| json!({ "version": e.seq, "element": e.hash, "author": e.author, "note": e.note }))
        .collect();
    Ok(json!({ "name": name, "element": h, "component": *el, "history": hist }))
}

fn expand(pod: &Pod, conn: &Connection, args: &Value) -> Result<Value, Value> {
    let objects = ConnObjects::new(conn);
    let id = if let Some(n) = args["app"].as_str().or(args["name"].as_str()) {
        let name = super::seal::name_of_key(conn, n)
            .map_err(store_err)?
            .or_else(|| any_component_name(n).ok())
            .unwrap_or_else(|| n.to_owned());
        cstore::name(conn, &name)
            .map_err(store_err)?
            .ok_or_else(|| refuse("not_found", format!("nothing is named '{n}'")))?
    } else {
        let node = args
            .get("node")
            .or(args.get("layout"))
            .filter(|n| n.is_object())
            .ok_or_else(|| refuse("invalid_arguments", "pass \"id\", \"name\" or \"node\""))?;
        let el = json!({ "kind": "element", "node": normalize(conn, pod.catalog(), node)? });
        let id = object_id(&el);
        objects.extra.borrow_mut().insert(id.clone(), Arc::new(el));
        id
    };
    let m = construct_elements::materialize(&objects, &id)
        .map_err(|e| refuse("invalid_layout", e.to_string()))?;
    let features = serde_json::from_value::<protocol::UiNode>(m.node.clone())
        .map(|n| protocol::required_features(&n))
        .unwrap_or_default();
    Ok(json!({
        "element": id,
        "expanded": m.node,
        "features": features,
        "edges": m.edges.iter().map(|e| json!({ "key": e.key, "name": e.name, "element": e.el })).collect::<Vec<_>>(),
    }))
}

/// A draft layout (a preview's `layout`) as plumbing, on a switched pod.
pub fn draft_plumbing(pod: &Pod, node: &Value) -> Result<Value, Value> {
    pod.store().with_conn(|conn| {
        let objects = ConnObjects::new(conn);
        let el = json!({ "kind": "element", "node": normalize(conn, pod.catalog(), node)? });
        let id = object_id(&el);
        objects.extra.borrow_mut().insert(id.clone(), Arc::new(el));
        construct_elements::materialize(&objects, &id)
            .map(|m| m.node)
            .map_err(|e| refuse("invalid_layout", e.to_string()))
    })
}

/// The theme scale changed: a new construct for every client, so each
/// expands its components again (the manifest carries the scale; a sealed
/// root is asked for again by its generation).
pub async fn scale_changed(pod: &Pod) {
    let p = pod.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = p.store().with_conn(cstore::bump_generation);
        publish(&p, &[(crate::HOME_APP.to_owned(), 0)]);
    })
    .await;
}

/// Have home bind `feed` too (a kit's collection, decision
/// `element-roles-and-reuse`), as a write of the construct. Answers whether
/// it was added (false: home bound it already).
pub async fn add_home_feed(pod: &Pod, feed: (String, String)) -> Result<bool, Value> {
    let r = run_write(pod, &json!({}), move |p, c, _| {
        let mut feeds = home_feeds(c);
        if feeds.contains(&feed) {
            return Ok((json!(false), Vec::new()));
        }
        if feeds.len() >= crate::layouts::MAX_HOME_FEEDS {
            return Err(refuse(
                "too_many_feeds",
                format!(
                    "home binds at most {} feeds",
                    crate::layouts::MAX_HOME_FEEDS
                ),
            ));
        }
        feeds.push(feed);
        let moved = set_home_feeds(p, c, &feeds)?;
        Ok((json!(true), moved))
    })
    .await?;
    Ok(r == json!(true))
}

/// Stop home binding `feed` (a source dropping one it no longer has, such
/// as a deleted collection), as a write of the construct. Home's feeds are
/// read in the write's transaction, so writes at once each drop their own
/// feed (issue #358). Answers whether home bound it.
pub async fn drop_home_feed(pod: &Pod, feed: (String, String)) -> Result<bool, Value> {
    let r = run_write(pod, &json!({}), move |p, c, _| {
        let feeds = home_feeds(c);
        if !feeds.contains(&feed) {
            return Ok((json!(false), Vec::new()));
        }
        let rest: Vec<_> = feeds.into_iter().filter(|f| *f != feed).collect();
        let moved = set_home_feeds(p, c, &rest)?;
        Ok((json!(true), moved))
    })
    .await?;
    Ok(r == json!(true))
}
