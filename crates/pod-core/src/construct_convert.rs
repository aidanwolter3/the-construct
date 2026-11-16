//! The old system as elements and names (decision
//! `content-addressed-construct`): what a switch writes and its dry run
//! renders. Reads only; [`Plan`] is applied by [`super::switch`].
//!
//! - Every server component `core:<name>` becomes `@core/<name>` (its latest
//!   definition), every `pod:<name>` `@me/<name>`; a use pinned to a
//!   `version` gets that version's element, pinned.
//! - Every blueprint becomes `@core/<id>`, its history every version the pod
//!   recorded (a client is sealed with the newest it can draw). A workspace
//!   becomes the page it stands for (`core:workspace`, `core:screen`).
//!   Settings loses its Layouts section and versions screen (no versions on
//!   the device).
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

use construct_elements::refs::{OPEN_INTENT, Ref, Site};
use construct_elements::{Objects, object_id};
use serde_json::{Map, Value, json};

use super::{CORE_PREFIX, ME_PREFIX, blueprint_name, component_name};
use crate::adapter::Catalog;
use crate::store::Store;

/// The author of what the conversion and the pod itself write.
pub const AUTHOR_POD: &str = "pod";

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

/// Convert the pod's live layouts and every component and blueprint.
pub fn convert(store: &Store, catalog: &Catalog) -> rusqlite::Result<Plan> {
    let layouts = store.with_conn(crate::custom::custom_apps_on)?;
    let hidden = crate::custom::hidden_apps(store)?;
    let home_feeds = store
        .with_conn(crate::custom::home_feeds_on)?
        .unwrap_or_default();
    let mut c = Converter {
        store,
        catalog,
        plan: Plan::default(),
        done: BTreeMap::new(),
        in_progress: BTreeSet::new(),
        layouts: layouts
            .iter()
            .map(|a| (a.id.clone(), (a.title.clone(), a.icon.clone())))
            .collect(),
        rows: layouts
            .iter()
            .filter(|a| a.id != crate::HOME_APP)
            .map(|a| Row {
                id: a.id.clone(),
                title: a.title.clone(),
                icon: a.icon.clone(),
                hidden: hidden.contains(&a.id),
            })
            .collect(),
        home_feeds,
        at: String::new(),
    };
    // Server components and blueprints first: every name ships.
    let comps = crate::components::list(store)?;
    for (meta, _) in &comps {
        if meta.name.starts_with(crate::components::CORE) {
            c.component(&meta.name, None)?;
        }
    }
    for (meta, _) in &comps {
        if meta.name.starts_with(crate::components::POD) {
            c.component(&meta.name, None)?;
        }
    }
    let apps: Vec<&'static str> = catalog.apps().map(|a| a.id).collect();
    for app in apps {
        c.blueprint(app)?;
    }
    // The pod's layouts, home first, then the ones home doesn't reach.
    if c.layouts.contains_key(crate::HOME_APP) {
        c.layout(crate::HOME_APP)?;
    }
    let ids: Vec<String> = c.layouts.keys().cloned().collect();
    for id in ids {
        c.layout(&id)?;
    }
    Ok(c.plan)
}

struct Row {
    id: String,
    title: String,
    icon: String,
    hidden: bool,
}

struct Converter<'a> {
    store: &'a Store,
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
    fn finish(&mut self, mut el: Value, layout: bool) -> String {
        let probe = object_id(&el);
        self.plan
            .objects
            .insert(probe.clone(), Arc::new(el.clone()));
        let (needs, features) = derive(self.catalog, &self.home_feeds, &self.plan, &probe, layout);
        self.plan.objects.remove(&probe);
        el["needs"] = needs;
        let id = object_id(&el);
        self.plan.objects.insert(id.clone(), Arc::new(el));
        self.plan.features.insert(id.clone(), features);
        id
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
    fn component(&mut self, old: &str, version: Option<i64>) -> rusqlite::Result<Option<String>> {
        let Some(name) = component_name(old) else {
            return Ok(None);
        };
        if version.is_none()
            && let Some(done) = self.done.get(&name)
        {
            return Ok(done.clone());
        }
        if self.in_progress.contains(&name) {
            return Ok(None);
        }
        let def = match version {
            Some(n) => crate::components::version(self.store, old, n)?.map(|d| (n, d)),
            None => crate::components::latest(self.store, old)?,
        };
        let Some((n, def)) = def else {
            if version.is_none() {
                self.done.insert(name, None);
            }
            return Ok(None);
        };
        self.in_progress.insert(name.clone());
        let saved = std::mem::replace(&mut self.at, name.clone());
        let template = self.tree(&def["template"], false)?;
        let examples = match def.get("examples") {
            Some(ex) => Some(self.tree(ex, false)?),
            None => None,
        };
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
        Ok(Some(id))
    }

    /// Blueprint `app`: every recorded version, oldest first. Its current id.
    fn blueprint(&mut self, app: &str) -> rusqlite::Result<Option<String>> {
        let name = blueprint_name(app);
        if let Some(done) = self.done.get(&name) {
            return Ok(done.clone());
        }
        if self.in_progress.contains(&name) {
            return Ok(None);
        }
        let Some(info) = self.catalog.app(app).copied() else {
            return Ok(None);
        };
        self.in_progress.insert(name.clone());
        let saved = std::mem::replace(&mut self.at, name.clone());
        let mut versions = Vec::new();
        let mut before = None;
        loop {
            let page = self.store.app_versions(app, before, 100)?;
            let Some(last) = page.last() else { break };
            before = Some(last.version);
            versions.extend(page);
        }
        versions.reverse();
        let mut current = None;
        for v in versions {
            let Some((_, json)) = self.store.app_version(app, v.version)? else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Value>(&json) else {
                continue;
            };
            let node = self.layout_node(&doc, app)?;
            let el =
                json!({ "kind": "element", "title": info.title, "icon": info.icon, "node": node });
            let id = self.finish(el, false);
            self.push_name(
                &name,
                &id,
                format!(
                    "Blueprint {app} version {}{}",
                    v.version,
                    v.note
                        .as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default()
                ),
            );
            current = Some(id);
        }
        self.at = saved;
        self.in_progress.remove(&name);
        self.done.insert(name, current.clone());
        Ok(current)
    }

    /// Part `part` of blueprint `app` (latest, or of `version`), as an
    /// element; named `@core/<app>/<part>` unless pinned.
    fn part(
        &mut self,
        app: &str,
        part: &str,
        version: Option<i64>,
    ) -> rusqlite::Result<Option<String>> {
        let name = format!("{CORE_PREFIX}{app}/{part}");
        if version.is_none()
            && let Some(done) = self.done.get(&name)
        {
            return Ok(done.clone());
        }
        let Some(parts) = crate::parts::load(self.store, app, version, None)? else {
            return Ok(None);
        };
        let Some(node) = parts.parts.get(part) else {
            return Ok(None);
        };
        let saved = std::mem::replace(&mut self.at, name.clone());
        let node = self.tree(node, false)?;
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
        Ok(Some(id))
    }

    /// Pod layout `id`: its current element (`None` while it is being
    /// converted: a reference to it closes a cycle).
    fn layout(&mut self, id: &str) -> rusqlite::Result<Option<String>> {
        if let Some(done) = self.done.get(id) {
            return Ok(done.clone());
        }
        if self.in_progress.contains(id) {
            return Ok(None);
        }
        let Some((title, icon)) = self.layouts.get(id).cloned() else {
            return Ok(None);
        };
        let Some((v, json)) = self.store.app_latest(id)? else {
            return Ok(None);
        };
        let Ok(doc) = serde_json::from_str::<Value>(&json) else {
            return Ok(None);
        };
        self.in_progress.insert(id.to_owned());
        let saved = std::mem::replace(&mut self.at, id.to_owned());
        let doc = self.launcher(doc);
        let node = self.layout_node(&doc, id)?;
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
            format!("Converted from layout {id} version {}", v.version),
        );
        self.done.insert(id.to_owned(), Some(hash.clone()));
        self.plan.layouts.push(id.to_owned());
        Ok(Some(hash))
    }

    /// A stored layout (`id`'s, or a blueprint version) as a page with
    /// references.
    fn layout_node(&mut self, doc: &Value, id: &str) -> rusqlite::Result<Value> {
        let mut doc = doc.clone();
        if id == "settings" {
            doc = strip_versions(doc);
        }
        // Screen parts can't be references (a page's screen is a page's).
        doc = self.expand_screen_parts(doc);
        if doc["type"].as_str() == Some("core://Workspace") {
            // Whether it fills the screen is read from its expansion.
            let expanded = if crate::parts::may_hold_refs(&doc.to_string()) {
                crate::parts::expand(
                    self.store,
                    self.catalog,
                    &doc,
                    None,
                    crate::parts::Mode::Lenient,
                )
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
                Some(crate::parts::REF_TYPE | crate::components::USE_TYPE)
            );
            if !is_ref {
                out.push(s);
                continue;
            }
            let wrapped = json!({ "screens": [s] });
            match crate::parts::expand(
                self.store,
                self.catalog,
                &wrapped,
                None,
                crate::parts::Mode::Lenient,
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
    fn tree(&mut self, v: &Value, in_screens: bool) -> rusqlite::Result<Value> {
        let _ = in_screens;
        match v {
            Value::Object(o) => self.object(o),
            Value::Array(a) => a.iter().map(|c| self.tree(c, false)).collect(),
            other => Ok(other.clone()),
        }
    }

    fn object(&mut self, o: &Map<String, Value>) -> rusqlite::Result<Value> {
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
            Some(crate::components::USE_TYPE | crate::parts::REF_TYPE)
        ) && !o.contains_key("ref")
        {
            let version = o.get("version").and_then(Value::as_i64);
            let (target, part) = match ty {
                Some(crate::parts::REF_TYPE) => (
                    o.get("app").and_then(Value::as_str).map(str::to_owned),
                    o.get("part").and_then(Value::as_str).map(str::to_owned),
                ),
                _ => {
                    let c = o.get("component").and_then(Value::as_str).unwrap_or("");
                    match c.strip_prefix(crate::components::CORE) {
                        Some(bare) => {
                            let (app, part) = bare.split_once('/').unwrap_or((bare, ""));
                            if !part.is_empty() || self.catalog.is_app(app) {
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
                out.insert(k.clone(), self.tree(c, false)?);
            }
            out.insert("type".into(), json!(crate::components::USE_TYPE));
            let made = match (target, part) {
                (Some(app), Some(part)) if part.is_empty() || part == crate::parts::LAYOUT_PART => {
                    // A whole blueprint (pinned: that version's element).
                    match version {
                        Some(n) => self
                            .blueprint_version(&app, n)?
                            .map(|el| sealed(blueprint_name(&app), el, true, None)),
                        None => self
                            .blueprint(&app)?
                            .map(|el| sealed(blueprint_name(&app), el, false, None)),
                    }
                }
                (Some(app), Some(part)) => self.part(&app, &part, version)?.map(|el| {
                    sealed(
                        format!("{CORE_PREFIX}{app}/{part}"),
                        el,
                        version.is_some(),
                        None,
                    )
                }),
                _ => {
                    let c = o.get("component").and_then(Value::as_str).unwrap_or("");
                    match (component_name(c), self.component(c, version)?) {
                        (Some(name), Some(el)) => Some(sealed(name, el, version.is_some(), None)),
                        _ => None,
                    }
                }
            };
            match made {
                Some(r) => {
                    out.insert("ref".into(), r);
                    return Ok(Value::Object(out));
                }
                None => {
                    self.note(
                        "unresolved_use",
                        format!(
                            "{} could not be converted: drawn as a placeholder",
                            label(o)
                        ),
                    );
                    return Ok(construct_elements::materialize::placeholder());
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
                    out.insert(k.clone(), self.tree(c, false)?);
                }
            }
            if target.contains('$') {
                self.note(
                    "dynamic_link",
                    format!("a link to the layout named by {target} stays as it is (no edge)"),
                );
                out.insert("layout".into(), json!(target));
                return Ok(Value::Object(out));
            }
            let found = if self.layouts.contains_key(target) {
                match self.layout(target)? {
                    Some(el) => Some(sealed(target.to_owned(), el, false, None)),
                    None => {
                        // A cycle: elements only go forward.
                        return Ok(match site {
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
                        });
                    }
                }
            } else if self.catalog.is_app(target) {
                self.blueprint(target)?
                    .map(|el| sealed(blueprint_name(target), el, false, Some(target.to_owned())))
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
            return Ok(Value::Object(out));
        }
        let mut out = Map::new();
        for (k, c) in o {
            out.insert(k.clone(), self.tree(c, k == "screens")?);
        }
        Ok(Value::Object(out))
    }

    /// One version of blueprint `app` as an element (pinned uses).
    fn blueprint_version(&mut self, app: &str, n: i64) -> rusqlite::Result<Option<String>> {
        let Some(info) = self.catalog.app(app).copied() else {
            return Ok(None);
        };
        let Some((_, json)) = self.store.app_version(app, n)? else {
            return Ok(None);
        };
        let Ok(doc) = serde_json::from_str::<Value>(&json) else {
            return Ok(None);
        };
        let node = self.layout_node(&doc, app)?;
        let el = json!({ "kind": "element", "title": info.title, "icon": info.icon, "node": node });
        Ok(Some(self.finish(el, false)))
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

/// An element's needs and features, from its materialization.
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
        crate::custom::bound_feeds(catalog, home_feeds, &node)
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

/// Whether `name` is one of the server's (`@core/…`).
pub fn is_core(name: &str) -> bool {
    name.starts_with(CORE_PREFIX)
}

/// Whether `name` is one of the pod's own components (`@me/…`).
pub fn is_me(name: &str) -> bool {
    name.starts_with(ME_PREFIX)
}
