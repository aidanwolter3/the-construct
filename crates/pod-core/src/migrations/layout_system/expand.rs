//! The old system's expansion of a stored layout (decision
//! `layout-parts`, decision `plumbing-and-porcelain`), kept only for the
//! dry run of a conversion: what a pod that wasn't switched drew before.
//! `core://Use` names a component (`core:<name>`, `pod:<name>`, a version
//! or the newest), a blueprint (`core:<app>`) or one of its parts
//! (`core:<app>/<part>`, as `core://Ref {app, part}` does), all read from
//! the old tables ([`super::tables::Old`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::tables::Old;
use crate::adapter::Catalog;
use crate::components::{CHILDREN_TYPE, CORE, Def, FRAGMENT_TYPE, IF_ARG_TYPE, POD, USE_TYPE};
use crate::validate::LayoutError;
use construct_elements::template::{
    MAX_EXPANDED_BYTES, Work, as_text, subst, subst_value, truthy, weight,
};

/// The old reference node.
pub const REF_TYPE: &str = "core://Ref";
/// The part that is a blueprint's whole layout.
pub const LAYOUT_PART: &str = "layout";
/// Uses inside parts and components are expanded this deep at most.
pub const MAX_DEPTH: usize = 8;

/// One built-in version's parts by name.
#[derive(Debug, Clone, Default)]
pub struct Parts {
    pub version: i64,
    pub parts: BTreeMap<String, Value>,
}

impl Parts {
    /// The parts of a layout (its screens and slots) and the declared ones.
    pub fn of(version: i64, layout: &Value, declared: Option<&Value>) -> Self {
        let mut parts = BTreeMap::new();
        parts.insert(LAYOUT_PART.to_owned(), layout.clone());
        for s in layout["screens"].as_array().into_iter().flatten() {
            if let Some(id) = s["id"].as_str() {
                parts.insert(format!("screen:{id}"), s.clone());
            }
        }
        for s in layout["slots"].as_array().into_iter().flatten() {
            if let Some(id) = s["slot_id"].as_str() {
                parts.insert(format!("slot:{id}"), s["content"].clone());
            }
        }
        for (k, v) in declared.and_then(Value::as_object).into_iter().flatten() {
            parts.insert(k.clone(), v.clone());
        }
        Self { version, parts }
    }
}

/// A reference found in a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartRef {
    pub app: String,
    pub part: String,
    pub version: Option<i64>,
}

fn as_ref(v: &Value) -> Option<PartRef> {
    let o = v.as_object()?;
    match o.get("type")?.as_str()? {
        REF_TYPE => Some(PartRef {
            app: o.get("app")?.as_str()?.to_owned(),
            part: o.get("part")?.as_str()?.to_owned(),
            version: o.get("version").and_then(Value::as_i64),
        }),
        USE_TYPE => {
            let name = o.get("component")?.as_str()?.strip_prefix(CORE)?;
            let (app, part) = name.split_once('/').unwrap_or((name, LAYOUT_PART));
            Some(PartRef {
                app: app.to_owned(),
                part: part.to_owned(),
                version: o.get("version").and_then(Value::as_i64),
            })
        }
        _ => None,
    }
}

/// Whether `json` (a stored layout) may hold a reference or a component
/// use (cheap check).
pub fn may_hold_refs(json: &str) -> bool {
    json.contains(REF_TYPE) || json.contains(USE_TYPE)
}

/// A blueprint version's parts: version `version`, else its newest.
pub fn parts(old: &Old, app: &str, version: Option<i64>) -> Option<Parts> {
    let (n, v) = old.version(app, version)?;
    let layout: Value = serde_json::from_str(&v.json).unwrap_or(Value::Null);
    let declared: Option<Value> = v
        .meta_json
        .as_deref()
        .and_then(|m| serde_json::from_str::<Value>(m).ok())
        .map(|m| m["parts"].clone());
    Some(Parts::of(n, &layout, declared.as_ref()))
}

/// How [`expand`] treats a reference it can't resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Refuse the layout (a write): the error names the ref.
    Strict,
    /// Serve a placeholder (a screen part is left out).
    Lenient,
}

fn placeholder() -> Value {
    json!({ "type": "core://Text", "style": "Caption", "value_expr": "This part isn't available." })
}

/// The arguments and children a component's template is expanded with.
struct Scope {
    name: String,
    args: Map<String, Value>,
    /// Already expanded.
    children: Vec<Value>,
}

/// What one value expands to.
enum Out {
    One(Value),
    /// Children spliced into a list.
    Many(Vec<Value>),
    /// Nothing (an unresolved screen part, an `IfArg` without a branch).
    Drop,
}

/// A definition as parsed (`Err`: why it is invalid).
type Parsed = Result<Arc<Def>, String>;

struct Expander<'a> {
    old: &'a Old,
    catalog: &'a Catalog,
    mode: Mode,
    cache: HashMap<(String, Option<i64>), Option<Parts>>,
    /// Each definition read and parsed once per expansion.
    defs: HashMap<(String, Option<i64>), Option<Parsed>>,
    work: Work,
}

fn err(at: &str, message: String) -> LayoutError {
    LayoutError {
        pointer: at.to_owned(),
        message,
        allowed: Vec::new(),
    }
}

impl Expander<'_> {
    fn part(&mut self, r: &PartRef) -> Option<Value> {
        let key = (r.app.clone(), r.version);
        if !self.cache.contains_key(&key) {
            let parts = if self.catalog.is_blueprint(&r.app) {
                parts(self.old, &r.app, r.version)
            } else {
                None
            };
            self.cache.insert(key.clone(), parts);
        }
        self.cache[&key]
            .as_ref()
            .and_then(|p| p.parts.get(&r.part).cloned())
    }

    /// A component's definition, parsed (`Err`: why it is invalid).
    fn def(&mut self, name: &str, version: Option<i64>) -> Option<Parsed> {
        let key = (name.to_owned(), version);
        if !self.defs.contains_key(&key) {
            let parsed = self
                .old
                .component(name, version)
                .map(|(_, raw)| Def::parse(&raw).map(Arc::new));
            self.defs.insert(key.clone(), parsed);
        }
        self.defs[&key].clone()
    }

    /// A use that can't be expanded: an error (strict) or a placeholder
    /// (lenient; a screen part is dropped).
    fn unresolved(
        &self,
        at: &str,
        what: &str,
        why: &str,
        in_screens: bool,
    ) -> Result<Out, LayoutError> {
        match self.mode {
            Mode::Strict => Err(err(at, format!("{what}: {why}"))),
            Mode::Lenient if in_screens => Ok(Out::Drop),
            Mode::Lenient => Ok(Out::One(placeholder())),
        }
    }

    fn one(
        &mut self,
        v: &Value,
        at: &str,
        in_screens: bool,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Option<Value>, LayoutError> {
        Ok(match self.walk(v, at, in_screens, depth, scope)? {
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
        in_screens: bool,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Out, LayoutError> {
        // Every visit costs something, so uses that expand to nothing still
        // add up.
        self.work.charge(match v {
            Value::Object(m) => m.keys().map(|k| k.len() + 3).sum::<usize>() + 2,
            Value::String(s) => s.len() + 2,
            _ => 4,
        })?;
        let ty = v.get("type").and_then(Value::as_str);
        // Template-only nodes.
        if let Some(sc) = scope {
            match ty {
                Some(CHILDREN_TYPE) => {
                    // A copy of the children: a template that names them
                    // twice doubles them.
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
                        match self.walk(
                            c,
                            &format!("{at}/children/{i}"),
                            in_screens,
                            depth,
                            scope,
                        )? {
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
                        Some(b) => self.walk(b, at, in_screens, depth, scope),
                        None => Ok(Out::Drop),
                    };
                }
                _ => {}
            }
        } else if matches!(ty, Some(CHILDREN_TYPE | IF_ARG_TYPE | FRAGMENT_TYPE)) {
            return self.unresolved(
                at,
                ty.unwrap_or(""),
                "only goes in a component's template",
                in_screens,
            );
        }
        if matches!(ty, Some(REF_TYPE | USE_TYPE)) {
            return self.usage(v, at, in_screens, depth, scope);
        }
        Ok(Out::One(match v {
            Value::Object(m) => {
                let mut out = Map::new();
                for (k, c) in m {
                    let here = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                    let screens = k == "screens" && c.is_array();
                    // A template may name a key by an argument too.
                    let key = match scope {
                        Some(sc) if k.contains("$arg.") => {
                            as_text(&subst(k, &sc.args, &mut self.work)?)
                        }
                        _ => k.clone(),
                    };
                    if let Some(x) = self.one(c, &here, screens, depth, scope)? {
                        out.insert(key, x);
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => {
                let mut out = Vec::new();
                for (i, c) in a.iter().enumerate() {
                    match self.walk(c, &format!("{at}/{i}"), in_screens, depth, scope)? {
                        Out::One(x) => out.push(x),
                        Out::Many(xs) => out.extend(xs),
                        Out::Drop => {}
                    }
                }
                Value::Array(out)
            }
            Value::String(s) => match scope {
                Some(sc) => subst(s, &sc.args, &mut self.work)?,
                None => v.clone(),
            },
            other => other.clone(),
        }))
    }

    /// A `core://Use` or `core://Ref`.
    fn usage(
        &mut self,
        v: &Value,
        at: &str,
        in_screens: bool,
        depth: usize,
        scope: Option<&Scope>,
    ) -> Result<Out, LayoutError> {
        let o = v.as_object().expect("a typed node is an object");
        let label = match o.get("component").and_then(Value::as_str) {
            Some(c) => format!("core://Use of '{c}'"),
            None => format!(
                "core://Ref to '{}' part '{}'",
                o.get("app").and_then(Value::as_str).unwrap_or(""),
                o.get("part").and_then(Value::as_str).unwrap_or("")
            ),
        };
        if depth >= MAX_DEPTH {
            return self.unresolved(at, &label, "uses nest too deep", in_screens);
        }
        // A use inside a template may pass the template's own arguments on
        // (its children are walked in the template's scope below).
        let resolved;
        let v = match scope {
            Some(sc) => {
                let mut r = v.clone();
                if let Some(o) = r.as_object_mut() {
                    for (k, x) in o.iter_mut() {
                        if k != "children" {
                            *x = subst_value(x, &sc.args, &mut self.work)?;
                        }
                    }
                }
                resolved = r;
                &resolved
            }
            None => v,
        };
        let o = v.as_object().expect("still an object");
        let name = o.get("component").and_then(Value::as_str);
        // A blueprint's layout or part (`core:<app>[/<part>]`, `core://Ref`).
        let part = match (o.get("type").and_then(Value::as_str), name) {
            (Some(REF_TYPE), _) => as_ref(v),
            (_, Some(n)) => n.strip_prefix(CORE).and_then(|bare| {
                let app = bare.split('/').next().unwrap_or(bare);
                (bare.contains('/') || self.catalog.is_blueprint(app))
                    .then(|| as_ref(v))
                    .flatten()
            }),
            _ => None,
        };
        if let Some(r) = part {
            return self.expand_part(v, &r, &label, at, in_screens, depth);
        }
        let Some(name) = name else {
            return self.unresolved(
                at,
                &label,
                "core://Use needs \"component\" (core:<name> or pod:<name>)",
                in_screens,
            );
        };
        if !(name.starts_with(CORE) || name.starts_with(POD)) {
            return self.unresolved(
                at,
                &label,
                "components are named core:<name> or pod:<name>",
                in_screens,
            );
        }
        if in_screens {
            return self.unresolved(
                at,
                &label,
                "components are node trees; screens hold screen parts (core:<app>/screen:<id>)",
                in_screens,
            );
        }
        let version = o.get("version").and_then(Value::as_i64);
        let found = self.def(name, version);
        let Some(parsed) = found else {
            let why = match version {
                Some(n) => format!("no version {n} (list_components lists them)"),
                None => "no such component (list_components lists them)".to_owned(),
            };
            return self.unresolved(at, &label, &why, in_screens);
        };
        let def = match parsed {
            Ok(d) => d,
            Err(e) => {
                return self.unresolved(
                    at,
                    &label,
                    &format!("its definition is invalid: {e}"),
                    in_screens,
                );
            }
        };
        let args = match o.get("args") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return self.unresolved(at, &label, "\"args\" is an object", in_screens),
        };
        let args = match def.bind(&args) {
            Ok(a) => a,
            Err(e) => return self.unresolved(&format!("{at}/args"), &label, &e, in_screens),
        };
        let mut children = Vec::new();
        match o.get("children") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                if !def.children && !a.is_empty() {
                    return self.unresolved(
                        at,
                        &label,
                        "this component takes no children",
                        in_screens,
                    );
                }
                for (i, c) in a.iter().enumerate() {
                    // In the caller's scope: a template passes its own
                    // children and arguments on.
                    match self.walk(c, &format!("{at}/children/{i}"), false, depth, scope)? {
                        Out::One(x) => children.push(x),
                        Out::Many(xs) => children.extend(xs),
                        Out::Drop => {}
                    }
                }
            }
            Some(_) => {
                return self.unresolved(at, &label, "\"children\" is a list of nodes", in_screens);
            }
        }
        let inner = Scope {
            name: name.to_owned(),
            args,
            children,
        };
        let out = self.walk(&def.template, at, false, depth + 1, Some(&inner));
        out.map_err(|mut e| {
            if !e.message.contains(&inner.name) {
                e.message = format!("in {}: {}", inner.name, e.message);
            }
            e
        })
    }

    fn expand_part(
        &mut self,
        v: &Value,
        r: &PartRef,
        label: &str,
        at: &str,
        in_screens: bool,
        depth: usize,
    ) -> Result<Out, LayoutError> {
        if r.part == LAYOUT_PART && (!at.is_empty() || in_screens) {
            return self.unresolved(
                at,
                label,
                "a whole layout can only be referenced at the root",
                in_screens,
            );
        }
        let screen_part = r.part.starts_with("screen:");
        if screen_part != in_screens {
            let why = if in_screens {
                "only screen parts (screen:<id>) go in screens"
            } else {
                "a screen part (screen:<id>) goes in screens"
            };
            return self.unresolved(at, label, why, in_screens);
        }
        let found = self.part(r);
        let Some(part) = found else {
            let why = if self.catalog.is_blueprint(&r.app) {
                "no such part (list_parts lists them)"
            } else {
                "only built-in apps' parts can be referenced"
            };
            return self.unresolved(at, label, why, in_screens);
        };
        let mut out = self.walk(&part, at, in_screens, depth + 1, None)?;
        // A whole layout keeps the referencing app's id (and title, when
        // the reference names one).
        if r.part == LAYOUT_PART
            && let (Out::One(Value::Object(o)), Some(from)) = (&mut out, v.as_object())
        {
            for k in ["id", "title"] {
                if let Some(x) = from.get(k) {
                    o.insert(k.to_owned(), x.clone());
                }
            }
        }
        Ok(out)
    }
}

/// `doc` with every reference and component use replaced by what it
/// expands to (parts and components at their newest, or the pinned
/// version).
pub fn expand(old: &Old, catalog: &Catalog, doc: &Value, mode: Mode) -> Result<Value, LayoutError> {
    let mut e = Expander {
        old,
        catalog,
        mode,
        cache: HashMap::new(),
        defs: HashMap::new(),
        work: Work::default(),
    };
    let out = e.one(doc, "", false, 0, None)?.unwrap_or(Value::Null);
    let size = serde_json::to_vec(&out).map_or(usize::MAX, |b| b.len());
    if size > MAX_EXPANDED_BYTES {
        return Err(err(
            "",
            format!("the expansion is {size} bytes (max {MAX_EXPANDED_BYTES})"),
        ));
    }
    Ok(out)
}

/// A stored layout as the old system served it to a client that draws the
/// plumbing: uses expanded leniently, a `core://Workspace` as the
/// `core://Page` it stands for.
pub fn served(old: &Old, catalog: &Catalog, json: &str) -> Value {
    let Ok(doc) = serde_json::from_str::<Value>(json) else {
        return Value::Null;
    };
    let expanded = if may_hold_refs(json) {
        expand(old, catalog, &doc, Mode::Lenient).unwrap_or(doc)
    } else {
        doc
    };
    if let Some(page) = crate::porcelain::page_of(&expanded)
        && let Ok(v) = expand(old, catalog, &page, Mode::Lenient)
    {
        return v;
    }
    expanded
}
