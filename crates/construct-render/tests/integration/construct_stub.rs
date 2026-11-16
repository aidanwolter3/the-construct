//! A pod's construct served from layout bundles, for the stub pods of these
//! tests (as client-core's tests serve it) (decision `one-layout-system`): each stub keeps answering a layout
//! as a bundle (`{workspace, entities, queries}`, or a status), and this
//! turns it into the construct's paths. Each layout is one element whose
//! node is the bundle's workspace; home's is the root (a blank page when the
//! stub serves no home), and the others are found by name.
//!
//! | Path | Answer |
//! |---|---|
//! | `GET /v1/construct` | `{root, generation}` |
//! | `POST /v1/objects` | the elements asked for |
//! | `GET /v1/construct/names/<id>` | `{name, el}` |
//! | `GET /v1/construct/data/<el>` | the bundle's `{entities, queries}` |

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

/// The events stream's path.
pub const EVENTS: &str = "/v1/construct/events";

/// A layout's answer: `(status, body)`; `None` is a 404.
pub type Answer = Option<(u16, String)>;

/// Layout `id`'s element: its workspace as the node.
pub fn element(id: &str, workspace: &Value) -> Value {
    json!({ "kind": "layout", "title": id, "node": workspace })
}

/// What home is when the stub serves none.
fn blank() -> Value {
    json!({ "type": "core://Page", "title": "Home",
        "root": { "type": "core://Column", "children": [] } })
}

#[derive(Default)]
pub struct Construct {
    /// Element id → (layout id, element).
    els: Mutex<HashMap<String, (String, Value)>>,
}

impl Construct {
    fn register(&self, id: &str, workspace: &Value) -> String {
        let el = element(id, workspace);
        let hash = construct_elements::object_id(&el);
        self.els
            .lock()
            .unwrap()
            .insert(hash.clone(), (id.to_owned(), el));
        hash
    }

    /// Home's element now, or the status the stub answers for home.
    pub fn root(&self, layout: &mut dyn FnMut(&str) -> Answer) -> Result<String, (u16, String)> {
        match layout("home") {
            Some((s, body)) if (200..300).contains(&s) => {
                let v: Value = serde_json::from_str(&body).unwrap_or_default();
                Ok(self.register("home", &v["workspace"]))
            }
            Some((404, _)) | None => Ok(self.register("home", &blank())),
            Some(other) => Err(other),
        }
    }

    /// The `construct` event of the root now (SSE text), if home answers.
    pub fn event(&self, layout: &mut dyn FnMut(&str) -> Answer) -> String {
        match self.root(layout) {
            Ok(root) => format!("event: construct\ndata: {{\"root\":\"{root}\"}}\n\n"),
            Err(_) => String::new(),
        }
    }

    /// The answer to a construct request (`path` with its query), or `None`
    /// when it is not one.
    pub fn answer(
        &self,
        method: &str,
        path: &str,
        body: &str,
        layout: &mut dyn FnMut(&str) -> Answer,
    ) -> Option<(u16, String)> {
        let path = path.split('?').next().unwrap_or(path);
        let ok = |v: Value| Some((200, v.to_string()));
        let not_found = || Some((404, json!({ "error": "not_found" }).to_string()));
        match (method, path) {
            ("GET", "/v1/construct") => match self.root(layout) {
                Ok(root) => ok(json!({ "root": root, "generation": 1 })),
                Err(e) => Some(e),
            },
            ("POST", "/v1/objects") => {
                let v: Value = serde_json::from_str(body).unwrap_or_default();
                let els = self.els.lock().unwrap();
                let objects: serde_json::Map<String, Value> = v["want"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(|id| Some((id.to_owned(), els.get(id)?.1.clone())))
                    .collect();
                ok(json!({ "objects": objects }))
            }
            ("GET", p) if p.starts_with("/v1/objects/") => {
                let id = &p["/v1/objects/".len()..];
                match self.els.lock().unwrap().get(id) {
                    Some((_, el)) => ok(el.clone()),
                    None => not_found(),
                }
            }
            ("GET", p) if p.starts_with("/v1/construct/names/") => {
                let name = &p["/v1/construct/names/".len()..];
                match layout(name) {
                    Some((s, body)) if (200..300).contains(&s) => {
                        let v: Value = serde_json::from_str(&body).unwrap_or_default();
                        let el = self.register(name, &v["workspace"]);
                        ok(json!({ "name": name, "el": el }))
                    }
                    Some((404, _)) | None => not_found(),
                    Some(other) => Some(other),
                }
            }
            ("GET", p) if p.starts_with("/v1/construct/data/") => {
                let el = &p["/v1/construct/data/".len()..];
                let Some(id) = self.els.lock().unwrap().get(el).map(|e| e.0.clone()) else {
                    return not_found();
                };
                match layout(&id) {
                    Some((s, body)) if (200..300).contains(&s) => {
                        let v: Value = serde_json::from_str(&body).unwrap_or_default();
                        let part = |k: &str| match &v[k] {
                            Value::Object(m) => Value::Object(m.clone()),
                            _ => json!({}),
                        };
                        ok(json!({ "entities": part("entities"), "queries": part("queries") }))
                    }
                    // A blank home has no data.
                    Some((404, _)) | None if id == "home" => {
                        ok(json!({ "entities": {}, "queries": {} }))
                    }
                    Some((404, _)) | None => not_found(),
                    Some(other) => Some(other),
                }
            }
            _ => None,
        }
    }
}

/// A status line's reason phrase (the stubs write `HTTP/1.1 <code> <reason>`).
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
