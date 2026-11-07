//! Taps that write to the pod, the device commands of the inbox, links,
//! dragging sheets and paging carousels.

use construct_render::Viewport;
use construct_render::interact::{Done, Effect, Loader, Session, Target, Write, Written};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A pod that records writes and answers them at once.
struct Pod {
    writes: Arc<Mutex<Vec<Write>>>,
    refuse: bool,
    layouts: Vec<(String, UiNode)>,
}

impl Loader for Pod {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        self.layouts
            .iter()
            .find(|(l, _)| l == id)
            .map(|(_, r)| (r.clone(), DataBundle::default()))
            .ok_or_else(|| format!("no layout {id}"))
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn send(&self, write: Write, done: Done) -> bool {
        self.writes.lock().unwrap().push(write);
        done(Written {
            result: if self.refuse {
                Err("HTTP 400".into())
            } else {
                Ok(json!({}))
            },
            refreshed: Vec::new(),
        });
        true
    }
}

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

fn data(v: Value) -> DataBundle {
    serde_json::from_value(v).unwrap()
}

fn session(root: Value, d: Value, refuse: bool) -> (Session, Arc<Mutex<Vec<Write>>>) {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut s = Session::new("home", node(root), data(d), Viewport::PHONE);
    s.loader = Some(Box::new(Pod {
        writes: writes.clone(),
        refuse,
        layouts: Vec::new(),
    }));
    (s, writes)
}

fn groceries() -> Value {
    json!({ "type": "core://Column", "children": [
        { "type": "core://Row", "children": [
            { "type": "core://TextField", "field": "name", "placeholder": "Add an item",
              "on_submit": { "intent": "action.mutation.create", "query_key": "g",
                "entity_uri": "x://g/pending-$nonce", "endpoint": "/v1/g/create",
                "entity": { "id": "pending-$nonce", "name": "$form.name", "done": false },
                "payload": { "client_id": "$nonce", "name": "$form.name", "quantity": "$form?.qty" } } },
            { "type": "core://IconButton", "icon": "add", "content_description": "Add item", "tag": "add",
              "on_click": { "intent": "action.mutation.create", "query_key": "g",
                "entity_uri": "x://g/pending-$nonce", "endpoint": "/v1/g/create",
                "entity": { "id": "pending-$nonce", "name": "$form.name", "done": false },
                "payload": { "client_id": "$nonce", "name": "$form.name", "quantity": "$form?.qty" } } }
        ] },
        { "type": "core://List", "query_key": "g", "item_template": { "type": "core://Row", "children": [
            { "type": "core://Tappable", "tag": "check_$item.id", "content_description": "Toggle",
              "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.name" },
              "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": "done", "value_expr": "!entity.done", "rollback_value": false,
                "endpoint": "/v1/g/update", "payload": { "done": "$value", "id": "$item.id" } } }
        ] } }
    ] })
}

fn grocery_data() -> Value {
    json!({ "entities": { "x://g/a": { "id": "a", "name": "Eggs", "done": false } },
            "queries": { "g": ["x://g/a"] } })
}

#[test]
fn a_checkbox_changes_at_once_and_is_sent() {
    let (mut s, writes) = session(groceries(), grocery_data(), false);
    let out = s.tap(&Target::Id("check_a".into())).unwrap();
    assert!(
        out.effects.contains(&Effect::Sent {
            path: "/v1/g/update".into()
        }),
        "{:?}",
        out.effects
    );
    assert_eq!(s.data.entities["x://g/a"].get_bool("done"), Some(true));
    let w = writes.lock().unwrap()[0].clone();
    // `$item.uri` is the item's own URI; `$value` the new value.
    assert_eq!(w.body, json!({ "done": true, "id": "a" }));
    s.scene().unwrap();
    assert!(!s.busy());
    assert_eq!(s.data.entities["x://g/a"].get_bool("done"), Some(true));
}

#[test]
fn a_refused_change_is_undone() {
    let (mut s, _) = session(groceries(), grocery_data(), true);
    s.tap(&Target::Id("check_a".into())).unwrap();
    assert_eq!(s.data.entities["x://g/a"].get_bool("done"), Some(true));
    // The answer arrives with the next frame.
    s.scene().unwrap();
    assert_eq!(s.data.entities["x://g/a"].get_bool("done"), Some(false));
    assert!(s.last_error.as_deref().unwrap().contains("400"));
}

#[test]
fn add_binds_the_form_and_a_nonce_and_lists_the_item() {
    let (mut s, writes) = session(groceries(), grocery_data(), false);
    // Blank: nothing is sent.
    let out = s.tap(&Target::Id("add".into())).unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
    s.type_text("name", "Milk");
    s.tap(&Target::Id("add".into())).unwrap();
    let w = writes.lock().unwrap()[0].clone();
    assert_eq!(w.path, "/v1/g/create");
    assert_eq!(w.body["name"], json!("Milk"));
    // `$form?.` binds a blank field to "".
    assert_eq!(w.body["quantity"], json!(""));
    let nonce = w.body["client_id"].as_str().unwrap().to_owned();
    assert_eq!(nonce.len(), 24);
    let uri = format!("x://g/pending-{nonce}");
    assert_eq!(s.data.queries["g"], vec!["x://g/a".to_owned(), uri.clone()]);
    assert_eq!(s.data.entities[&uri].get_str("name"), Some("Milk"));
    // The layouts on the stack are loaded again once the pod has it.
    assert_eq!(w.refresh, vec!["home".to_owned()]);
    assert!(s.scene().unwrap().find_text("Milk").is_some());
}

#[test]
fn a_fields_send_button_sends_and_empties_it() {
    let (mut s, writes) = session(groceries(), grocery_data(), false);
    s.type_text("name", "Bread");
    s.tap(&Target::Id("send:name".into())).unwrap();
    assert_eq!(writes.lock().unwrap()[0].body["name"], json!("Bread"));
    assert!(!s.form.contains_key("name"));
}

#[test]
fn opening_a_notification_marks_it_read_and_opens_its_layout() {
    let inbox = json!({ "type": "core://List", "query_key": "local:notifications", "item_template": {
        "type": "core://Tappable", "tag": "n_$item.id",
        "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.title" },
        "on_click": { "intent": "action.device", "command": "open_notification", "id": "$item.id" } } });
    let page = json!({ "type": "core://Page", "id": "groceries", "title": "Groceries", "root": { "type": "core://Text", "style": "Body", "value_expr": "Groceries list" },
        "screens": [{ "id": "item", "route": "x://g/{id}", "root": { "type": "core://Text", "style": "Body", "value_expr": "Item $route.id" } }] });
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut s = Session::new(
        "home",
        node(json!({ "type": "core://Text", "style": "Body", "value_expr": "Home" })),
        data(json!({ "entities": {
            "local://notifications/n1": { "id": "n1", "title": "Milk is low", "unread": true,
                "app_id": "groceries", "target_uri": "x://g/a" },
            "local://inbox": { "unread": 1, "label": "1 unread" } },
            "queries": { "local:notifications": ["local://notifications/n1"] } })),
        Viewport::PHONE,
    );
    s.layouts.insert("inbox".into(), node(inbox));
    s.loader = Some(Box::new(Pod {
        writes: writes.clone(),
        refuse: false,
        layouts: vec![("groceries".into(), node(page))],
    }));
    s.dispatch(
        &json!({ "intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet" }),
    )
    .unwrap();
    s.tap(&Target::Id("n_n1".into())).unwrap();
    assert_eq!(
        s.data.entities["local://notifications/n1"].get_bool("unread"),
        Some(false)
    );
    assert_eq!(
        s.data.entities["local://inbox"].get_str("label"),
        Some("All read")
    );
    let w = writes.lock().unwrap()[0].clone();
    assert_eq!(
        (w.path.as_str(), w.body),
        ("/v1/notifications/read", json!({ "ids": ["n1"] }))
    );
    // The inbox closed; the notification's layout and screen opened.
    assert_eq!(
        s.state()["stack"],
        json!(["home", "groceries", "groceries"])
    );
    assert!(s.scene().unwrap().find_text("Item a").is_some());
    // Mark all read.
    let out = s
        .dispatch(&json!({ "intent": "action.device", "command": "mark_all_read" }))
        .unwrap();
    assert!(out.effects.contains(&Effect::Device {
        command: "mark_all_read".into()
    }));
    assert_eq!(writes.lock().unwrap()[1].body, json!({ "all": true }));
}

#[test]
fn a_link_is_opened_by_the_platform() {
    let (mut s, _) = session(
        json!({ "type": "core://Text", "style": "Body", "value_expr": "x" }),
        json!({}),
        false,
    );
    let out = s
        .dispatch(&json!({ "intent": "action.link.open", "url": "https://tickets.example/a" }))
        .unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::OpenLink {
            url: "https://tickets.example/a".into()
        }]
    );
    let out = s
        .dispatch(&json!({ "intent": "action.link.open", "url": "javascript:alert(1)" }))
        .unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
}

#[test]
fn dragging_a_sheets_handle_down_closes_it() {
    let (mut s, _) = session(
        json!({ "type": "core://Text", "style": "Body", "value_expr": "Home" }),
        json!({}),
        false,
    );
    s.layouts.insert(
        "inbox".into(),
        node(json!({ "type": "core://Text", "style": "Body", "value_expr": "Inbox" })),
    );
    s.dispatch(
        &json!({ "intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet" }),
    )
    .unwrap();
    let h = Viewport::PHONE.height;
    let top = h - h * 0.85;
    // Only the handle strip grabs the sheet.
    assert!(s.frame_drag_at(100.0, top + 8.0).is_some());
    assert!(s.frame_drag_at(100.0, top + 200.0).is_none());
    // A short drag settles back; a long one closes it.
    s.drag_frame(0.0, 40.0);
    assert!(!s.release_frame(0.0, 0.0));
    assert_eq!(s.current(), "inbox");
    s.drag_frame(0.0, 300.0);
    assert!(s.release_frame(0.0, 0.0));
    assert_eq!(s.current(), "home");
    // A fast flick down closes it too.
    s.dispatch(
        &json!({ "intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet" }),
    )
    .unwrap();
    s.drag_frame(0.0, 30.0);
    assert!(s.release_frame(0.0, 900.0));
}

#[test]
fn a_carousel_pages_on_a_swipe() {
    let root = json!({ "type": "core://Carousel", "tag": "photos",
        "items": ["x://p/1", "x://p/2", "x://p/3"],
        "item_template": { "type": "core://Text", "style": "Body", "value_expr": "Photo $item.id" } });
    let d = json!({ "entities": { "x://p/1": { "id": "1" }, "x://p/2": { "id": "2" }, "x://p/3": { "id": "3" } },
        "queries": { "p": ["x://p/1", "x://p/2", "x://p/3"] } });
    let (mut s, _) = session(root, d, false);
    assert!(s.scene().unwrap().find_text("1/3").is_some());
    assert!(s.scene().unwrap().find_text("Photo 1").is_some());
    let id = s.pager_at(100.0, 100.0).unwrap();
    assert_eq!(id, "photos");
    // A short flick left turns to the next page.
    s.drag_pager(&id, 40.0);
    s.release_pager(&id, -800.0);
    let scene = s.scene().unwrap();
    assert!(scene.find_text("2/3").is_some());
    assert!(scene.find_text("Photo 2").is_some());
    assert!(scene.find_text("Photo 1").is_none());
    // Not past the last page.
    s.drag_pager(&id, 10_000.0);
    s.release_pager(&id, 0.0);
    assert!(s.scene().unwrap().find_text("3/3").is_some());
}
