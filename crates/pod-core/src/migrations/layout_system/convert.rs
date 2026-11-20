//! The old system as elements and names (decision
//! `content-addressed-construct`): what the conversion of a pod that wasn't
//! switched writes. Reads only what [`super::tables::Old`] read; the
//! [`Plan`] is applied by [`super::apply`].
//!
//! - Every server component `core:<name>` becomes `@core/<name>` (its latest
//!   recorded definition), every `pod:<name>` `@me/<name>`; a use pinned to
//!   a `version` gets that version's element, pinned.
//! - Every blueprint becomes `@core/<id>`, its history every version the pod
//!   recorded (a client is sealed with the newest it can draw). A workspace
//!   becomes the page it stands for (`core:workspace`, `core:screen`).
//!   Settings loses its Layouts section and versions screen (no versions on
//!   the device). The pod then moves these names to this build's files
//!   ([`crate::construct::core::refresh`]).
//! - A blueprint's node part `core:<app>/<part>` becomes `@core/<app>/<part>`;
//!   a screen part is expanded in place (it can't be a page's screen).
//! - Every live pod layout keeps its id as its name. Links and embeds
//!   (`layout: <id>`) become references; one to a blueprint keeps the id as
//!   its edge (`as`). A link that would close a cycle (elements only go
//!   forward) becomes `action.navigation.pop_to` (`pop_to_root` for home),
//!   an embed that would a placeholder; a link to a layout named by a
//!   binding (`$item.id`) stays as it is. A list over `local:layouts` is
//!   written out as one row per live layout. Each of these is a [`Note`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use construct_elements::Objects;
use construct_elements::refs::{OPEN_INTENT, Ref, Site};
use serde_json::{Map, Value, json};

use super::tables::Old;
use crate::adapter::Catalog;
use crate::construct::{CORE_PREFIX, component_name, core_layout_name};
use crate::layouts::AUTHOR_POD;

/// One planned move of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub hash: String,
    pub author: String,
    pub note: String,
}

/// Something the conversion changed in meaning, for the dry run's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The layout (or blueprint, component) it is in.
    pub at: String,
    pub kind: &'static str,
    pub detail: String,
}

impl Note {
    pub fn to_json(&self) -> Value {
        json!({ "at": self.at, "kind": self.kind, "detail": self.detail })
    }
}

/// What a conversion makes: objects, each name's history, and notes.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub objects: BTreeMap<String, Arc<Value>>,
    pub features: BTreeMap<String, String>,
    /// Oldest first; the last is where the name points.
    pub names: BTreeMap<String, Vec<Move>>,
    pub notes: Vec<Note>,
    /// The pod's own layouts converted, home first.
    pub layouts: Vec<String>,
}

impl Plan {
    /// Where `name` points in this plan.
    pub fn current(&self, name: &str) -> Option<&str> {
        self.names
            .get(name)
            .and_then(|h| h.last())
            .map(|m| m.hash.as_str())
    }
}

impl Objects for Plan {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.objects.get(id).cloned()
    }
}

/// Convert the pod's live layouts and every component and blueprint (see
/// the module docs) from what the old tables hold.
pub fn convert(old: &Old, catalog: &Catalog) -> Plan {
    let mut c = Converter {
        old,
        catalog,
        plan: Plan::default(),
        done: BTreeMap::new(),
        in_progress: BTreeSet::new(),
        layouts: old
            .layouts
            .iter()
            .map(|a| (a.id.clone(), (a.title.clone(), a.icon.clone())))
            .collect(),
        rows: old
            .layouts
            .iter()
            .filter(|a| a.id != crate::HOME_APP)
            .map(|a| Row {
                id: a.id.clone(),
                title: a.title.clone(),
                icon: a.icon.clone(),
                hidden: old.hidden.contains(&a.id),
            })
            .collect(),
        home_feeds: old.home_feeds.clone(),
        at: String::new(),
    };
    // Server components and blueprints first: every name ships.
    let names: Vec<String> = old.components.keys().cloned().collect();
    for n in names
        .iter()
        .filter(|n| n.starts_with(crate::components::CORE))
    {
        c.component(n, None);
    }
    for n in names
        .iter()
        .filter(|n| n.starts_with(crate::components::POD))
    {
        c.component(n, None);
    }
    let apps: Vec<&'static str> = catalog.core_layouts().map(|a| a.id).collect();
    for app in apps {
        c.core_layout(app);
    }
    // The pod's layouts, home first, then the ones home doesn't reach.
    if c.layouts.contains_key(crate::HOME_APP) {
        c.layout(crate::HOME_APP);
    }
    let ids: Vec<String> = c.layouts.keys().cloned().collect();
    for id in ids {
        c.layout(&id);
    }
    c.plan
}

struct Row {
    id: String,
    title: String,
    icon: String,
    hidden: bool,
}

struct Converter<'a> {
    old: &'a Old,
    catalog: &'a Catalog,
    plan: Plan,
    /// Names converted: their current hash (`None`: can't be).
    done: BTreeMap<String, Option<String>>,
    /// Names being converted (a reference to one closes a cycle).
    in_progress: BTreeSet<String>,
    /// Live pod layouts: id → (title, icon).
    layouts: BTreeMap<String, (String, String)>,
    /// `local:layouts` rows (every layout but home).
    rows: Vec<Row>,
    home_feeds: Vec<(String, String)>,
    /// What is being converted, for notes.
    at: String,
}

impl Converter<'_> {
    fn note(&mut self, kind: &'static str, detail: String) {
        self.plan.notes.push(Note {
            at: self.at.clone(),
            kind,
            detail,
        });
    }

    /// Hash `el`, work out its needs and features, keep it. Answers its id.
    fn finish(&mut self, el: Value, layout: bool) -> String {
        crate::construct::core::finish(
            self.catalog,
            &mut self.plan.objects,
            &mut self.plan.features,
            &self.home_feeds,
            el,
            layout,
        )
    }

    fn push_name(&mut self, name: &str, hash: &str, note: String) {
        let h = self.plan.names.entry(name.to_owned()).or_default();
        if h.last().is_some_and(|m| m.hash == hash) {
            return;
        }
        h.push(Move {
            hash: hash.to_owned(),
            author: AUTHOR_POD.to_owned(),
            note,
        });
    }

    /// `old` (`core:x` or `pod:x`), latest or `version`: its element id.
    fn component(&mut self, old: &str, version: Option<i64>) -> Option<String> {
        let name = component_name(old)?;
        if version.is_none()
            && let Some(done) = self.done.get(&name)
        {
            return done.clone();
        }
        if self.in_progress.contains(&name) {
            return None;
        }
        let def = self.old.component(old, version);
        let Some((n, def)) = def else {
            if version.is_none() {
                self.done.insert(name, None);
            }
            return None;
        };
        self.in_progress.insert(name.clone());
        let saved = std::mem::replace(&mut self.at, name.clone());
        let template = self.tree(&def["template"], false);
        let examples = def.get("examples").map(|ex| self.tree(ex, false));
        self.at = saved;
        self.in_progress.remove(&name);
        let mut el = json!({
            "kind": "element",
            "doc": def["doc"],
            "params": def.get("params").cloned().unwrap_or(json!([])),
            "children": def["children"].as_bool().unwrap_or(false),
            "node": template,
        });
        if let Some(ex) = examples {
            el["examples"] = ex;
        }
        let id = self.finish(el, false);
        if version.is_none() {
            self.push_name(&name, &id, format!("Converted from {old} version {n}"));
            self.done.insert(name, Some(id.clone()));
        }
        Some(id)
    }

    /// Blueprint `app`: every recorded version, oldest first. Its current id.
    fn core_layout(&mut self, app: &str) -> Option<String> {
        let name = core_layout_name(app);
        if let Some(done) = self.done.get(&name) {
            return done.clone();
        }
        if self.in_progress.contains(&name) {
            return None;
        }
        let info = self.catalog.core_layout(app).copied()?;
        self.in_progress.insert(name.clone());
        let saved = std::mem::replace(&mut self.at, name.clone());
        let versions: Vec<(i64, Option<String>, String)> = self
            .old
            .versions
            .get(app)
            .into_iter()
            .flatten()
            .map(|(n, v)| (*n, v.note.clone(), v.json.clone()))
            .collect();
        let mut current = None;
        for (version, note, json) in versions {
            let Ok(doc) = serde_json::from_str::<Value>(&json) else {
                continue;
            };
            let node = self.layout_node(&doc, app);
            let el =
                json!({ "kind": "element", "title": info.title, "icon": info.icon, "node": node });
            let id = self.finish(el, false);
            self.push_name(
                &name,
                &id,
                format!(
                    "Blueprint {app} version {}{}",
                    version,
                    note.as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default()
                ),
            );
            current = Some(id);
        }
        self.at = saved;
        self.in_progress.remove(&name);
        self.done.insert(name, current.clone());
        current
    }

    /// Part `part` of blueprint `app` (latest, or of `version`), as an
    /// element; named `@core/<app>/<part>` unless pinned.
    fn part(&mut self, app: &str, part: &str, version: Option<i64>) -> Option<String> {
        let name = format!("{CORE_PREFIX}{app}/{part}");
        if version.is_none()
            && let Some(done) = self.done.get(&name)
        {
            return done.clone();
        }
        let parts = super::expand::parts(self.old, app, version)?;
        let node = parts.parts.get(part)?;
        let saved = std::mem::replace(&mut self.at, name.clone());
        let node = self.tree(node, false);
        self.at = saved;
        let id = self.finish(json!({ "kind": "element", "node": node }), false);
        if version.is_none() {
            self.push_name(
                &name,
                &id,
                format!("Part {part} of blueprint {app} version {}", parts.version),
            );
            self.done.insert(name, Some(id.clone()));
        }
        Some(id)
    }

    /// Pod layout `id`: its current element (`None` while it is being
    /// converted: a reference to it closes a cycle).
    fn layout(&mut self, id: &str) -> Option<String> {
        if let Some(done) = self.done.get(id) {
            return done.clone();
        }
        if self.in_progress.contains(id) {
            return None;
        }
        let (title, icon) = self.layouts.get(id).cloned()?;
        let (version, v) = self.old.version(id, None)?;
        let Ok(doc) = serde_json::from_str::<Value>(&v.json) else {
            return None;
        };
        self.in_progress.insert(id.to_owned());
        let saved = std::mem::replace(&mut self.at, id.to_owned());
        let doc = self.launcher(doc);
        let node = self.layout_node(&doc, id);
        self.at = saved;
        self.in_progress.remove(id);
        let mut el = json!({ "kind": "element", "title": title, "icon": icon, "node": node });
        if id == crate::HOME_APP {
            el["bind"] = json!({ "feeds": self.home_feeds });
        }
        let hash = self.finish(el, true);
        self.push_name(
            id,
            &hash,
            format!("Converted from layout {id} version {version}"),
        );
        self.done.insert(id.to_owned(), Some(hash.clone()));
        self.plan.layouts.push(id.to_owned());
        Some(hash)
    }

    /// A stored layout (`id`'s, or a blueprint version) as a page with
    /// references.
    fn layout_node(&mut self, doc: &Value, id: &str) -> Value {
        let mut doc = doc.clone();
        if id == "settings" {
            doc = strip_versions(doc);
        }
        // Screen parts can't be references (a page's screen is a page's).
        doc = self.expand_screen_parts(doc);
        if doc["type"].as_str() == Some("core://Workspace") {
            // Whether it fills the screen is read from its expansion.
            let expanded = if super::expand::may_hold_refs(&doc.to_string()) {
                super::expand::expand(self.old, self.catalog, &doc, super::expand::Mode::Lenient)
                    .unwrap_or_else(|_| doc.clone())
            } else {
                doc.clone()
            };
            let slots = expanded["slots"].as_array().cloned().unwrap_or_default();
            let fills = crate::porcelain::fills_screen(&slots);
            if let Some(page) = crate::porcelain::page_of_with(&doc, fills) {
                doc = page;
            }
        }
        self.tree(&doc, false)
    }

    /// `screens` entries that are screen parts, expanded in place.
    fn expand_screen_parts(&mut self, mut doc: Value) -> Value {
        let Some(screens) = doc.get_mut("screens").and_then(Value::as_array_mut) else {
            return doc;
        };
        let mut out = Vec::new();
        for s in screens.drain(..) {
            let is_ref = matches!(
                s["type"].as_str(),
                Some(super::expand::REF_TYPE | crate::components::USE_TYPE)
            );
            if !is_ref {
                out.push(s);
                continue;
            }
            let wrapped = json!({ "screens": [s] });
            match super::expand::expand(
                self.old,
                self.catalog,
                &wrapped,
                super::expand::Mode::Lenient,
            ) {
                Ok(v) => {
                    out.extend(v["screens"].as_array().cloned().unwrap_or_default());
                    self.note(
                        "screen_part_copied",
                        "a screen part was copied in: it no longer follows its blueprint".into(),
                    );
                }
                Err(_) => out.push(s),
            }
        }
        *screens = out;
        doc
    }

    /// Lists over `local:layouts` written out as rows of links.
    fn launcher(&mut self, v: Value) -> Value {
        match v {
            Value::Object(o) => {
                let q = o.get("query_key").and_then(Value::as_str).unwrap_or("");
                if o.get("type").and_then(Value::as_str) == Some("core://List")
                    && matches!(q, "local:layouts" | "local:apps" | "local:all_layouts")
                {
                    let all = q == "local:all_layouts";
                    let template = o.get("item_template").cloned().unwrap_or(Value::Null);
                    let rows: Vec<Value> = self
                        .rows
                        .iter()
                        .filter(|r| all || !r.hidden)
                        .map(|r| fill_row(&template, r))
                        .collect();
                    self.note(
                        "launcher_written_out",
                        format!(
                            "a list of {q} became {} rows of links (versions aren't shown)",
                            rows.len()
                        ),
                    );
                    if rows.is_empty() {
                        return json!({ "type": "core://Text", "style": "Secondary",
                            "value_expr": o.get("empty_text").cloned().unwrap_or(json!("")) });
                    }
                    return json!({ "type": "core://Column", "children": rows });
                }
                Value::Object(o.into_iter().map(|(k, c)| (k, self.launcher(c))).collect())
            }
            Value::Array(a) => Value::Array(a.into_iter().map(|c| self.launcher(c)).collect()),
            Value::String(s) if s.contains("${local://layouts#count}") => {
                let n = self.rows.iter().filter(|r| !r.hidden).count();
                Value::String(s.replace("${local://layouts#count}", &n.to_string()))
            }
            other => other,
        }
    }

    /// A tree with its uses, parts and links as references.
    fn tree(&mut self, v: &Value, in_screens: bool) -> Value {
        let _ = in_screens;
        match v {
            Value::Object(o) => self.object(o),
            Value::Array(a) => Value::Array(a.iter().map(|c| self.tree(c, false)).collect()),
            other => other.clone(),
        }
    }

    fn object(&mut self, o: &Map<String, Value>) -> Value {
        let ty = o.get("type").and_then(Value::as_str);
        let sealed = |name: String, el: String, pin: bool, as_: Option<String>| {
            Ref {
                name,
                el: Some(el),
                pin,
                as_,
            }
            .to_value()
        };
        // A use: a component, a blueprint or one of its parts.
        if matches!(
            ty,
            Some(crate::components::USE_TYPE | super::expand::REF_TYPE)
        ) && !o.contains_key("ref")
        {
            let version = o.get("version").and_then(Value::as_i64);
            let (target, part) = match ty {
                Some(super::expand::REF_TYPE) => (
                    o.get("app").and_then(Value::as_str).map(str::to_owned),
                    o.get("part").and_then(Value::as_str).map(str::to_owned),
                ),
                _ => {
                    let c = o.get("component").and_then(Value::as_str).unwrap_or("");
                    match c.strip_prefix(crate::components::CORE) {
                        Some(bare) => {
                            let (app, part) = bare.split_once('/').unwrap_or((bare, ""));
                            if !part.is_empty() || self.catalog.is_core_layout(app) {
                                (Some(app.to_owned()), Some(part.to_owned()))
                            } else {
                                (None, None)
                            }
                        }
                        None => (None, None),
                    }
                }
            };
            let mut out = Map::new();
            for (k, c) in o {
                if matches!(k.as_str(), "component" | "version" | "app" | "part") {
                    continue;
                }
                out.insert(k.clone(), self.tree(c, false));
            }
            out.insert("type".into(), json!(crate::components::USE_TYPE));
            let made = match (target, part) {
                (Some(app), Some(part))
                    if part.is_empty() || part == super::expand::LAYOUT_PART =>
                {
                    // A whole blueprint (pinned: that version's element).
                    match version {
                        Some(n) => self
                            .core_layout_version(&app, n)
                            .map(|el| sealed(core_layout_name(&app), el, true, None)),
                        None => self
                            .core_layout(&app)
                            .map(|el| sealed(core_layout_name(&app), el, false, None)),
                    }
                }
                (Some(app), Some(part)) => self.part(&app, &part, version).map(|el| {
                    sealed(
                        format!("{CORE_PREFIX}{app}/{part}"),
                        el,
                        version.is_some(),
                        None,
                    )
                }),
                _ => {
                    let c = o.get("component").and_then(Value::as_str).unwrap_or("");
                    match (component_name(c), self.component(c, version)) {
                        (Some(name), Some(el)) => Some(sealed(name, el, version.is_some(), None)),
                        _ => None,
                    }
                }
            };
            match made {
                Some(r) => {
                    out.insert("ref".into(), r);
                    return Value::Object(out);
                }
                None => {
                    self.note(
                        "unresolved_use",
                        format!(
                            "{} could not be converted: drawn as a placeholder",
                            label(o)
                        ),
                    );
                    return construct_elements::materialize::placeholder();
                }
            }
        }
        // An edge: an embed or a link to another layout.
        let site = if ty == Some("core://Layout") {
            Some(Site::Embed)
        } else if o.get("intent").and_then(Value::as_str) == Some(OPEN_INTENT) {
            Some(Site::Link)
        } else {
            None
        };
        if let Some(site) = site
            && !o.contains_key("ref")
            && let Some(target) = o.get("layout").and_then(Value::as_str)
        {
            let mut out = Map::new();
            for (k, c) in o {
                if k != "layout" {
                    out.insert(k.clone(), self.tree(c, false));
                }
            }
            if target.contains('$') {
                self.note(
                    "dynamic_link",
                    format!("a link to the layout named by {target} stays as it is (no edge)"),
                );
                out.insert("layout".into(), json!(target));
                return Value::Object(out);
            }
            let found = if self.layouts.contains_key(target) {
                match self.layout(target) {
                    Some(el) => Some(sealed(target.to_owned(), el, false, None)),
                    None => {
                        // A cycle: elements only go forward.
                        return match site {
                            Site::Link if target == crate::HOME_APP => {
                                self.note(
                                    "link_became_pop",
                                    "a link back to home became action.navigation.pop_to_root"
                                        .into(),
                                );
                                json!({ "intent": "action.navigation.pop_to_root" })
                            }
                            Site::Link => {
                                self.note(
                                    "link_became_pop",
                                    format!(
                                        "a link back to {target} (which leads here) became \
                                         action.navigation.pop_to"
                                    ),
                                );
                                json!({ "intent": "action.navigation.pop_to", "edge": target })
                            }
                            _ => {
                                self.note(
                                    "embed_dropped",
                                    format!("an embed of {target}, which embeds this, was dropped"),
                                );
                                construct_elements::materialize::placeholder()
                            }
                        };
                    }
                }
            } else if self.catalog.is_core_layout(target) {
                self.core_layout(target)
                    .map(|el| sealed(core_layout_name(target), el, false, Some(target.to_owned())))
            } else {
                None
            };
            match found {
                Some(r) => {
                    out.insert("ref".into(), r);
                }
                None => {
                    self.note(
                        "unknown_link",
                        format!("a link to {target}, which isn't a layout, stays as it is"),
                    );
                    out.insert("layout".into(), json!(target));
                }
            }
            return Value::Object(out);
        }
        let mut out = Map::new();
        for (k, c) in o {
            out.insert(k.clone(), self.tree(c, k == "screens"));
        }
        Value::Object(out)
    }

    /// One version of blueprint `app` as an element (pinned uses).
    fn core_layout_version(&mut self, app: &str, n: i64) -> Option<String> {
        let info = self.catalog.core_layout(app).copied()?;
        let (_, v) = self.old.version(app, Some(n))?;
        let Ok(doc) = serde_json::from_str::<Value>(&v.json) else {
            return None;
        };
        let node = self.layout_node(&doc, app);
        let el = json!({ "kind": "element", "title": info.title, "icon": info.icon, "node": node });
        Some(self.finish(el, false))
    }
}

fn label(o: &Map<String, Value>) -> String {
    match o.get("component").and_then(Value::as_str) {
        Some(c) => format!("a use of {c}"),
        None => format!(
            "a reference to {}/{}",
            o.get("app").and_then(Value::as_str).unwrap_or(""),
            o.get("part").and_then(Value::as_str).unwrap_or("")
        ),
    }
}

/// A `local:layouts` row's template filled in for `row`.
fn fill_row(template: &Value, row: &Row) -> Value {
    match template {
        Value::String(s) if s.contains("$item.") => {
            let filled = s
                .replace("$item.id", &row.id)
                .replace("$item.title", &row.title)
                .replace("$item.icon", &row.icon)
                .replace("$item.kind", "custom")
                .replace("$item.hidden", if row.hidden { "true" } else { "false" })
                .replace("$item.version", "")
                .replace("$item.detail", "")
                .replace("$item.unread", "");
            Value::String(filled)
        }
        Value::Array(a) => Value::Array(a.iter().map(|c| fill_row(c, row)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, c)| (k.clone(), fill_row(c, row)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Settings without its Layouts section and versions screen (no versions on
/// a switched pod's devices).
pub fn strip_versions(mut doc: Value) -> Value {
    if let Some(slots) = doc.get_mut("slots").and_then(Value::as_array_mut) {
        slots.retain(|s| s["provider_uri"].as_str() != Some("local://layouts"));
    }
    if let Some(screens) = doc.get_mut("screens").and_then(Value::as_array_mut) {
        screens.retain(|s| {
            !s["route"]
                .as_str()
                .is_some_and(|r| r.starts_with("settings://versions"))
                && !s.to_string().contains("local:versions")
        });
    }
    doc
}
