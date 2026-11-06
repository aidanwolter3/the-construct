//! Built-in parts (decision `layout-parts`): pieces of built-in apps that
//! custom layouts reuse by reference, so they stay current.
//!
//! A built-in app's parts, per version: every screen (`screen:<id>`), every
//! slot's content (`slot:<id>`), and the named parts its layout file declares
//! (`"parts": {"post_card": <node>}`). A custom layout names one with a
//! pod-only node, `{"type": "core://Ref", "app": "<built-in app>", "part":
//! "<name>", "version"?: n}`: in `screens` a screen part, anywhere else a
//! node part. Without `version` it follows the app: the pod expands it when
//! serving, from the newest version of that app the client can draw, and a
//! built-in deploy appends a `ref_update` version to the custom apps that
//! reference it ([`crate::custom::ref_updates`]). Clients never see a ref;
//! one that can't be expanded is served as a short "This part isn't
//! available" text (a screen part is left out).
//!
//! The same expansion runs components (decision `plumbing-and-porcelain`,
//! [`crate::components`]): `core://Use` names a component, `core:<app>`
//! (a whole blueprint) or `core:<app>/<part>` (a part, as `core://Ref`
//! does, which stays as an alias), with `args` and `children`.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

use protocol::ClientCaps;

use crate::adapter::Catalog;
use crate::components::{CHILDREN_TYPE, CORE, Def, FRAGMENT_TYPE, IF_ARG_TYPE, POD, USE_TYPE};
use crate::store::Store;
use crate::validate::LayoutError;

/// The node type of a reference.
pub const REF_TYPE: &str = "core://Ref";
/// The part that is a blueprint's whole layout (decision
/// `blueprints-and-home`): only a layout's root may reference it, and the
/// expansion keeps the referencing app's own id.
pub const LAYOUT_PART: &str = "layout";
/// Uses inside parts and components are expanded this deep at most.
pub const MAX_DEPTH: usize = 8;
/// Largest expansion served (a write is held to
/// [`crate::validate::MAX_LAYOUT_BYTES`]).
pub const MAX_EXPANDED_BYTES: usize = 1024 * 1024;
/// The most an expansion may build along the way, in approximate JSON
/// bytes: every value it visits or copies (a component's children, an
/// argument) counts. Checked as it goes, so a component that repeats its
/// children or arguments can't multiply a use into gigabytes before
/// [`MAX_EXPANDED_BYTES`] is checked at the end.
pub const MAX_EXPANSION_WORK: usize = 4 * MAX_EXPANDED_BYTES;

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

/// A use of a component in a layout: `(name, pinned version)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentUse {
    pub name: String,
    pub version: Option<i64>,
}

/// Every `core://Use` of a component in `doc` (blueprint uses included:
/// `core:<app>` names may be either; see [`refs`]).
pub fn uses(doc: &Value) -> Vec<ComponentUse> {
    let mut out = Vec::new();
    collect_uses(doc, &mut out);
    out
}

fn collect_uses(v: &Value, out: &mut Vec<ComponentUse>) {
    match v {
        Value::Object(m) => {
            if m.get("type").and_then(Value::as_str) == Some(USE_TYPE)
                && let Some(name) = m.get("component").and_then(Value::as_str)
            {
                out.push(ComponentUse {
                    name: name.to_owned(),
                    version: m.get("version").and_then(Value::as_i64),
                });
            }
            m.values().for_each(|c| collect_uses(c, out));
        }
        Value::Array(a) => a.iter().for_each(|c| collect_uses(c, out)),
        _ => {}
    }
}

/// Every reference in `doc`.
pub fn refs(doc: &Value) -> Vec<PartRef> {
    let mut out = Vec::new();
    collect(doc, &mut out);
    out
}

fn collect(v: &Value, out: &mut Vec<PartRef>) {
    if let Some(r) = as_ref(v) {
        out.push(r);
        return;
    }
    match v {
        Value::Object(m) => m.values().for_each(|c| collect(c, out)),
        Value::Array(a) => a.iter().for_each(|c| collect(c, out)),
        _ => {}
    }
}

/// Whether `json` (a stored layout) may hold a reference or a component
/// use (cheap check).
pub fn may_hold_refs(json: &str) -> bool {
    json.contains(REF_TYPE) || json.contains(USE_TYPE)
}

/// A built-in version's parts: version `version`, else the newest one
/// `client` can draw, else the latest. Blocking.
pub fn load(
    store: &Store,
    app: &str,
    version: Option<i64>,
    client: Option<&ClientCaps>,
) -> rusqlite::Result<Option<Parts>> {
    let v = match (version, client) {
        (Some(n), _) => store.app_version(app, n)?.map(|(v, _)| v),
        (None, Some(s)) => crate::layouts::newest_compatible(store, app, s)?,
        (None, None) => store.app_latest_meta(app)?,
    };
    let Some(v) = v else { return Ok(None) };
    let Some((meta, json)) = store.app_version(app, v.version)? else {
        return Ok(None);
    };
    let layout: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
    let declared: Option<Value> = meta
        .meta_json
        .as_deref()
        .and_then(|m| serde_json::from_str::<Value>(m).ok())
        .map(|m| m["parts"].clone());
    Ok(Some(Parts::of(v.version, &layout, declared.as_ref())))
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

/// Component definitions not stored yet (server files being checked before
/// they are recorded, a `pod:` definition being written): consulted first.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    /// `(name, version, definition)`.
    pub defs: Vec<(String, i64, Value)>,
}

impl Pending {
    fn get(&self, name: &str, version: Option<i64>) -> Option<Value> {
        self.defs
            .iter()
            .filter(|(n, v, _)| n == name && version.is_none_or(|w| w == *v))
            .max_by_key(|(_, v, _)| *v)
            .map(|(_, _, d)| d.clone())
    }
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

struct Expander<'a> {
    /// `None`: layout files, which use only server components (from the
    /// embedded files) and no parts.
    store: Option<&'a Store>,
    catalog: &'a Catalog,
    client: Option<&'a ClientCaps>,
    mode: Mode,
    pending: &'a Pending,
    cache: HashMap<(String, Option<i64>), Option<Parts>>,
    defs: HashMap<(String, Option<i64>), Option<Value>>,
    work: Work,
}

fn err(at: &str, message: String) -> LayoutError {
    LayoutError {
        pointer: at.to_owned(),
        message,
        allowed: Vec::new(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty() && s != "0" && s != "false",
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

fn as_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The approximate JSON size of `v`, without serializing it.
fn weight(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(weight).sum::<usize>() + 2,
        Value::Object(m) => {
            m.iter()
                .map(|(k, c)| k.len() + 3 + weight(c))
                .sum::<usize>()
                + 2
        }
        _ => 4,
    }
}

/// The work an expansion has done so far ([`MAX_EXPANSION_WORK`]).
#[derive(Default)]
struct Work(usize);

impl Work {
    fn charge(&mut self, n: usize) -> Result<(), LayoutError> {
        self.0 = self.0.saturating_add(n);
        if self.0 > MAX_EXPANSION_WORK {
            return Err(err(
                "",
                format!(
                    "the expansion is too large (over {MAX_EXPANSION_WORK} bytes of work; \
                     a component repeats its children or arguments too often)"
                ),
            ));
        }
        Ok(())
    }
}

/// `s` with each `$arg.<name>` replaced; a string that is only `$arg.<name>`
/// becomes the argument itself (its own JSON type).
fn subst(s: &str, args: &Map<String, Value>, work: &mut Work) -> Result<Value, LayoutError> {
    if let Some(name) = s.strip_prefix("$arg.")
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        let arg = args.get(name);
        work.charge(arg.map_or(4, weight))?;
        return Ok(arg.cloned().unwrap_or(Value::Null));
    }
    work.charge(s.len() + 2)?;
    if !s.contains("$arg.") {
        return Ok(Value::String(s.to_owned()));
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("$arg.") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 5..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let piece = as_text(args.get(&after[..end]).unwrap_or(&Value::Null));
        work.charge(piece.len())?;
        out.push_str(&piece);
        rest = &after[end..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn subst_value(
    v: &Value,
    args: &Map<String, Value>,
    work: &mut Work,
) -> Result<Value, LayoutError> {
    Ok(match v {
        Value::String(s) => subst(s, args, work)?,
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|c| subst_value(c, args, work))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(m) => {
            work.charge(2)?;
            let mut out = Map::new();
            for (k, c) in m {
                let key = as_text(&subst(k, args, work)?);
                out.insert(key, subst_value(c, args, work)?);
            }
            Value::Object(out)
        }
        other => {
            work.charge(4)?;
            other.clone()
        }
    })
}

impl Expander<'_> {
    fn part(&mut self, r: &PartRef) -> rusqlite::Result<Option<Value>> {
        let key = (r.app.clone(), r.version);
        if !self.cache.contains_key(&key) {
            let parts = if let Some(store) = self.store
                && self.catalog.is_app(&r.app)
            {
                load(store, &r.app, r.version, self.client)?
            } else {
                None
            };
            self.cache.insert(key.clone(), parts);
        }
        Ok(self.cache[&key]
            .as_ref()
            .and_then(|p| p.parts.get(&r.part).cloned()))
    }

    fn def(&mut self, name: &str, version: Option<i64>) -> rusqlite::Result<Option<Value>> {
        let key = (name.to_owned(), version);
        if !self.defs.contains_key(&key) {
            let found = match (self.pending.get(name, version), self.store) {
                (Some(d), _) => Some(d),
                (None, None) => None,
                (None, Some(store)) => match version {
                    Some(n) => crate::components::version(store, name, n)?,
                    None => crate::components::latest(store, name)?.map(|(_, d)| d),
                },
            };
            self.defs.insert(key.clone(), found);
        }
        Ok(self.defs[&key].clone())
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
                (bare.contains('/') || self.catalog.is_app(app))
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
        let found = self
            .def(name, version)
            .map_err(|e| err(at, format!("reading the component: {e}")))?;
        let Some(raw) = found else {
            let why = match version {
                Some(n) => format!("no version {n} (list_components lists them)"),
                None => "no such component (list_components lists them)".to_owned(),
            };
            return self.unresolved(at, &label, &why, in_screens);
        };
        let def = match Def::parse(&raw) {
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
        let template = def.template.clone();
        let out = self.walk(&template, at, false, depth + 1, Some(&inner));
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
        let found = self
            .part(r)
            .map_err(|e| err(at, format!("reading the built-in part: {e}")))?;
        let Some(part) = found else {
            let why = if self.catalog.is_app(&r.app) {
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
/// expands to (parts from the newest version `client` can draw; `None`:
/// the latest). Blocking.
pub fn expand(
    store: &Store,
    catalog: &Catalog,
    doc: &Value,
    client: Option<&ClientCaps>,
    mode: Mode,
) -> Result<Value, LayoutError> {
    expand_with(store, catalog, doc, client, mode, &Pending::default())
}

/// [`expand`] with definitions not stored yet ([`Pending`]).
pub fn expand_with(
    store: &Store,
    catalog: &Catalog,
    doc: &Value,
    client: Option<&ClientCaps>,
    mode: Mode,
    pending: &Pending,
) -> Result<Value, LayoutError> {
    expand_in(Some(store), catalog, doc, client, mode, pending)
}

/// A layout file's `core://Use`s of server components expanded (strict),
/// from the components embedded in this build: what the pod records.
pub fn expand_file(catalog: &Catalog, doc: &Value) -> Result<Value, LayoutError> {
    expand_in(
        None,
        catalog,
        doc,
        None,
        Mode::Strict,
        catalog.component_defs(),
    )
}

fn expand_in(
    store: Option<&Store>,
    catalog: &Catalog,
    doc: &Value,
    client: Option<&ClientCaps>,
    mode: Mode,
    pending: &Pending,
) -> Result<Value, LayoutError> {
    let mut e = Expander {
        store,
        catalog,
        client,
        mode,
        pending,
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

/// A component use as a node: `{"type": "core://Use", "component": name,
/// "version"?, "args", "children"}`.
pub fn use_node(name: &str, version: Option<i64>, args: &Value, children: &Value) -> Value {
    let mut v = json!({ "type": USE_TYPE, "component": name });
    if let Some(n) = version {
        v["version"] = json!(n);
    }
    if !args.is_null() {
        v["args"] = args.clone();
    }
    if !children.is_null() {
        v["children"] = children.clone();
    }
    v
}

/// Check a definition (`name`, about to be stored as `version`): it parses
/// and every example expands strictly to plumbing nodes this build knows.
/// `Err` names the example. Blocking.
pub fn check_definition(
    store: &Store,
    catalog: &Catalog,
    name: &str,
    version: i64,
    def: &Value,
    pending: &Pending,
) -> Result<Def, String> {
    let parsed = Def::parse(def)?;
    let mut pending = pending.clone();
    pending.defs.push((name.to_owned(), version, def.clone()));
    for (i, ex) in parsed.examples.iter().enumerate() {
        let node = use_node(
            name,
            Some(version),
            &Value::Object(ex.args.clone()),
            &Value::Array(ex.children.clone()),
        );
        let fail = |e: String| format!("examples/{i}: {e}");
        let out = expand_with(store, catalog, &node, None, Mode::Strict, &pending)
            .map_err(|e| fail(e.to_string()))?;
        let parsed: protocol::UiNode = serde_json::from_value(out)
            .map_err(|e| fail(format!("expands to an invalid node: {e}")))?;
        if let Some(t) = crate::validate::unknown_node_types(&parsed).first() {
            return Err(fail(format!("expands to an unknown node type '{t}'")));
        }
        if let Some(f) = protocol::required_features(&parsed)
            .into_iter()
            .find(|f| protocol::is_retired(f))
        {
            return Err(fail(format!(
                "uses '{f}', which was removed from the vocabulary"
            )));
        }
    }
    Ok(parsed)
}

/// A stored layout as served to `client`: refs expanded leniently (a JSON
/// text in, a JSON text out). Blocking.
///
/// A client that draws the plumbing ([`crate::porcelain::draws_plumbing`])
/// gets a `core://Workspace` as the `core://Page` it stands for (decision
/// `plumbing-and-porcelain`).
pub fn serve(store: &Store, catalog: &Catalog, json: &str, client: Option<&ClientCaps>) -> String {
    let plumbing = client.is_some_and(crate::porcelain::draws_plumbing);
    if !plumbing && !may_hold_refs(json) {
        return json.to_owned();
    }
    let Ok(doc) = serde_json::from_str::<Value>(json) else {
        return json.to_owned();
    };
    let expanded = if may_hold_refs(json) {
        match expand(store, catalog, &doc, client, Mode::Lenient) {
            Ok(v) => v,
            Err(_) => return json.to_owned(),
        }
    } else {
        doc
    };
    if plumbing && let Some(page) = crate::porcelain::page_of(&expanded) {
        // A client that draws plumbing draws the porcelain's expansion;
        // the raw workspace if (unexpectedly) it can't be expanded.
        if let Ok(v) = expand(store, catalog, &page, client, Mode::Lenient) {
            return v.to_string();
        }
    }
    expanded.to_string()
}
