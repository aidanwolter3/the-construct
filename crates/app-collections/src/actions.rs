//! Insert-from-query actions (decision `collection-primitives`): a named,
//! declarative "upsert a parent, add a child" stored on the parent's
//! collection and run with parameters, from a layout
//! (`POST /v1/collections/<c>/actions/<name>`) or MCP (`run_action`).
//!
//! For each live row of the `source` collection matching `where`, an action
//! finds the row of its own collection whose `match` fields equal the
//! values given (else creates it with `match` and `create`), applies
//! `update` to a row it found (first, when that row matches `reset_when`,
//! deleting the children it had, once per call), then adds a row of the
//! `child` collection whose values are given by `child.values`, one of which
//! links it to the parent (`"$parent.id"`). A child value marked `scaled` is
//! multiplied by `scale.times / scale.per` when both are numbers (else 1).
//!
//! Values are literals or bindings: `$param.<name>` (a parameter of the
//! call), `$row.<field>` (the source row; through refs:
//! `$row.<ref>.<field>`, and `.id`) and, in child values, `$parent.id` or
//! `$parent.<field>`. They are stored by field id, so renames keep them.
//!
//! A call is one edit across both collections (`undo_edit` reverts it),
//! and a repeated `client_id` answers the first call's edit.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Caps;
use crate::compute::{self, Job};
use crate::db::{self, EditRow, Row};
use crate::model::{self, Definition, Defs, Field, Kind, Write};

/// Most actions per collection.
pub const MAX_ACTIONS: usize = 10;
/// Most parameters of an action.
pub const MAX_PARAMS: usize = 8;
/// Most conditions of a `where` or `reset_when`.
pub const MAX_CONDITIONS: usize = 8;
/// Longest path through refs (`$row.a.b.c`).
pub const MAX_PATH: usize = 4;

/// A value of an action: a literal or a binding (field ids in paths).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expr {
    Value(Value),
    Param(String),
    /// The source row: field ids, each but the last a ref; `id` last for a
    /// row's id.
    Row(Vec<String>),
    /// The parent row (in child values).
    Parent(Vec<String>),
}

/// A condition: field `field` (an id) equals `equals`, or is (not) empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cond {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Expr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<bool>,
}

/// A field (id) set to a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assign {
    pub field: String,
    pub value: Expr,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scaled: bool,
}

/// A parameter's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Text,
    Number,
    /// A row of `collection`: its id or `{"<unique field>": value}`.
    Ref,
}

/// A parameter of a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ParamKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// The factor scaled child values are multiplied by: `times / per`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scale {
    pub times: Expr,
    pub per: Expr,
}

/// The rows an action adds for each source row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Child {
    pub collection: String,
    pub values: Vec<Assign>,
}

/// A named insert-from-query action of a collection (the parent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    pub source: String,
    #[serde(rename = "where", default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Cond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
    #[serde(rename = "match")]
    pub matching: Vec<Assign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub create: Vec<Assign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update: Vec<Assign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset_when: Vec<Cond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<Child>,
}

impl Action {
    /// The child field that links a child to its parent (`$parent.id`).
    pub fn link(&self) -> Option<&str> {
        self.child
            .as_ref()?
            .values
            .iter()
            .find_map(|a| match &a.value {
                Expr::Parent(p) if p.len() == 1 && p[0] == model::ROW_ID => Some(a.field.as_str()),
                _ => None,
            })
    }
}

/// The action `name` of `def`.
pub fn find<'a>(def: &'a Definition, name: &str) -> Option<&'a Action> {
    def.actions.iter().find(|a| a.name == name)
}

// ---------- parsing ----------

/// Where a binding may appear.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// `where`, `scale`, `match`, `create`, `update`: params and the row.
    Row,
    /// `reset_when`: params and literals.
    Reset,
    /// Child values: params, the row and the parent.
    Child,
}

struct Ctx<'a> {
    def: &'a Definition,
    source: &'a Definition,
    defs: &'a Defs,
    params: &'a [Param],
}

impl Ctx<'_> {
    /// `names` (a dotted path) from `start` as field ids, through refs.
    fn path(&self, start: &Definition, names: &str, what: &str) -> Result<Vec<String>, String> {
        let parts: Vec<&str> = names.split('.').collect();
        if parts.is_empty() || parts.len() > MAX_PATH || parts.iter().any(|p| p.is_empty()) {
            return Err(format!(
                "{what}: a path is 1..={MAX_PATH} field names joined by dots"
            ));
        }
        let mut d = start;
        let mut out = Vec::new();
        for (i, n) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            if *n == model::ROW_ID {
                if !last {
                    return Err(format!("{what}: 'id' ends a path"));
                }
                out.push(model::ROW_ID.to_owned());
                break;
            }
            let f = d.field(n).ok_or_else(|| {
                format!(
                    "{what}: '{n}' is not a field of '{}' ({})",
                    d.name,
                    d.live_fields()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            out.push(f.id.clone());
            if !last {
                let target = f.collection.as_deref().filter(|_| f.kind == Kind::Ref);
                d = target.and_then(|t| self.defs.get(t)).ok_or_else(|| {
                    format!("{what}: '{n}' is not a ref field, so the path ends there")
                })?;
            }
        }
        Ok(out)
    }

    fn expr(&self, v: &Value, place: Place, what: &str) -> Result<Expr, String> {
        if let Value::Object(o) = v
            && o.len() == 1
            && let Some(x) = o.get("value")
        {
            return Ok(Expr::Value(x.clone()));
        }
        let Some(s) = v.as_str().map(str::trim).filter(|s| s.starts_with('$')) else {
            return Ok(Expr::Value(v.clone()));
        };
        if let Some(p) = s.strip_prefix("$param.") {
            if !self.params.iter().any(|x| x.name == p) {
                return Err(format!(
                    "{what}: '{s}' names no parameter (params: {})",
                    self.params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            return Ok(Expr::Param(p.to_owned()));
        }
        if let Some(p) = s.strip_prefix("$row.") {
            if place == Place::Reset {
                return Err(format!(
                    "{what}: reset_when compares the parent with literals or $param values"
                ));
            }
            return Ok(Expr::Row(self.path(self.source, p, what)?));
        }
        if let Some(p) = s.strip_prefix("$parent.") {
            if place != Place::Child {
                return Err(format!("{what}: $parent is only in child values"));
            }
            return Ok(Expr::Parent(self.path(self.def, p, what)?));
        }
        Err(format!(
            "{what}: '{s}' is not a binding ($param.<name>, $row.<field>, $parent.<field>); give a literal starting with $ as {{\"value\": ...}}"
        ))
    }

    fn conds(
        &self,
        v: Option<&Value>,
        on: &Definition,
        place: Place,
        what: &str,
    ) -> Result<Vec<Cond>, String> {
        let items: Vec<&Value> = match v {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Array(a)) => a.iter().collect(),
            Some(o @ Value::Object(_)) => vec![o],
            Some(_) => {
                return Err(format!(
                    "{what} is {{field, equals}} or {{field, empty}}, or a list of them"
                ));
            }
        };
        if items.len() > MAX_CONDITIONS {
            return Err(format!("{what} has at most {MAX_CONDITIONS} conditions"));
        }
        items
            .iter()
            .map(|c| {
                let n = c["field"].as_str().map(str::trim).unwrap_or("");
                let f = on
                    .field(n)
                    .ok_or_else(|| format!("{what}: '{n}' is not a field of '{}'", on.name))?;
                let equals = match c.get("equals") {
                    None | Some(Value::Null) => None,
                    Some(e) => Some(self.expr(e, place, what)?),
                };
                let empty = match c.get("empty") {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(b)) => Some(*b),
                    Some(_) => return Err(format!("{what}: empty is true or false")),
                };
                if equals.is_some() == empty.is_some() {
                    return Err(format!(
                        "{what}: a condition on '{n}' has equals or empty (one of them)"
                    ));
                }
                Ok(Cond {
                    field: f.id.clone(),
                    equals,
                    empty,
                })
            })
            .collect()
    }

    fn assigns(
        &self,
        v: Option<&Value>,
        on: &Definition,
        place: Place,
        what: &str,
    ) -> Result<Vec<Assign>, String> {
        let Some(v) = v.filter(|v| !v.is_null()) else {
            return Ok(Vec::new());
        };
        let Some(o) = v.as_object() else {
            return Err(format!("{what} is {{field: value}}"));
        };
        let mut out: Vec<Assign> = Vec::new();
        for (n, x) in o {
            let f = on.field(n).ok_or_else(|| {
                format!(
                    "{what}: '{n}' is not a field of '{}' ({})",
                    on.name,
                    on.live_fields()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            if out.iter().any(|a| a.field == f.id) {
                return Err(format!("{what}: '{n}' is given twice"));
            }
            let (value, scaled) = match x {
                Value::Object(m) if m.contains_key("from") => {
                    let scaled = match m.get("scaled") {
                        None | Some(Value::Null) => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => return Err(format!("{what}: '{n}'.scaled is true or false")),
                    };
                    (
                        self.expr(&m["from"], place, &format!("{what}.{n}"))?,
                        scaled,
                    )
                }
                other => (self.expr(other, place, &format!("{what}.{n}"))?, false),
            };
            out.push(Assign {
                field: f.id.clone(),
                value,
                scaled,
            });
        }
        Ok(out)
    }
}

fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

/// Parse an action spec for collection `def` (`defs`: every collection):
/// `{name, title?, params?, source: {collection, where?}, scale?: {times,
/// per}, match, create?, update?, reset_when?, child?: {collection,
/// values}}`.
pub fn parse(def: &Definition, spec: &Value, defs: &Defs) -> Result<Action, String> {
    let Some(o) = spec.as_object() else {
        return Err("an action is an object {name, source, match, ...}".into());
    };
    let name = str_of(o, "name").to_owned();
    if !model::valid_name(&name) {
        return Err(format!(
            "action name '{name}' must be [a-z][a-z0-9_]*, at most {} characters",
            model::MAX_NAME_CHARS
        ));
    }
    let label = name.clone();
    let at = |e: String| format!("action '{label}': {e}");
    let title = str_of(o, "title").to_owned();
    if title.chars().count() > model::MAX_TITLE_CHARS {
        return Err(at(format!(
            "title is at most {} characters",
            model::MAX_TITLE_CHARS
        )));
    }
    let mut params: Vec<Param> = Vec::new();
    match o.get("params") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            if items.len() > MAX_PARAMS {
                return Err(at(format!("at most {MAX_PARAMS} params")));
            }
            for p in items {
                let p = p.as_object().ok_or_else(|| {
                    at("each param is {name, type, collection?, optional?}".into())
                })?;
                let pn = str_of(p, "name").to_owned();
                if !model::valid_name(&pn) || pn == "client_id" {
                    return Err(at(format!(
                        "param name '{pn}' must be [a-z][a-z0-9_]* (not client_id)"
                    )));
                }
                if params.iter().any(|x| x.name == pn) {
                    return Err(at(format!("param '{pn}' is listed twice")));
                }
                let kind: ParamKind = serde_json::from_value(json!(str_of(p, "type")))
                    .map_err(|_| at(format!("param '{pn}': type is text, number or ref")))?;
                let collection = match kind {
                    ParamKind::Ref => {
                        let c = str_of(p, "collection");
                        if !defs.contains_key(c) && c != def.name {
                            return Err(at(format!(
                                "param '{pn}': a ref param needs collection (an existing collection)"
                            )));
                        }
                        Some(c.to_owned())
                    }
                    _ => None,
                };
                params.push(Param {
                    name: pn,
                    kind,
                    collection,
                    optional: p.get("optional").and_then(Value::as_bool).unwrap_or(false),
                });
            }
        }
        Some(_) => {
            return Err(at(
                "params is a list of {name, type, collection?, optional?}".into(),
            ));
        }
    }
    let source_spec = o
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| at("source is {collection, where?}: the rows it reads".into()))?;
    let source_name = str_of(source_spec, "collection").to_owned();
    if source_name == def.name {
        return Err(at("source is another collection than its own".into()));
    }
    let source = defs
        .get(&source_name)
        .ok_or_else(|| at(format!("source collection '{source_name}' doesn't exist")))?;
    let ctx = Ctx {
        def,
        source,
        defs,
        params: &params,
    };
    let conditions = ctx
        .conds(source_spec.get("where"), source, Place::Row, "source.where")
        .map_err(at)?;
    let scale = match o.get("scale") {
        None | Some(Value::Null) => None,
        Some(Value::Object(s)) => Some(Scale {
            times: ctx
                .expr(
                    s.get("times").unwrap_or(&Value::Null),
                    Place::Row,
                    "scale.times",
                )
                .map_err(at)?,
            per: ctx
                .expr(
                    s.get("per").unwrap_or(&Value::Null),
                    Place::Row,
                    "scale.per",
                )
                .map_err(at)?,
        }),
        Some(_) => return Err(at("scale is {times, per}".into())),
    };
    let matching = ctx
        .assigns(o.get("match"), def, Place::Row, "match")
        .map_err(at)?;
    if matching.is_empty() {
        return Err(at("match names the fields that find the parent row, e.g. {\"ingredient\": \"$row.ingredient\"}".into()));
    }
    let create = ctx
        .assigns(o.get("create"), def, Place::Row, "create")
        .map_err(at)?;
    let update = ctx
        .assigns(o.get("update"), def, Place::Row, "update")
        .map_err(at)?;
    let reset_when = ctx
        .conds(o.get("reset_when"), def, Place::Reset, "reset_when")
        .map_err(at)?;
    let child = match o.get("child") {
        None | Some(Value::Null) => None,
        Some(Value::Object(c)) => {
            let cn = str_of(c, "collection").to_owned();
            let cdef = defs.get(&cn).filter(|_| cn != def.name).ok_or_else(|| {
                at(format!(
                    "child collection '{cn}' doesn't exist (or is this one)"
                ))
            })?;
            Some(Child {
                values: ctx
                    .assigns(c.get("values"), cdef, Place::Child, "child.values")
                    .map_err(at)?,
                collection: cn,
            })
        }
        Some(_) => return Err(at("child is {collection, values}".into())),
    };
    for a in matching.iter().chain(&create).chain(&update) {
        if a.scaled {
            return Err(at("only child values are scaled".into()));
        }
    }
    let action = Action {
        name,
        title,
        params,
        source: source_name,
        conditions,
        scale,
        matching,
        create,
        update,
        reset_when,
        child,
    };
    check(def, &action, defs).map_err(at)?;
    Ok(action)
}

// ---------- checking ----------

/// Check `action` of `def` against the live definitions (also after other
/// changes: a field it names can't be removed while it names it).
pub fn check(def: &Definition, action: &Action, defs: &Defs) -> Result<(), String> {
    let get = |n: &str| -> Result<&Definition, String> {
        if n == def.name {
            return Ok(def);
        }
        defs.get(n)
            .ok_or_else(|| format!("collection '{n}' doesn't exist; change the action first"))
    };
    let source = get(&action.source)?;
    let field_of = |d: &Definition, id: &str| -> Result<Field, String> {
        d.by_id(id).filter(|f| f.live()).cloned().ok_or_else(|| {
            format!(
                "it names a field of '{}' that is removed or gone; change the action first",
                d.name
            )
        })
    };
    let path = |start: &Definition, p: &[String]| -> Result<(), String> {
        let mut d = start.clone();
        for (i, id) in p.iter().enumerate() {
            if id == model::ROW_ID {
                return Ok(());
            }
            let f = field_of(&d, id)?;
            if i + 1 < p.len() {
                let t = f
                    .collection
                    .clone()
                    .filter(|_| f.kind == Kind::Ref)
                    .ok_or_else(|| {
                        format!("'{}' is no longer a ref; change the action first", f.name)
                    })?;
                d = get(&t)?.clone();
            }
        }
        Ok(())
    };
    let expr = |e: &Expr| -> Result<(), String> {
        match e {
            Expr::Row(p) => path(source, p),
            Expr::Parent(p) => path(def, p),
            Expr::Param(_) | Expr::Value(_) => Ok(()),
        }
    };
    for p in &action.params {
        if let Some(c) = &p.collection {
            get(c)?;
        }
    }
    for c in &action.conditions {
        field_of(source, &c.field)?;
        c.equals.as_ref().map_or(Ok(()), expr)?;
    }
    for c in &action.reset_when {
        field_of(def, &c.field)?;
    }
    if let Some(s) = &action.scale {
        expr(&s.times)?;
        expr(&s.per)?;
    }
    for a in action
        .matching
        .iter()
        .chain(&action.create)
        .chain(&action.update)
    {
        field_of(def, &a.field)?;
        expr(&a.value)?;
    }
    // A new parent gets match and create: every required field is there.
    let given: HashSet<&str> = action
        .matching
        .iter()
        .chain(&action.create)
        .map(|a| a.field.as_str())
        .collect();
    if let Some(f) = def.live_fields().find(|f| {
        !f.optional && f.default.is_none() && !f.computed() && !given.contains(f.id.as_str())
    }) {
        return Err(format!(
            "a parent it creates has no '{}' (required, no default): give it in match or create",
            f.name
        ));
    }
    if let Some(child) = &action.child {
        let cdef = get(&child.collection)?;
        for a in &child.values {
            let f = field_of(cdef, &a.field)?;
            expr(&a.value)?;
            if a.scaled {
                if action.scale.is_none() {
                    return Err(format!(
                        "child value '{}' is scaled, but the action has no scale",
                        f.name
                    ));
                }
                if !matches!(f.kind, Kind::Number | Kind::Quantity) {
                    return Err(format!(
                        "child value '{}' is scaled, so it is a number or quantity field",
                        f.name
                    ));
                }
            }
        }
        let Some(link) = action.link() else {
            return Err(
                "child.values links each child to its parent: a ref field set to \"$parent.id\""
                    .into(),
            );
        };
        let lf = field_of(cdef, link)?;
        if lf.kind != Kind::Ref || lf.collection.as_deref() != Some(def.name.as_str()) {
            return Err(format!(
                "child field '{}' holds $parent.id, so it is a ref to '{}'",
                lf.name, def.name
            ));
        }
        let given: HashSet<&str> = child.values.iter().map(|a| a.field.as_str()).collect();
        if let Some(f) = cdef.live_fields().find(|f| {
            !f.optional && f.default.is_none() && !f.computed() && !given.contains(f.id.as_str())
        }) {
            return Err(format!(
                "a child it adds has no '{}' (required, no default): give it in child.values",
                f.name
            ));
        }
    } else if !action.reset_when.is_empty() {
        return Err("reset_when deletes a parent's children, so the action has a child".into());
    }
    Ok(())
}

/// Every action of `def` against the live definitions.
pub fn check_all(def: &Definition, defs: &Defs) -> Result<(), String> {
    if def.actions.len() > MAX_ACTIONS {
        return Err(format!("a collection has at most {MAX_ACTIONS} actions"));
    }
    for (i, a) in def.actions.iter().enumerate() {
        if def.actions[..i].iter().any(|o| o.name == a.name) {
            return Err(format!("two actions are named '{}'", a.name));
        }
        check(def, a, defs).map_err(|e| format!("action '{}' of '{}': {e}", a.name, def.name))?;
    }
    Ok(())
}

/// Whether any action of any collection reads or writes collection `name`
/// (another's: refused while one does).
pub fn users_of(defs: &Defs, name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for d in defs.values().filter(|d| d.name != name) {
        for a in &d.actions {
            let uses = a.source == name
                || a.child.as_ref().is_some_and(|c| c.collection == name)
                || a.params
                    .iter()
                    .any(|p| p.collection.as_deref() == Some(name));
            if uses {
                out.push(json!({ "collection": d.name, "action": a.name }));
            }
        }
    }
    out
}

// ---------- describing ----------

/// A path as written: `$row.ingredient.name`.
fn path_text(start: &Definition, p: &[String], defs: &Defs) -> String {
    let mut d = Some(start);
    let mut names = Vec::new();
    for id in p {
        if id == model::ROW_ID {
            names.push(model::ROW_ID.to_owned());
            break;
        }
        let f = d.and_then(|d| d.by_id(id));
        names.push(f.map_or_else(|| id.clone(), |f| f.name.clone()));
        d = f
            .and_then(|f| f.collection.as_deref())
            .and_then(|c| defs.get(c).or((c == start.name).then_some(start)));
    }
    names.join(".")
}

fn expr_text(e: &Expr, def: &Definition, source: Option<&Definition>, defs: &Defs) -> Value {
    match e {
        Expr::Value(v) if v.as_str().is_some_and(|s| s.starts_with('$')) => json!({ "value": v }),
        Expr::Value(v) => v.clone(),
        Expr::Param(p) => json!(format!("$param.{p}")),
        Expr::Row(p) => match source {
            Some(s) => json!(format!("$row.{}", path_text(s, p, defs))),
            None => json!(format!("$row.{}", p.join("."))),
        },
        Expr::Parent(p) => json!(format!("$parent.{}", path_text(def, p, defs))),
    }
}

/// An action as a spec, with names (as [`parse`] takes it).
pub fn describe(def: &Definition, a: &Action, defs: &Defs) -> Value {
    let source = defs.get(&a.source);
    let name_in =
        |d: Option<&Definition>, id: &str| d.map_or_else(|| id.to_owned(), |d| d.name_of(id));
    let conds = |cs: &[Cond], on: Option<&Definition>| -> Value {
        Value::Array(
            cs.iter()
                .map(|c| {
                    let mut v = json!({ "field": name_in(on, &c.field) });
                    if let Some(e) = &c.equals {
                        v["equals"] = expr_text(e, def, source, defs);
                    }
                    if let Some(b) = c.empty {
                        v["empty"] = json!(b);
                    }
                    v
                })
                .collect(),
        )
    };
    let assigns = |xs: &[Assign], on: Option<&Definition>| -> Value {
        let mut m = Map::new();
        for x in xs {
            let v = expr_text(&x.value, def, source, defs);
            m.insert(
                name_in(on, &x.field),
                if x.scaled {
                    json!({ "from": v, "scaled": true })
                } else {
                    v
                },
            );
        }
        Value::Object(m)
    };
    let mut src = json!({ "collection": a.source });
    if !a.conditions.is_empty() {
        src["where"] = conds(&a.conditions, source);
    }
    let mut v = json!({
        "name": a.name,
        "params": a.params,
        "source": src,
        "match": assigns(&a.matching, Some(def)),
        "endpoint": format!("/v1/collections/{}/actions/{}", def.name, a.name),
    });
    if !a.title.is_empty() {
        v["title"] = json!(a.title);
    }
    if let Some(s) = &a.scale {
        v["scale"] = json!({ "times": expr_text(&s.times, def, source, defs),
                             "per": expr_text(&s.per, def, source, defs) });
    }
    if !a.create.is_empty() {
        v["create"] = assigns(&a.create, Some(def));
    }
    if !a.update.is_empty() {
        v["update"] = assigns(&a.update, Some(def));
    }
    if !a.reset_when.is_empty() {
        v["reset_when"] = conds(&a.reset_when, Some(def));
    }
    if let Some(c) = &a.child {
        v["child"] = json!({ "collection": c.collection,
            "values": assigns(&c.values, defs.get(&c.collection)) });
    }
    v
}

// ---------- running ----------

/// A value as a comparable key (`None`: empty).
fn key_of(v: &Value) -> Option<String> {
    crate::lookup::text_of(v).map(|t| model::cache_key(&t))
}

/// The first number in a value: a number, a quantity's amount, or the
/// first number written in text ("Serves 4" → 4, "1 1/2" → 1.5).
pub fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let start = s.find(|c: char| c.is_ascii_digit())?;
            let q = crate::quantity::parse(&s[start..]).or_else(|| {
                let end = s[start..]
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '/'))
                    .map_or(s.len(), |e| start + e);
                crate::quantity::parse(&s[start..end])
            })?;
            q.parts.first().map(|p| p.amount)
        }
        other => crate::quantity::from_value(other)
            .filter(|q| q.parts.len() == 1)
            .map(|q| q.parts[0].amount),
    }
    .filter(|x| x.is_finite())
}

/// What a call did, for its answer.
#[derive(Debug, Default)]
struct Report {
    rows: usize,
    created: usize,
    updated: usize,
    reset: usize,
    added: usize,
    skipped: Vec<Value>,
    unparsed: Vec<Value>,
    parents: Vec<String>,
}

/// The state of the rows a call touches.
struct Run<'a> {
    tx: &'a Connection,
    defs: &'a Defs,
    caps: Caps,
    now: i64,
    /// Rows touched, in order: (collection, id) → state before (`None`:
    /// made by the call).
    touched: Vec<(String, String, Option<Value>)>,
    jobs: BTreeMap<String, Vec<Job>>,
    /// Rows read through refs, by (collection, id).
    cache: HashMap<(String, String), Option<Row>>,
}

impl Run<'_> {
    fn touch(&mut self, collection: &str, id: &str, before: Option<Value>) {
        if !self
            .touched
            .iter()
            .any(|(c, i, _)| c == collection && i == id)
        {
            self.touched
                .push((collection.to_owned(), id.to_owned(), before));
        }
    }

    fn live_row(&mut self, collection: &str, id: &str) -> rusqlite::Result<Option<Row>> {
        let k = (collection.to_owned(), id.to_owned());
        if let Some(r) = self.cache.get(&k) {
            return Ok(r.clone());
        }
        let r = db::row(self.tx, collection, id)?.filter(|r| r.deleted_at.is_none());
        self.cache.insert(k, r.clone());
        Ok(r)
    }

    /// The value at path `p` from row `row` of `start`.
    fn walk(&mut self, start: &Definition, row: &Row, p: &[String]) -> rusqlite::Result<Value> {
        let mut d = start.clone();
        let mut r = row.clone();
        for (i, id) in p.iter().enumerate() {
            if id == model::ROW_ID {
                return Ok(json!(r.id));
            }
            let Some(f) = d.by_id(id).cloned() else {
                return Ok(Value::Null);
            };
            let v = r.values.get(id).cloned().unwrap_or_else(|| f.empty_value());
            if i + 1 == p.len() {
                return Ok(v);
            }
            let (Some(target), Some(next)) = (f.collection.clone(), v.as_str()) else {
                return Ok(Value::Null);
            };
            let Some(nr) = self.live_row(&target, next)? else {
                return Ok(Value::Null);
            };
            let Some(nd) = self.defs.get(&target).cloned() else {
                return Ok(Value::Null);
            };
            d = nd;
            r = nr;
        }
        Ok(Value::Null)
    }

    fn eval(
        &mut self,
        e: &Expr,
        params: &Map<String, Value>,
        source: (&Definition, &Row),
        parent: Option<(&Definition, &Row)>,
    ) -> rusqlite::Result<Value> {
        Ok(match e {
            Expr::Value(v) => v.clone(),
            Expr::Param(p) => params.get(p).cloned().unwrap_or(Value::Null),
            Expr::Row(p) => self.walk(source.0, source.1, p)?,
            Expr::Parent(p) => match parent {
                Some((d, r)) => self.walk(d, r, p)?,
                None => Value::Null,
            },
        })
    }

    /// A new row of `def` from `write` (names), or why not.
    fn create(
        &mut self,
        def: &Definition,
        write: &Map<String, Value>,
    ) -> rusqlite::Result<Result<Row, String>> {
        let write = match crate::refs::prepare_write(self.tx, def, write)? {
            Ok(w) => w,
            Err(e) => return Ok(Err(e)),
        };
        let mut row = Row {
            id: db::new_id("r"),
            client_id: None,
            values: Map::new(),
            created_at: self.now,
            updated_at: self.now,
            deleted_at: None,
            filler: None,
        };
        if let Err(e) = model::apply_write(
            def,
            &mut row.values,
            &write,
            Write::Create,
            self.caps.text_chars,
        ) {
            return Ok(Err(format!("'{}': {e}", def.name)));
        }
        let jobs = compute::after_write(self.tx, def, None, &mut row, &write, self.now, self.caps)?;
        if let Err(e) = crate::refs::check_unique(self.tx, def, &row)? {
            return Ok(Err(e));
        }
        let bytes = Value::Object(row.values.clone()).to_string().len() as i64;
        if let Err(e) = crate::check_usage(self.tx, self.caps, 1, bytes)? {
            return Ok(Err(e["detail"]
                .as_str()
                .unwrap_or("the pod is full")
                .to_owned()));
        }
        db::insert_row(self.tx, &def.name, &row)?;
        self.jobs.entry(def.name.clone()).or_default().extend(jobs);
        self.touch(&def.name, &row.id, None);
        Ok(Ok(row))
    }

    /// Change `row` of `def` by `write` (names); whether it changed.
    fn update(
        &mut self,
        def: &Definition,
        row: &mut Row,
        write: &Map<String, Value>,
    ) -> rusqlite::Result<Result<bool, String>> {
        let write = match crate::refs::prepare_write(self.tx, def, write)? {
            Ok(w) => w,
            Err(e) => return Ok(Err(e)),
        };
        let before = row.clone();
        if let Err(e) = model::apply_write(
            def,
            &mut row.values,
            &write,
            Write::Update,
            self.caps.text_chars,
        ) {
            return Ok(Err(format!("'{}': {e}", def.name)));
        }
        let jobs = compute::after_write(
            self.tx,
            def,
            Some(&before.values),
            row,
            &write,
            self.now,
            self.caps,
        )?;
        if row.values == before.values {
            return Ok(Ok(false));
        }
        if let Err(e) = crate::refs::check_unique(self.tx, def, row)? {
            return Ok(Err(e));
        }
        row.updated_at = self.now;
        self.touch(&def.name, &row.id, Some(before.state()));
        db::save_row(self.tx, &def.name, row)?;
        self.jobs.entry(def.name.clone()).or_default().extend(jobs);
        Ok(Ok(true))
    }
}

/// Whether row `values` of `on` matches every condition.
fn matches(
    run: &mut Run<'_>,
    conds: &[Cond],
    on: &Definition,
    row: &Row,
    params: &Map<String, Value>,
    source: Option<(&Definition, &Row)>,
) -> rusqlite::Result<bool> {
    for c in conds {
        let v = row
            .values
            .get(&c.field)
            .cloned()
            .or_else(|| on.by_id(&c.field).map(Field::empty_value))
            .unwrap_or(Value::Null);
        let k = key_of(&v);
        let ok = match (&c.equals, c.empty) {
            (_, Some(empty)) => k.is_none() == empty,
            (Some(e), None) => {
                let want = match source {
                    Some(s) => run.eval(e, params, s, None)?,
                    None => run.eval(e, params, (on, row), None)?,
                };
                k.is_some() && k == key_of(&want)
            }
            (None, None) => true,
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// What a finished call hands back: its answer, the model jobs by
/// collection, and the computes it moved (finish them after the commit).
pub struct Done {
    pub answer: Value,
    pub jobs: BTreeMap<String, Vec<Job>>,
    pub followed: crate::follow::Followed,
}

/// Run action `name` of collection `collection` with `args` (`{param:
/// value, client_id?}`), inside `tx`. `Ok(Err(body))`: refused (nothing
/// stays).
pub fn run(
    tx: &Connection,
    collection: &str,
    name: &str,
    args: &Map<String, Value>,
    note: Option<&str>,
    caps: Caps,
    now: i64,
) -> rusqlite::Result<Result<Done, Value>> {
    let refuse = |status: u16, error: &str, detail: String| {
        Ok(Err(
            json!({ "error": error, "detail": detail, "status": status,
                        "collection": collection, "action": name }),
        ))
    };
    let defs = db::defs_by_name(tx)?;
    let Some(def) = defs.get(collection).cloned() else {
        return refuse(
            404,
            "collection_not_found",
            format!("there is no collection '{collection}'"),
        );
    };
    let Some(action) = find(&def, name).cloned() else {
        return refuse(
            404,
            "action_not_found",
            format!(
                "'{collection}' has no action '{name}' (actions: {})",
                def.actions
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };
    let client_id = args
        .get("client_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(c) = &client_id {
        if c.len() > 128 {
            return refuse(
                400,
                "invalid_request",
                "\"client_id\" is at most 128 characters".into(),
            );
        }
        if let Some(edit) = db::edit_by_client(tx, c)? {
            // A retry: the first call's edit.
            return Ok(Ok(Done {
                answer: json!({ "ok": true, "action": name, "edit": edit, "repeated": true }),
                jobs: BTreeMap::new(),
                followed: crate::follow::Followed::default(),
            }));
        }
    }
    if let Err(e) = check(&def, &action, &defs) {
        return refuse(409, "invalid_action", e);
    }
    // Parameters.
    let mut params = Map::new();
    for (k, _) in args {
        if k != "client_id" && !action.params.iter().any(|p| p.name == *k) {
            return refuse(
                422,
                "invalid_params",
                format!(
                    "'{k}' is not a parameter of '{name}' (params: {})",
                    action
                        .params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }
    for p in &action.params {
        let v = args.get(&p.name).cloned().unwrap_or(Value::Null);
        let blank = v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty());
        if blank {
            if !p.optional {
                return refuse(422, "invalid_params", format!("'{}' is required", p.name));
            }
            continue;
        }
        let value = match p.kind {
            ParamKind::Text => match &v {
                Value::String(s) => json!(s.trim()),
                Value::Number(_) | Value::Bool(_) => json!(v.to_string()),
                _ => return refuse(422, "invalid_params", format!("'{}' is text", p.name)),
            },
            ParamKind::Number => match number_of(&v).filter(|_| !v.is_object()) {
                Some(n) => json!(n),
                None => return refuse(422, "invalid_params", format!("'{}' is a number", p.name)),
            },
            ParamKind::Ref => {
                let f = model::parse_field(
                    &json!({ "name": p.name, "type": "ref", "collection": p.collection }),
                    "p".into(),
                    caps.text_chars,
                )
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
                match crate::refs::resolve(tx, &f, &v)? {
                    Ok(id) => json!(id),
                    Err(e) => return refuse(422, "invalid_params", e),
                }
            }
        };
        params.insert(p.name.clone(), value);
    }
    let Some(source) = defs.get(&action.source).cloned() else {
        return refuse(
            409,
            "invalid_action",
            format!("source '{}' doesn't exist", action.source),
        );
    };
    let child_def = action
        .child
        .as_ref()
        .and_then(|c| defs.get(&c.collection))
        .cloned();
    let mut run = Run {
        tx,
        defs: &defs,
        caps,
        now,
        touched: Vec::new(),
        jobs: BTreeMap::new(),
        cache: HashMap::new(),
    };
    // The source rows, in the source's order.
    let mut rows = Vec::new();
    for r in db::rows(tx, &source)? {
        if matches(&mut run, &action.conditions, &source, &r, &params, None)? {
            rows.push(r);
        }
    }
    if rows.len() > caps.edit_rows {
        return refuse(
            409,
            "too_many_rows",
            format!(
                "{} rows match; one call handles at most {}",
                rows.len(),
                caps.edit_rows
            ),
        );
    }
    let mut report = Report {
        rows: rows.len(),
        ..Report::default()
    };
    let mut parents: Vec<Row> = db::all_rows(tx, &def.name, false)?;
    let mut reset_done: HashSet<String> = HashSet::new();
    let mut updated: HashSet<String> = HashSet::new();
    for r in &rows {
        let src = (&source, r);
        // The parent's match values.
        let mut wanted: Vec<(String, Value)> = Vec::new();
        for a in &action.matching {
            wanted.push((a.field.clone(), run.eval(&a.value, &params, src, None)?));
        }
        if let Some((f, _)) = wanted.iter().find(|(_, v)| key_of(v).is_none()) {
            report.skipped.push(json!({ "row": r.id,
                "reason": format!("no value for {}", def.name_of(f)) }));
            continue;
        }
        let found = parents.iter().position(|p| {
            wanted.iter().all(|(f, v)| {
                let have = p.values.get(f).cloned().unwrap_or(Value::Null);
                key_of(&have) == key_of(v)
            })
        });
        let parent = match found {
            Some(i) => {
                let mut p = parents[i].clone();
                if !reset_done.contains(&p.id)
                    && !action.reset_when.is_empty()
                    && matches(&mut run, &action.reset_when, &def, &p, &params, None)?
                    && let (Some(child), Some(cdef), Some(link)) =
                        (&action.child, &child_def, action.link())
                {
                    for mut c in db::all_rows(tx, &child.collection, false)? {
                        if c.values.get(link).and_then(Value::as_str) == Some(p.id.as_str()) {
                            run.touch(&cdef.name, &c.id, Some(c.state()));
                            c.deleted_at = Some(now);
                            c.updated_at = now;
                            db::save_row(tx, &cdef.name, &c)?;
                            report.reset += 1;
                        }
                    }
                }
                reset_done.insert(p.id.clone());
                if !action.update.is_empty() && !updated.contains(&p.id) {
                    let mut write = Map::new();
                    for a in &action.update {
                        write.insert(
                            def.name_of(&a.field),
                            run.eval(&a.value, &params, src, None)?,
                        );
                    }
                    match run.update(&def, &mut p, &write)? {
                        Ok(true) => report.updated += 1,
                        Ok(false) => {}
                        Err(e) => return refuse(422, "invalid_value", e),
                    }
                    updated.insert(p.id.clone());
                    parents[i] = p.clone();
                }
                p
            }
            None => {
                let mut write = Map::new();
                for (f, v) in &wanted {
                    write.insert(def.name_of(f), v.clone());
                }
                for a in &action.create {
                    write.insert(
                        def.name_of(&a.field),
                        run.eval(&a.value, &params, src, None)?,
                    );
                }
                let p = match run.create(&def, &write)? {
                    Ok(p) => p,
                    Err(e) => return refuse(422, "invalid_value", e),
                };
                report.created += 1;
                reset_done.insert(p.id.clone());
                updated.insert(p.id.clone());
                parents.push(p.clone());
                p
            }
        };
        if !report.parents.contains(&parent.id) {
            report.parents.push(parent.id.clone());
        }
        let (Some(child), Some(cdef)) = (&action.child, &child_def) else {
            continue;
        };
        let k = match &action.scale {
            Some(s) => {
                let t = number_of(&run.eval(&s.times, &params, src, None)?);
                let per = number_of(&run.eval(&s.per, &params, src, None)?);
                match (t, per) {
                    (Some(t), Some(p)) if t > 0.0 && p > 0.0 => t / p,
                    _ => 1.0,
                }
            }
            None => 1.0,
        };
        let mut write = Map::new();
        for a in &child.values {
            let Some(f) = cdef.by_id(&a.field).cloned() else {
                continue;
            };
            let mut v = run.eval(&a.value, &params, src, Some((&def, &parent)))?;
            if f.kind == Kind::Quantity {
                // Text is read as a quantity here, so it can be scaled.
                let q = crate::quantity::from_value(&v)
                    .or_else(|| v.as_str().and_then(crate::quantity::parse));
                match q {
                    Some(q) if a.scaled => v = crate::quantity::scale(&q, k).to_value(),
                    Some(q) => v = q.to_value(),
                    None if crate::quantity::is_unparsed(&v) => {
                        report
                            .unparsed
                            .push(json!({ "row": r.id, "field": f.name, "value": v }));
                    }
                    None => {}
                }
            } else if a.scaled
                && let Some(n) = number_of(&v)
            {
                v = json!(n * k);
            }
            write.insert(f.name.clone(), v);
        }
        if let Err(e) = run.create(cdef, &write)? {
            return refuse(422, "invalid_value", e);
        }
        report.added += 1;
    }
    // Computes that read these rows settle now, so the edit records the
    // rows as they end up (and undo finds them unchanged).
    let followed = crate::follow::settle(tx, now, caps)?;
    let mut records = Vec::new();
    for (c, id, before) in &run.touched {
        let Some(after) = db::row(tx, c, id)? else {
            continue;
        };
        records.push(EditRow {
            collection: c.clone(),
            id: id.clone(),
            before: before.clone(),
            after: after.state(),
        });
    }
    let title = if action.title.is_empty() {
        format!("Ran {name}")
    } else {
        action.title.clone()
    };
    let note: String = note
        .map(str::to_owned)
        .unwrap_or(title)
        .chars()
        .take(pod_core::custom::MAX_NOTE_CHARS)
        .collect();
    let edit = if records.is_empty() {
        None
    } else {
        Some(db::record_edit_rows(
            tx,
            collection,
            &note,
            &records,
            client_id.as_deref(),
            now,
        )?)
    };
    let jobs = std::mem::take(&mut run.jobs);
    Ok(Ok(Done {
        answer: json!({
            "ok": true, "action": name, "edit": edit,
            "rows": report.rows, "created": report.created, "updated": report.updated,
            "reset": report.reset, "added": report.added, "parents": report.parents,
            "skipped": report.skipped, "unparsed": report.unparsed,
        }),
        jobs,
        followed,
    }))
}
