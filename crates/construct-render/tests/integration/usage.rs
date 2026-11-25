//! Usage trails (decision `usage-trails`): a session tells its loader what
//! the owner did, with names from the layout only; a loader that doesn't
//! record (a preview's, a walk's) gets nothing it has to handle.

use construct_render::Viewport;
use construct_render::interact::{Done, Loader, Session, Target, Write, Written};
use protocol::usage::UsageEvent;
use protocol::{DataBundle, UiNode};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Recorder {
    events: Arc<Mutex<Vec<UsageEvent>>>,
}

impl Loader for Recorder {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let root = serde_json::from_value(
            json!({ "type": "core://Text", "style": "Body", "value_expr": "Recipe" }),
        )
        .unwrap();
        if id == "c-recipe" {
            Ok((root, DataBundle::default()))
        } else {
            Err(format!("no layout {id}"))
        }
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn send(&self, _write: Write, done: Done) -> bool {
        done(Written {
            result: Ok(json!({})),
            refreshed: Vec::new(),
        });
        true
    }
    fn usage(&self, event: &UsageEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

#[test]
fn a_tap_and_the_screen_it_opens_are_reported_by_name() {
    let root: UiNode = serde_json::from_value(json!({ "type": "core://Column", "children": [
        { "type": "core://List", "query_key": "meals", "item_template":
          { "type": "core://Button", "label": "$item.name",
            "on_click": { "intent": "action.navigation.open", "layout": "c-recipe", "presentation": "Push" } } }
    ]}))
    .unwrap();
    let data: DataBundle = serde_json::from_value(json!({
        "entities": { "collections://meals/m1": { "name": "Secret Family Stew" } },
        "queries": { "meals": ["collections://meals/m1"] } }))
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut s = Session::new("home", root, data, Viewport::PHONE);
    s.animate = false;
    s.loader = Some(Box::new(Recorder {
        events: events.clone(),
    }));
    s.tap(&Target::Text("Secret Family Stew".into())).unwrap();
    let got = events.lock().unwrap().clone();
    let tap = got.iter().find(|e| e.kind == "tap").expect("a tap");
    assert_eq!(tap.layout, "home");
    assert_eq!(tap.target.as_deref(), Some("c-recipe"));
    assert_eq!(tap.effect.as_deref(), Some("navigated"));
    let screen = got.iter().find(|e| e.kind == "screen").expect("a screen");
    assert_eq!(screen.layout, "c-recipe");
    assert_eq!(screen.from.as_deref(), Some("home"));
    // Names only: what the row showed is nowhere in what was reported.
    let all = serde_json::to_string(&got).unwrap();
    assert!(!all.contains("Stew"), "{all}");
}
