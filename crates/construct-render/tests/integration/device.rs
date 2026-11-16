//! What the shared UI's device does beyond writes: Settings and the inbox,
//! notification switches, older notifications, servers, the notice bar,
//! `follow_end`, dark mode, dates and nodes this build doesn't draw. The
//! old layout system's commands (hiding, deleting and pinning layouts) are
//! gone (decision `one-layout-system`).

use construct_render::interact::{Done, Effect, Loader, Session, Target, Write, Written};
use construct_render::{Input, SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

fn data(v: Value) -> DataBundle {
    serde_json::from_value(v).unwrap()
}

fn text(s: &str) -> Value {
    json!({ "type": "core://Text", "style": "Body", "value_expr": s })
}

/// A pod that answers device commands and records them.
#[derive(Default)]
struct Pod {
    commands: Arc<Mutex<Vec<(String, Value)>>>,
    refuse: bool,
}

impl Loader for Pod {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        Ok((node(text(&format!("{id} latest"))), DataBundle::default()))
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn device_command(&self, command: &str, args: &Value) -> Option<Result<DataBundle, String>> {
        self.commands
            .lock()
            .unwrap()
            .push((command.to_owned(), args.clone()));
        if self.refuse {
            return Some(Err("/v1/x: HTTP 403 Forbidden not yours".into()));
        }
        Some(Ok(match command {
            "load_older_notifications" => data(json!({
                "entities": {
                    "local://notifications/old": { "id": "old", "title": "Last week's note" },
                    "local://inbox": { "has_older": false, "next_before": "" } },
                "queries": { "local:notifications": ["local://notifications/old"] } })),
            _ => DataBundle::default(),
        }))
    }
    fn send(&self, _write: Write, done: Done) -> bool {
        done(Written {
            result: Err("/v1/g/update: HTTP 400 Bad Request".into()),
            refreshed: Vec::new(),
        });
        true
    }
}

fn session(root: Value, d: Value, pod: Pod) -> Session {
    let mut s = Session::new("home", node(root), data(d), Viewport::PHONE);
    s.loader = Some(Box::new(pod));
    s
}

fn device(command: &str, extra: Value) -> Value {
    let mut v = json!({ "intent": "action.device", "command": command });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn settings_opens_as_a_panel_and_the_inbox_as_a_sheet() {
    let mut s = session(text("Home"), json!({}), Pod::default());
    s.dispatch(&device("open_settings", json!({}))).unwrap();
    assert_eq!(s.state()["stack"], json!(["home", "settings"]));
    let scene = s.scene().unwrap();
    // A panel: 86% of the width, at most 340 points, from the start edge.
    let shown = scene.find_text("settings latest").unwrap();
    assert!(shown.bounds[0] < 20.0, "{:?}", shown.bounds);
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    s.dispatch(&device("open_inbox", json!({}))).unwrap();
    assert_eq!(s.state()["stack"], json!(["home", "inbox"]));
    // A sheet: 85% of the height, so it starts below the top.
    let shown = s.scene().unwrap().find_text("inbox latest").unwrap();
    assert!(shown.bounds[1] > 844.0 * 0.15, "{:?}", shown.bounds);
}

fn channels() -> (Value, Value) {
    let root = json!({ "type": "core://List", "query_key": "local:channels", "item_template": {
        "type": "core://Switch", "checked": "$item.enabled", "content_description": "$item.title",
        "on_click": { "intent": "action.device", "command": "toggle_channel",
                      "layout": "$item.layout", "id": "$item.id" } } });
    let d = json!({ "entities": { "local://channels/events/match": {
            "layout": "events", "id": "match", "title": "Matches", "enabled": true } },
        "queries": { "local:channels": ["local://channels/events/match"] } });
    (root, d)
}

#[test]
fn a_channel_switch_flips_and_is_sent() {
    let (root, d) = channels();
    let pod = Pod::default();
    let commands = pod.commands.clone();
    let mut s = session(root, d, pod);
    let out = s.tap(&Target::Text("Matches".into())).unwrap();
    assert!(out.effects.contains(&Effect::Device {
        command: "toggle_channel".into()
    }));
    let c = &s.data.entities["local://channels/events/match"];
    assert_eq!(c.get_bool("enabled"), Some(false));
    assert_eq!(
        commands.lock().unwrap()[0],
        (
            "toggle_channel".into(),
            json!({ "layout": "events", "id": "match", "enabled": false })
        )
    );
}

#[test]
fn a_refused_switch_flips_back_and_says_why_until_dismissed() {
    let (root, d) = channels();
    let mut s = session(
        root,
        d,
        Pod {
            refuse: true,
            ..Pod::default()
        },
    );
    s.tap(&Target::Text("Matches".into())).unwrap();
    let c = &s.data.entities["local://channels/events/match"];
    assert_eq!(c.get_bool("enabled"), Some(true));
    let scene = s.scene().unwrap();
    let bar = scene.find_node("notice").unwrap().bounds;
    // At the bottom, the screen's width less 16 points each side.
    assert!(
        (bar[0] - 16.0).abs() < 0.5 && (bar[2] - (390.0 - 32.0)).abs() < 0.5,
        "{bar:?}"
    );
    assert!((bar[1] + bar[3] - (844.0 - 16.0)).abs() < 0.5, "{bar:?}");
    assert!(
        scene
            .find_text("/v1/x: HTTP 403 Forbidden not yours")
            .is_some()
    );
    let out = s.tap(&Target::Id("notice-dismiss".into())).unwrap();
    assert!(s.notice.is_none(), "{out:?}");
    assert!(s.scene().unwrap().find_node("notice").is_none());
}

#[test]
fn a_refused_write_shows_the_notice() {
    let root = json!({ "type": "core://Tappable", "tag": "like", "content": text("Like"),
        "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "x://p/1",
            "field": "liked", "value_expr": "true", "endpoint": "/v1/g/update", "payload": {} } });
    let mut s = session(root, json!({}), Pod::default());
    s.tap(&Target::Id("like".into())).unwrap();
    s.scene().unwrap();
    assert_eq!(
        s.notice.as_deref(),
        Some("/v1/g/update: HTTP 400 Bad Request")
    );
    assert!(s.scene().unwrap().find_node("notice-dismiss").is_some());
}

#[test]
fn older_notifications_go_under_the_newer_ones() {
    let pod = Pod::default();
    let commands = pod.commands.clone();
    let mut s = session(
        text("Inbox"),
        json!({ "entities": {
            "local://notifications/new": { "id": "new", "title": "Today's note" },
            "local://inbox": { "unread": 2, "label": "2 unread", "has_older": true, "next_before": "c50" } },
            "queries": { "local:notifications": ["local://notifications/new"] } }),
        pod,
    );
    s.dispatch(&device("load_older_notifications", json!({})))
        .unwrap();
    assert_eq!(
        commands.lock().unwrap()[0],
        (
            "load_older_notifications".into(),
            json!({ "before": "c50" })
        )
    );
    assert_eq!(
        s.data.queries["local:notifications"],
        vec!["local://notifications/new", "local://notifications/old"]
    );
    let inbox = &s.data.entities["local://inbox"];
    assert_eq!(inbox.get_bool("has_older"), Some(false));
    assert_eq!(inbox.get_str("label"), Some("2 unread"));
    // No more pages: nothing is asked.
    s.dispatch(&device("load_older_notifications", json!({})))
        .unwrap();
    assert_eq!(commands.lock().unwrap().len(), 1);
}

/// The old layout system's device commands are gone: nothing is sent to
/// the pod, and what is open stays.
#[test]
fn hiding_deleting_and_pinning_layouts_are_not_sent() {
    let pod = Pod::default();
    let commands = pod.commands.clone();
    let mut s = session(text("Home"), json!({}), pod);
    s.dispatch(&json!({ "intent": "action.navigation.open", "layout": "groceries" }))
        .unwrap();
    for c in [
        "hide_layout",
        "unhide_layout",
        "delete_layout",
        "show_versions",
        "follow_latest",
    ] {
        let intent = device(c, json!({ "layout": "groceries", "id": "3" }));
        let out = s.dispatch(&intent).unwrap();
        assert_eq!(out.effects, vec![Effect::NotSent { intent }], "{c}");
    }
    assert!(commands.lock().unwrap().is_empty());
    assert_eq!(s.state()["stack"], json!(["home", "groceries"]));
    assert!(s.scene().unwrap().find_text("groceries latest").is_some());
}

#[test]
fn set_server_asks_the_platform_for_another_server() {
    let d = json!({ "entities": {
        "local://servers/staging": { "id": "staging", "title": "Staging",
            "url": "https://pod.example", "selected": true },
        "local://servers/local": { "id": "local", "title": "Local",
            "url": "http://127.0.0.1:8080", "selected": false } },
        "queries": { "local:servers": ["local://servers/staging", "local://servers/local"] } });
    let mut s = session(text("Settings"), d, Pod::default());
    let out = s
        .dispatch(&device("set_server", json!({ "id": "local" })))
        .unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::SwitchServer {
            url: "http://127.0.0.1:8080".into()
        }]
    );
    let out = s
        .dispatch(&device("set_server", json!({ "id": "staging" })))
        .unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing], "already in use");
}

#[test]
fn a_follow_end_scroll_goes_to_the_end_after_send_but_not_after_a_like() {
    let rows: Vec<String> = (0..30).map(|i| format!("x://c/{i}")).collect();
    let mut entities = serde_json::Map::new();
    for (i, u) in rows.iter().enumerate() {
        entities.insert(
            u.clone(),
            json!({ "id": i, "text": format!("Comment {i}") }),
        );
    }
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://Scroll", "follow_end": true, "content": {
            "type": "core://List", "query_key": "c", "item_template": {
                "type": "core://Tappable", "tag": "like_$item.id", "content": {
                    "type": "core://Box", "padding_v_dp": 16, "content": {
                        "type": "core://Text", "style": "Body", "value_expr": "$item.text" } },
                "on_click": { "intent": "action.local.set", "entity_uri": "$item.uri",
                              "field": "liked", "value_expr": "true" } } } },
        { "type": "core://Button", "label": "Send", "on_click": {
            "intent": "action.mutation.create", "query_key": "c", "entity_uri": "x://c/new",
            "endpoint": "/v1/c", "entity": { "id": "new", "text": "New comment" }, "payload": {} } }
    ] });
    let mut s = Session::new(
        "home",
        node(root),
        data(json!({ "entities": entities, "queries": { "c": rows } })),
        Viewport::PHONE,
    );
    s.scene().unwrap();
    s.tap(&Target::Id("like_0".into())).unwrap();
    s.scene().unwrap();
    assert_eq!(
        s.state()["scroll"],
        json!({}),
        "a like inside doesn't move it"
    );
    s.tap(&Target::Text("Send".into())).unwrap();
    s.scene().unwrap();
    let scroll = s.state()["scroll"].clone();
    let (_, offset) = scroll.as_object().unwrap().iter().next().unwrap();
    assert!(offset.as_f64().unwrap() > 500.0, "{scroll}");
    assert!(
        s.scene()
            .unwrap()
            .find_text("New comment")
            .is_some_and(|n| n.bounds[1] < 844.0)
    );
}

#[test]
fn dark_mode_draws_the_dark_theme() {
    let mut s = Session::new(
        "home",
        node(text("Hi")),
        DataBundle::default(),
        Viewport::PHONE,
    );
    let light = s.scene().unwrap().background;
    s.set_dark(true);
    let dark = s.scene().unwrap().background;
    assert!(dark[0] < 0.2 && light[0] > 0.9, "{light:?} {dark:?}");
}

fn draw(root: Value, d: Value, today: &str) -> construct_render::Scene {
    let mut b = SceneBuilder::new();
    b.clock.fixed = construct_render::dates::iso_date(today).map(|d| d * 86_400 + 12 * 3_600);
    b.build(&Input {
        root: &node(root),
        data: &data(d),
        form: &BTreeMap::new(),
        route: &BTreeMap::new(),
        layouts: &BTreeMap::new(),
        scroll: &BTreeMap::new(),
        viewport: Viewport::PHONE,
        dark: false,
        item: None,
        insets: [0.0; 4],
        focused: None,
    })
    .unwrap()
}

#[test]
fn text_format_draws_dates_for_the_viewer() {
    let d = json!({ "entities": {
        "x://t/1": { "due": "2026-11-08", "done": "2026-11-01" } },
        "queries": { "t": ["x://t/1"] } });
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://Text", "style": "Body", "value_expr": "Due ${x://t/1#due}",
          "format": { "date": "Relative" } },
        { "type": "core://Text", "style": "Body", "value_expr": "${x://t/1#done}",
          "format": { "date": "Short", "past": "Overdue" } },
        { "type": "core://List", "query_key": "t", "item_template": {
            "type": "core://Text", "style": "Body", "value_expr": "$item.due",
            "format": { "date": "Short" } } },
        { "type": "core://Text", "style": "Body", "value_expr": "${x://t/1#due}" }
    ] });
    let scene = draw(root, d, "2026-11-07");
    for t in ["Due tomorrow", "Overdue", "Nov 8", "2026-11-08"] {
        assert!(scene.find_text(t).is_some(), "{t}");
    }
}

#[test]
fn removed_and_unknown_nodes_are_named_as_the_shared_ui_names_them() {
    let root = json!({ "type": "core://Column", "children": [
        { "type": "core://AppFrame", "app": "groceries" },
        { "type": "core://Hologram", "beam": 3 }
    ] });
    let scene = draw(root, json!({}), "2026-11-07");
    for t in [
        "Unsupported component: core://AppFrame",
        "Unsupported component: core://Hologram",
    ] {
        assert!(scene.find_text(t).is_some(), "{t}");
    }
}

#[test]
fn a_long_notice_wraps_and_keeps_its_dismiss_on_screen() {
    let mut s = Session::new(
        "home",
        node(text("Hi")),
        DataBundle::default(),
        Viewport::PHONE,
    );
    s.notice = Some("The pod couldn't save that change because something on it went wrong, try again in a minute".into());
    let scene = s.scene().unwrap();
    let dismiss = scene.find_node("notice-dismiss").unwrap().bounds;
    assert!(dismiss[0] + dismiss[2] <= 390.0 - 16.0 + 0.5, "{dismiss:?}");
    let bar = scene.find_node("notice").unwrap().bounds;
    assert!(bar[3] > 60.0, "two lines: {bar:?}");
}
