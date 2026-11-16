//! Server components (decision `plumbing-and-porcelain`): porcelain defined
//! as data. A component is a node tree with parameters, `{doc, params,
//! children?, template, examples}`; clients expand it (decision
//! `content-addressed-construct`, `construct_elements::materialize`). A
//! layout uses one with `{"type": "core://Use", "ref": "@core/section_header",
//! "args": {...}, "children": [...]}` (`"component": "core:section_header"` is
//! accepted when written).
//!
//! Server components are files `crates/pod-core/components/<name>/NNNN.json`
//! (`{"note", "component"}`), or a source's own
//! `crates/source-*/components/<name>/NNNN.json` (`Source::component_files`),
//! embedded in the binary; each pod names the newest file of each
//! `@core/<name>` when it opens ([`crate::construct::core`]). A deployed file
//! never changes and is never removed. The pod's own components are
//! `@me/<name>`, written by an agent.
//!
//! In a template, `$arg.<name>` is the argument (a whole string `"$arg.n"`
//! takes the argument's own JSON type), `{"type": "core://Children"}` is
//! where the caller's children go (`"at": i` for one of them), and
//! `{"type": "core://IfArg", "arg", "equals"?, "then", "else"?}` keeps a
//! branch by an argument, and `{"type": "core://Fragment", "children"}`
//! stands for several siblings. Runtime bindings (`$item`, `$form`, `${uri#f}`,
//! `$route`) pass through untouched.

use serde_json::Value;

pub use construct_elements::def::{
    CHILDREN_TYPE, Def, Example, FRAGMENT_TYPE, IF_ARG_TYPE, MAX_DEF_BYTES, MAX_EXAMPLES,
    MAX_NAME_CHARS, MAX_PARAMS, Param, ParamType, USE_TYPE, arg_names, valid_bare_name,
};

/// One embedded server component file.
#[derive(Debug, Clone, Copy)]
pub struct ComponentSource {
    /// The crate's folder, `crates/<crate>/components`, for messages.
    pub dir: &'static str,
    pub name: &'static str,
    /// `NNNN`.
    pub file: &'static str,
    pub text: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/component_files.rs"));

/// The other spelling of a server component's name in a write (`core:x`
/// for `@core/x`).
pub const CORE: &str = "core:";
/// The other spelling of the pod's own components' names (`pod:x` for
/// `@me/x`).
pub const POD: &str = "pod:";

/// A parsed server component file.
#[derive(Debug, Clone)]
pub struct CoreFile {
    /// `core:<name>`.
    pub name: String,
    pub number: i64,
    pub path: String,
    pub note: String,
    pub def: Value,
}

/// Parse the embedded server component files (`Err`: the file and why).
/// Numbers run without gaps from each component's lowest file.
pub fn core_files(sources: &[ComponentSource]) -> Result<Vec<CoreFile>, String> {
    let mut out: Vec<CoreFile> = Vec::new();
    for s in sources {
        let path = format!("{}/{}/{}.json", s.dir, s.name, s.file);
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
            def,
        });
    }
    Ok(out)
}
