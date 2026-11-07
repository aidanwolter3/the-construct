//! Taps that write to the pod, the device commands of the inbox, links,
//! dragging sheets and paging carousels.

use construct_render::Viewport;
use construct_render::interact::{Changes, Done, Effect, Loader, Session, Target, Write, Written};
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
    fn open_session(&self, path: &str, payload: &Value) -> Option<Result<String, String>> {
        Some(match (path, payload["token"].as_str()) {
            ("/v1/pods/open", Some("cpod_good")) => Ok("cpod_good".into()),
            ("/v1/pods/temporary", _) => Ok("cpod_temp".into()),
            _ => Err("That pod code didn't open a pod.".into()),
        })
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

#[test]
fn a_carousel_of_one_fills_its_width() {
    // As a post screen has it: in a list item's column, in a scroll.
    let root = json!({ "type": "core://Scroll", "content": { "type": "core://Column", "children": [
        { "type": "core://List", "query_key": "post", "item_template": { "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": "Aidan" },
            { "type": "core://Carousel", "tag": "c", "aspect_field": "aspect_ratio", "items": "$item.media_uris",
              "item_template": { "type": "core://Image", "url": "$item.url", "blurhash": "$item.blurhash" } }
        ] } }
    ] } });
    for media in [vec!["x://m/1"], vec!["x://m/1", "x://m/2"]] {
        let d = json!({ "entities": {
            "x://p/1": { "media_uris": media },
            "x://m/1": { "url": "https://media.example/1.jpg", "aspect_ratio": 1.0 },
            "x://m/2": { "url": "https://media.example/2.jpg", "aspect_ratio": 1.0 } },
            "queries": { "post": ["x://p/1"] } });
        let (mut s, _) = session(root.clone(), d, false);
        let scene = s.scene().unwrap();
        let img = scene
            .draws
            .iter()
            .find_map(|d| match d {
                construct_render::scene::Draw::Image(i) => Some(i.rect),
                _ => None,
            })
            .unwrap();
        assert!(
            (img[2] - Viewport::PHONE.width).abs() < 0.5 && (img[3] - img[2]).abs() < 0.5,
            "{} pages: {img:?}",
            media.len()
        );
    }
}

#[test]
fn a_confirm_asks_first_and_cancel_runs_nothing() {
    let root = json!({ "type": "core://Button", "label": "Leave pod on this device", "on_click": {
        "intent": "action.confirm", "title": "Leave this pod?", "message": "This device forgets the pod.",
        "confirm_label": "Leave", "then": { "intent": "action.device", "command": "leave_pod" } } });
    let (mut s, _) = session(root, json!({}), false);
    let out = s
        .tap(&Target::Text("Leave pod on this device".into()))
        .unwrap();
    assert!(!out.effects.contains(&Effect::LeftPod), "{:?}", out.effects);
    assert!(s.scene().unwrap().find_text("Leave this pod?").is_some());
    // Cancel (or a tap outside, or Back) closes it; nothing ran.
    let out = s.tap(&Target::Id("confirm:cancel".into())).unwrap();
    assert_eq!(out.effects, vec![Effect::Back]);
    assert!(s.scene().unwrap().find_text("Leave this pod?").is_none());
    // Confirm runs `then`.
    s.tap(&Target::Text("Leave pod on this device".into()))
        .unwrap();
    let out = s.tap(&Target::Id("confirm:ok".into())).unwrap();
    assert!(out.effects.contains(&Effect::LeftPod), "{:?}", out.effects);
    assert_eq!(s.current(), "home");
}

#[test]
fn signing_in_answers_the_session_and_the_code_to_remember() {
    let field = |payload: Value, path: &str| {
        json!({ "type": "core://Button", "label": path,
        "on_click": { "intent": "action.auth.session", "path": path, "payload": payload } })
    };
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://TextField", "field": "pod_token", "placeholder": "Pod code" },
        field(json!({ "token": "$form.pod_token" }), "/v1/pods/open"),
        field(json!({}), "/v1/pods/temporary") ] });
    let (mut s, _) = session(root, json!({}), false);
    // A blank code sends nothing.
    let out = s.tap(&Target::Text("/v1/pods/open".into())).unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
    s.type_text("pod_token", "cpod_bad");
    let out = s.tap(&Target::Text("/v1/pods/open".into())).unwrap();
    assert!(
        matches!(out.effects[0], Effect::LoadFailed { .. }),
        "{:?}",
        out.effects
    );
    s.type_text("pod_token", "cpod_good");
    let out = s.tap(&Target::Text("/v1/pods/open".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::SignedIn {
            token: "cpod_good".into(),
            code: "cpod_good".into()
        }]
    );
    // A temporary pod's code is its session.
    let out = s.tap(&Target::Text("/v1/pods/temporary".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::SignedIn {
            token: "cpod_temp".into(),
            code: "cpod_temp".into()
        }]
    );
}

#[test]
fn text_fields_edit_like_android() {
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://Text", "style": "Body", "value_expr": "Enter your pod code" },
        { "type": "core://TextField", "field": "pod_token", "placeholder": "Pod code", "max_length": 12 } ] });
    let (mut s, _) = session(root, json!({}), false);
    let f = s
        .scene()
        .unwrap()
        .find_node("field:pod_token")
        .unwrap()
        .bounds;
    // An empty field: a long press offers Paste only.
    s.long_press(f[0] + 20.0, f[1] + 20.0).unwrap();
    assert_eq!(s.focused.as_deref(), Some("pod_token"));
    assert!(s.scene().unwrap().find_node("edit:paste").is_some());
    assert!(s.scene().unwrap().find_node("edit:copy").is_none());
    let out = s.tap(&Target::Id("edit:paste".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Paste {
            field: "pod_token".into()
        }]
    );
    // The platform pastes its clipboard (no further than max_length).
    s.paste("pod_token", "cpod_abc def\n");
    assert_eq!(s.form["pod_token"], "cpod_abc def");
    // Typing goes in at the cursor, Backspace removes before it.
    s.move_cursor(construct_render::interact::CursorMove::Home, false);
    s.insert_text("x");
    assert_eq!(s.form["pod_token"], "xcpod_abc de", "max_length");
    s.backspace();
    assert_eq!(s.form["pod_token"], "cpod_abc de");
    // A long press selects the word under it; Copy and Cut give it to the
    // platform.
    s.long_press(f[0] + 20.0, f[1] + f[3] / 2.0).unwrap();
    let out = s.tap(&Target::Id("edit:copy".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Copy {
            text: "cpod_abc".into()
        }]
    );
    // Select all, then typing replaces it all.
    s.long_press(f[0] + 20.0, f[1] + f[3] / 2.0).unwrap();
    s.tap(&Target::Id("edit:select_all".into())).unwrap();
    let out = s.tap(&Target::Id("edit:cut".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Copy {
            text: "cpod_abc de".into()
        }]
    );
    assert_eq!(s.form["pod_token"], "");
    s.insert_text("cpod_new");
    // A tap in the text puts the cursor there.
    s.tap(&Target::Point(f[0] + 16.0, f[1] + f[3] / 2.0))
        .unwrap();
    s.insert_text("!");
    assert_eq!(s.form["pod_token"], "!cpod_new");
    // A tap elsewhere ends editing.
    s.tap(&Target::Text("Enter your pod code".into())).unwrap();
    assert!(s.focused.is_none());
    assert!(s.scene().unwrap().find_node("edit:paste").is_none());
}

#[test]
fn the_keyboards_input_connection_edits_the_field() {
    use construct_render::ime::{Connection, Tell};
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://TextField", "field": "pod_token", "placeholder": "Pod code", "max_length": 12 } ] });
    let (mut s, _) = session(root, json!({}), false);
    let f = s
        .scene()
        .unwrap()
        .find_node("field:pod_token")
        .unwrap()
        .bounds;
    let underlines = |s: &mut Session| {
        s.scene()
            .unwrap()
            .quads()
            .filter(|q| q.rect[3] == 1.5 && q.rect[1] > f[1] && q.rect[1] < f[1] + f[3])
            .count()
    };
    let mut keyboard = Connection::default();
    assert_eq!(keyboard.sync(s.text_state().as_ref()), (None, None));
    // A tap focuses the field: the keyboard starts on it.
    s.tap(&Target::Id("field:pod_token".into())).unwrap();
    assert_eq!(
        keyboard.sync(s.text_state().as_ref()),
        (None, Some(Tell::Restart))
    );
    // The keyboard composes a word: it is in the field, underlined.
    keyboard.edit(|t| t.set_composing_text("cpod", 1));
    let (edit, _) = keyboard.sync(s.text_state().as_ref());
    s.set_text_state(&edit.unwrap().state.unwrap());
    assert_eq!(s.form["pod_token"], "cpod");
    assert_eq!(underlines(&mut s), 1);
    assert_eq!(keyboard.sync(s.text_state().as_ref()), (None, None));
    // Its suggestion replaces the word, no further than max_length: the
    // keyboard starts over with what the field kept.
    keyboard.edit(|t| t.commit_text("cpod_abcdefghij", 1));
    let (edit, _) = keyboard.sync(s.text_state().as_ref());
    s.set_text_state(&edit.unwrap().state.unwrap());
    assert_eq!(s.form["pod_token"], "cpod_abcdefg");
    assert_eq!(underlines(&mut s), 0);
    assert_eq!(
        keyboard.sync(s.text_state().as_ref()),
        (None, Some(Tell::Restart))
    );
    // The engine's own editing: the keyboard hears where the cursor went.
    s.move_cursor(construct_render::interact::CursorMove::Home, false);
    assert_eq!(
        keyboard.sync(s.text_state().as_ref()),
        (None, Some(Tell::Selection([0, 0, -1, -1])))
    );
    // Its Done key ends editing.
    keyboard.edit(|t| t.delete_surrounding(0, 1, false));
    assert!(keyboard.action());
    let (edit, _) = keyboard.sync(s.text_state().as_ref());
    let edit = edit.unwrap();
    assert!(edit.done);
    s.set_text_state(&edit.state.unwrap());
    assert_eq!(s.form["pod_token"], "pod_abcdefg");
}

/// A loader with an outbox of its own: it takes writes, and hands the
/// session what it hears of.
struct Outbox {
    taken: Arc<Mutex<Vec<Value>>>,
    news: Arc<Mutex<Option<Changes>>>,
    garden: UiNode,
}

impl Loader for Outbox {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        match id {
            "garden" => Ok((self.garden.clone(), DataBundle::default())),
            _ => Err(format!("no layout {id}")),
        }
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn take(&self, intent: &Value) -> bool {
        self.taken.lock().unwrap().push(intent.clone());
        true
    }
    fn changes(&self) -> Option<Changes> {
        self.news.lock().unwrap().take()
    }
}

fn texts(s: &mut Session) -> Vec<String> {
    s.scene()
        .unwrap()
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect()
}

#[test]
fn a_loader_with_an_outbox_takes_writes_and_brings_changes() {
    let garden = |title: &str| {
        node(json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Title", "value_expr": title },
            { "type": "core://Text", "style": "Body", "value_expr": "${x://g/a#name}" }
        ] }))
    };
    let taken = Arc::new(Mutex::new(Vec::new()));
    let news = Arc::new(Mutex::new(None));
    let mut s = Session::new(
        "home",
        node(groceries()),
        data(grocery_data()),
        Viewport::PHONE,
    );
    s.loader = Some(Box::new(Outbox {
        taken: taken.clone(),
        news: news.clone(),
        garden: garden("Garden"),
    }));
    // The write goes to the loader as the layout wrote it; the change shows
    // here at once and nothing waits on an answer.
    let out = s.tap(&Target::Id("check_a".into())).unwrap();
    assert!(
        out.effects.iter().any(|e| matches!(e, Effect::Sent { .. })),
        "{:?}",
        out.effects
    );
    assert_eq!(
        taken.lock().unwrap()[0]["intent"],
        "action.mutation.optimistic"
    );
    assert_eq!(s.data.entities["x://g/a"].get("done"), Some(&json!(true)));
    assert!(!s.busy());

    // A layout opened on the stack changes elsewhere, and so does the data.
    s.dispatch(
        &json!({ "intent": "action.navigation.open", "layout": "garden", "presentation": "Push" }),
    )
    .unwrap();
    assert!(texts(&mut s).contains(&"Garden".to_owned()));
    *news.lock().unwrap() = Some(Changes {
        data: data(
            json!({ "entities": { "x://g/a": { "id": "a", "name": "Duck eggs", "done": true } } }),
        ),
        layouts: vec![("garden".into(), garden("Garden, edited"))],
    });
    let shown = texts(&mut s);
    assert!(shown.contains(&"Garden, edited".to_owned()), "{shown:?}");
    assert!(shown.contains(&"Duck eggs".to_owned()), "{shown:?}");
}

#[test]
fn the_notification_permission_is_asked_by_the_platform() {
    let (mut s, writes) = session(
        json!({ "type": "core://Text", "style": "Body", "value_expr": "x" }),
        json!({}),
        false,
    );
    let out = s
        .dispatch(
            &json!({ "intent": "action.device", "command": "request_notification_permission" }),
        )
        .unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Device {
            command: "request_notification_permission".into()
        }]
    );
    assert!(writes.lock().unwrap().is_empty());
}

#[test]
fn liking_a_post_counts_the_like_and_unliking_takes_it_back() {
    // The sample feed's Like: a ternary with arithmetic in an `also` update.
    let root = json!({ "type": "core://List", "query_key": "feed", "item_template": {
        "type": "core://Row", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": "$item.like_count" },
            { "type": "core://Tappable", "tag": "like_$item.id", "content_description": "Like",
              "content": { "type": "core://Text", "style": "Label", "value_expr": "Like" },
              "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": "is_liked", "value_expr": "!entity.is_liked", "rollback_value": false,
                "also": [ { "field": "like_count",
                  "value_expr": "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1" } ],
                "endpoint": "/v1/posts/like", "payload": { "liked": "$value", "id": "$item.id" } } }
        ] } });
    let d = json!({ "entities": { "x://posts/101": { "id": "101", "is_liked": false, "like_count": 89 } },
                    "queries": { "feed": ["x://posts/101"] } });
    let (mut s, _) = session(root, d, false);
    let like = Target::Id("like_101".into());
    s.tap(&like).unwrap();
    let post = &s.data.entities["x://posts/101"];
    assert_eq!(post.get("like_count"), Some(&json!(90)));
    assert_eq!(post.get_bool("is_liked"), Some(true));
    assert!(texts(&mut s).contains(&"90".to_owned()));
    s.tap(&like).unwrap();
    s.scene().unwrap();
    assert_eq!(
        s.data.entities["x://posts/101"].get("like_count"),
        Some(&json!(89))
    );
    assert!(texts(&mut s).contains(&"89".to_owned()));
}

#[test]
fn a_local_set_evaluates_a_ternary() {
    let root = json!({ "type": "core://Tappable", "tag": "step", "content_description": "Step",
        "content": { "type": "core://Text", "style": "Body", "value_expr": "Step" },
        "on_click": { "intent": "action.local.set", "entity_uri": "local://counter",
          "field": "n", "value_expr": "entity.big ? entity.n + 10 : entity.n + 1" } });
    let d = json!({ "entities": { "local://counter": { "n": 1, "big": true } } });
    let (mut s, _) = session(root, d, false);
    s.tap(&Target::Id("step".into())).unwrap();
    assert_eq!(
        s.data.entities["local://counter"].get("n"),
        Some(&json!(11))
    );
}
