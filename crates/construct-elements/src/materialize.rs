//! Materializing an element: what the renderer draws (decision
//! `content-addressed-construct`).
//!
//! Every `core://Use` with a `ref` is expanded in place from the element it
//! names (a component: its template with the use's `args`, or `bind`, and
//! children; any other element: its node). Every edge (`core://Layout`,
//! `action.navigation.open`) keeps its own fields and is written back with
//! `layout: <key>` instead of its `ref`, so the renderer opens it by key as it
//! opens a layout id; the edges are listed with their targets, for the
//! caller to materialize and to build the edge graph from.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::def::{CHILDREN_TYPE, Def, EACH_TYPE, FRAGMENT_TYPE, IF_ARG_TYPE, MATCH_TYPE, MAX_EACH};
use crate::refs::{BIND_FIELD, EdgeRef, Ref, Site, site_of};
use crate::template::{
    Error, MAX_EXPANDED_BYTES, Work, as_text, subst_in, subst_value_in, truthy, weight,
};

/// Uses are expanded this deep at most.
pub const MAX_DEPTH: usize = 12;

/// Where the objects come from.
pub trait Objects {
    /// The object with this id, if it is here.
    fn object(&self, id: &str) -> Option<Arc<Value>>;

    /// The element `name` points at, for a reference that follows a name
    /// without holding its hash: a construct's manifest on a device
    /// (feature `construct_manifest`), the name's current element on the
    /// pod. `None` where there are only objects.
    fn resolve(&self, _name: &str) -> Option<String> {
        None
    }

    /// A value of the pod's theme scale (`$theme.<name>` in a template,
    /// feature `theme_scale`), where one is set: the construct's manifest
    /// carries it to a device, the pod reads its own. `None`: the default
    /// ([`scale_default`]).
    fn scale(&self, _name: &str) -> Option<Value> {
        None
    }
}

/// The theme scale's names and their defaults: what core components drew
/// before a pod could set them.
pub const SCALE: &[(&str, &str)] = &[
    ("corner_s", "8"),
    ("corner_m", "14"),
    ("corner_l", "18"),
    ("pad_s", "8"),
    ("pad_m", "12"),
    ("pad_l", "16"),
    ("title_weight", "\"SemiBold\""),
];

/// The default of scale value `name` (`None`: not a scale name).
pub fn scale_default(name: &str) -> Option<Value> {
    SCALE
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| serde_json::from_str(v).ok())
}

/// `v` with every string that is only `$theme.<name>` replaced by that
/// value of the scale.
fn themed(objects: &dyn Objects, v: &mut Value) {
    match v {
        Value::String(s) if s.starts_with("$theme.") => {
            let name = &s["$theme.".len()..];
            if let Some(x) = objects.scale(name).or_else(|| scale_default(name)) {
                *v = x;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|c| themed(objects, c)),
        Value::Object(o) => o.values_mut().for_each(|c| themed(objects, c)),
        _ => {}
    }
}

/// Whether `v` (an element's `node`) reads the theme scale.
pub fn uses_scale(v: &Value) -> bool {
    match v {
        Value::String(s) => s.starts_with("$theme."),
        Value::Array(a) => a.iter().any(uses_scale),
        Value::Object(o) => o.values().any(uses_scale),
        _ => false,
    }
}

/// The element a reference points at: the hash it holds, else what its
/// name resolves to ([`Objects::resolve`]).
pub fn target(objects: &dyn Objects, r: &Ref) -> Option<String> {
    r.el.clone().or_else(|| objects.resolve(&r.name))
}

/// The kind of object a construct's manifest is.
pub const MANIFEST_KIND: &str = "manifest";

/// The names of manifest `v` (`{"kind": "manifest", "home", "names"}`) and
/// the name of its home; `None` when `v` is not one.
pub fn manifest_names(v: &Value) -> Option<(String, BTreeMap<String, String>)> {
    if v["kind"] != MANIFEST_KIND {
        return None;
    }
    let names = v["names"]
        .as_object()?
        .iter()
        .filter_map(|(k, h)| Some((k.clone(), h.as_str()?.to_owned())))
        .collect();
    Some((v["home"].as_str()?.to_owned(), names))
}

/// Element `v` as a client with a manifest is sent it: every reference that
/// follows its name without the hash it was sealed with, and no examples
/// (a device expands a component, it doesn't try it). What is left changes
/// only when the element itself is edited, so its hash stays while the
/// things it references move.
pub fn stripped(v: &Value) -> Value {
    let mut out = v.clone();
    if let Some(o) = out.as_object_mut() {
        o.remove("examples");
    }
    crate::refs::map(&mut out["node"], &mut |r: &mut Ref, _| {
        if !r.pin {
            r.el = None;
        }
    });
    out
}

impl Objects for BTreeMap<String, Arc<Value>> {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.get(id).cloned()
    }
}

impl Objects for std::collections::HashMap<String, Arc<Value>> {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.get(id).cloned()
    }
}

/// An element as the renderer gets it.
#[derive(Debug, Clone, PartialEq)]
pub struct Materialized {
    /// The node, every use expanded and every edge keyed.
    pub node: Value,
    /// Its edges, in the order they appear (a key may repeat).
    pub edges: Vec<EdgeRef>,
    /// The elements it used (components), its own id not included.
    pub used: BTreeSet<String>,
    /// Objects that weren't there: drawn as a placeholder; fetch them and
    /// materialize again.
    pub missing: BTreeSet<String>,
}

/// The text a use draws when its element can't be expanded.
pub fn placeholder() -> Value {
    json!({ "type": "core://Text", "style": "Caption", "value_expr": "This part isn't available." })
}

/// The definition of a component element: `{params, children, node}`, or
/// `None` for an element without parameters or children (a part, a layout).
pub fn element_def(el: &Value) -> Result<Option<Def>, String> {
    let has_params = el.get("params").is_some_and(|p| !p.is_null());
    let has_children = el.get("children").and_then(Value::as_bool).unwrap_or(false);
    if !has_params && !has_children && el.get("doc").is_none() {
        return Ok(None);
    }
    let examples = match el.get("examples") {
        Some(Value::Array(a)) if !a.is_empty() => Value::Array(a.clone()),
        _ => json!([{ "args": {} }]),
    };
    let doc = el
        .get("doc")
        .and_then(Value::as_str)
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("(an element)");
    let raw = json!({
        "doc": doc,
        "params": el.get("params").cloned().unwrap_or(json!([])),
        "children": has_children,
        "template": el.get("node").cloned().unwrap_or(Value::Null),
        "examples": examples,
    });
    Def::parse(&raw).map(Some)
}

/// Element `id`, materialized from `objects`. `Err` only for a template that
/// can't expand (too much work, too large); a missing object is drawn as a
/// placeholder and listed in [`Materialized::missing`].
pub fn materialize(objects: &dyn Objects, id: &str) -> Result<Materialized, Error> {
    materialize_with(objects, id, &Map::new())
}

/// [`materialize`] with the element's inputs bound: each `$in.<name>` of its
/// own tree that `inputs` names reads that value (a string that is only
/// `$in.<name>` becomes the value itself), so a layout embedded or opened
/// with `bind` draws what its use site bound. Inputs it isn't given stay as
/// written.
pub fn materialize_with(
    objects: &dyn Objects,
    id: &str,
    inputs: &Map<String, Value>,
) -> Result<Materialized, Error> {
    let mut m = Mat {
        objects,
        work: Work::default(),
        edges: Vec::new(),
        used: BTreeSet::new(),
        missing: BTreeSet::new(),
        defs: BTreeMap::new(),
    };
    let Some(el) = objects.object(id) else {
        m.missing.insert(id.to_owned());
        return Ok(Materialized {
            node: placeholder(),
            edges: Vec::new(),
            used: BTreeSet::new(),
            missing: m.missing,
        });
    };
    let mut node = el.get("node").cloned().unwrap_or(Value::Null);
    if !inputs.is_empty() {
        node = crate::template::bind_inputs(&node, inputs, &mut m.work)?;
    }
    let node = m.one(&node, "/node", 0, None)?.unwrap_or(Value::Null);
    let size = serde_json::to_vec(&node).map_or(usize::MAX, |b| b.len());
    if size > MAX_EXPANDED_BYTES {
        return Err(Error::at(
            "",
            format!("the expansion is {size} bytes (max {MAX_EXPANDED_BYTES})"),
        ));
    }
    Ok(Materialized {
        node,
        edges: m.edges,
        used: m.used,
        missing: m.missing,
    })
}

struct Scope {
    args: Map<String, Value>,
    children: Vec<Value>,
    /// The use's named slots, each already expanded where it was written.
    slots: BTreeMap<String, Vec<Value>>,
}

enum Out {
    One(Value),
    Many(Vec<Value>),
    Drop,
}

type Parsed = Result<Option<Arc<Def>>, String>;

struct Mat<'a> {
    objects: &'a dyn Objects,
    work: Work,
    edges: Vec<EdgeRef>,
    used: BTreeSet<String>,
    missing: BTreeSet<String>,
    defs: BTreeMap<String, Parsed>,
}

impl Mat<'_> {
    fn one(
        &mut self,
        v: &Value,
        at: &str,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Option<Value>, Error> {
        Ok(match self.walk(v, at, depth, scope)? {
            Out::One(x) => Some(x),
            Out::Many(mut xs) if xs.len() == 1 => xs.pop(),
            Out::Many(xs) => Some(json!({ "type": "core://Column", "children": xs })),
            Out::Drop => None,
        })
    }

    fn walk(
        &mut self,
        v: &Value,
        at: &str,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Out, Error> {
        self.work.charge(match v {
            Value::Object(m) => m.keys().map(|k| k.len() + 3).sum::<usize>() + 2,
            Value::String(s) => s.len() + 2,
            _ => 4,
        })?;
        let ty = v.get("type").and_then(Value::as_str);
        if let Some(sc) = scope {
            match ty {
                Some(CHILDREN_TYPE) if v.get("slot").is_some() => {
                    // The name may come from an argument (an Each's entry).
                    let name =
                        as_text(&subst_value_in(&v["slot"], &sc.args, &mut self.work, true)?);
                    return Ok(match sc.slots.get(&name) {
                        Some(nodes) if !nodes.is_empty() => {
                            self.work.charge(nodes.iter().map(weight).sum())?;
                            Out::Many(nodes.clone())
                        }
                        _ => Out::Drop,
                    });
                }
                Some(EACH_TYPE) => {
                    let arg = v["arg"].as_str().unwrap_or("");
                    let name = v["as"].as_str().unwrap_or("item");
                    let entries = match sc.args.get(arg) {
                        Some(Value::Array(a)) => a.as_slice(),
                        _ => &[],
                    };
                    if entries.len() > MAX_EACH {
                        return Err(Error::at(
                            at,
                            format!("core://Each repeats over at most {MAX_EACH} entries"),
                        ));
                    }
                    let mut out = Vec::new();
                    for (i, entry) in entries.iter().enumerate() {
                        let mut args = sc.args.clone();
                        args.insert(name.to_owned(), entry.clone());
                        args.insert(format!("{name}_index"), json!(i));
                        if let Value::Object(fields) = entry {
                            for (k, x) in fields {
                                args.insert(format!("{name}_{k}"), x.clone());
                            }
                        }
                        self.work.charge(weight(entry))?;
                        let inner = Scope {
                            args,
                            children: sc.children.clone(),
                            slots: sc.slots.clone(),
                        };
                        match self.walk(
                            &v["template"],
                            &format!("{at}/template"),
                            depth,
                            Some(&inner),
                        )? {
                            Out::One(x) => out.push(x),
                            Out::Many(xs) => out.extend(xs),
                            Out::Drop => {}
                        }
                    }
                    return Ok(Out::Many(out));
                }
                Some(MATCH_TYPE) => {
                    let cases = v["cases"].as_object();
                    let other = v.get("else").filter(|b| !b.is_null());
                    // By an argument: one case, chosen now.
                    if let Some(arg) = v.get("arg").and_then(Value::as_str) {
                        let key = as_text(sc.args.get(arg).unwrap_or(&Value::Null));
                        let branch = cases.and_then(|c| c.get(&key)).or(other);
                        return match branch {
                            Some(b) => self.walk(b, at, depth, scope),
                            None => Ok(Out::Drop),
                        };
                    }
                    // By a binding: every case, each shown while the
                    // binding equals its key.
                    let value = subst_value_in(&v["value"], &sc.args, &mut self.work, true)?;
                    let mut out = Vec::new();
                    for (key, case) in cases.into_iter().flatten() {
                        if let Some(content) =
                            self.one(case, &format!("{at}/cases/{key}"), depth, scope)?
                        {
                            out.push(json!({ "type": "core://Conditional", "value": value,
                                "equals": key, "content": content }));
                        }
                    }
                    if let Some(b) = other
                        && let Some(mut content) =
                            self.one(b, &format!("{at}/else"), depth, scope)?
                    {
                        // None of them: a `not` test for each key, nested.
                        for key in cases.into_iter().flatten().map(|(k, _)| k).rev() {
                            content = json!({ "type": "core://Conditional", "value": value,
                                "equals": key, "not": true, "content": content });
                        }
                        out.push(content);
                    }
                    return Ok(Out::Many(out));
                }
                Some(CHILDREN_TYPE) => {
                    return Ok(match v.get("at").and_then(Value::as_u64) {
                        Some(i) => match sc.children.get(i as usize) {
                            Some(c) => {
                                self.work.charge(weight(c))?;
                                Out::One(c.clone())
                            }
                            None => Out::Drop,
                        },
                        None => {
                            self.work.charge(sc.children.iter().map(weight).sum())?;
                            Out::Many(sc.children.clone())
                        }
                    });
                }
                Some(FRAGMENT_TYPE) => {
                    let mut out = Vec::new();
                    for (i, c) in v["children"].as_array().into_iter().flatten().enumerate() {
                        match self.walk(c, &format!("{at}/children/{i}"), depth, scope)? {
                            Out::One(x) => out.push(x),
                            Out::Many(xs) => out.extend(xs),
                            Out::Drop => {}
                        }
                    }
                    return Ok(Out::Many(out));
                }
                Some(IF_ARG_TYPE) => {
                    let holds = if let Some(slot) = v.get("slot") {
                        // Whether the use passed that slot anything.
                        let slot = as_text(&subst_value_in(slot, &sc.args, &mut self.work, true)?);
                        sc.slots.get(&slot).is_some_and(|n| !n.is_empty())
                    } else {
                        let arg = v["arg"].as_str().unwrap_or("");
                        let value = sc.args.get(arg).cloned().unwrap_or(Value::Null);
                        match v.get("equals") {
                            // What it equals may itself be an argument (an
                            // Each's entry against another parameter).
                            Some(e) => {
                                let e = subst_value_in(e, &sc.args, &mut self.work, true)?;
                                as_text(&value) == as_text(&e)
                            }
                            None => truthy(&value),
                        }
                    };
                    let branch = if holds { v.get("then") } else { v.get("else") };
                    return match branch.filter(|b| !b.is_null()) {
                        Some(b) => self.walk(b, at, depth, scope),
                        None => Ok(Out::Drop),
                    };
                }
                _ => {}
            }
        }
        if let Value::Object(o) = v
            && let Some(site) = site_of(o)
        {
            if site == Site::Use {
                return self.usage(o, at, depth, scope);
            }
            return self.edge(o, at, depth, scope, site);
        }
        Ok(Out::One(match v {
            Value::Object(m) => Value::Object(self.object(m, at, depth, scope)?),
            Value::Array(a) => {
                let mut out = Vec::new();
                for (i, c) in a.iter().enumerate() {
                    match self.walk(c, &format!("{at}/{i}"), depth, scope)? {
                        Out::One(x) => out.push(x),
                        Out::Many(xs) => out.extend(xs),
                        Out::Drop => {}
                    }
                }
                Value::Array(out)
            }
            Value::String(s) => match scope {
                // A value of the theme scale, with its own type (a number
                // where a size goes).
                Some(_) if s.starts_with("$theme.") => {
                    let name = &s["$theme.".len()..];
                    self.objects
                        .scale(name)
                        .or_else(|| scale_default(name))
                        .unwrap_or_else(|| v.clone())
                }
                Some(sc) => subst_in(s, &sc.args, &mut self.work, true)?,
                None => v.clone(),
            },
            other => other.clone(),
        }))
    }

    fn object(
        &mut self,
        m: &Map<String, Value>,
        at: &str,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Map<String, Value>, Error> {
        let mut out = Map::new();
        for (k, c) in m {
            let here = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
            let key = match scope {
                Some(sc) if k.contains("$arg.") || k.contains("$in.") => {
                    as_text(&subst_in(k, &sc.args, &mut self.work, true)?)
                }
                _ => k.clone(),
            };
            if let Some(x) = self.one(c, &here, depth, scope)? {
                out.insert(key, x);
            }
        }
        Ok(out)
    }

    /// An edge: its fields walked, its `ref` replaced by `layout: <key>`.
    fn edge(
        &mut self,
        o: &Map<String, Value>,
        at: &str,
        depth: usize,
        scope: Option<&Scope>,
        site: Site,
    ) -> Result<Out, Error> {
        let raw = match scope {
            Some(sc) => subst_value_in(&o["ref"], &sc.args, &mut self.work, true)?,
            None => o["ref"].clone(),
        };
        let mut rest = o.clone();
        rest.remove("ref");
        let mut out = self.object(&rest, at, depth, scope)?;
        // The target's bindings go with the edge, not to the renderer.
        let bind = match out.remove(BIND_FIELD) {
            Some(Value::Object(b)) => b,
            _ => Map::new(),
        };
        if let Some(r) = Ref::parse(&raw) {
            out.insert("layout".into(), json!(r.key()));
            self.edges.push(EdgeRef {
                key: r.key().to_owned(),
                name: r.name.clone(),
                el: target(self.objects, &r),
                site,
                bind,
            });
        }
        Ok(Out::One(Value::Object(out)))
    }

    fn def(&mut self, id: &str, el: &Value) -> Parsed {
        if !self.defs.contains_key(id) {
            let d = element_def(el).map(|d| d.map(Arc::new));
            self.defs.insert(id.to_owned(), d);
        }
        self.defs[id].clone()
    }

    /// A `core://Use` with a `ref`.
    fn usage(
        &mut self,
        o: &Map<String, Value>,
        at: &str,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Out, Error> {
        if depth >= MAX_DEPTH {
            return Ok(Out::One(placeholder()));
        }
        // A use inside a template passes the template's arguments on.
        let resolved;
        let o = match scope {
            Some(sc) => {
                let mut r = o.clone();
                for (k, x) in r.iter_mut() {
                    if k != "children" && k != "slots" {
                        *x = subst_value_in(x, &sc.args, &mut self.work, true)?;
                        // A template may pass a value of the theme scale on.
                        themed(self.objects, x);
                    }
                }
                resolved = r;
                &resolved
            }
            None => o,
        };
        let Some(r) = Ref::parse(&o["ref"]) else {
            return Ok(Out::One(placeholder()));
        };
        let Some(id) = target(self.objects, &r) else {
            return Ok(Out::One(placeholder()));
        };
        let Some(el) = self.objects.object(&id) else {
            self.missing.insert(id);
            return Ok(Out::One(placeholder()));
        };
        self.used.insert(id.clone());
        let def = match self.def(&id, &el) {
            Ok(d) => d,
            Err(_) => return Ok(Out::One(placeholder())),
        };
        let args = match o.get("args").or_else(|| o.get("bind")) {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Ok(Out::One(placeholder())),
        };
        let args = match &def {
            Some(d) => match d.bind(&args) {
                Ok(a) => a,
                Err(_) => return Ok(Out::One(placeholder())),
            },
            None => args,
        };
        let mut children = Vec::new();
        if let Some(Value::Array(a)) = o.get("children") {
            for (i, c) in a.iter().enumerate() {
                match self.walk(c, &format!("{at}/children/{i}"), depth, scope)? {
                    Out::One(x) => children.push(x),
                    Out::Many(xs) => children.extend(xs),
                    Out::Drop => {}
                }
            }
        }
        // Named slots, each expanded where the use is written, like its
        // children.
        let mut slots = BTreeMap::new();
        if let Some(Value::Object(named)) = o.get("slots") {
            for (name, nodes) in named {
                let mut list = Vec::new();
                for (i, c) in nodes.as_array().into_iter().flatten().enumerate() {
                    match self.walk(c, &format!("{at}/slots/{name}/{i}"), depth, scope)? {
                        Out::One(x) => list.push(x),
                        Out::Many(xs) => list.extend(xs),
                        Out::Drop => {}
                    }
                }
                slots.insert(name.clone(), list);
            }
        }
        // Borrowed, not cloned: a large component used many times would be
        // copied whole at each use, even where it takes a small branch.
        let node = el.get("node").unwrap_or(&Value::Null);
        let inner = Scope {
            args,
            children,
            slots,
        };
        let mut out = self.walk(node, at, depth + 1, Some(&inner))?;
        // A whole layout used at a root keeps the user's id and title.
        if let Out::One(Value::Object(x)) = &mut out {
            for k in ["id", "title"] {
                if let Some(v) = o.get(k) {
                    x.insert(k.to_owned(), v.clone());
                }
            }
        }
        Ok(out)
    }
}

/// What one key of the construct draws: its element and the inputs its use
/// site bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyed {
    pub el: String,
    pub inputs: Map<String, Value>,
}

/// The id a materialized element is kept under: its own id, or with bound
/// inputs, its id and their canonical JSON (the same element bound two ways
/// draws two things).
pub fn instance_id(el: &str, inputs: &Map<String, Value>) -> String {
    if inputs.is_empty() {
        return el.to_owned();
    }
    format!(
        "{el}?{}",
        crate::canonical::canonical(&Value::Object(inputs.clone()))
    )
}

/// Every key reachable from `root` (keyed `root_key`), breadth first: the
/// element each draws and the inputs bound at the first use site that
/// reaches it (decision `content-addressed-construct`). Elements not in
/// `objects` are left out.
pub fn keyed(objects: &dyn Objects, root_key: &str, root: &str) -> BTreeMap<String, Keyed> {
    let mut out = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([(
        root_key.to_owned(),
        Keyed {
            el: root.to_owned(),
            inputs: Map::new(),
        },
    )]);
    while let Some((key, k)) = queue.pop_front() {
        if out.contains_key(&key) || objects.object(&k.el).is_none() {
            continue;
        }
        if let Ok(m) = materialize_with(objects, &k.el, &k.inputs) {
            for e in m.edges {
                if let Some(el) = e.el
                    && !out.contains_key(&e.key)
                {
                    queue.push_back((e.key, Keyed { el, inputs: e.bind }));
                }
            }
        }
        out.insert(key, k);
    }
    out
}

/// The edge graph of materialized elements: each key, the keys it reaches —
/// its own edges and, through every embed (`core://Layout`), the edges of
/// what it embeds. A stack entry still resolves while the entry below it
/// reaches its key.
pub fn reach(direct: &BTreeMap<String, Vec<EdgeRef>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for key in direct.keys() {
        let mut seen = BTreeSet::new();
        let mut embedded = vec![key.clone()];
        let mut visited = BTreeSet::new();
        while let Some(k) = embedded.pop() {
            if !visited.insert(k.clone()) {
                continue;
            }
            for e in direct.get(&k).into_iter().flatten() {
                seen.insert(e.key.clone());
                if e.site == Site::Embed {
                    embedded.push(e.key.clone());
                }
            }
        }
        out.insert(key.clone(), seen);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::object_id;

    fn store(objs: &[Value]) -> (BTreeMap<String, Arc<Value>>, Vec<String>) {
        let mut m = BTreeMap::new();
        let mut ids = Vec::new();
        for o in objs {
            let id = object_id(o);
            m.insert(id.clone(), Arc::new(o.clone()));
            ids.push(id);
        }
        (m, ids)
    }

    #[test]
    fn expands_components_and_keys_edges() {
        let card = json!({"kind": "element", "doc": "A card",
            "params": [{"name": "title", "type": "text", "required": true}], "children": true,
            "node": {"type": "core://Column", "children": [
                {"type": "core://Text", "value_expr": "$in.title"},
                {"type": "core://Children"}]}});
        let (mut objs, ids) = store(&[card]);
        let page = json!({"kind": "element", "title": "Home", "node": {"type": "core://Page", "id": "home",
            "root": {"type": "core://Use", "ref": {"name": "@me/card", "el": ids[0]},
                "args": {"title": "Hi"}, "children": [
                    {"type": "core://Button", "label": "Go", "on_click": {"intent": "action.navigation.open",
                        "ref": {"name": "c-a", "el": "e1-target"}, "presentation": "Push"}},
                    {"type": "core://Layout", "ref": {"name": "@core/inbox", "as": "inbox"}}]}}});
        let pid = object_id(&page);
        objs.insert(pid.clone(), Arc::new(page));
        let m = materialize(&objs, &pid).unwrap();
        assert_eq!(m.node["root"]["children"][0]["value_expr"], json!("Hi"));
        assert_eq!(
            m.node["root"]["children"][1]["on_click"]["layout"],
            json!("c-a")
        );
        assert!(
            m.node["root"]["children"][1]["on_click"]
                .get("ref")
                .is_none()
        );
        assert_eq!(m.node["root"]["children"][2]["layout"], json!("inbox"));
        assert_eq!(m.edges.len(), 2);
        assert_eq!(m.edges[0].site, Site::Link);
        assert_eq!(m.edges[1].site, Site::Embed);
        assert!(m.used.contains(&ids[0]));
        assert!(m.missing.is_empty());
    }

    #[test]
    fn a_json_parameter_puts_an_intent_or_a_query_in_whole() {
        let link = json!({"kind": "element", "doc": "A text link",
            "params": [{"name": "label", "type": "text", "required": true},
                {"name": "on_click", "type": "json", "required": true}],
            "node": {"type": "core://Tappable", "on_click": "$arg.on_click",
                "content": {"type": "core://Text", "value_expr": "$arg.label"}}});
        let empty = json!({"kind": "element", "doc": "What an empty list says",
            "params": [{"name": "text", "type": "text", "required": true},
                {"name": "count", "type": "json", "required": true}],
            "node": {"type": "core://Conditional", "count": "$arg.count", "not": true,
                "content": {"type": "core://Text", "value_expr": "$arg.text"}}});
        let (mut objs, ids) = store(&[link, empty]);
        let open = json!({"intent": "action.link.open", "url": "$item.url"});
        let count = json!({"query_key": "collection:log", "where": [{"field": "private", "equals": "false"}]});
        let page = json!({"kind": "element", "node": {"type": "core://Column", "children": [
            {"type": "core://Use", "ref": {"name": "@me/text_link", "el": ids[0]},
                "args": {"label": "Open", "on_click": open}},
            {"type": "core://Use", "ref": {"name": "@me/empty_note", "el": ids[1]},
                "args": {"text": "Nothing yet", "count": count}},
            // Null is no argument: a required one is missing, so a placeholder.
            {"type": "core://Use", "ref": {"name": "@me/text_link", "el": ids[0]},
                "args": {"label": "Open", "on_click": null}}]}});
        let pid = object_id(&page);
        objs.insert(pid.clone(), Arc::new(page));
        let m = materialize(&objs, &pid).unwrap();
        let kids = &m.node["children"];
        assert_eq!(kids[0]["on_click"], open);
        assert_eq!(kids[0]["content"]["value_expr"], json!("Open"));
        assert_eq!(kids[1]["count"], count);
        assert_eq!(kids[2], placeholder());
    }

    /// The template directives of feature `component_templates` (decision
    /// `element-roles-and-reuse`).
    #[test]
    fn a_template_repeats_chooses_and_takes_named_slots() {
        let tabs = json!({"kind": "element", "doc": "Tabs",
            "params": [{"name": "tabs", "type": "json", "required": true},
                {"name": "state", "type": "binding", "required": true},
                {"name": "look", "type": "text", "default": "bar"}],
            "children": true,
            "node": {"type": "core://Column", "children": [
                // One body per tab, shown while the state names it.
                {"type": "core://Each", "arg": "tabs", "as": "tab", "template":
                    {"type": "core://Conditional", "value": "$arg.state", "equals": "$arg.tab_key",
                        "content": {"type": "core://Children", "slot": "$arg.tab_key"}}},
                // One button per tab.
                {"type": "core://Row", "children": [
                    {"type": "core://Each", "arg": "tabs", "as": "tab", "template":
                        {"type": "core://Button", "label": "$arg.tab_title", "tag": "$arg.tab_index"}}]},
                {"type": "core://Match", "arg": "look", "cases": {
                    "bar": {"type": "core://Text", "value_expr": "a bar"},
                    "rail": {"type": "core://Text", "value_expr": "a rail"}},
                    "else": {"type": "core://Text", "value_expr": "plain"}},
                {"type": "core://Match", "value": "$arg.state", "cases": {
                    "a": {"type": "core://Text", "value_expr": "on a"},
                    "b": {"type": "core://Text", "value_expr": "on b"}},
                    "else": {"type": "core://Text", "value_expr": "nowhere"}},
                {"type": "core://IfArg", "slot": "footer", "then":
                    {"type": "core://Box", "content": {"type": "core://Children", "slot": "footer"}}},
                {"type": "core://IfArg", "slot": "header", "then":
                    {"type": "core://Text", "value_expr": "has a header"}}]}});
        let (mut objs, ids) = store(&[tabs]);
        let page = json!({"kind": "element", "node": {"type": "core://Use",
            "ref": {"name": "@me/tabs", "el": ids[0]},
            "args": {"state": "${local://nav_tabs#tab}", "look": "rail",
                "tabs": [{"key": "a", "title": "First"}, {"key": "b", "title": "Second"}]},
            "slots": {
                "a": [{"type": "core://Layout", "ref": {"name": "c-first", "el": "e1-first"}}],
                "b": [{"type": "core://Text", "value_expr": "Second body"}],
                "footer": [{"type": "core://Text", "value_expr": "Foot"}]}}});
        let pid = object_id(&page);
        objs.insert(pid.clone(), Arc::new(page));
        let m = materialize(&objs, &pid).unwrap();
        let kids = m.node["children"].as_array().unwrap();
        // Each: a body per tab, its slot inside; the layout in a slot is an
        // edge like any other.
        assert_eq!(kids[0]["type"], "core://Conditional");
        assert_eq!(kids[0]["value"], "${local://nav_tabs#tab}");
        assert_eq!(kids[0]["equals"], "a");
        assert_eq!(kids[0]["content"]["layout"], "c-first");
        assert_eq!(kids[1]["equals"], "b");
        assert_eq!(kids[1]["content"]["value_expr"], "Second body");
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].key, "c-first");
        // Each inside a list splices its copies in; the index is a number.
        let buttons = kids[2]["children"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[1]["label"], "Second");
        assert_eq!(buttons[1]["tag"], json!(1));
        // Match by an argument: one case, now.
        assert_eq!(kids[3]["value_expr"], "a rail");
        // Match by a binding: a conditional per case, then the else under a
        // `not` for each key.
        assert_eq!(kids[4]["equals"], "a");
        assert_eq!(kids[4]["content"]["value_expr"], "on a");
        assert_eq!(kids[5]["equals"], "b");
        assert_eq!(kids[6]["not"], true);
        assert_eq!(kids[6]["equals"], "a");
        assert_eq!(kids[6]["content"]["equals"], "b");
        assert_eq!(kids[6]["content"]["content"]["value_expr"], "nowhere");
        // A slot the use filled is drawn, one it left out is not.
        assert_eq!(kids[7]["content"]["value_expr"], "Foot");
        assert_eq!(kids.len(), 8);
        // Nothing of the directives is left for a renderer.
        let text = m.node.to_string();
        for gone in [
            "core://Each",
            "core://Match",
            "core://Children",
            "core://IfArg",
            "slots",
        ] {
            assert!(!text.contains(gone), "{gone}: {text}");
        }
        assert!(crate::def::uses_templates(
            objs[&ids[0]].get("node").unwrap()
        ));
        assert!(crate::def::uses_templates(objs[&pid].get("node").unwrap()));
        assert!(!crate::def::uses_templates(
            &json!({"type": "core://Children", "at": 0})
        ));
    }

    #[test]
    fn each_is_bounded_and_match_falls_back() {
        let list = json!({"kind": "element", "doc": "A list",
            "params": [{"name": "items", "type": "json", "required": true},
                {"name": "kind", "type": "text", "default": ""}],
            "node": {"type": "core://Column", "children": [
                {"type": "core://Each", "arg": "items", "as": "it", "template":
                    {"type": "core://Text", "value_expr": "$arg.it"}},
                {"type": "core://Match", "arg": "kind", "cases": {"x": {"type": "core://Spacer"}}}]}});
        let (mut objs, ids) = store(&[list]);
        let page = |items: Value| {
            json!({"kind": "element", "node": {"type": "core://Use",
            "ref": {"name": "@me/list", "el": ids[0]}, "args": {"items": items}}})
        };
        // Plain values repeat as themselves; a Match with no case and no
        // else leaves nothing.
        let ok = page(json!(["one", "two"]));
        let id = object_id(&ok);
        objs.insert(id.clone(), Arc::new(ok));
        let m = materialize(&objs, &id).unwrap();
        assert_eq!(
            m.node["children"],
            json!([
            {"type": "core://Text", "value_expr": "one"},
            {"type": "core://Text", "value_expr": "two"}])
        );
        // Not a list: nothing.
        let none = page(json!("oops"));
        let id = object_id(&none);
        objs.insert(id.clone(), Arc::new(none));
        assert_eq!(materialize(&objs, &id).unwrap().node["children"], json!([]));
        // Too many entries is refused rather than drawn.
        let many = page(json!((0..=crate::def::MAX_EACH).collect::<Vec<_>>()));
        let id = object_id(&many);
        objs.insert(id.clone(), Arc::new(many));
        assert!(materialize(&objs, &id).is_err());
    }

    #[test]
    fn a_missing_component_is_a_placeholder_and_listed() {
        let page = json!({"kind": "element", "node": {"type": "core://Use",
            "ref": {"name": "@me/x", "el": "e1-gone"}}});
        let (objs, ids) = store(&[page]);
        let m = materialize(&objs, &ids[0]).unwrap();
        assert_eq!(m.node, placeholder());
        assert!(m.missing.contains("e1-gone"));
    }

    #[test]
    fn reach_goes_through_embeds() {
        let e = |key: &str, site| EdgeRef {
            key: key.into(),
            name: key.into(),
            el: None,
            site,
            bind: Map::new(),
        };
        let mut direct = BTreeMap::new();
        direct.insert(
            "home".into(),
            vec![e("feed", Site::Embed), e("a", Site::Link)],
        );
        direct.insert("feed".into(), vec![e("post", Site::Link)]);
        direct.insert("a".into(), vec![e("b", Site::Link)]);
        let r = reach(&direct);
        assert!(r["home"].contains("post"));
        assert!(r["home"].contains("a"));
        assert!(!r["home"].contains("b"));
    }
}
