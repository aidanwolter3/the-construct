//! References between elements (decision `content-addressed-construct`).
//!
//! A reference sits under `ref` in a `core://Use` (transparent: the target is
//! expanded in place), a `core://Layout` (an embed: an edge) or an
//! `action.navigation.open` (a link: an edge). Sealed it is `{"el": "e1-…",
//! "name": "@core/card", "pin": false, "as"?: "<edge>"}`; an author may write
//! just the name, `"ref": "@core/card"`, and the pod fills in the rest.

use serde_json::{Map, Value, json};

use crate::def::USE_TYPE;

/// The node that embeds another element.
pub const LAYOUT_TYPE: &str = "core://Layout";
/// The intent that presents another element.
pub const OPEN_INTENT: &str = "action.navigation.open";

/// A reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    /// The name it follows (or followed, pinned).
    pub name: String,
    /// The element it was sealed with; `None` before the pod sealed it.
    pub el: Option<String>,
    /// A pinned reference keeps `el` when its name moves.
    pub pin: bool,
    /// The edge's key, when not the name.
    pub as_: Option<String>,
}

/// What a reference site is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// `core://Use`: no edge, expanded in place.
    Use,
    /// `core://Layout`: an edge drawn in place or over the screen.
    Embed,
    /// `action.navigation.open`: an edge the user follows.
    Link,
}

/// An edge an element has: its key and the element it leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRef {
    pub key: String,
    pub name: String,
    pub el: Option<String>,
    pub site: Site,
}

impl Ref {
    /// A reference as written: a name, or the sealed object.
    pub fn parse(v: &Value) -> Option<Ref> {
        match v {
            Value::String(name) if !name.is_empty() => Some(Ref {
                name: name.clone(),
                el: None,
                pin: false,
                as_: None,
            }),
            Value::Object(o) => {
                let name = o.get("name").and_then(Value::as_str).unwrap_or("");
                let el = o.get("el").and_then(Value::as_str).map(str::to_owned);
                if name.is_empty() && el.is_none() {
                    return None;
                }
                Some(Ref {
                    name: name.to_owned(),
                    el,
                    pin: o.get("pin").and_then(Value::as_bool).unwrap_or(false),
                    as_: o
                        .get("as")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                })
            }
            _ => None,
        }
    }

    /// The sealed form.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        if let Some(el) = &self.el {
            o.insert("el".into(), json!(el));
        }
        o.insert("name".into(), json!(self.name));
        o.insert("pin".into(), json!(self.pin));
        if let Some(a) = &self.as_ {
            o.insert("as".into(), json!(a));
        }
        Value::Object(o)
    }

    /// The edge's key: `as`, else the name.
    pub fn key(&self) -> &str {
        self.as_.as_deref().unwrap_or(&self.name)
    }
}

/// Which site `o` (an object holding `ref`) is.
pub fn site_of(o: &Map<String, Value>) -> Option<Site> {
    if !o.contains_key("ref") {
        return None;
    }
    match o.get("type").and_then(Value::as_str) {
        Some(USE_TYPE) => return Some(Site::Use),
        Some(LAYOUT_TYPE) => return Some(Site::Embed),
        _ => {}
    }
    (o.get("intent").and_then(Value::as_str) == Some(OPEN_INTENT)).then_some(Site::Link)
}

/// Call `f` with every reference in `v`, outermost first.
pub fn visit(v: &Value, f: &mut dyn FnMut(&Ref, Site)) {
    match v {
        Value::Object(o) => {
            if let Some(site) = site_of(o)
                && let Some(r) = Ref::parse(&o["ref"])
            {
                f(&r, site);
            }
            o.values().for_each(|c| visit(c, f));
        }
        Value::Array(a) => a.iter().for_each(|c| visit(c, f)),
        _ => {}
    }
}

/// Every reference in `v`.
pub fn all(v: &Value) -> Vec<(Ref, Site)> {
    let mut out = Vec::new();
    visit(v, &mut |r, s| out.push((r.clone(), s)));
    out
}

/// Let `f` change every reference in `v` (written back sealed).
pub fn map(v: &mut Value, f: &mut dyn FnMut(&mut Ref, Site)) {
    match v {
        Value::Object(o) => {
            if let Some(site) = site_of(o)
                && let Some(mut r) = Ref::parse(&o["ref"])
            {
                f(&mut r, site);
                o.insert("ref".into(), r.to_value());
            }
            o.values_mut().for_each(|c| map(c, f));
        }
        Value::Array(a) => a.iter_mut().for_each(|c| map(c, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_rewrites_references() {
        let mut v = json!({"type": "core://Column", "children": [
            {"type": "core://Use", "ref": "@core/card"},
            {"type": "core://Layout", "ref": {"name": "c-a", "el": "e1-x"}},
            {"type": "core://Button", "on_click": {"intent": "action.navigation.open",
                "ref": {"name": "@core/settings", "as": "settings"}}},
            {"type": "core://Text", "ref": "not a site"}
        ]});
        let found = all(&v);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].1, Site::Use);
        assert_eq!(found[1].0.el.as_deref(), Some("e1-x"));
        assert_eq!(found[2].0.key(), "settings");
        map(&mut v, &mut |r, _| r.el = Some("e1-y".into()));
        assert_eq!(
            v["children"][0]["ref"],
            json!({"el": "e1-y", "name": "@core/card", "pin": false})
        );
        assert_eq!(v["children"][3]["ref"], json!("not a site"));
    }
}
