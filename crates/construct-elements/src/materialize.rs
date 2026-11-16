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

use crate::def::{CHILDREN_TYPE, Def, FRAGMENT_TYPE, IF_ARG_TYPE};
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
                    let arg = v["arg"].as_str().unwrap_or("");
                    let value = sc.args.get(arg).cloned().unwrap_or(Value::Null);
                    let holds = match v.get("equals") {
                        Some(e) => as_text(&value) == as_text(e),
                        None => truthy(&value),
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
                el: r.el.clone(),
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
                    if k != "children" {
                        *x = subst_value_in(x, &sc.args, &mut self.work, true)?;
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
        let Some(id) = r.el.clone() else {
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
        let node = el.get("node").cloned().unwrap_or(Value::Null);
        let inner = Scope { args, children };
        let mut out = self.walk(&node, at, depth + 1, Some(&inner))?;
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
