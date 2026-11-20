//! Values a device keeps (feature `local_keep`, decision
//! `element-roles-and-reuse`): `action.local.set` with `keep` on a
//! `local://` entity outlives the session, and the next one opens with it.

use construct_render::Viewport;
use construct_render::interact::{Loader, Session};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Kept = Arc<Mutex<Vec<(String, String, Value)>>>;

/// A loader with somewhere to keep things, as the engine's store is.
struct Device {
    kept: Kept,
}

impl Loader for Device {
    fn layout(&self, _id: &str) -> Result<(UiNode, DataBundle), String> {
        Err("no layouts".into())
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn keep_local(&self, uri: &str, field: &str, value: &Value) {
        let mut kept = self.kept.lock().unwrap();
        kept.retain(|(u, f, _)| !(u == uri && f == field));
        kept.push((uri.to_owned(), field.to_owned(), value.clone()));
    }
    fn kept_locals(&self) -> Vec<(String, String, Value)> {
        self.kept.lock().unwrap().clone()
    }
}

/// Two tabs: the body of the one `local://shell#tab` names, and a button
/// for each. Nothing chosen yet shows the first.
fn tabs() -> UiNode {
    let body = |key: &str, text: &str| {
        json!({"type": "core://Conditional", "value": "${local://shell#tab}", "equals": key,
            "content": {"type": "core://Text", "style": "Body", "value_expr": text}})
    };
    let button = |key: &str, label: &str, keep: bool| {
        json!({"type": "core://Button", "label": label, "on_click": {
            "intent": "action.local.set", "entity_uri": "local://shell", "field": "tab",
            "value_expr": format!("\"{key}\""), "keep": keep}})
    };
    serde_json::from_value(json!({"type": "core://Column", "children": [
        {"type": "core://Conditional", "value": "${local://shell#tab}", "not": true,
            "content": {"type": "core://Text", "style": "Body", "value_expr": "Home body"}},
        body("home", "Home body"),
        body("meals", "Meals body"),
        body("more", "More body"),
        button("home", "Home", true),
        button("meals", "Meals", true),
        button("more", "More", false)]}))
    .unwrap()
}

fn session(kept: &Kept) -> Session {
    let mut s = Session::new("home", tabs(), DataBundle::default(), Viewport::PHONE);
    s.loader = Some(Box::new(Device { kept: kept.clone() }));
    s
}

fn texts(s: &mut Session) -> Vec<String> {
    s.scene().unwrap().texts().map(|t| t.text.clone()).collect()
}

#[test]
fn a_kept_local_value_is_there_when_the_app_starts_again() {
    let kept = Kept::default();
    let mut first = session(&kept);
    assert!(texts(&mut first).contains(&"Home body".to_owned()));
    first
        .dispatch(
            &json!({"intent": "action.local.set", "entity_uri": "local://shell",
            "field": "tab", "value_expr": "\"meals\"", "keep": true}),
        )
        .unwrap();
    assert!(texts(&mut first).contains(&"Meals body".to_owned()));
    assert_eq!(
        *kept.lock().unwrap(),
        [("local://shell".to_owned(), "tab".to_owned(), json!("meals"))]
    );
    drop(first);

    // The next start draws its first frame on the tab that was left open.
    let mut second = session(&kept);
    let drawn = texts(&mut second);
    assert!(drawn.contains(&"Meals body".to_owned()), "{drawn:?}");
    assert!(!drawn.contains(&"Home body".to_owned()), "{drawn:?}");

    // Without `keep` a value lasts while the app runs, as before.
    second
        .dispatch(
            &json!({"intent": "action.local.set", "entity_uri": "local://shell",
            "field": "tab", "value_expr": "\"more\"", "keep": false}),
        )
        .unwrap();
    assert!(texts(&mut second).contains(&"More body".to_owned()));
    drop(second);
    let mut third = session(&kept);
    assert!(texts(&mut third).contains(&"Meals body".to_owned()));
}

#[test]
fn only_the_devices_own_entities_are_kept() {
    let kept = Kept::default();
    let mut s = Session::new(
        "home",
        tabs(),
        serde_json::from_value(json!({"entities": {"todos://1": {"done": false}}})).unwrap(),
        Viewport::PHONE,
    );
    s.loader = Some(Box::new(Device { kept: kept.clone() }));
    s.dispatch(
        &json!({"intent": "action.local.set", "entity_uri": "todos://1",
        "field": "done", "value_expr": "true", "keep": true}),
    )
    .unwrap();
    assert!(
        kept.lock().unwrap().is_empty(),
        "a pod's entity is the pod's to keep"
    );
}
