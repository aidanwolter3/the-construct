//! Component definitions (decision `plumbing-and-porcelain`), moved here
//! from `pod_core::components` so the pod and the engine expand them the
//! same way (decision `content-addressed-construct`).
//!
//! A definition is `{doc, params, children?, template, examples}`. In a
//! template, `$arg.<name>` (or `$in.<name>`) is the argument (a whole string
//! `"$arg.n"` takes the argument's own JSON type), `{"type":
//! "core://Children"}` is where the caller's children go (`"at": i` for one
//! of them), `{"type": "core://IfArg", "arg", "equals"?, "then", "else"?}`
//! keeps a branch by an argument, and `{"type": "core://Fragment",
//! "children"}` stands for several siblings. [`EACH_TYPE`] repeats a piece
//! over a list argument, [`MATCH_TYPE`] chooses one by a key, and a
//! `core://Children` with `"slot": "<name>"` takes the nodes a use passes
//! as `"slots": {"<name>": [...]}` (`core://IfArg` with `slot` tests
//! whether one was passed).

use serde_json::{Map, Value};

/// The node that uses a component.
pub const USE_TYPE: &str = "core://Use";
/// Where a template puts the caller's children.
pub const CHILDREN_TYPE: &str = "core://Children";
/// Several sibling nodes from one template (spliced into the list it is
/// in; a `core://Column` of them elsewhere).
pub const FRAGMENT_TYPE: &str = "core://Fragment";
/// A template branch chosen by an argument.
pub const IF_ARG_TYPE: &str = "core://IfArg";
/// A template piece repeated once per entry of a list argument (decision
/// `element-roles-and-reuse`): `{"type": "core://Each", "arg": "<list
/// argument>", "as": "<name>", "template": <node>}`. Inside the template,
/// `$arg.<name>` is the entry (when it is one value), `$arg.<name>_<key>`
/// a key of an entry that is an object, and `$arg.<name>_index` its place
/// from 0.
pub const EACH_TYPE: &str = "core://Each";
/// A template piece chosen by a key: `{"type": "core://Match", "arg" |
/// "value", "cases": {"<key>": <node>}, "else"?: <node>}`. With `arg` the
/// case the argument names is kept when the component is expanded. With
/// `value` (a binding) every case stays, each under a `core://Conditional`
/// that shows it while the binding equals its key, and `else` while it
/// equals none of them.
pub const MATCH_TYPE: &str = "core://Match";
/// The most entries a `core://Each` repeats over.
pub const MAX_EACH: usize = 200;

/// The `as` of every `core://Each` in `v` (`item` when it has none).
fn each_names<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some(EACH_TYPE) {
                out.push(o.get("as").and_then(Value::as_str).unwrap_or("item"));
            }
            for c in o.values() {
                each_names(c, out);
            }
        }
        Value::Array(a) => {
            for c in a {
                each_names(c, out);
            }
        }
        _ => {}
    }
}

/// Whether `v` (an element's `node`, or any node) uses what feature
/// `component_templates` covers: `core://Each`, `core://Match`, a
/// `core://Children` or `core://IfArg` with `slot`, or a use with `slots`.
pub fn uses_templates(v: &Value) -> bool {
    match v {
        Value::Object(o) => {
            let ty = o.get("type").and_then(Value::as_str);
            matches!(ty, Some(EACH_TYPE | MATCH_TYPE))
                || (matches!(ty, Some(CHILDREN_TYPE | IF_ARG_TYPE)) && o.contains_key("slot"))
                || (ty == Some(USE_TYPE) && o.contains_key("slots"))
                || o.values().any(uses_templates)
        }
        Value::Array(a) => a.iter().any(uses_templates),
        _ => false,
    }
}
/// Longest component name (after the prefix).
pub const MAX_NAME_CHARS: usize = 48;
/// Most parameters of one component.
pub const MAX_PARAMS: usize = 24;
/// Most examples of one component.
pub const MAX_EXAMPLES: usize = 8;
/// Largest definition.
pub const MAX_DEF_BYTES: usize = 64 * 1024;
/// The kind of value a parameter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// Any text (bindings such as `$item.title` included).
    Text,
    Number,
    Bool,
    /// `scheme://...`, or a binding to one.
    Uri,
    /// A binding (`$item.x`, `${uri#f}`, `$form.x`) or text.
    Binding,
    /// Any JSON value but null, put in whole where the template's string is
    /// only `$arg.<name>`: an intent, a `count` query, a list of `where`
    /// conditions (decision `component-json-params`).
    Json,
}

impl ParamType {
    pub const NAMES: [&'static str; 6] = ["text", "number", "bool", "uri", "binding", "json"];

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => Self::Text,
            "number" => Self::Number,
            "bool" => Self::Bool,
            "uri" => Self::Uri,
            "binding" => Self::Binding,
            "json" => Self::Json,
            _ => return None,
        })
    }

    /// Whether `v` is a value of this type.
    pub fn accepts(self, v: &Value) -> bool {
        match (self, v) {
            (Self::Number, Value::Number(_)) | (Self::Bool, Value::Bool(_)) => true,
            (Self::Text | Self::Binding, Value::String(_)) => true,
            (Self::Uri, Value::String(s)) => s.contains("://") || s.starts_with('$'),
            (Self::Json, v) => !v.is_null(),
            _ => false,
        }
    }
}

/// A parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub kind: ParamType,
    pub default: Option<Value>,
    pub required: bool,
    pub doc: String,
}

/// An example use (`args`, `children`), expanded when the definition is
/// checked and by `get_component`.
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    pub args: Map<String, Value>,
    pub children: Vec<Value>,
    pub note: String,
}

/// A parsed component definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    pub doc: String,
    pub params: Vec<Param>,
    /// Takes children (`core://Children` in the template).
    pub children: bool,
    pub template: Value,
    pub examples: Vec<Example>,
}

/// Whether `name` is `[a-z][a-z0-9_]*`, at most [`MAX_NAME_CHARS`].
pub fn valid_bare_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_CHARS
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The `$arg.<name>` names a string uses.
pub fn arg_names(s: &str) -> Vec<&str> {
    s.match_indices("$arg.")
        .map(|(i, m)| {
            let rest = &s[i + m.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect()
}

fn collect_strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|c| collect_strings(c, out)),
        Value::Object(m) => m.iter().for_each(|(k, c)| {
            out.push(k);
            collect_strings(c, out);
        }),
        _ => {}
    }
}

fn has_type(v: &Value, t: &str) -> bool {
    match v {
        Value::Object(m) => {
            m.get("type").and_then(Value::as_str) == Some(t) || m.values().any(|c| has_type(c, t))
        }
        Value::Array(a) => a.iter().any(|c| has_type(c, t)),
        _ => false,
    }
}

impl Def {
    /// Parse and check a definition (`Err`: what is wrong, for the writer).
    pub fn parse(v: &Value) -> Result<Def, String> {
        let Value::Object(o) = v else {
            return Err(
                "a component is {\"doc\", \"params\", \"children\"?, \"template\", \"examples\"}"
                    .into(),
            );
        };
        if let Some(k) = o.keys().find(|k| {
            !matches!(
                k.as_str(),
                "doc" | "params" | "children" | "template" | "examples"
            )
        }) {
            return Err(format!(
                "unknown key '{k}' (doc, params, children, template, examples)"
            ));
        }
        if serde_json::to_vec(v).map_or(usize::MAX, |b| b.len()) > MAX_DEF_BYTES {
            return Err(format!("the definition is over {MAX_DEF_BYTES} bytes"));
        }
        let doc = o
            .get("doc")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .ok_or("\"doc\" (what it draws and when to use it) is a non-empty string")?
            .to_owned();
        let mut params = Vec::new();
        for (i, p) in o
            .get("params")
            .map(|p| p.as_array().ok_or("\"params\" is a list"))
            .transpose()?
            .into_iter()
            .flatten()
            .enumerate()
        {
            let at = format!("params/{i}");
            let name = p["name"]
                .as_str()
                .filter(|n| valid_bare_name(n))
                .ok_or_else(|| format!("{at}: \"name\" is [a-z][a-z0-9_]*"))?;
            if params.iter().any(|q: &Param| q.name == name) {
                return Err(format!("{at}: '{name}' is listed twice"));
            }
            let kind = p["type"]
                .as_str()
                .and_then(ParamType::parse)
                .ok_or_else(|| {
                    format!("{at}: \"type\" is one of {}", ParamType::NAMES.join(", "))
                })?;
            let default = p.get("default").filter(|d| !d.is_null()).cloned();
            if let Some(d) = &default
                && !kind.accepts(d)
            {
                return Err(format!("{at}: the default doesn't fit type {}", p["type"]));
            }
            let required = p["required"].as_bool().unwrap_or(false);
            if required && default.is_some() {
                return Err(format!("{at}: a required parameter has no default"));
            }
            params.push(Param {
                name: name.to_owned(),
                kind,
                default,
                required,
                doc: p["doc"].as_str().unwrap_or("").trim().to_owned(),
            });
        }
        if params.len() > MAX_PARAMS {
            return Err(format!("at most {MAX_PARAMS} parameters"));
        }
        let children = o.get("children").and_then(Value::as_bool).unwrap_or(false);
        let template = o
            .get("template")
            .filter(|t| t.get("type").and_then(Value::as_str).is_some())
            .ok_or("\"template\" is a node ({\"type\": ...})")?
            .clone();
        let mut strings = Vec::new();
        collect_strings(&template, &mut strings);
        // What a `core://Each` names its entry: `$arg.<as>`, its keys
        // `$arg.<as>_<key>` and `$arg.<as>_index`.
        let mut entries = Vec::new();
        each_names(&template, &mut entries);
        for s in strings {
            for n in arg_names(s) {
                let of_each = entries
                    .iter()
                    .any(|e| n == *e || n.strip_prefix(e).is_some_and(|r| r.starts_with('_')));
                if !of_each && !params.iter().any(|p| p.name == n) {
                    return Err(format!(
                        "the template uses $arg.{n}, which is not a parameter"
                    ));
                }
            }
        }
        if !children && has_type(&template, CHILDREN_TYPE) {
            return Err("the template has core://Children but \"children\" is not true".into());
        }
        let mut examples = Vec::new();
        for (i, e) in o
            .get("examples")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let args = match e.get("args") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(m)) => m.clone(),
                _ => return Err(format!("examples/{i}: \"args\" is an object")),
            };
            let kids = match e.get("children") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => a.clone(),
                _ => return Err(format!("examples/{i}: \"children\" is a list of nodes")),
            };
            examples.push(Example {
                args,
                children: kids,
                note: e["note"].as_str().unwrap_or("").to_owned(),
            });
        }
        if examples.is_empty() {
            return Err("give at least one example ({\"args\": {...}, \"note\": \"...\"})".into());
        }
        if examples.len() > MAX_EXAMPLES {
            return Err(format!("at most {MAX_EXAMPLES} examples"));
        }
        Ok(Def {
            doc,
            params,
            children,
            template,
            examples,
        })
    }

    /// The arguments of a use with defaults filled in; `Err`: what is wrong.
    pub fn bind(&self, args: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        if let Some(k) = args
            .keys()
            .find(|k| !self.params.iter().any(|p| &p.name == *k))
        {
            return Err(format!(
                "no parameter '{k}' (parameters: {})",
                self.param_names().join(", ")
            ));
        }
        let mut out = Map::new();
        for p in &self.params {
            match args.get(&p.name).filter(|v| !v.is_null()) {
                Some(v) if p.kind.accepts(v) => {
                    out.insert(p.name.clone(), v.clone());
                }
                Some(v) => {
                    return Err(format!(
                        "'{}' is {}, not {v}",
                        p.name,
                        ParamType::NAMES[p.kind as usize]
                    ));
                }
                None if p.required => return Err(format!("'{}' is required", p.name)),
                None => {
                    out.insert(p.name.clone(), p.default.clone().unwrap_or(Value::Null));
                }
            }
        }
        Ok(out)
    }

    fn param_names(&self) -> Vec<&str> {
        self.params.iter().map(|p| p.name.as_str()).collect()
    }
}
