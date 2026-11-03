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

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::adapter::Catalog;
use crate::store::Store;
use crate::validate::LayoutError;

/// The node type of a reference.
pub const REF_TYPE: &str = "core://Ref";
/// Refs inside parts are expanded this deep at most.
const MAX_DEPTH: usize = 4;

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
    if o.get("type")?.as_str()? != REF_TYPE {
        return None;
    }
    Some(PartRef {
        app: o.get("app")?.as_str()?.to_owned(),
        part: o.get("part")?.as_str()?.to_owned(),
        version: o.get("version").and_then(Value::as_i64),
    })
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

/// Whether `json` (a stored layout) may hold a reference (cheap check).
pub fn may_hold_refs(json: &str) -> bool {
    json.contains(REF_TYPE)
}

/// A built-in version's parts: version `version`, else the newest one a
/// client of `schema` can draw, else the latest. Blocking.
pub fn load(
    store: &Store,
    app: &str,
    version: Option<i64>,
    schema: Option<u32>,
) -> rusqlite::Result<Option<Parts>> {
    let v = match (version, schema) {
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

struct Expander<'a> {
    store: &'a Store,
    catalog: &'a Catalog,
    schema: Option<u32>,
    mode: Mode,
    cache: HashMap<(String, Option<i64>), Option<Parts>>,
}

impl Expander<'_> {
    fn part(&mut self, r: &PartRef) -> rusqlite::Result<Option<Value>> {
        let key = (r.app.clone(), r.version);
        if !self.cache.contains_key(&key) {
            let parts = if self.catalog.is_app(&r.app) {
                load(self.store, &r.app, r.version, self.schema)?
            } else {
                None
            };
            self.cache.insert(key.clone(), parts);
        }
        Ok(self.cache[&key]
            .as_ref()
            .and_then(|p| p.parts.get(&r.part).cloned()))
    }

    fn unresolved(&self, at: &str, r: &PartRef, why: &str) -> Result<Option<Value>, LayoutError> {
        match self.mode {
            Mode::Strict => Err(LayoutError {
                pointer: at.to_owned(),
                message: format!("core://Ref to '{}' part '{}': {why}", r.app, r.part),
                allowed: Vec::new(),
            }),
            Mode::Lenient => Ok(None),
        }
    }

    /// `v` with its refs expanded; `None`: drop it (an unresolved screen ref
    /// in lenient mode).
    fn walk(
        &mut self,
        v: &Value,
        at: &str,
        in_screens: bool,
        depth: usize,
    ) -> Result<Option<Value>, LayoutError> {
        if let Some(r) = as_ref(v) {
            if depth >= MAX_DEPTH {
                return self
                    .unresolved(at, &r, "references nest too deep")
                    .map(|_| None);
            }
            let screen_part = r.part.starts_with("screen:");
            if screen_part != in_screens {
                let why = if in_screens {
                    "only screen parts (screen:<id>) go in screens"
                } else {
                    "a screen part (screen:<id>) goes in screens"
                };
                return Ok(self
                    .unresolved(at, &r, why)?
                    .or_else(|| (!in_screens).then(placeholder)));
            }
            let found = self.part(&r).map_err(|e| LayoutError {
                pointer: at.to_owned(),
                message: format!("reading the built-in part: {e}"),
                allowed: Vec::new(),
            })?;
            let Some(part) = found else {
                let why = if self.catalog.is_app(&r.app) {
                    "no such part (list_parts lists them)"
                } else {
                    "only built-in apps' parts can be referenced"
                };
                return Ok(self
                    .unresolved(at, &r, why)?
                    .or_else(|| (!in_screens).then(placeholder)));
            };
            return self.walk(&part, at, in_screens, depth + 1);
        }
        Ok(Some(match v {
            Value::Object(m) => {
                let mut out = serde_json::Map::new();
                for (k, c) in m {
                    let here = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                    if let Some(x) = self.walk(c, &here, k == "screens" && c.is_array(), depth)? {
                        out.insert(k.clone(), x);
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => {
                let mut out = Vec::new();
                for (i, c) in a.iter().enumerate() {
                    if let Some(x) = self.walk(c, &format!("{at}/{i}"), in_screens, depth)? {
                        out.push(x);
                    }
                }
                Value::Array(out)
            }
            other => other.clone(),
        }))
    }
}

/// `doc` with every reference replaced by its part (from the newest version
/// a client of `schema` can draw; `None`: the latest). Blocking.
pub fn expand(
    store: &Store,
    catalog: &Catalog,
    doc: &Value,
    schema: Option<u32>,
    mode: Mode,
) -> Result<Value, LayoutError> {
    let mut e = Expander {
        store,
        catalog,
        schema,
        mode,
        cache: HashMap::new(),
    };
    Ok(e.walk(doc, "", false, 0)?.unwrap_or(Value::Null))
}

/// A stored layout as served to a client of `schema`: refs expanded
/// leniently (a JSON text in, a JSON text out). Blocking.
pub fn serve(store: &Store, catalog: &Catalog, json: &str, schema: Option<u32>) -> String {
    if !may_hold_refs(json) {
        return json.to_owned();
    }
    let Ok(doc) = serde_json::from_str::<Value>(json) else {
        return json.to_owned();
    };
    match expand(store, catalog, &doc, schema, Mode::Lenient) {
        Ok(v) => v.to_string(),
        Err(_) => json.to_owned(),
    }
}
