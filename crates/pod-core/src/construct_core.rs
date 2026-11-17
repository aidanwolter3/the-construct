//! What ships with the server, as elements (decision
//! `content-addressed-construct`): every `@core/` name built straight from
//! this build's files, and kept pointing at them.
//!
//! - A server component file (`crates/pod-core/components/<name>/NNNN.json`,
//!   or a source's own) is `@core/<name>`: its newest file's definition.
//! - A blueprint's layout files (`crates/source-*/layouts/<id>/NNNN.json`)
//!   are `@core/<id>`, one element per file, oldest first: its history, so
//!   a client is sealed the newest file it can draw. A file's server
//!   components are expanded and its `core://Workspace` becomes the page it
//!   stands for (uses of `core:workspace` and `core:screen`).
//! - Each slot of a blueprint's newest file, and each part it declares
//!   (`"parts"`), is `@core/<id>/<part>` (`slot:<slot id>`).
//!
//! [`load`] parses and checks every file (a bad one stops the start, as
//! before); [`refresh`] moves the pod's `@core/` names to them when it
//! opens, re-sealing what follows them; [`seed_home`] gives a new pod its
//! home.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use construct_elements::refs::Ref;
use construct_elements::{Objects, object_id};
use serde_json::{Map, Value, json};

use super::store as cstore;
use super::{CORE_PREFIX, blueprint_name};
use crate::adapter::Catalog;
use crate::layouts::{AUTHOR_POD, LayoutSource};
use crate::store::Store;

/// This build's `@core/` elements.
#[derive(Debug, Clone, Default)]
pub struct Core {
    pub objects: BTreeMap<String, Arc<Value>>,
    /// Each object's features (comma-separated).
    pub features: BTreeMap<String, String>,
    /// Each name's elements, oldest first, with the note of its move: a
    /// blueprint's files; a component's or part's one element.
    pub names: BTreeMap<String, Vec<(String, String)>>,
}

impl Objects for Core {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.objects.get(id).cloned()
    }
}

impl Core {
    /// Where `name` points in this build.
    pub fn current(&self, name: &str) -> Option<&str> {
        self.names
            .get(name)
            .and_then(|v| v.last())
            .map(|(h, _)| h.as_str())
    }
}

/// An element's needs and features, from its materialization (`home_feeds`:
/// what a layout element's feeds are found among).
pub fn derive(
    catalog: &Catalog,
    home_feeds: &[(String, String)],
    objects: &dyn Objects,
    id: &str,
    layout: bool,
) -> (Value, String) {
    let Some(el) = objects.object(id) else {
        return (json!({}), String::new());
    };
    let m = construct_elements::materialize(objects, id).ok();
    let node = m.as_ref().map_or(Value::Null, |m| m.node.clone());
    let mut params: BTreeSet<String> = el["params"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect();
    let text = el["node"].to_string();
    for prefix in ["$in.", "$arg."] {
        for (i, _) in text.match_indices(prefix) {
            let rest = &text[i + prefix.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if end > 0 {
                params.insert(rest[..end].to_owned());
            }
        }
    }
    let mut data = BTreeSet::new();
    let mut endpoints = BTreeSet::new();
    collect_paths(&node, &mut data, &mut endpoints);
    let feeds = if layout {
        crate::layouts::bound_feeds(catalog, home_feeds, &node)
    } else {
        Vec::new()
    };
    let sources: BTreeSet<String> = feeds.iter().map(|(s, _)| s.clone()).collect();
    let mut jacks = BTreeSet::new();
    for s in &sources {
        if let Some((adapter, _)) = catalog.source(s) {
            for j in adapter.jacks() {
                jacks.insert(j.id.to_owned());
            }
        }
    }
    let mut features: Vec<&str> = serde_json::from_value::<protocol::UiNode>(node)
        .map(|n| protocol::required_features(&n))
        .unwrap_or_default();
    // Bindings between layouts are drawn by the engine, not the renderer: an
    // element that binds or reads them needs a client that fills them in.
    let binds = m
        .as_ref()
        .is_some_and(|m| m.edges.iter().any(|e| !e.bind.is_empty()));
    if layout && (binds || !construct_elements::template::inputs_of(&el).is_empty()) {
        features.push(protocol::feature::LAYOUT_BIND);
        features.sort_unstable();
        features.dedup();
    }
    let features = features.join(",");
    (
        json!({
            "params": params,
            "feeds": feeds,
            "sources": sources,
            "data": data,
            "endpoints": endpoints,
            "jacks": jacks,
        }),
        features,
    )
}

fn collect_paths(v: &Value, data: &mut BTreeSet<String>, endpoints: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            for (k, c) in o {
                match (k.as_str(), c) {
                    ("data" | "path", Value::String(s)) if s.starts_with('/') => {
                        data.insert(s.clone());
                    }
                    ("endpoint", Value::String(s)) if s.starts_with('/') => {
                        endpoints.insert(s.clone());
                    }
                    _ => collect_paths(c, data, endpoints),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|c| collect_paths(c, data, endpoints)),
        _ => {}
    }
}

/// Put `el` (an element) among `objects` with its needs and features: its
/// id.
pub fn finish(
    catalog: &Catalog,
    objects: &mut BTreeMap<String, Arc<Value>>,
    features: &mut BTreeMap<String, String>,
    home_feeds: &[(String, String)],
    mut el: Value,
    layout: bool,
) -> String {
    let probe = object_id(&el);
    objects.insert(probe.clone(), Arc::new(el.clone()));
    let (needs, f) = derive(catalog, home_feeds, &*objects, &probe, layout);
    objects.remove(&probe);
    el["needs"] = needs;
    let id = object_id(&el);
    objects.insert(id.clone(), Arc::new(el));
    features.insert(id.clone(), f);
    id
}

/// Parse and check this build's files: `Err` names the file and stops the
/// start.
pub fn load(catalog: &Catalog) -> Result<Core, String> {
    load_with(catalog, catalog.files())
}

/// [`load`] with these blueprint files instead of the catalog's (tests).
pub fn load_with(catalog: &Catalog, sources: &[LayoutSource]) -> Result<Core, String> {
    let mut defs: BTreeMap<String, (i64, Value, String)> = BTreeMap::new();
    for f in crate::components::core_files(catalog.component_files())? {
        let bare = f
            .name
            .strip_prefix(crate::components::CORE)
            .unwrap_or(&f.name)
            .to_owned();
        if catalog.is_blueprint(&bare) {
            return Err(format!(
                "{}: '{bare}' is a blueprint's id; name the component otherwise",
                f.path
            ));
        }
        if defs.get(&bare).is_none_or(|(n, _, _)| *n < f.number) {
            defs.insert(bare, (f.number, f.def, f.path));
        }
    }
    let mut b = Builder {
        catalog,
        defs,
        core: Core::default(),
        done: BTreeMap::new(),
        busy: BTreeSet::new(),
    };
    let names: Vec<String> = b.defs.keys().cloned().collect();
    for n in &names {
        b.component(n)?;
    }
    let mut by_blueprint: BTreeMap<&str, Vec<&LayoutSource>> = BTreeMap::new();
    for f in sources {
        if !catalog.is_blueprint(f.app_id) {
            return Err(format!(
                "{}/{}/{}.json: '{}' is not a blueprint",
                f.dir, f.app_id, f.name, f.app_id
            ));
        }
        by_blueprint.entry(f.app_id).or_default().push(f);
    }
    for info in catalog.blueprints() {
        let mut files = by_blueprint.remove(info.id).unwrap_or_default();
        files.sort_by(|a, b| a.name.cmp(b.name));
        let mut expected = None;
        for (i, src) in files.iter().enumerate() {
            let path = format!("{}/{}/{}.json", src.dir, src.app_id, src.name);
            let number = match src.name.parse::<u32>() {
                Ok(n)
                    if n >= 1
                        && src.name.len() == 4
                        && src.name.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    n
                }
                _ => {
                    return Err(format!(
                        "{path}: file names are 4-digit version numbers from 0001 (NNNN.json)"
                    ));
                }
            };
            if let Some(e) = expected
                && number != e
            {
                return Err(if number < e {
                    format!("{path} is listed twice")
                } else {
                    format!(
                        "{path}: layout files are numbered without gaps, expected {e:04}.json next"
                    )
                });
            }
            expected = Some(number + 1);
            b.blueprint_file(info, src, &path, i + 1 == files.len())?;
        }
    }
    b.link_blueprints();
    Ok(b.core)
}

/// The blueprints a node's links and embeds name (`"layout": <id>`).
fn blueprint_links(catalog: &Catalog, v: &Value, out: &mut BTreeSet<&'static str>) {
    match v {
        Value::Array(a) => a.iter().for_each(|c| blueprint_links(catalog, c, out)),
        Value::Object(o) => {
            if let Some(t) = link_target(o)
                && let Some(info) = catalog.blueprint(t)
            {
                out.insert(info.id);
            }
            o.values().for_each(|c| blueprint_links(catalog, c, out));
        }
        _ => {}
    }
}

/// What an embed (`core://Layout`) or a link (`action.navigation.open`)
/// written by key names, if it is one.
fn link_target(o: &Map<String, Value>) -> Option<&str> {
    let site = o.get("type").and_then(Value::as_str) == Some("core://Layout")
        || o.get("intent").and_then(Value::as_str) == Some(construct_elements::refs::OPEN_INTENT);
    if !site || o.contains_key("ref") {
        return None;
    }
    o.get("layout")
        .and_then(Value::as_str)
        .filter(|t| !t.contains('$'))
}

struct Builder<'a> {
    catalog: &'a Catalog,
    /// Each server component's newest file: number, definition, path.
    defs: BTreeMap<String, (i64, Value, String)>,
    core: Core,
    /// Components built: their element.
    done: BTreeMap<String, String>,
    busy: BTreeSet<String>,
}

impl Builder<'_> {
    fn finish(&mut self, el: Value) -> String {
        finish(
            self.catalog,
            &mut self.core.objects,
            &mut self.core.features,
            &[],
            el,
            false,
        )
    }

    /// Server component `bare`: its element.
    fn component(&mut self, bare: &str) -> Result<String, String> {
        if let Some(id) = self.done.get(bare) {
            return Ok(id.clone());
        }
        let Some((number, def, path)) = self.defs.get(bare).cloned() else {
            return Err(format!("no server component 'core:{bare}'"));
        };
        if !self.busy.insert(bare.to_owned()) {
            return Err(format!("{path}: it uses itself"));
        }
        let fail = |e: String| format!("{path}: {e}");
        let node = self.uses(&def["template"]).map_err(fail)?;
        let mut el = json!({
            "kind": "element",
            "doc": def["doc"],
            "params": def.get("params").cloned().unwrap_or(json!([])),
            "children": def["children"].as_bool().unwrap_or(false),
            "node": node,
        });
        if let Some(ex) = def.get("examples") {
            el["examples"] = self.uses(ex).map_err(fail)?;
        }
        let id = self.finish(el);
        self.check_examples(&id).map_err(fail)?;
        self.busy.remove(bare);
        self.core.names.insert(
            format!("{CORE_PREFIX}{bare}"),
            vec![(
                id.clone(),
                format!("Component core:{bare} file {number:04}"),
            )],
        );
        self.done.insert(bare.to_owned(), id.clone());
        Ok(id)
    }

    /// Every example of component element `id` expands to plumbing this
    /// build draws, with no removed vocabulary.
    fn check_examples(&mut self, id: &str) -> Result<(), String> {
        let Some(el) = self.core.objects.get(id).cloned() else {
            return Ok(());
        };
        for (i, ex) in el["examples"].as_array().into_iter().flatten().enumerate() {
            let use_ = json!({ "kind": "element", "node": { "type": "core://Use",
                "ref": { "name": "@core/x", "el": id, "pin": true },
                "args": ex.get("args").cloned().unwrap_or(json!({})),
                "children": ex.get("children").cloned().unwrap_or(json!([])) } });
            let uid = object_id(&use_);
            self.core.objects.insert(uid.clone(), Arc::new(use_));
            let m = construct_elements::materialize(&self.core, &uid);
            self.core.objects.remove(&uid);
            let fail = |e: String| format!("examples/{i}: {e}");
            let m = m.map_err(|e| fail(e.to_string()))?;
            let node: protocol::UiNode = serde_json::from_value(m.node)
                .map_err(|e| fail(format!("expands to an invalid node: {e}")))?;
            if let Some(t) = crate::validate::unknown_node_types(&node).first() {
                return Err(fail(format!("expands to an unknown node type '{t}'")));
            }
            if let Some(f) = protocol::required_features(&node)
                .into_iter()
                .find(|f| protocol::is_retired(f))
            {
                return Err(fail(format!(
                    "uses '{f}', which was removed from the vocabulary"
                )));
            }
        }
        Ok(())
    }

    /// Every blueprint's links and embeds of another blueprint by key
    /// (`"layout": "inbox"`) as edges to its element (`{"ref": {"name":
    /// "@core/inbox", "as": "inbox"}}`), so a device reaches it from the
    /// root. Elements only go forward: a link that would close a cycle
    /// stays as written (home's blueprint first, then the catalog's order,
    /// as the old system's switch converted them).
    fn link_blueprints(&mut self) {
        let mut order: Vec<&'static str> = Vec::new();
        if let Some(home) = self.catalog.blueprint(crate::HOME_BLUEPRINT) {
            order.push(home.id);
        }
        order.extend(self.catalog.blueprints().map(|b| b.id));
        let (mut done, mut busy) = (BTreeSet::new(), BTreeSet::new());
        for id in order {
            self.link_one(id, &mut done, &mut busy);
        }
    }

    fn link_one(
        &mut self,
        id: &'static str,
        done: &mut BTreeSet<&'static str>,
        busy: &mut BTreeSet<&'static str>,
    ) {
        if done.contains(id) || !busy.insert(id) {
            return;
        }
        let name = blueprint_name(id);
        let parts = format!("{name}/");
        let names: Vec<String> = self
            .core
            .names
            .keys()
            .filter(|n| **n == name || n.starts_with(&parts))
            .cloned()
            .collect();
        let mut targets = BTreeSet::new();
        for n in &names {
            for (h, _) in &self.core.names[n] {
                if let Some(el) = self.core.objects.get(h) {
                    blueprint_links(self.catalog, &el["node"], &mut targets);
                }
            }
        }
        for t in targets {
            self.link_one(t, done, busy);
        }
        for n in names {
            let entries = self.core.names[&n].clone();
            let mut moved = Vec::with_capacity(entries.len());
            for (h, note) in entries {
                let Some(el) = self.core.objects.get(&h).cloned() else {
                    moved.push((h, note));
                    continue;
                };
                let node = self.linked(&el["node"], done);
                if node == el["node"] {
                    moved.push((h, note));
                    continue;
                }
                let mut el = (*el).clone();
                el["node"] = node;
                moved.push((self.finish(el), note));
            }
            self.core.names.insert(n, moved);
        }
        busy.remove(id);
        done.insert(id);
    }

    /// `v` with its links and embeds of the blueprints in `done` as edges.
    fn linked(&self, v: &Value, done: &BTreeSet<&'static str>) -> Value {
        match v {
            Value::Array(a) => Value::Array(a.iter().map(|c| self.linked(c, done)).collect()),
            Value::Object(o) => {
                let target = link_target(o)
                    .and_then(|t| self.catalog.blueprint(t))
                    .filter(|b| done.contains(b.id))
                    .and_then(|b| {
                        let name = blueprint_name(b.id);
                        self.core
                            .current(&name)
                            .map(|el| (b.id, name.clone(), el.to_owned()))
                    });
                let mut out = Map::new();
                for (k, c) in o {
                    if k == "layout" && target.is_some() {
                        continue;
                    }
                    out.insert(k.clone(), self.linked(c, done));
                }
                if let Some((key, name, el)) = target {
                    out.insert(
                        "ref".into(),
                        Ref {
                            name,
                            el: Some(el),
                            pin: false,
                            as_: Some(key.to_owned()),
                        }
                        .to_value(),
                    );
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    /// `v` with every `core://Use` of a server component (`"component":
    /// "core:<name>"`) as a reference to its element.
    fn uses(&mut self, v: &Value) -> Result<Value, String> {
        match v {
            Value::Array(a) => a.iter().map(|c| self.uses(c)).collect(),
            Value::Object(o) => {
                if o.get("type").and_then(Value::as_str) == Some(crate::components::USE_TYPE)
                    && !o.contains_key("ref")
                {
                    let c = o.get("component").and_then(Value::as_str).unwrap_or("");
                    let Some(bare) = c.strip_prefix(crate::components::CORE) else {
                        return Err(format!(
                            "core://Use of {c:?}: files use server components (core:<name>)"
                        ));
                    };
                    if o.contains_key("version") {
                        return Err(format!(
                            "core://Use of {c:?}: files use a component's newest file (no \"version\")"
                        ));
                    }
                    let el = self.component(bare)?;
                    let mut out = Map::new();
                    for (k, x) in o {
                        if k != "component" {
                            out.insert(k.clone(), self.uses(x)?);
                        }
                    }
                    out.insert(
                        "ref".into(),
                        Ref {
                            name: format!("{CORE_PREFIX}{bare}"),
                            el: Some(el),
                            pin: false,
                            as_: None,
                        }
                        .to_value(),
                    );
                    return Ok(Value::Object(out));
                }
                let mut out = Map::new();
                for (k, x) in o {
                    out.insert(k.clone(), self.uses(x)?);
                }
                Ok(Value::Object(out))
            }
            other => Ok(other.clone()),
        }
    }

    /// `v` with its server components expanded (plumbing).
    fn expand(&mut self, v: &Value) -> Result<Value, String> {
        let with_refs = self.uses(v)?;
        let el = json!({ "kind": "element", "node": with_refs });
        let id = object_id(&el);
        self.core.objects.insert(id.clone(), Arc::new(el));
        let m = construct_elements::materialize(&self.core, &id);
        self.core.objects.remove(&id);
        let m = m.map_err(|e| e.to_string())?;
        if let Some(x) = m.missing.iter().next() {
            return Err(format!("it uses {x}, which isn't there"));
        }
        Ok(m.node)
    }

    /// One blueprint file: checked, and its element pushed on the
    /// blueprint's name (with its parts, for the newest file).
    fn blueprint_file(
        &mut self,
        info: &crate::BlueprintInfo,
        src: &LayoutSource,
        path: &str,
        newest: bool,
    ) -> Result<(), String> {
        let fail = |e: String| format!("{path}: {e}");
        let doc: Value =
            serde_json::from_str(src.text).map_err(|e| fail(format!("not valid JSON: {e}")))?;
        let Value::Object(obj) = &doc else {
            return Err(fail(r#"expected {"note": "...", "layout": {...}}"#.into()));
        };
        if let Some(k) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "note" | "schema" | "layout" | "parts"))
        {
            return Err(fail(format!(
                "unknown key '{k}' (only \"note\", \"layout\" and \"parts\")"
            )));
        }
        let note = obj
            .get("note")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| fail("\"note\" must be a non-empty string".into()))?
            .to_owned();
        let written = obj
            .get("layout")
            .ok_or_else(|| fail("\"layout\" is missing".into()))?;
        let expanded = self
            .expand(written)
            .map_err(|e| fail(format!("invalid layout: {e}")))?;
        let parsed = crate::validate::validate_structure(src.app_id, &expanded)
            .map_err(|e| fail(format!("invalid layout: {e}")))?;
        // An unknown intent tag would deserialize as `ActionIntent::Unknown`
        // and ship as a dead action.
        if crate::validate::intents(&parsed)
            .iter()
            .any(|i| matches!(i, protocol::ActionIntent::Unknown))
        {
            let mut found = Vec::new();
            crate::layouts::unknown_intent_tags(written, "/layout", &mut found);
            let what = match found.first() {
                Some((at, tag)) => format!("unknown action intent '{tag}' at {at}"),
                None => "unknown action intent".to_owned(),
            };
            return Err(fail(format!(
                "invalid layout: {what} (a typo? this build does not know it)"
            )));
        }
        if let Some(t) = crate::validate::unknown_node_types(&parsed).first() {
            return Err(fail(format!(
                "invalid layout: unknown node type '{t}' (a typo? this build does not know it)"
            )));
        }
        let sources = self.catalog.blueprint_sources(src.app_id);
        crate::source::check_routes(&parsed, &sources)
            .map_err(|e| fail(format!("invalid layout: {e}")))?;
        let mut parts: Vec<(String, Value)> = Vec::new();
        if let Some(p) = obj.get("parts") {
            let Value::Object(map) = p else {
                return Err(fail("\"parts\" must be an object of named nodes".into()));
            };
            for (name, v) in map {
                if name.is_empty()
                    || name.contains(':')
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    return Err(fail(format!("part '{name}' must be [a-z0-9_]+")));
                }
                let v = self
                    .expand(v)
                    .map_err(|e| fail(format!("part '{name}': {e}")))?;
                let node: protocol::UiNode = serde_json::from_value(v)
                    .map_err(|e| fail(format!("part '{name}' is not a node: {e}")))?;
                if let Some(t) = crate::validate::unknown_node_types(&node).first() {
                    return Err(fail(format!("part '{name}': unknown node type '{t}'")));
                }
                crate::source::check_routes(&node, &sources)
                    .map_err(|e| fail(format!("part '{name}': {e}")))?;
                parts.push((name.clone(), crate::layouts::node_value(&node)));
            }
        }
        // What the pod serves: the plumbing the file stands for, as this
        // build's types write it; a workspace as its page.
        let recorded = crate::layouts::node_value(&parsed);
        let node = if recorded["type"].as_str() == Some("core://Workspace") {
            let slots = recorded["slots"].as_array().cloned().unwrap_or_default();
            crate::porcelain::page_of_with(&recorded, crate::porcelain::fills_screen(&slots))
                .unwrap_or(recorded.clone())
        } else {
            recorded.clone()
        };
        let node = self.uses(&node).map_err(fail)?;
        // A blueprint reads its own feeds wherever it is drawn (the inbox
        // reads the notifications; decision `notifications-in-a-collection`).
        let feeds: Vec<(String, String)> = info
            .feeds
            .iter()
            .map(|(s, f)| ((*s).to_owned(), (*f).to_owned()))
            .collect();
        let el = json!({
            "kind": "element", "title": info.title, "icon": info.icon, "node": node,
        });
        let id = if feeds.is_empty() {
            self.finish(el)
        } else {
            finish(
                self.catalog,
                &mut self.core.objects,
                &mut self.core.features,
                &feeds,
                el,
                true,
            )
        };
        let name = blueprint_name(info.id);
        self.core.names.entry(name).or_default().push((
            id,
            format!("Blueprint {} file {}: {note}", info.id, src.name),
        ));
        if newest {
            for s in recorded["slots"].as_array().into_iter().flatten() {
                if let Some(slot) = s["slot_id"].as_str() {
                    parts.push((format!("slot:{slot}"), s["content"].clone()));
                }
            }
            for (part, node) in parts {
                let node = self.uses(&node).map_err(fail)?;
                let id = self.finish(json!({ "kind": "element", "node": node }));
                self.core.names.insert(
                    format!("{CORE_PREFIX}{}/{part}", info.id),
                    vec![(
                        id,
                        format!("Part {part} of blueprint {} file {}", info.id, src.name),
                    )],
                );
            }
        }
        Ok(())
    }
}

/// Put `id` and every element it reaches (in `core`) in the store.
fn put_reachable(conn: &rusqlite::Connection, core: &Core, id: &str) -> rusqlite::Result<()> {
    let mut queue = vec![id.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(x) = queue.pop() {
        if !seen.insert(x.clone()) {
            continue;
        }
        let Some(v) = core.objects.get(&x) else {
            continue;
        };
        let f = core.features.get(&x).map_or("", String::as_str);
        cstore::put_object(conn, &x, v, f)?;
        for (r, _) in construct_elements::refs::all(&v["node"]) {
            if let Some(e) = r.el {
                queue.push(e);
            }
        }
        if let Some(ex) = v.get("examples") {
            for (r, _) in construct_elements::refs::all(ex) {
                if let Some(e) = r.el {
                    queue.push(e);
                }
            }
        }
    }
    Ok(())
}

/// Move the pod's `@core/` names to `core` (this build's files), re-sealing
/// what follows them: the names moved and their new `seq`. A name new to
/// the pod gets the whole history `core` has for it; a name behind gets
/// the files after the newest one it recorded (the newest only when it
/// recorded none of them). Blocking; one transaction.
pub fn refresh(
    store: &Store,
    catalog: &Catalog,
    core: &Core,
) -> rusqlite::Result<Vec<(String, i64)>> {
    store.transaction(|tx| {
        cstore::ensure_tables(tx)?;
        let mut moved = Vec::new();
        let mut changed = Vec::new();
        for (name, els) in &core.names {
            let Some((target, _)) = els.last() else {
                continue;
            };
            let current = cstore::name(tx, name)?;
            if current.as_deref() == Some(target.as_str()) {
                continue;
            }
            let recorded: BTreeSet<String> = cstore::history(tx, name)?
                .into_iter()
                .map(|e| e.hash)
                .collect();
            let push: &[(String, String)] = if recorded.is_empty() {
                els
            } else {
                match els.iter().rposition(|(h, _)| recorded.contains(h)) {
                    Some(i) if i + 1 < els.len() => &els[i + 1..],
                    _ => &els[els.len() - 1..],
                }
            };
            for (h, note) in push {
                put_reachable(tx, core, h)?;
                if let Some(seq) = cstore::move_name(tx, name, h, AUTHOR_POD, Some(note))? {
                    moved.push((name.clone(), seq));
                }
            }
            if let Some(v) = core.objects.get(target) {
                cstore::set_follows(tx, name, &super::write::followed(v))?;
            }
            changed.push(name.clone());
        }
        for name in &changed {
            moved.extend(super::write::reseal_followers(tx, catalog, name)?);
        }
        if !moved.is_empty() {
            cstore::bump_generation(tx)?;
            cstore::collect_garbage(tx)?;
        }
        Ok(moved)
    })
}

/// The home a new pod starts with: the whole of [`crate::HOME_BLUEPRINT`],
/// binding no feeds (what a pod got before the construct, converted).
pub fn home_element() -> Value {
    json!({
        "kind": "element",
        "title": "Home",
        "icon": "dashboard",
        "node": { "type": crate::components::USE_TYPE, "id": crate::HOME_APP,
                  "ref": blueprint_name(crate::HOME_BLUEPRINT) },
        "bind": { "feeds": [] },
    })
}

/// The home of a pod whose build has no home blueprint (a test's set of
/// sources): a blank page.
fn blank_home() -> Value {
    json!({
        "kind": "element",
        "title": "Home",
        "icon": "dashboard",
        "node": { "type": "core://Page", "id": crate::HOME_APP, "title": "Home",
                  "root": { "type": "core://Spacer" } },
        "bind": { "feeds": [] },
    })
}

/// Give a pod without a home its first one: [`home_element`], or a blank
/// page when this build has no home blueprint. Answers the names moved.
/// Blocking.
pub fn seed_home(store: &Store, catalog: &Catalog) -> Result<Vec<(String, i64)>, String> {
    let mut out = Ok(Vec::new());
    let r = store.transaction(|tx| {
        if cstore::name(tx, crate::HOME_APP)?.is_some()
            || cstore::last_seq(tx, crate::HOME_APP)? > 0
        {
            return Ok(());
        }
        let mut el = if catalog.is_blueprint(crate::HOME_BLUEPRINT) {
            home_element()
        } else {
            blank_home()
        };
        let written = super::write::normalize(tx, catalog, &el["node"]).and_then(|node| {
            el["node"] = node;
            super::write::put(
                tx,
                catalog,
                crate::HOME_APP,
                el,
                true,
                AUTHOR_POD,
                "Started from the Home blueprint",
            )
        });
        match written {
            Ok(moved) => {
                tracing::info!("the pod's home started from the Home blueprint");
                out = Ok(moved);
                Ok(())
            }
            Err(e) => {
                out = Err(e.to_string());
                // Roll back what the write began.
                Err(rusqlite::Error::QueryReturnedNoRows)
            }
        }
    });
    match (r, out) {
        (_, Err(e)) => Err(format!("starting the pod's home: {e}")),
        (Err(e), _) => Err(format!("starting the pod's home: {e}")),
        (Ok(()), Ok(moved)) => Ok(moved),
    }
}

/// `v` (a layout or node of a file, a hand-written bundle's) as this build's
/// clients draw it: a `core://Workspace` as its page, every use of a server
/// component (`"component": "core:<name>"`) expanded with this build's.
pub fn plumbing(core: &Core, v: &Value) -> Result<Value, String> {
    fn refs(core: &Core, v: &Value) -> Result<Value, String> {
        match v {
            Value::Array(a) => a.iter().map(|c| refs(core, c)).collect(),
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, x) in o {
                    if k != "component" {
                        out.insert(k.clone(), refs(core, x)?);
                    }
                }
                if o.get("type").and_then(Value::as_str) == Some(crate::components::USE_TYPE)
                    && let Some(c) = o.get("component").and_then(Value::as_str)
                {
                    let name = super::component_name(c).ok_or(format!("no component {c:?}"))?;
                    let el = core.current(&name).ok_or(format!("no component {c:?}"))?;
                    out.insert(
                        "ref".into(),
                        Ref {
                            name,
                            el: Some(el.to_owned()),
                            pin: false,
                            as_: None,
                        }
                        .to_value(),
                    );
                } else if let Some(c) = o.get("component") {
                    out.insert("component".into(), c.clone());
                }
                Ok(Value::Object(out))
            }
            other => Ok(other.clone()),
        }
    }
    let page = if v["type"].as_str() == Some("core://Workspace") {
        let slots = v["slots"].as_array().cloned().unwrap_or_default();
        crate::porcelain::page_of_with(v, crate::porcelain::fills_screen(&slots))
            .unwrap_or_else(|| v.clone())
    } else {
        v.clone()
    };
    let el = json!({ "kind": "element", "node": refs(core, &page)? });
    let id = object_id(&el);
    let mut objects = core.objects.clone();
    objects.insert(id.clone(), Arc::new(el));
    construct_elements::materialize(&objects, &id)
        .map(|m| m.node)
        .map_err(|e| e.to_string())
}
