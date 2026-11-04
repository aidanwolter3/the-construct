//! A collection's definition and its values (decision `collections`): typed
//! fields with permanent ids, aliases for old names, soft removal, and the
//! checks every write goes through.
//!
//! Rows store their values as JSON keyed by **field id** (`f1`, `f2`, ...),
//! so a rename moves no data. Every name a field ever had stays an alias:
//! writes and bindings that use it still reach the field.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Every collection's live definition, by name (lookups read other
/// collections).
pub type Defs = HashMap<String, Definition>;

/// Longest collection or field name.
pub const MAX_NAME_CHARS: usize = 32;
/// Longest title.
pub const MAX_TITLE_CHARS: usize = 60;
/// Most options of a choice field.
pub const MAX_OPTIONS: usize = 50;
/// Longest choice option.
pub const MAX_OPTION_CHARS: usize = 60;
/// Longest `compute` prompt.
pub const MAX_PROMPT_CHARS: usize = 500;
/// Most source fields of a `model` compute.
pub const MAX_SOURCES: usize = 4;
/// Most kinds in a `compute` list (tried in order).
pub const MAX_COMPUTES: usize = 4;
/// Most fields a collection is ordered by.
pub const MAX_ORDER_KEYS: usize = 4;

/// Names every row has (never a field's name): its id, its URI, whether it
/// was deleted, when it was made and last changed, and the client's id of
/// its create.
pub const SYSTEM_FIELDS: &[&str] = &[
    "id",
    "uri",
    "deleted",
    "created_at",
    "updated_at",
    "client_id",
];

/// What a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Number,
    Bool,
    /// A calendar date, `YYYY-MM-DD`.
    Date,
    /// One of the field's `options`.
    Choice,
}

impl Kind {
    pub const ALL: &[&str] = &["text", "number", "bool", "date", "choice"];

    pub fn parse(s: &str) -> Option<Kind> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Number => "number",
            Kind::Bool => "bool",
            Kind::Date => "date",
            Kind::Choice => "choice",
        }
    }
}

/// One field of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Permanent: `f<n>`, never reused in its collection.
    pub id: String,
    /// What layouts and writes call it.
    pub name: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// A choice field's values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// May be empty.
    #[serde(default)]
    pub optional: bool,
    /// What an empty value reads as (and a create without it gets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Names it had before: still accepted from writes and bindings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// How the pod computes its value from the row's other fields
    /// (decisions `collection-computed-fields`, `collection-lookups`): kinds
    /// tried in order, the first with an answer wins; empty: not computed.
    /// Stored as one object when there is one kind (as before lists), else
    /// a list.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "ser_compute",
        deserialize_with = "de_compute"
    )]
    pub compute: Vec<Compute>,
    /// Soft-removed at (unix seconds): hidden, its data kept for the purge
    /// window and restorable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<i64>,
    /// Its data was purged (after the window): it can't come back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub purged: bool,
}

/// How a `lookup` compute matches a rule's key against the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    /// The whole input, ignoring case and spacing.
    #[default]
    Exact,
    /// The key's words appear in the input, in order and whole.
    Contains,
    /// The key is a regular expression (case-insensitive) found in the input.
    Regex,
}

impl Match {
    pub const ALL: &[&str] = &["exact", "contains", "regex"];

    pub fn as_str(self) -> &'static str {
        match self {
            Match::Exact => "exact",
            Match::Contains => "contains",
            Match::Regex => "regex",
        }
    }
}

/// One way a field's value is computed from the row's other fields (field
/// ids, so a rename keeps it), whenever they change; a value written by
/// hand is kept until they change again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Compute {
    /// An expression ([`crate::expr`]) over the row, its field references
    /// stored as ids; evaluated in the write itself.
    Expr { expr: String },
    /// A model's answer to `prompt` about the values of fields `from`,
    /// coerced to the field's type (a choice: one of its options); asked
    /// in the background, cached per input, and taught by hand edits.
    Model { from: Vec<String>, prompt: String },
    /// The `value` of the best row of collection `collection` whose `key`
    /// matches field `from` (field ids on both sides); evaluated in the
    /// write itself ([`crate::lookup`]). `learn`: a later kind's answer
    /// and a hand edit become rules.
    Lookup {
        from: String,
        collection: String,
        key: String,
        value: String,
        #[serde(rename = "match", default)]
        mode: Match,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        learn: bool,
    },
}

impl Compute {
    pub fn kind(&self) -> &'static str {
        match self {
            Compute::Expr { .. } => "expr",
            Compute::Model { .. } => "model",
            Compute::Lookup { .. } => "lookup",
        }
    }

    /// The parsed expression of an `expr` compute (ids).
    pub fn parsed(&self) -> Option<crate::expr::Expr> {
        match self {
            Compute::Expr { expr } => crate::expr::parse(expr).ok(),
            _ => None,
        }
    }

    /// The field ids (of its own row) it reads.
    pub fn sources(&self) -> Vec<String> {
        match self {
            Compute::Expr { .. } => self.parsed().map(|e| e.fields()).unwrap_or_default(),
            Compute::Model { from, .. } => from.clone(),
            Compute::Lookup { from, .. } => vec![from.clone()],
        }
    }
}

fn ser_compute<S: serde::Serializer>(v: &[Compute], s: S) -> Result<S::Ok, S::Error> {
    match v {
        [one] => one.serialize(s),
        _ => v.serialize(s),
    }
}

fn de_compute<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Compute>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Spec {
        One(Compute),
        Many(Vec<Compute>),
        None(()),
    }
    Ok(match Spec::deserialize(d)? {
        Spec::One(c) => vec![c],
        Spec::Many(v) => v,
        Spec::None(()) => Vec::new(),
    })
}

impl Field {
    pub fn live(&self) -> bool {
        self.removed_at.is_none()
    }

    /// Whether the pod computes it.
    pub fn computed(&self) -> bool {
        !self.compute.is_empty()
    }

    /// The field ids every kind of its compute reads, each once.
    pub fn compute_sources(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self.compute.iter().flat_map(Compute::sources) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Its `lookup` kind with `learn` (answers and hand edits become rules).
    pub fn learning_lookup(&self) -> Option<&Compute> {
        self.compute
            .iter()
            .find(|c| matches!(c, Compute::Lookup { learn: true, .. }))
    }

    /// Its `model` kind's sources and prompt.
    pub fn model_step(&self) -> Option<(&[String], &str)> {
        self.compute.iter().find_map(|c| match c {
            Compute::Model { from, prompt } => Some((from.as_slice(), prompt.as_str())),
            _ => None,
        })
    }

    /// Whether `name` is this field's name or one of its aliases.
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|a| a == name)
    }

    /// The value an empty slot reads as.
    pub fn empty_value(&self) -> Value {
        self.default.clone().unwrap_or(Value::Null)
    }

    /// `{name, type, ...}` for people and Claude.
    pub fn describe(&self) -> Value {
        let mut v = json!({ "id": self.id, "name": self.name, "type": self.kind.as_str(),
                            "optional": self.optional });
        if !self.options.is_empty() {
            v["options"] = json!(self.options);
        }
        if let Some(d) = &self.default {
            v["default"] = d.clone();
        }
        if !self.aliases.is_empty() {
            v["aliases"] = json!(self.aliases);
        }
        v
    }
}

/// A collection's definition: one stored version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    /// Permanent: entity URIs (`collections://<name>/<id>`), query keys
    /// (`collection:<name>`), routes and feeds use it.
    pub name: String,
    pub title: String,
    /// In order, removed ones too.
    pub fields: Vec<Field>,
    /// The field ids rows are listed by, first key first (else oldest
    /// first). Stored as a string when there is one (as before multi-key
    /// sorting), else a list.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "ser_order",
        deserialize_with = "de_order"
    )]
    pub order_by: Vec<String>,
    /// Every order key descending.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
    /// The next field id's number.
    pub next_field: u32,
    /// Notifications when a row's date is reached (decision
    /// `collection-reminders`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<crate::remind::Reminder>,
    /// What keeps the rows current (decision `collection-fillers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filler: Option<crate::filler::Filler>,
}

impl Definition {
    pub fn live_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| f.live())
    }

    /// The live field `name` names (its name, else an alias).
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.live_fields()
            .find(|f| f.name == name)
            .or_else(|| self.live_fields().find(|f| f.answers_to(name)))
    }

    /// The field with id `id`.
    pub fn by_id(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// Whether `name` is a removed field's name or alias (writes ignore it).
    pub fn is_removed_name(&self, name: &str) -> bool {
        self.fields.iter().any(|f| !f.live() && f.answers_to(name))
    }

    /// Every name a binding may use for a row: live names, aliases and the
    /// system fields.
    pub fn bindable(&self) -> Vec<String> {
        let mut out: Vec<String> = SYSTEM_FIELDS.iter().map(|s| (*s).to_owned()).collect();
        for f in self.live_fields() {
            out.push(f.name.clone());
            out.extend(f.aliases.iter().cloned());
        }
        out
    }

    /// The name of field id `id` (the id itself if there is none).
    pub fn name_of(&self, id: &str) -> String {
        self.by_id(id)
            .map_or_else(|| id.to_owned(), |f| f.name.clone())
    }

    /// One kind of a compute as a spec, with field names (`others`: the
    /// collections a lookup reads).
    pub fn describe_compute(&self, c: &Compute, others: &Defs) -> Value {
        match c {
            Compute::Expr { expr } => {
                let text = c
                    .parsed()
                    .and_then(|e| e.map_fields(&|id| Ok(self.name_of(id))).ok())
                    .map_or_else(|| expr.clone(), |e| e.to_string());
                json!({ "kind": "expr", "expr": text })
            }
            Compute::Model { from, prompt } => {
                let from: Vec<String> = from.iter().map(|id| self.name_of(id)).collect();
                json!({ "kind": "model", "from": from, "prompt": prompt })
            }
            Compute::Lookup {
                from,
                collection,
                key,
                value,
                mode,
                learn,
            } => {
                let other = |id: &str| {
                    others
                        .get(collection)
                        .map_or_else(|| id.to_owned(), |d| d.name_of(id))
                };
                let mut v = json!({ "kind": "lookup", "from": self.name_of(from),
                    "collection": collection, "key": other(key), "value": other(value),
                    "match": mode.as_str() });
                if *learn {
                    v["learn"] = json!(true);
                }
                v
            }
        }
    }

    /// [`Field::describe`] plus its `compute` (one kind: an object; else a
    /// list), with field names.
    pub fn describe_field(&self, f: &Field, others: &Defs) -> Value {
        let mut v = f.describe();
        match f.compute.as_slice() {
            [] => {}
            [one] => v["compute"] = self.describe_compute(one, others),
            many => {
                v["compute"] = Value::Array(
                    many.iter()
                        .map(|c| self.describe_compute(c, others))
                        .collect(),
                )
            }
        }
        v
    }

    /// The live fields rows are ordered by.
    pub fn order_fields(&self) -> impl Iterator<Item = &Field> {
        self.order_by
            .iter()
            .filter_map(|id| self.by_id(id))
            .filter(|f| f.live())
    }

    /// `{name, title, fields, removed_fields, order_by}` for Claude.
    pub fn describe(&self, now: i64, window: i64, others: &Defs) -> Value {
        let removed: Vec<Value> = self
            .fields
            .iter()
            .filter(|f| !f.live() && !f.purged)
            .map(|f| {
                let mut v = self.describe_field(f, others);
                let left = (f.removed_at.unwrap_or(now) + window - now).max(0);
                v["restorable_for_days"] = json!((left + 86_399) / 86_400);
                v
            })
            .collect();
        json!({
            "name": self.name,
            "title": self.title,
            "fields": self.live_fields().map(|f| self.describe_field(f, others)).collect::<Vec<_>>(),
            "removed_fields": removed,
            "order_by": order_json(self.order_fields().map(|f| f.name.clone()).collect()),
            "descending": self.descending,
            "reminders": self.reminders.iter().map(|r| r.describe(self)).collect::<Vec<_>>(),
            "filler": self.filler.as_ref().map(|f| f.describe(self)),
        })
    }
}

/// An order as JSON: `null`, one name, or a list of names.
pub fn order_json(names: Vec<String>) -> Value {
    match names.len() {
        0 => Value::Null,
        1 => json!(names[0]),
        _ => json!(names),
    }
}

fn ser_order<S: serde::Serializer>(v: &[String], s: S) -> Result<S::Ok, S::Error> {
    match v {
        [one] => s.serialize_str(one),
        _ => v.serialize(s),
    }
}

fn de_order<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Order {
        One(String),
        Many(Vec<String>),
        None(()),
    }
    Ok(match Order::deserialize(d)? {
        Order::One(s) => vec![s],
        Order::Many(v) => v,
        Order::None(()) => Vec::new(),
    })
}

/// The names an order spec gives: a name, a list of names, or empty
/// (`null`, `""`, `[]`).
pub fn order_names(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.trim().to_owned()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "an order is a field name or a list of field names".to_owned())
            })
            .collect(),
        Some(_) => Err("an order is a field name or a list of field names".into()),
    }
}

/// Field ids for the order `names` (names or aliases of live fields).
pub fn resolve_order(def: &Definition, names: &[String]) -> Result<Vec<String>, String> {
    names
        .iter()
        .map(|n| {
            def.field(n).map(|f| f.id.clone()).ok_or_else(|| {
                format!(
                    "order field '{n}' is not one of the fields ({})",
                    def.live_fields()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
        })
        .collect()
}

/// Set field `id`'s `compute` from a spec (`null`: not computed; `None`:
/// unchanged): one kind or a list of kinds tried in order, each
/// `{kind: "expr", expr: "<expression over entity.<field>>"}`,
/// `{kind: "model", from: <field> | [<fields>], prompt}` or
/// `{kind: "lookup", from, collection, key, value, match?, learn?}`. Field
/// names resolve against `def` (a lookup's key and value against
/// `others`) and are stored as ids.
pub fn set_compute(
    def: &mut Definition,
    id: &str,
    v: Option<&Value>,
    others: &Defs,
) -> Result<(), String> {
    let Some(v) = v else { return Ok(()) };
    let name = def.by_id(id).map(|f| f.name.clone()).unwrap_or_default();
    let compute = match v {
        Value::Null => Vec::new(),
        Value::Array(items) => {
            if items.is_empty() || items.len() > MAX_COMPUTES {
                return Err(format!(
                    "field '{name}': a compute list has 1..={MAX_COMPUTES} kinds"
                ));
            }
            items
                .iter()
                .map(|i| parse_compute(def, &name, i, others))
                .collect::<Result<_, _>>()?
        }
        one => vec![parse_compute(def, &name, one, others)?],
    };
    if let Some(f) = def.fields.iter_mut().find(|f| f.id == id) {
        f.compute = compute;
    }
    Ok(())
}

/// One kind of a compute spec (see [`set_compute`]).
fn parse_compute(
    def: &Definition,
    name: &str,
    v: &Value,
    others: &Defs,
) -> Result<Compute, String> {
    let field_id = |n: &str| {
        def.field(n).map(|f| f.id.clone()).ok_or_else(|| {
            format!(
                "field '{name}': compute reads '{n}', which is not one of the fields ({})",
                def.live_fields()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    };
    let shape = || {
        format!(
            "field '{name}': compute is {{kind: \"expr\", expr}}, {{kind: \"model\", from, prompt}}, {{kind: \"lookup\", from, collection, key, value, match?, learn?}}, a list of those (tried in order) or null"
        )
    };
    let Value::Object(o) = v else {
        return Err(shape());
    };
    match o.get("kind").and_then(Value::as_str) {
        Some("expr") => {
            let text = o.get("expr").and_then(Value::as_str).unwrap_or("");
            let parsed = crate::expr::parse(text)
                .map_err(|e| format!("field '{name}': compute.expr: {e}"))?;
            let ids = parsed.map_fields(&|n| field_id(n))?;
            Ok(Compute::Expr {
                expr: ids.to_string(),
            })
        }
        Some("model") => {
            let names = match o.get("from") {
                Some(Value::String(s)) => vec![s.trim().to_owned()],
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|x| x.as_str().map(|s| s.trim().to_owned()))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        format!("field '{name}': compute.from is a field or a list of fields")
                    })?,
                _ => {
                    return Err(format!(
                        "field '{name}': compute.from names the fields the model reads"
                    ));
                }
            };
            if names.is_empty() || names.len() > MAX_SOURCES {
                return Err(format!(
                    "field '{name}': compute.from is 1..={MAX_SOURCES} fields"
                ));
            }
            let mut from: Vec<String> = Vec::new();
            for n in &names {
                let id = field_id(n)?;
                if from.contains(&id) {
                    return Err(format!("field '{name}': compute.from lists '{n}' twice"));
                }
                from.push(id);
            }
            let prompt = o
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
                return Err(format!(
                    "field '{name}': compute.prompt (what to answer about the fields) is 1..={MAX_PROMPT_CHARS} characters"
                ));
            }
            Ok(Compute::Model {
                from,
                prompt: prompt.to_owned(),
            })
        }
        Some("lookup") => {
            let arg = |k: &str| {
                o.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("field '{name}': a lookup needs \"{k}\" (a name)"))
            };
            let from = field_id(arg("from")?)?;
            let collection = arg("collection")?.to_owned();
            if collection == def.name {
                return Err(format!(
                    "field '{name}': a lookup reads another collection, not its own"
                ));
            }
            let Some(other) = others.get(&collection) else {
                return Err(format!(
                    "field '{name}': compute.collection '{collection}' is not a collection ({}); define it first",
                    others.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            };
            let other_id = |k: &str| -> Result<String, String> {
                let n = arg(k)?;
                other.field(n).map(|f| f.id.clone()).ok_or_else(|| {
                    format!(
                        "field '{name}': compute.{k} '{n}' is not a field of '{collection}' ({})",
                        other
                            .live_fields()
                            .map(|f| f.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            };
            let key = other_id("key")?;
            let value = other_id("value")?;
            let mode = match o.get("match") {
                None | Some(Value::Null) => Match::Exact,
                Some(m) => serde_json::from_value(m.clone()).map_err(|_| {
                    format!(
                        "field '{name}': compute.match is one of {}",
                        Match::ALL.join(", ")
                    )
                })?,
            };
            let learn = match o.get("learn") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(format!("field '{name}': compute.learn is true or false")),
            };
            Ok(Compute::Lookup {
                from,
                collection,
                key,
                value,
                mode,
                learn,
            })
        }
        _ => Err(shape()),
    }
}

/// Check the lookups of `def` against the collections they read (`defs`:
/// every live definition, `def`'s own included): the collection and its
/// fields exist, the key is text, the value's type fits, and a collection
/// a lookup learns into can take a new row of a key and a value.
pub fn check_lookups(def: &Definition, defs: &Defs) -> Result<(), String> {
    for f in def.live_fields() {
        for c in &f.compute {
            let Compute::Lookup {
                collection,
                key,
                value,
                learn,
                ..
            } = c
            else {
                continue;
            };
            let at = format!("field '{}' of '{}'", f.name, def.name);
            if *collection == def.name {
                return Err(format!(
                    "{at}: a lookup reads another collection, not its own"
                ));
            }
            let Some(other) = defs.get(collection) else {
                return Err(format!("{at} looks up '{collection}', which doesn't exist"));
            };
            let live = |id: &str, role: &str| {
                other.by_id(id).filter(|x| x.live()).ok_or_else(|| {
                    format!(
                        "{at} looks up '{collection}' by a {role} field that is removed or gone; change that lookup first"
                    )
                })
            };
            let k = live(key, "key")?;
            let v = live(value, "value")?;
            if k.kind != Kind::Text {
                return Err(format!(
                    "{at}: the lookup's key '{}' of '{collection}' must be text (it holds the patterns)",
                    k.name
                ));
            }
            if !(v.kind == f.kind || v.kind == Kind::Text || f.kind == Kind::Text) {
                return Err(format!(
                    "{at}: the lookup's value '{}' of '{collection}' is {}, which doesn't fit a {} field (use text or the same type)",
                    v.name,
                    v.kind.as_str(),
                    f.kind.as_str()
                ));
            }
            if *learn {
                if let Some(x) = other.live_fields().find(|x| x.computed()) {
                    return Err(format!(
                        "{at} learns into '{collection}', whose field '{}' is computed (not supported)",
                        x.name
                    ));
                }
                if let Some(x) = other
                    .live_fields()
                    .find(|x| x.id != k.id && x.id != v.id && !x.optional && x.default.is_none())
                {
                    return Err(format!(
                        "{at} learns into '{collection}', but its field '{}' is required without a default (a learned rule has only the key and the value)",
                        x.name
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The text `text` as a cache key: lowercase, inner spaces collapsed.
pub fn cache_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether `s` is a valid collection or field name: `[a-z][a-z0-9_]*`, at
/// most [`MAX_NAME_CHARS`].
pub fn valid_name(s: &str) -> bool {
    let mut b = s.bytes();
    s.len() <= MAX_NAME_CHARS
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

// ---------- values ----------

/// Whether `s` is a real calendar date `YYYY-MM-DD`.
pub fn valid_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    let [y, m, d] = p.as_slice() else {
        return false;
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

/// A JSON number from `f`, integral when it is.
fn number(f: f64) -> Option<Value> {
    if !f.is_finite() {
        return None;
    }
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Some(json!(f as i64))
    } else {
        serde_json::Number::from_f64(f).map(Value::Number)
    }
}

/// `value` as a value of `field` (lenient about how forms send things: the
/// text "3" for a number, "true" for a bool). `Ok(None)`: empty. `Err`: why
/// it isn't one. `max_chars`: the longest text.
pub fn coerce(field: &Field, value: &Value, max_chars: usize) -> Result<Option<Value>, String> {
    coerce_kind(field.kind, &field.options, value, max_chars, false)
}

/// [`coerce`] for converting stored values to a new type (also bool <->
/// number as 1/0).
pub fn convert(field: &Field, value: &Value, max_chars: usize) -> Result<Option<Value>, String> {
    coerce_kind(field.kind, &field.options, value, max_chars, true)
}

fn coerce_kind(
    kind: Kind,
    options: &[String],
    value: &Value,
    max_chars: usize,
    converting: bool,
) -> Result<Option<Value>, String> {
    if value.is_null() || value.as_str().is_some_and(|s| s.trim().is_empty()) {
        return Ok(None);
    }
    let text = match value {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    let fail = || {
        Err(format!(
            "{} is not {}",
            value,
            match kind {
                Kind::Text => "text".to_owned(),
                Kind::Number => "a number".to_owned(),
                Kind::Bool => "true or false".to_owned(),
                Kind::Date => "a date (YYYY-MM-DD)".to_owned(),
                Kind::Choice => format!("one of {}", options.join(", ")),
            }
        ))
    };
    let Some(text) = text else { return fail() };
    match kind {
        Kind::Text => {
            if text.chars().count() > max_chars {
                return Err(format!("text is at most {max_chars} characters"));
            }
            // Keep the text as typed (inner spaces), trimmed at the ends.
            Ok(Some(json!(text)))
        }
        Kind::Number => match value {
            Value::Number(_) => Ok(Some(value.clone())),
            Value::Bool(b) if converting => Ok(Some(json!(i64::from(*b)))),
            _ => match text.parse::<f64>().ok().and_then(number) {
                Some(n) => Ok(Some(n)),
                None => fail(),
            },
        },
        Kind::Bool => match value {
            Value::Bool(b) => Ok(Some(json!(*b))),
            Value::Number(n)
                if converting && (n.as_f64() == Some(0.0) || n.as_f64() == Some(1.0)) =>
            {
                Ok(Some(json!(n.as_f64() == Some(1.0))))
            }
            _ => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(Some(json!(true))),
                "false" | "no" | "0" => Ok(Some(json!(false))),
                _ => fail(),
            },
        },
        Kind::Date => {
            // A timestamp keeps its date.
            let date = text.get(..10).unwrap_or(&text);
            if valid_date(date) && (text.len() == 10 || text.as_bytes().get(10) == Some(&b'T')) {
                Ok(Some(json!(date)))
            } else {
                fail()
            }
        }
        Kind::Choice => options
            .iter()
            .find(|o| **o == text)
            .or_else(|| options.iter().find(|o| o.eq_ignore_ascii_case(&text)))
            .map(|o| Some(json!(o)))
            .map_or_else(fail, Ok),
    }
}

// ---------- field specs ----------

/// Parse a field spec `{name, type, options?, optional?, default?}` (a new
/// field, id `id`).
pub fn parse_field(spec: &Value, id: String, max_chars: usize) -> Result<Field, String> {
    let name = spec["name"].as_str().unwrap_or("").trim().to_owned();
    if !valid_name(&name) {
        return Err(format!(
            "field name '{name}' must be [a-z][a-z0-9_]*, at most {MAX_NAME_CHARS} characters"
        ));
    }
    if SYSTEM_FIELDS.contains(&name.as_str()) {
        return Err(format!(
            "'{name}' is a name every row has ({}); pick another",
            SYSTEM_FIELDS.join(", ")
        ));
    }
    let kind_text = spec["type"].as_str().unwrap_or("");
    let kind = Kind::parse(kind_text).ok_or_else(|| {
        format!(
            "field '{name}': type '{kind_text}' is not one of {}",
            Kind::ALL.join(", ")
        )
    })?;
    let mut field = Field {
        id,
        name,
        kind,
        options: Vec::new(),
        optional: spec["optional"].as_bool().unwrap_or(false),
        default: None,
        aliases: Vec::new(),
        compute: Vec::new(),
        removed_at: None,
        purged: false,
    };
    set_options(&mut field, spec.get("options"))?;
    set_default(&mut field, spec.get("default"), max_chars)?;
    Ok(field)
}

/// Set a choice field's options from `v` (a list of strings).
pub fn set_options(field: &mut Field, v: Option<&Value>) -> Result<(), String> {
    match (field.kind, v) {
        (Kind::Choice, Some(Value::Array(items))) => {
            let mut opts: Vec<String> = Vec::new();
            for o in items {
                let o = o.as_str().map(str::trim).unwrap_or("");
                if o.is_empty() || o.chars().count() > MAX_OPTION_CHARS {
                    return Err(format!(
                        "field '{}': options are 1..={MAX_OPTION_CHARS} characters",
                        field.name
                    ));
                }
                if opts.iter().any(|x| x == o) {
                    return Err(format!(
                        "field '{}': option '{o}' is listed twice",
                        field.name
                    ));
                }
                opts.push(o.to_owned());
            }
            if opts.is_empty() || opts.len() > MAX_OPTIONS {
                return Err(format!(
                    "field '{}': a choice has 1..={MAX_OPTIONS} options",
                    field.name
                ));
            }
            field.options = opts;
            Ok(())
        }
        (Kind::Choice, None) if !field.options.is_empty() => Ok(()),
        (Kind::Choice, _) => Err(format!(
            "field '{}': a choice needs \"options\": [..]",
            field.name
        )),
        (_, None | Some(Value::Null)) => {
            field.options.clear();
            Ok(())
        }
        (_, Some(_)) => Err(format!("field '{}': only a choice has options", field.name)),
    }
}

/// Set a field's default from `v` (checked against its type).
pub fn set_default(field: &mut Field, v: Option<&Value>, max_chars: usize) -> Result<(), String> {
    match v {
        None => {
            // A default that no longer fits the type goes.
            if let Some(d) = field.default.clone() {
                field.default = coerce(field, &d, max_chars).ok().flatten();
            }
            Ok(())
        }
        Some(Value::Null) => {
            field.default = None;
            Ok(())
        }
        Some(d) => {
            field.default = Some(
                coerce(field, d, max_chars)
                    .map_err(|e| format!("field '{}': default {e}", field.name))?
                    .ok_or_else(|| format!("field '{}': the default is empty", field.name))?,
            );
            Ok(())
        }
    }
}

/// Check a whole definition: unique names (aliases included), valid order
/// fields, computed fields that can be computed.
pub fn check_definition(def: &Definition, max_fields: usize) -> Result<(), String> {
    if !valid_name(&def.name) {
        return Err(format!(
            "collection name '{}' must be [a-z][a-z0-9_]*, at most {MAX_NAME_CHARS} characters",
            def.name
        ));
    }
    let t = def.title.trim();
    if t.is_empty() || t.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("\"title\" is 1..={MAX_TITLE_CHARS} characters"));
    }
    let live: Vec<&Field> = def.live_fields().collect();
    if live.is_empty() {
        return Err("a collection has at least one field".into());
    }
    if live.len() > max_fields {
        return Err(format!("a collection has at most {max_fields} fields"));
    }
    for (i, f) in live.iter().enumerate() {
        for other in &live[..i] {
            if let Some(n) = std::iter::once(&f.name)
                .chain(&f.aliases)
                .find(|n| other.answers_to(n))
            {
                return Err(format!(
                    "fields '{}' and '{}' would both answer to '{n}'",
                    other.name, f.name
                ));
            }
        }
    }
    for (i, o) in def.order_by.iter().enumerate() {
        if !def.by_id(o).is_some_and(Field::live) {
            return Err("an order field is not a live field".into());
        }
        if def.order_by[..i].contains(o) {
            return Err("a field is listed twice in the order".into());
        }
    }
    crate::remind::check(def)?;
    crate::filler::check(def, u32::MAX)?;
    if def.order_by.len() > MAX_ORDER_KEYS {
        return Err(format!(
            "rows are ordered by at most {MAX_ORDER_KEYS} fields"
        ));
    }
    for f in &live {
        if !f.computed() {
            continue;
        }
        if !f.optional && f.default.is_none() {
            return Err(format!(
                "field '{}' is computed, so it must be optional or have a default (what a row reads until it is computed)",
                f.name
            ));
        }
        if f.compute.len() > MAX_COMPUTES {
            return Err(format!(
                "field '{}': a compute list has at most {MAX_COMPUTES} kinds",
                f.name
            ));
        }
        for (i, c) in f.compute.iter().enumerate() {
            match c {
                Compute::Expr { expr } if c.parsed().is_none() => {
                    return Err(format!(
                        "field '{}': compute.expr '{expr}' doesn't parse",
                        f.name
                    ));
                }
                Compute::Model { .. } if i + 1 != f.compute.len() => {
                    return Err(format!(
                        "field '{}': a model answers in the background, so it is the last kind of the compute list",
                        f.name
                    ));
                }
                _ => {}
            }
            if c.sources().is_empty() {
                return Err(format!(
                    "field '{}' is computed from no field (an expression reads entity.<field>)",
                    f.name
                ));
            }
        }
        if f.compute
            .iter()
            .filter(|c| matches!(c, Compute::Lookup { learn: true, .. }))
            .count()
            > 1
        {
            return Err(format!(
                "field '{}': only one lookup of its compute list may learn",
                f.name
            ));
        }
        for id in f.compute_sources() {
            match def.by_id(&id) {
                Some(src) if src.id == f.id => {
                    return Err(format!("field '{}' can't be computed from itself", f.name));
                }
                Some(src) if !src.live() => {
                    return Err(format!(
                        "field '{}' is computed from '{}', which is removed; change or drop its compute first",
                        f.name, src.name
                    ));
                }
                Some(src) if src.computed() => {
                    return Err(format!(
                        "field '{}' is computed from '{}', which is computed itself (not supported yet)",
                        f.name, src.name
                    ));
                }
                Some(_) => {}
                None => {
                    return Err(format!(
                        "field '{}' is computed from a field that doesn't exist",
                        f.name
                    ));
                }
            }
        }
    }
    Ok(())
}

// ---------- rows ----------

/// A row's values as an entity: `{id, deleted, created_at, updated_at,
/// <every live name and alias>: value}` (empty: the default, else null).
pub fn row_entity(
    def: &Definition,
    id: &str,
    values: &Map<String, Value>,
    created_at: &str,
    updated_at: &str,
) -> Map<String, Value> {
    let mut e = Map::new();
    e.insert("id".into(), json!(id));
    e.insert("deleted".into(), json!(false));
    e.insert("created_at".into(), json!(created_at));
    e.insert("updated_at".into(), json!(updated_at));
    for f in def.live_fields() {
        let v = values
            .get(&f.id)
            .cloned()
            .unwrap_or_else(|| f.empty_value());
        for n in &f.aliases {
            e.insert(n.clone(), v.clone());
        }
        e.insert(f.name.clone(), v);
    }
    e
}

/// A row's values by name (live fields only), for exports and previews.
pub fn by_name(def: &Definition, values: &Map<String, Value>) -> Map<String, Value> {
    def.live_fields()
        .map(|f| {
            (
                f.name.clone(),
                values
                    .get(&f.id)
                    .cloned()
                    .unwrap_or_else(|| f.empty_value()),
            )
        })
        .collect()
}

/// How a write treats names it doesn't have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// A new row: required fields without a default must be given.
    Create,
    /// Changing some fields of a row.
    Update,
}

/// Apply a write's `{name: value}` pairs to `values` (by field id), under
/// `def`: names resolve through aliases; removed fields' names and the
/// system fields are ignored (a write made under an older definition still
/// lands); an unknown name or a value of the wrong type is refused.
pub fn apply_write(
    def: &Definition,
    values: &mut Map<String, Value>,
    write: &Map<String, Value>,
    mode: Write,
    max_chars: usize,
) -> Result<(), String> {
    for (name, v) in write {
        if SYSTEM_FIELDS.contains(&name.as_str()) {
            continue;
        }
        let Some(field) = def.field(name) else {
            if def.is_removed_name(name) {
                continue;
            }
            return Err(format!(
                "'{name}' is not a field of '{}' (fields: {})",
                def.name,
                def.live_fields()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        match coerce(field, v, max_chars).map_err(|e| format!("'{name}': {e}"))? {
            Some(v) => {
                values.insert(field.id.clone(), v);
            }
            None if field.optional || field.default.is_some() => {
                values.remove(&field.id);
            }
            None => return Err(format!("'{}' can't be empty", field.name)),
        }
    }
    if mode == Write::Create {
        for f in def.live_fields() {
            if !values.contains_key(&f.id) && !f.optional && f.default.is_none() {
                return Err(format!("'{}' is required", f.name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(spec: Value) -> Field {
        parse_field(&spec, "f1".into(), 100).unwrap()
    }

    fn todos() -> Definition {
        Definition {
            name: "todos".into(),
            title: "Todos".into(),
            fields: vec![
                field(json!({"name": "title", "type": "text"})),
                Field {
                    id: "f2".into(),
                    ..field(json!({"name": "done", "type": "bool", "default": false}))
                },
                Field {
                    id: "f3".into(),
                    ..field(json!({"name": "due", "type": "date", "optional": true}))
                },
            ],
            order_by: Vec::new(),
            descending: false,
            next_field: 4,
            reminders: Vec::new(),
            filler: None,
        }
    }

    #[test]
    fn values_are_coerced_leniently_and_checked() {
        let n = field(json!({"name": "n", "type": "number"}));
        assert_eq!(coerce(&n, &json!("3"), 9), Ok(Some(json!(3))));
        assert_eq!(coerce(&n, &json!(" 2.5 "), 9), Ok(Some(json!(2.5))));
        assert_eq!(coerce(&n, &json!(""), 9), Ok(None));
        assert!(coerce(&n, &json!("three"), 9).is_err());
        assert!(coerce(&n, &json!(true), 9).is_err());
        assert_eq!(convert(&n, &json!(true), 9), Ok(Some(json!(1))));
        let b = field(json!({"name": "b", "type": "bool"}));
        assert_eq!(coerce(&b, &json!("true"), 9), Ok(Some(json!(true))));
        assert_eq!(coerce(&b, &json!("No"), 9), Ok(Some(json!(false))));
        assert!(coerce(&b, &json!("maybe"), 9).is_err());
        let d = field(json!({"name": "d", "type": "date"}));
        assert_eq!(
            coerce(&d, &json!("2026-11-04"), 9),
            Ok(Some(json!("2026-11-04")))
        );
        assert_eq!(
            coerce(&d, &json!("2026-11-04T10:00:00Z"), 9),
            Ok(Some(json!("2026-11-04")))
        );
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "tomorrow",
            "2026-1-4",
            "2026-11-04x",
        ] {
            assert!(coerce(&d, &json!(bad), 9).is_err(), "{bad}");
        }
        assert!(valid_date("2024-02-29") && !valid_date("2023-02-29"));
        let c = field(json!({"name": "p", "type": "choice", "options": ["low", "high"]}));
        assert_eq!(coerce(&c, &json!("HIGH"), 9), Ok(Some(json!("high"))));
        assert!(coerce(&c, &json!("mid"), 9).is_err());
        let t = field(json!({"name": "t", "type": "text"}));
        assert_eq!(coerce(&t, &json!(12), 9), Ok(Some(json!("12"))));
        assert!(coerce(&t, &json!("x".repeat(10)), 9).is_err());
        assert!(coerce(&t, &json!({"a": 1}), 9).is_err());
    }

    #[test]
    fn field_specs_are_checked() {
        for bad in [
            json!({"name": "Title", "type": "text"}),
            json!({"name": "id", "type": "text"}),
            json!({"name": "x", "type": "money"}),
            json!({"name": "x", "type": "choice"}),
            json!({"name": "x", "type": "choice", "options": ["a", "a"]}),
            json!({"name": "x", "type": "text", "options": ["a"]}),
            json!({"name": "x", "type": "number", "default": "lots"}),
        ] {
            assert!(parse_field(&bad, "f1".into(), 9).is_err(), "{bad}");
        }
        let f = field(json!({"name": "n", "type": "number", "default": "4"}));
        assert_eq!(f.default, Some(json!(4)));
    }

    #[test]
    fn writes_resolve_aliases_ignore_removed_fields_and_refuse_bad_types() {
        let mut def = todos();
        // `due` was renamed `deadline`; `done` was removed.
        def.fields[2].aliases.push("due".into());
        def.fields[2].name = "deadline".into();
        def.fields[1].removed_at = Some(1);
        let mut values = Map::new();
        let w = json!({"title": "Milk", "due": "2026-11-05", "done": true, "id": "x"});
        apply_write(&def, &mut values, w.as_object().unwrap(), Write::Create, 99).unwrap();
        assert_eq!(
            values,
            *json!({"f1": "Milk", "f3": "2026-11-05"})
                .as_object()
                .unwrap()
        );
        let bad = json!({"deadline": "soon"});
        assert!(
            apply_write(
                &def,
                &mut values,
                bad.as_object().unwrap(),
                Write::Update,
                99
            )
            .is_err()
        );
        let unknown = json!({"colour": "red"});
        assert!(
            apply_write(
                &def,
                &mut values,
                unknown.as_object().unwrap(),
                Write::Update,
                99
            )
            .is_err()
        );
        let empty_required = json!({"title": " "});
        assert!(
            apply_write(
                &def,
                &mut values,
                empty_required.as_object().unwrap(),
                Write::Update,
                99
            )
            .is_err()
        );
        let missing = json!({"deadline": "2026-11-05"});
        let mut fresh = Map::new();
        assert!(
            apply_write(
                &def,
                &mut fresh,
                missing.as_object().unwrap(),
                Write::Create,
                99
            )
            .is_err()
        );
        let e = row_entity(&def, "r1", &values, "a", "b");
        assert_eq!(e["deadline"], json!("2026-11-05"));
        assert_eq!(e["due"], json!("2026-11-05"), "the old name is served too");
        assert!(!e.contains_key("done"));
    }

    #[test]
    fn definitions_need_unique_names() {
        let mut def = todos();
        assert_eq!(check_definition(&def, 10), Ok(()));
        def.fields[2].aliases.push("title".into());
        assert!(check_definition(&def, 10).is_err());
        let mut def = todos();
        assert!(check_definition(&def, 2).is_err());
        def.order_by = vec!["f9".into()];
        assert!(check_definition(&def, 10).is_err());
        def.order_by = vec!["f1".into(), "f1".into()];
        assert!(check_definition(&def, 10).is_err());
    }

    #[test]
    fn orders_read_old_single_strings_and_write_one_key_as_a_string() {
        let mut def = todos();
        def.order_by = vec!["f2".into()];
        let text = serde_json::to_value(&def).unwrap();
        assert_eq!(text["order_by"], json!("f2"), "one key stays a string");
        let back: Definition = serde_json::from_value(text).unwrap();
        assert_eq!(back.order_by, vec!["f2".to_owned()]);
        def.order_by = vec!["f2".into(), "f1".into()];
        let text = serde_json::to_value(&def).unwrap();
        assert_eq!(text["order_by"], json!(["f2", "f1"]));
        let back: Definition = serde_json::from_value(text).unwrap();
        assert_eq!(back.order_by, def.order_by);
        def.order_by.clear();
        let text = serde_json::to_value(&def).unwrap();
        assert!(text.get("order_by").is_none());
        let mut with_null = text.clone();
        with_null["order_by"] = Value::Null;
        let back: Definition = serde_json::from_value(with_null).unwrap();
        assert!(back.order_by.is_empty());
        assert_eq!(
            order_names(Some(&json!(["a", " b "]))),
            Ok(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(order_names(Some(&json!([1]))).is_err());
    }

    #[test]
    fn computed_fields_read_other_live_fields_by_id() {
        let mut def = todos();
        let section = Field {
            id: "f4".into(),
            ..field(json!({"name": "section", "type": "choice",
                           "options": ["Produce", "Other"], "default": "Other"}))
        };
        let label = Field {
            id: "f5".into(),
            ..field(json!({"name": "label", "type": "text", "optional": true}))
        };
        def.fields.push(section);
        def.fields.push(label);
        set_compute(
            &mut def,
            "f4",
            Some(&json!({"kind": "model", "from": "title", "prompt": "Which store section?"})),
            &Defs::default(),
        )
        .unwrap();
        assert_eq!(
            def.by_id("f4").unwrap().compute,
            vec![Compute::Model {
                from: vec!["f1".into()],
                prompt: "Which store section?".into()
            }]
        );
        set_compute(
            &mut def,
            "f5",
            Some(&json!({"kind": "expr", "expr": "entity.done ? \"✓ \" + entity.title : entity.title"})), &Defs::default(),
        )
        .unwrap();
        assert_eq!(
            def.by_id("f5").unwrap().compute,
            vec![Compute::Expr {
                expr: "entity.f2 ? \"✓ \" + entity.f1 : entity.f1".into()
            }]
        );
        assert_eq!(check_definition(&def, 10), Ok(()));
        let d = def.describe_field(def.by_id("f5").unwrap(), &Defs::default());
        assert_eq!(
            d["compute"],
            json!({"kind": "expr", "expr": "entity.done ? \"✓ \" + entity.title : entity.title"})
        );
        let d = def.describe_field(def.by_id("f4").unwrap(), &Defs::default());
        assert_eq!(
            d["compute"],
            json!({"kind": "model", "from": ["title"], "prompt": "Which store section?"})
        );
        // Any type can be computed; a rename keeps the sources.
        def.fields[0].name = "name".into();
        assert_eq!(
            def.describe_field(def.by_id("f5").unwrap(), &Defs::default())["compute"]["expr"],
            "entity.done ? \"✓ \" + entity.name : entity.name"
        );
        def.fields[0].name = "title".into();
        // From itself, from a computed field, from a removed one.
        let mut bad = def.clone();
        set_compute(
            &mut bad,
            "f4",
            Some(&json!({"kind": "model", "from": "section", "prompt": "x"})),
            &Defs::default(),
        )
        .unwrap();
        assert!(check_definition(&bad, 10).is_err());
        let mut bad = def.clone();
        set_compute(
            &mut bad,
            "f5",
            Some(&json!({"kind": "expr", "expr": "entity.section"})),
            &Defs::default(),
        )
        .unwrap();
        assert!(check_definition(&bad, 10).is_err());
        let mut bad = def.clone();
        bad.fields[0].removed_at = Some(1);
        assert!(check_definition(&bad, 10).is_err());
        // Never required without a default; never from nothing.
        let mut bad = def.clone();
        bad.fields[3].default = None;
        assert!(check_definition(&bad, 10).is_err());
        let mut bad = def.clone();
        set_compute(
            &mut bad,
            "f5",
            Some(&json!({"kind": "expr", "expr": "\"x\""})),
            &Defs::default(),
        )
        .unwrap();
        assert!(check_definition(&bad, 10).is_err());
        for spec in [
            json!({"kind": "model", "from": "nope", "prompt": "x"}),
            json!({"kind": "model", "from": "title"}),
            json!({"kind": "model", "from": [], "prompt": "x"}),
            json!({"kind": "model", "from": ["title", "title"], "prompt": "x"}),
            json!({"kind": "expr", "expr": "entity.nope"}),
            json!({"kind": "expr", "expr": "size(entity.title)"}),
            json!({"kind": "sql", "expr": "x"}),
            json!("entity.title"),
        ] {
            assert!(
                set_compute(&mut def.clone(), "f4", Some(&spec), &Defs::default()).is_err(),
                "{spec}"
            );
        }
        set_compute(&mut def, "f4", Some(&Value::Null), &Defs::default()).unwrap();
        assert!(def.by_id("f4").unwrap().compute.is_empty());
        assert_eq!(cache_key("  Oat   MILK "), "oat milk");
    }
}
