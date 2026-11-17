//! Where each layout is drawn (decision `trace-from-home`): from home, the
//! construct's root, every edge (a `core://Layout` or an
//! `action.navigation.open`, after components are expanded) is followed
//! breadth first, so each layout the user can reach gets the shortest path
//! that reaches it and how each step is taken ("on_swipe_down, Overlay").
//! `list_layouts` and `get_layout` answer it, so an agent changing what the
//! user sees edits the element that is actually drawn: a pod layout copied
//! from a blueprint doesn't follow the blueprint.

use std::collections::{BTreeMap, VecDeque};

use rusqlite::Connection;
use serde_json::{Value, json};

use super::store as cstore;
use super::write::ConnObjects;

/// One step of a path: from a layout, how, to a layout (or blueprint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub from: String,
    pub to: String,
    /// What takes it: the field holding the edge (`on_click`,
    /// `on_swipe_down`, `slots`…), its presentation, and the nearest label.
    pub how: String,
}

/// Each name reached from home and the shortest path to it (home: none).
pub type Reach = BTreeMap<String, Vec<Step>>;

/// The names reached from home on `conn`, with their paths.
pub fn from_home(conn: &Connection) -> rusqlite::Result<Reach> {
    let mut out = Reach::new();
    let Some(root) = cstore::name(conn, crate::HOME_APP)? else {
        return Ok(out);
    };
    let objects = ConnObjects::new(conn);
    out.insert(crate::HOME_APP.to_owned(), Vec::new());
    let mut queue = VecDeque::from([(crate::HOME_APP.to_owned(), root)]);
    while let Some((name, el)) = queue.pop_front() {
        let Ok(m) = construct_elements::materialize(&objects, &el) else {
            continue;
        };
        for e in &m.edges {
            if out.contains_key(&e.name) {
                continue;
            }
            let target = match &e.el {
                Some(el) => Some(el.clone()),
                None => cstore::name(conn, &e.name)?,
            };
            let mut path = out[&name].clone();
            path.push(Step {
                from: name.clone(),
                to: e.name.clone(),
                how: how(&m.node, &e.key),
            });
            out.insert(e.name.clone(), path);
            if let Some(t) = target {
                queue.push_back((e.name.clone(), t));
            }
        }
    }
    Ok(out)
}

/// How the edge keyed `key` is taken in a materialized node (edges are
/// written `layout: <key>` there).
fn how(node: &Value, key: &str) -> String {
    fn find(v: &Value, key: &str, field: &str, label: &str) -> Option<String> {
        match v {
            Value::Object(o) => {
                let label = ["content_description", "label"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(label);
                if o.get("layout").and_then(Value::as_str) == Some(key) {
                    let shown = o.get("presentation").and_then(Value::as_str).map(|p| {
                        match o.get("edge").and_then(Value::as_str) {
                            Some(e) => format!("{p} from {e}"),
                            None => p.to_owned(),
                        }
                    });
                    let kind = if o.get("type").and_then(Value::as_str) == Some("core://Layout") {
                        "embedded".to_owned()
                    } else {
                        field.to_owned()
                    };
                    let mut parts = vec![kind];
                    parts.extend(shown);
                    if !label.is_empty() {
                        parts.push(format!("\"{label}\""));
                    }
                    return Some(parts.join(", "));
                }
                o.iter().find_map(|(k, c)| find(c, key, k, label))
            }
            Value::Array(a) => a.iter().find_map(|c| find(c, key, field, label)),
            _ => None,
        }
    }
    find(node, key, "", "").unwrap_or_default()
}

/// A path as people read it: `home → on_swipe_down, Overlay from top →
/// c-ejdjtgh6dx`.
pub fn describe(path: &[Step]) -> String {
    let mut out = crate::HOME_APP.to_owned();
    for s in path {
        if s.how.is_empty() {
            out.push_str(&format!(" → {}", s.to));
        } else {
            out.push_str(&format!(" → ({}) → {}", s.how, s.to));
        }
    }
    out
}

/// What `list_layouts` and `get_layout` say of layout `name`: how home
/// reaches it, or that it doesn't.
pub fn reached(reach: &Reach, name: &str) -> Value {
    match reach.get(name) {
        Some(p) => json!(describe(p)),
        None => json!(
            "not reached from home: the user can't open it (unless a notification or another pod's link names it)"
        ),
    }
}

/// The blueprint a pod layout titled `title` may be a copy of: one with
/// that title it doesn't reference. A copy is the layout as it was when
/// copied; it doesn't follow the blueprint.
pub fn copy_of<'a>(
    title: &str,
    uses: &[String],
    blueprints: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let t = title.trim().to_lowercase();
    blueprints
        .filter(|(_, bt)| bt.trim().to_lowercase() == t)
        .map(|(id, _)| super::blueprint_name(id))
        .find(|bp| {
            !uses
                .iter()
                .any(|u| u == bp || u.starts_with(&format!("{bp}/")))
        })
}

/// The names an element's own node references (uses, embeds and links).
pub fn names_used(el: &Value) -> Vec<String> {
    let mut out = Vec::new();
    construct_elements::refs::visit(&el["node"], &mut |r, _| {
        if !r.name.is_empty() && !out.contains(&r.name) {
            out.push(r.name.clone());
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn how_names_the_field_presentation_and_label() {
        let node = json!({"type": "core://Gesture",
            "on_swipe_down": {"intent": "action.navigation.open", "presentation": "Overlay",
                "edge": "top", "layout": "inbox-copy"},
            "content": {"type": "core://Column", "children": [
                {"type": "core://Tappable", "content_description": "Calendar",
                 "on_click": {"intent": "action.navigation.open", "layout": "cal"}},
                {"type": "core://Layout", "layout": "today"}]}});
        assert_eq!(how(&node, "inbox-copy"), "on_swipe_down, Overlay from top");
        assert_eq!(how(&node, "cal"), "on_click, \"Calendar\"");
        assert_eq!(how(&node, "today"), "embedded");
        let path = vec![Step {
            from: "home".into(),
            to: "inbox-copy".into(),
            how: how(&node, "inbox-copy"),
        }];
        assert_eq!(
            describe(&path),
            "home → (on_swipe_down, Overlay from top) → inbox-copy"
        );
    }

    #[test]
    fn a_layout_titled_as_a_blueprint_it_doesnt_use_is_a_copy() {
        let bps = [("inbox", "Notifications"), ("events", "Events")];
        assert_eq!(
            copy_of("Notifications", &[], bps.iter().copied()),
            Some("@core/inbox".into())
        );
        assert_eq!(
            copy_of(
                "Notifications",
                &["@core/inbox".into()],
                bps.iter().copied()
            ),
            None
        );
        assert_eq!(copy_of("Groceries", &[], bps.iter().copied()), None);
    }
}
