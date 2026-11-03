//! A collection's definition and its values (decision `collections`): typed
//! fields with permanent ids, aliases for old names, soft removal, and the
//! checks every write goes through.
//!
//! Rows store their values as JSON keyed by **field id** (`f1`, `f2`, ...),
//! so a rename moves no data. Every name a field ever had stays an alias:
//! writes and bindings that use it still reach the field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Longest collection or field name.
pub const MAX_NAME_CHARS: usize = 32;
/// Longest title.
pub const MAX_TITLE_CHARS: usize = 60;
/// Most options of a choice field.
pub const MAX_OPTIONS: usize = 50;
/// Longest choice option.
pub const MAX_OPTION_CHARS: usize = 60;
/// Longest `infer` hint.
pub const MAX_HINT_CHARS: usize = 120;
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
    /// Where an empty value comes from (decision
    /// `collection-inferred-fields`): a choice field the pod fills in from
    /// another field's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infer: Option<Infer>,
    /// Soft-removed at (unix seconds): hidden, its data kept for the purge
    /// window and restorable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<i64>,
    /// Its data was purged (after the window): it can't come back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub purged: bool,
}

/// How a choice field's value is inferred: from the text of field `from`
/// (a field id, so a rename keeps it), classified into one of the options
/// with `hint` saying what the options are ("grocery store section").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Infer {
    pub from: String,
    pub hint: String,
}

impl Field {
    pub fn live(&self) -> bool {
        self.removed_at.is_none()
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

    /// [`Field::describe`] plus its `infer` (`{from: <field name>, hint}`).
    pub fn describe_field(&self, f: &Field) -> Value {
        let mut v = f.describe();
        if let Some(i) = &f.infer {
            let from = self
                .by_id(&i.from)
                .map_or(i.from.as_str(), |s| s.name.as_str());
            v["infer"] = json!({ "from": from, "hint": i.hint });
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
    pub fn describe(&self, now: i64, window: i64) -> Value {
        let removed: Vec<Value> = self
            .fields
            .iter()
            .filter(|f| !f.live() && !f.purged)
            .map(|f| {
                let mut v = self.describe_field(f);
                let left = (f.removed_at.unwrap_or(now) + window - now).max(0);
                v["restorable_for_days"] = json!((left + 86_399) / 86_400);
                v
            })
            .collect();
        json!({
            "name": self.name,
            "title": self.title,
            "fields": self.live_fields().map(|f| self.describe_field(f)).collect::<Vec<_>>(),
            "removed_fields": removed,
            "order_by": order_json(self.order_fields().map(|f| f.name.clone()).collect()),
            "descending": self.descending,
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

/// Set field `id`'s `infer` from a spec `{from, hint?}` (`null`: no
/// inference; `None`: unchanged). `from` names a field of `def`.
pub fn set_infer(def: &mut Definition, id: &str, v: Option<&Value>) -> Result<(), String> {
    let Some(v) = v else { return Ok(()) };
    let name = def.by_id(id).map(|f| f.name.clone()).unwrap_or_default();
    let infer = match v {
        Value::Null => None,
        Value::Object(o) => {
            let from_name = o.get("from").and_then(Value::as_str).unwrap_or("").trim();
            let from = def
                .field(from_name)
                .ok_or_else(|| {
                    format!("field '{name}': infer.from '{from_name}' is not one of the fields")
                })?
                .id
                .clone();
            let hint = match o.get("hint") {
                None | Some(Value::Null) => name.replace('_', " "),
                Some(Value::String(h)) => h.trim().to_owned(),
                Some(_) => return Err(format!("field '{name}': infer.hint is text")),
            };
            if hint.is_empty() || hint.chars().count() > MAX_HINT_CHARS {
                return Err(format!(
                    "field '{name}': infer.hint is 1..={MAX_HINT_CHARS} characters"
                ));
            }
            Some(Infer { from, hint })
        }
        _ => {
            return Err(format!(
                "field '{name}': infer is {{from: <text field>, hint?}} or null"
            ));
        }
    };
    if let Some(f) = def.fields.iter_mut().find(|f| f.id == id) {
        f.infer = infer;
    }
    Ok(())
}

/// The text `text` as a cache key: lowercase, inner spaces collapsed.
pub fn infer_key(text: &str) -> String {
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
        infer: None,
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
/// fields, inferred fields that can be inferred.
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
    if def.order_by.len() > MAX_ORDER_KEYS {
        return Err(format!(
            "rows are ordered by at most {MAX_ORDER_KEYS} fields"
        ));
    }
    for f in &live {
        let Some(infer) = &f.infer else { continue };
        if f.kind != Kind::Choice {
            return Err(format!(
                "field '{}' infers its value, so it must be a choice (the answer is one of its options)",
                f.name
            ));
        }
        if !f.optional && f.default.is_none() {
            return Err(format!(
                "field '{}' infers its value, so it must be optional or have a default (what a row reads until it is inferred)",
                f.name
            ));
        }
        match def.by_id(&infer.from) {
            Some(src) if src.live() && src.kind == Kind::Text && src.id != f.id => {}
            Some(src) if src.live() && src.id != f.id => {
                return Err(format!(
                    "field '{}' infers from '{}', which must be a text field",
                    f.name, src.name
                ));
            }
            Some(src) if src.id != f.id => {
                return Err(format!(
                    "field '{}' infers from '{}', which is removed; change or drop its infer first",
                    f.name, src.name
                ));
            }
            _ => {
                return Err(format!(
                    "field '{}' must infer from another text field",
                    f.name
                ));
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
    fn inferred_fields_are_choices_from_a_text_field() {
        let mut def = todos();
        let section = Field {
            id: "f4".into(),
            ..field(json!({"name": "section", "type": "choice",
                           "options": ["Produce", "Other"], "default": "Other"}))
        };
        def.fields.push(section);
        set_infer(&mut def, "f4", Some(&json!({"from": "title"}))).unwrap();
        assert_eq!(
            def.by_id("f4").unwrap().infer,
            Some(Infer {
                from: "f1".into(),
                hint: "section".into()
            })
        );
        assert_eq!(check_definition(&def, 10), Ok(()));
        let d = def.describe_field(def.by_id("f4").unwrap());
        assert_eq!(d["infer"], json!({"from": "title", "hint": "section"}));
        // From a non-text field, or itself.
        let mut bad = def.clone();
        set_infer(&mut bad, "f4", Some(&json!({"from": "done"}))).unwrap();
        assert!(check_definition(&bad, 10).is_err());
        let mut bad = def.clone();
        set_infer(&mut bad, "f4", Some(&json!({"from": "section"}))).unwrap();
        assert!(check_definition(&bad, 10).is_err());
        // Its source removed.
        let mut bad = def.clone();
        bad.fields[0].removed_at = Some(1);
        assert!(check_definition(&bad, 10).is_err());
        // Only a choice; never required without a default.
        let mut bad = def.clone();
        bad.fields[3].kind = Kind::Text;
        assert!(check_definition(&bad, 10).is_err());
        let mut bad = def.clone();
        bad.fields[3].default = None;
        assert!(check_definition(&bad, 10).is_err());
        assert!(set_infer(&mut def.clone(), "f4", Some(&json!({"from": "nope"}))).is_err());
        assert!(set_infer(&mut def.clone(), "f4", Some(&json!("title"))).is_err());
        set_infer(&mut def, "f4", Some(&Value::Null)).unwrap();
        assert!(def.by_id("f4").unwrap().infer.is_none());
        assert_eq!(infer_key("  Oat   MILK "), "oat milk");
    }
}
