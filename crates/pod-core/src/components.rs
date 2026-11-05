//! Components (decision `plumbing-and-porcelain`): porcelain defined as
//! data. A component is a node tree with parameters, `{doc, params,
//! children?, template, examples}`, that the pod expands into plumbing
//! before serving ([`crate::parts`]); clients never see one. A layout uses
//! one with `{"type": "core://Use", "component": "core:section_header",
//! "args": {...}, "children": [...], "version"?: n}`.
//!
//! Two sources, one namespace each:
//!
//! - `core:<name>`: server components, files `crates/pod-core/components/
//!   <name>/NNNN.json` (`{"note", "component"}`) embedded in the binary and
//!   recorded in each pod's `components` table at start. A deployed file
//!   never changes (the pod refuses to start, as for layout files) and is
//!   never removed. Blueprints are components too: `core:<blueprint>` is a
//!   whole layout and `core:<blueprint>/<part>` one of its parts
//!   (`screen:<id>`, `slot:<id>`, a declared part).
//! - `pod:<name>`: the pod's own, written by an agent ([`put_pod`]),
//!   versioned, restorable, deletable while no layout uses them.
//!
//! In a template, `$arg.<name>` is the argument (a whole string `"$arg.n"`
//! takes the argument's own JSON type), `{"type": "core://Children"}` is
//! where the caller's children go (`"at": i` for one of them), and
//! `{"type": "core://IfArg", "arg", "equals"?, "then", "else"?}` keeps a
//! branch by an argument, and `{"type": "core://Fragment", "children"}`
//! stands for several siblings. Runtime bindings (`$item`, `$form`, `${uri#f}`,
//! `$route`) pass through untouched.

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::store::Store;
use crate::util::now_secs;

/// One embedded server component file.
#[derive(Debug, Clone, Copy)]
pub struct ComponentSource {
    pub name: &'static str,
    /// `NNNN`.
    pub file: &'static str,
    pub text: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/component_files.rs"));

/// Prefix of server components.
pub const CORE: &str = "core:";
/// Prefix of the pod's own components.
pub const POD: &str = "pod:";
/// The node that uses a component.
pub const USE_TYPE: &str = "core://Use";
/// Where a template puts the caller's children.
pub const CHILDREN_TYPE: &str = "core://Children";
/// Several sibling nodes from one template (spliced into the list it is
/// in; a `core://Column` of them elsewhere).
pub const FRAGMENT_TYPE: &str = "core://Fragment";
/// A template branch chosen by an argument.
pub const IF_ARG_TYPE: &str = "core://IfArg";
/// Longest component name (after the prefix).
pub const MAX_NAME_CHARS: usize = 48;
/// Most parameters of one component.
pub const MAX_PARAMS: usize = 24;
/// Most examples of one component.
pub const MAX_EXAMPLES: usize = 8;
/// Largest definition.
pub const MAX_DEF_BYTES: usize = 64 * 1024;
/// Most `pod:` components a pod keeps.
pub const MAX_POD_COMPONENTS: usize = 200;
/// Versions kept per `pod:` component (the oldest are pruned).
pub const MAX_POD_VERSIONS: usize = 100;
/// `author` of server components.
pub const AUTHOR_SERVER: &str = "server";

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
}

impl ParamType {
    pub const NAMES: [&'static str; 5] = ["text", "number", "bool", "uri", "binding"];

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => Self::Text,
            "number" => Self::Number,
            "bool" => Self::Bool,
            "uri" => Self::Uri,
            "binding" => Self::Binding,
            _ => return None,
        })
    }

    /// Whether `v` is a value of this type.
    pub fn accepts(self, v: &Value) -> bool {
        match (self, v) {
            (Self::Number, Value::Number(_)) | (Self::Bool, Value::Bool(_)) => true,
            (Self::Text | Self::Binding, Value::String(_)) => true,
            (Self::Uri, Value::String(s)) => s.contains("://") || s.starts_with('$'),
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
        for s in strings {
            for n in arg_names(s) {
                if !params.iter().any(|p| p.name == n) {
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

/// A component version's metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionMeta {
    pub name: String,
    pub version: i64,
    pub note: String,
    pub author: String,
    pub created_at: i64,
    /// `{"copied_from": {"component", "version"}}`.
    pub meta: Option<Value>,
}

fn meta_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VersionMeta> {
    let meta: Option<String> = r.get(5)?;
    Ok(VersionMeta {
        name: r.get(0)?,
        version: r.get(1)?,
        note: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        author: r.get(3)?,
        created_at: r.get(4)?,
        meta: meta.and_then(|m| serde_json::from_str(&m).ok()),
    })
}

/// The newest version of `name` (`core:x` / `pod:x`): `(version, definition)`.
pub fn latest(store: &Store, name: &str) -> rusqlite::Result<Option<(i64, Value)>> {
    store
        .with_conn(|c| {
            c.query_row(
            "SELECT version, json FROM components WHERE name = ?1 ORDER BY version DESC LIMIT 1",
            [name],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        })
        .map(|o| o.map(|(v, j)| (v, serde_json::from_str(&j).unwrap_or(Value::Null))))
}

/// Version `version` of `name`.
pub fn version(store: &Store, name: &str, version: i64) -> rusqlite::Result<Option<Value>> {
    store
        .with_conn(|c| {
            c.query_row(
                "SELECT json FROM components WHERE name = ?1 AND version = ?2",
                params![name, version],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .map(|o| o.map(|j| serde_json::from_str(&j).unwrap_or(Value::Null)))
}

/// The versions of `name`, newest first.
pub fn history(store: &Store, name: &str) -> rusqlite::Result<Vec<VersionMeta>> {
    store.with_conn(|c| {
        let mut st = c.prepare(
            "SELECT name, version, note, author, created_at, meta_json FROM components
             WHERE name = ?1 ORDER BY version DESC",
        )?;
        st.query_map([name], meta_row)?.collect()
    })
}

/// Every component's newest version: `(meta, definition)`, by name.
pub fn list(store: &Store) -> rusqlite::Result<Vec<(VersionMeta, Value)>> {
    store.with_conn(|c| {
        let mut st = c.prepare(
            "SELECT c.name, c.version, c.note, c.author, c.created_at, c.meta_json, c.json
             FROM components c
             JOIN (SELECT name, MAX(version) AS v FROM components GROUP BY name) m
               ON m.name = c.name AND m.v = c.version
             ORDER BY c.name",
        )?;
        st.query_map([], |r| {
            let json: String = r.get(6)?;
            Ok((
                meta_row(r)?,
                serde_json::from_str(&json).unwrap_or(Value::Null),
            ))
        })?
        .collect()
    })
}

/// Append a version of `pod:<name>` (checked by the caller): the new
/// version number. Prunes beyond [`MAX_POD_VERSIONS`].
pub fn put_pod(
    store: &Store,
    name: &str,
    def: &Value,
    note: &str,
    author: &str,
    meta: Option<&Value>,
) -> rusqlite::Result<i64> {
    debug_assert!(name.starts_with(POD));
    store.transaction(|tx| {
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM components WHERE name = ?1",
            [name],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO components (name, version, json, note, author, file_hash, meta_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)",
            params![
                name,
                next,
                def.to_string(),
                note,
                author,
                meta.map(Value::to_string),
                now_secs()
            ],
        )?;
        tx.execute(
            "DELETE FROM components WHERE name = ?1 AND version <= ?2",
            params![name, next - MAX_POD_VERSIONS as i64],
        )?;
        Ok(next)
    })
}

/// Delete every version of `pod:<name>`; false when there was none.
pub fn delete_pod(store: &Store, name: &str) -> rusqlite::Result<bool> {
    debug_assert!(name.starts_with(POD));
    store
        .with_conn(|c| c.execute("DELETE FROM components WHERE name = ?1", [name]))
        .map(|n| n > 0)
}

/// How many `pod:` components the pod has.
pub fn pod_count(store: &Store) -> rusqlite::Result<usize> {
    store.with_conn(|c| {
        c.query_row(
            "SELECT COUNT(DISTINCT name) FROM components WHERE name LIKE 'pod:%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as usize)
    })
}

/// A parsed server component file.
#[derive(Debug, Clone)]
pub struct CoreFile {
    /// `core:<name>`.
    pub name: String,
    pub number: i64,
    pub path: String,
    pub note: String,
    pub def: Value,
    pub file_hash: String,
}

/// Parse the embedded server component files (`Err`: the file and why).
/// Numbers run without gaps from each component's lowest file.
pub fn core_files(sources: &[ComponentSource]) -> Result<Vec<CoreFile>, String> {
    let mut out: Vec<CoreFile> = Vec::new();
    for s in sources {
        let path = format!("crates/pod-core/components/{}/{}.json", s.name, s.file);
        let fail = |e: String| format!("{path}: {e}");
        if !valid_bare_name(s.name) {
            return Err(fail("component folders are named [a-z][a-z0-9_]*".into()));
        }
        let number = match s.file.parse::<i64>() {
            Ok(n) if n >= 1 && s.file.len() == 4 => n,
            _ => {
                return Err(fail(
                    "file names are 4-digit version numbers (NNNN.json)".into(),
                ));
            }
        };
        let doc: Value =
            serde_json::from_str(s.text).map_err(|e| fail(format!("not valid JSON: {e}")))?;
        let Some(o) = doc.as_object() else {
            return Err(fail(
                r#"expected {"note": "...", "component": {...}}"#.into(),
            ));
        };
        if let Some(k) = o
            .keys()
            .find(|k| !matches!(k.as_str(), "note" | "component"))
        {
            return Err(fail(format!(
                "unknown key '{k}' (only \"note\" and \"component\")"
            )));
        }
        let note = o
            .get("note")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| fail("\"note\" must be a non-empty string".into()))?
            .to_owned();
        let def = o
            .get("component")
            .cloned()
            .ok_or_else(|| fail("\"component\" is missing".into()))?;
        Def::parse(&def).map_err(fail)?;
        let name = format!("{CORE}{}", s.name);
        if let Some(prev) = out.iter().rev().find(|f| f.name == name)
            && prev.number + 1 != number
        {
            return Err(fail(format!(
                "component files are numbered without gaps, expected {:04}.json",
                prev.number + 1
            )));
        }
        out.push(CoreFile {
            name,
            number,
            path,
            note,
            file_hash: crate::layouts::canonical_hash(&doc),
            def,
        });
    }
    Ok(out)
}

/// Record new server component files in the store (one transaction):
/// `Ok(appended (name, version))`, `Err` to stop the start with (a deployed
/// file changed, or a new one is numbered below a deployed one).
pub fn sync_core(store: &Store, files: &[CoreFile]) -> Result<Vec<(String, i64)>, String> {
    let r = store.transaction(|tx| {
        let mut appended = Vec::new();
        for f in files {
            let recorded: Option<Option<String>> = tx
                .query_row(
                    "SELECT file_hash FROM components WHERE name = ?1 AND version = ?2",
                    params![f.name, f.number],
                    |r| r.get(0),
                )
                .optional()?;
            match recorded {
                Some(h) if h.as_deref() == Some(f.file_hash.as_str()) => continue,
                Some(_) => {
                    return Ok(Err(format!(
                        "{} changed after it was deployed; component files are append-only: \
                         restore it and put the change in the next file",
                        f.path
                    )));
                }
                None => {}
            }
            let max: i64 = tx.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM components WHERE name = ?1",
                [&f.name],
                |r| r.get(0),
            )?;
            if max >= f.number {
                return Ok(Err(format!(
                    "{} is new but numbered below a deployed version ({max})",
                    f.path
                )));
            }
            tx.execute(
                "INSERT INTO components (name, version, json, note, author, file_hash, meta_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
                params![
                    f.name,
                    f.number,
                    f.def.to_string(),
                    f.note,
                    AUTHOR_SERVER,
                    f.file_hash,
                    now_secs()
                ],
            )?;
            appended.push((f.name.clone(), f.number));
        }
        Ok(Ok(appended))
    });
    match r {
        Ok(r) => r,
        Err(e) => Err(format!("recording the server components: {e}")),
    }
}

/// `{"name", "version", "note", "author", "doc", "params", "children",
/// "copied_from"?}` of a definition (the MCP listing).
pub fn summary(meta: &VersionMeta, def: &Value) -> Value {
    let mut v = json!({
        "name": meta.name,
        "version": meta.version,
        "note": meta.note,
        "author": meta.author,
        "doc": def["doc"],
        "params": def["params"],
        "children": def["children"].as_bool().unwrap_or(false),
    });
    if let Some(c) = meta
        .meta
        .as_ref()
        .map(|m| &m["copied_from"])
        .filter(|c| !c.is_null())
    {
        v["copied_from"] = c.clone();
    }
    v
}
