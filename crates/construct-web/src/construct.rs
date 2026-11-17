//! The construct as the web client holds it (decision `web-client`): the
//! objects it has fetched, home's root, and each layout materialized from
//! them, with no I/O, so it runs (and is tested) anywhere. The browser half
//! (`crate::web`) fetches what this asks for.

use construct_elements::{Objects, keyed, materialize_with, object_id};
use protocol::UiNode;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The key home has in the construct.
pub const HOME: &str = "home";

/// Features this client can't draw (decision `web-client`): no photo
/// picker and no notification permission in a browser yet, so the pod
/// serves the newest versions without them. Upstream sign-ins
/// (`auth_login`) go through the page itself (decision
/// `web-upstream-sign-in`).
pub const NOT_ON_THE_WEB: [&str; 2] = ["media_picker", "device_notification_permission"];

/// The features this client sends in `x-construct-features`: every one the
/// renderer draws but [`NOT_ON_THE_WEB`].
pub fn features_header() -> String {
    protocol::features_header()
        .split(',')
        .filter(|f| !NOT_ON_THE_WEB.contains(f))
        .collect::<Vec<_>>()
        .join(",")
}

/// The objects fetched so far and home's root.
#[derive(Default)]
pub struct Construct {
    objects: HashMap<String, Arc<Value>>,
    root: Option<String>,
}

/// A layout ready to draw: its plumbing and where its data comes from.
#[derive(Debug, Clone)]
pub struct Layout {
    pub node: UiNode,
    pub title: Option<String>,
    /// `/v1/construct/data/<element>`, with its use site's bindings.
    pub data_path: String,
}

struct Have<'a>(&'a HashMap<String, Arc<Value>>);

impl Objects for Have<'_> {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.0.get(id).cloned()
    }
}

impl Construct {
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    pub fn set_root(&mut self, root: &str) {
        self.root = Some(root.to_owned());
    }

    /// The ids of the objects held (`POST /v1/objects` `have`).
    pub fn have(&self) -> Vec<String> {
        self.objects.keys().cloned().collect()
    }

    /// Takes the objects of a `POST /v1/objects` answer. An object is its
    /// hash: one that doesn't match its id is not taken.
    pub fn take(&mut self, answer: &Value) -> usize {
        let mut n = 0;
        for (id, o) in answer["objects"].as_object().into_iter().flatten() {
            if object_id(o) == *id {
                self.objects.insert(id.clone(), Arc::new(o.clone()));
                n += 1;
            }
        }
        n
    }

    /// What `els` still need that isn't here (empty when they can all be
    /// materialized).
    pub fn missing(&self, els: &[String]) -> Vec<String> {
        let have = Have(&self.objects);
        let mut out = Vec::new();
        for el in els {
            match materialize_with(&have, el, &Map::new()) {
                Ok(m) => out.extend(m.missing),
                Err(_) if !self.objects.contains_key(el) => out.push(el.clone()),
                Err(_) => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Every layout home reaches, by key, with the inputs its use site binds.
    pub fn keys(&self) -> BTreeMap<String, (String, Map<String, Value>)> {
        let Some(root) = &self.root else {
            return BTreeMap::new();
        };
        keyed(&Have(&self.objects), HOME, root)
            .into_iter()
            .map(|(k, v)| (k, (v.el, v.inputs)))
            .collect()
    }

    /// Layout `key` (home, or a layout home reaches), materialized; `None`
    /// when home doesn't reach it or its objects aren't all here.
    pub fn layout(&self, key: &str) -> Option<Layout> {
        let (el, inputs) = self.keys().remove(key)?;
        self.element(&el, &inputs)
    }

    /// An element (by id) bound with `inputs`, materialized.
    pub fn element(&self, el: &str, inputs: &Map<String, Value>) -> Option<Layout> {
        let have = Have(&self.objects);
        let m = materialize_with(&have, el, inputs).ok()?;
        if !m.missing.is_empty() {
            return None;
        }
        let node = serde_json::from_value(m.node).ok()?;
        let title = have
            .object(el)
            .and_then(|v| v["title"].as_str().map(str::to_owned));
        Some(Layout {
            node,
            title,
            data_path: data_path(el, inputs),
        })
    }

    /// Each layout's title by key (the device data names layouts with them).
    pub fn titles(&self) -> HashMap<String, String> {
        let have = Have(&self.objects);
        self.keys()
            .into_iter()
            .filter_map(|(k, (el, _))| {
                let t = have.object(&el)?["title"].as_str()?.to_owned();
                Some((k, t))
            })
            .collect()
    }
}

/// `/v1/construct/data/<el>`, with `?bind=<JSON>` when the use site binds
/// inputs.
pub fn data_path(el: &str, inputs: &Map<String, Value>) -> String {
    let mut path = format!("/v1/construct/data/{el}");
    if !inputs.is_empty() {
        path.push_str("?bind=");
        path.push_str(&encode(&Value::Object(inputs.clone()).to_string()));
    }
    path
}

/// Percent-encodes a query value (everything but unreserved characters).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `data` of server-sent events in a buffer of text: whole events are
/// taken out as `(event, data)` and what follows the last one is left in
/// `buf` for the next chunk.
pub fn take_events(buf: &mut String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Some(end) = buf.find("\n\n") {
        let block: String = buf.drain(..end + 2).collect();
        let (mut event, mut data) = (String::from("message"), Vec::new());
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().to_owned();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v).to_owned());
            }
        }
        if !data.is_empty() {
            out.push((event, data.join("\n")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> (String, Value) {
        (object_id(&v), v)
    }

    #[test]
    fn the_web_leaves_out_what_a_browser_cant_draw() {
        let h = features_header();
        assert!(h.split(',').any(|f| f == "list"));
        for f in NOT_ON_THE_WEB {
            assert!(!h.split(',').any(|x| x == f), "{f}");
        }
    }

    #[test]
    fn home_materializes_once_its_objects_are_here() {
        let (id, home) = obj(json!({ "kind": "element", "title": "Home",
            "node": { "type": "core://Page", "id": "home", "title": "Home",
                "root": { "type": "core://Text", "style": "Body", "value_expr": "Hello" } } }));
        let mut c = Construct::default();
        c.set_root(&id);
        assert_eq!(c.missing(std::slice::from_ref(&id)), vec![id.clone()]);
        assert!(c.layout(HOME).is_none());
        // A forged object (its id isn't its hash) is not taken.
        assert_eq!(
            c.take(&json!({ "objects": { "e1-forged": home.clone() } })),
            0
        );
        assert_eq!(c.take(&json!({ "objects": { id.clone(): home } })), 1);
        assert!(c.missing(std::slice::from_ref(&id)).is_empty());
        let l = c.layout(HOME).expect("home");
        assert_eq!(l.title.as_deref(), Some("Home"));
        assert_eq!(l.data_path, format!("/v1/construct/data/{id}"));
        assert_eq!(c.titles().get(HOME).map(String::as_str), Some("Home"));
    }

    #[test]
    fn bindings_go_in_the_data_path_encoded() {
        let mut inputs = Map::new();
        inputs.insert("items".into(), json!("collection:books"));
        assert_eq!(
            data_path("e1-x", &inputs),
            "/v1/construct/data/e1-x?bind=%7B%22items%22%3A%22collection%3Abooks%22%7D"
        );
    }

    #[test]
    fn events_are_taken_whole_and_the_rest_waits() {
        let mut buf = String::from(
            "event: construct\ndata: {\"root\":\"e1-a\"}\n\n: keep-alive\n\nevent: data\ndata: {\"keys\"",
        );
        let got = take_events(&mut buf);
        assert_eq!(
            got,
            vec![("construct".into(), "{\"root\":\"e1-a\"}".into())]
        );
        buf.push_str(":[]}\n\n");
        assert_eq!(
            take_events(&mut buf),
            vec![("data".into(), "{\"keys\":[]}".into())]
        );
        assert!(buf.is_empty());
    }
}
