//! Decision `wide-screen-panels`: an inline `core://Layout`'s `params`, a
//! layout opened from a `panels: true` embed on a wide screen opening beside
//! it, and the mouse's hover.

use construct_render::Viewport;
use construct_render::interact::{Effect, Session, Target};
use protocol::UiNode;
use serde_json::{Value, json};

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

const WIDE: Viewport = Viewport {
    width: 1440.0,
    height: 900.0,
    scale: 1.0,
};

fn open(layout: &str, tag: &str, params: Value) -> Value {
    json!({ "type": "core://Tappable", "tag": tag,
        "on_click": { "intent": "action.navigation.open", "layout": layout, "presentation": "Push",
                      "params": params },
        "content": { "type": "core://Text", "style": "Body", "value_expr": format!("Open {tag}") } })
}

/// A sidebar and, beside it, an inline list whose rows open a detail, the
/// detail opening "more" and closing itself.
fn session(viewport: Viewport) -> Session {
    let home = node(json!({ "type": "core://Row", "children": [
        { "type": "core://Box", "width_dp": 300,
          "content": { "type": "core://Text", "style": "Body", "value_expr": "Sidebar" } },
        { "type": "core://Box", "fill": true,
          "content": { "type": "core://Layout", "layout": "list", "panels": true } } ] }));
    let mut s = Session::new("home", home, Default::default(), viewport);
    s.layouts.insert(
        "list".into(),
        node(json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": "List" },
            open("detail", "salmon", json!({ "name": "Salmon" })) ] })),
    );
    s.layouts.insert(
        "detail".into(),
        node(json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": "Detail $route.name" },
            open("more", "deeper", json!({})),
            { "type": "core://Tappable", "tag": "close",
              "on_click": { "intent": "action.navigation.back" },
              "content": { "type": "core://Text", "style": "Body", "value_expr": "Close" } } ] })),
    );
    s.layouts.insert(
        "more".into(),
        node(json!({ "type": "core://Text", "style": "Body", "value_expr": "More" })),
    );
    s
}

fn shows(s: &mut Session, text: &str) -> Option<[f32; 4]> {
    s.scene().unwrap().find_text(text).map(|n| n.bounds)
}

#[test]
fn an_inline_layouts_params_bind_as_its_route() {
    let home = node(json!({ "type": "core://Column", "children": [
        { "type": "core://Layout", "layout": "area", "params": { "area": "House" } },
        { "type": "core://Layout", "layout": "area", "params": { "area": "Yard" } } ] }));
    let mut s = Session::new("home", home, Default::default(), Viewport::PHONE);
    s.layouts.insert(
        "area".into(),
        node(json!({ "type": "core://Text", "style": "Body", "value_expr": "Area: $route.area" })),
    );
    assert!(shows(&mut s, "Area: House").is_some());
    assert!(shows(&mut s, "Area: Yard").is_some());
    assert!(shows(&mut s, "Area: $route.area").is_none());
}

#[test]
fn a_push_from_a_panel_host_opens_beside_it_and_back_closes_it() {
    let mut s = session(WIDE);
    let out = s.tap(&Target::Id("salmon".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Opened {
            layout: "detail".into()
        }]
    );
    // Nothing pushed over the screen: the detail is a panel right of the
    // list, with the opening row's params as its route, the sidebar kept.
    assert_eq!(s.current(), "home");
    let list = shows(&mut s, "List").unwrap();
    let detail = shows(&mut s, "Detail Salmon").expect("the panel");
    assert!(shows(&mut s, "Sidebar").is_some());
    assert!(detail[0] > list[0] + 300.0, "{list:?} {detail:?}");
    // A hairline between the columns.
    let scene = s.scene().unwrap();
    assert!(
        scene
            .quads()
            .any(|q| q.rect[2] == 1.0 && q.rect[0] > list[0] && q.rect[0] < detail[0]),
        "a line between them"
    );

    // Deeper: a third column; two show, so the list moves out of sight.
    s.tap(&Target::Id("deeper".into())).unwrap();
    assert!(shows(&mut s, "More").is_some());
    assert!(shows(&mut s, "Detail Salmon").is_some());
    assert!(shows(&mut s, "List").is_none());

    // System Back closes the rightmost panel.
    let out = s
        .dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert_eq!(out.effects, vec![Effect::Back]);
    assert!(shows(&mut s, "More").is_none());
    assert!(shows(&mut s, "List").is_some());

    // A back button in a panel closes that panel.
    s.tap(&Target::Id("close".into())).unwrap();
    assert!(shows(&mut s, "Detail Salmon").is_none());
    assert_eq!(s.current(), "home");
    // With none open, Back is as before (nothing under home).
    let out = s
        .dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
}

#[test]
fn a_push_from_a_panel_column_replaces_the_panels_past_it() {
    let mut s = session(WIDE);
    s.tap(&Target::Id("salmon".into())).unwrap();
    s.tap(&Target::Id("deeper".into())).unwrap();
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    // From the list again: the detail opens anew, nothing past it.
    s.tap(&Target::Id("salmon".into())).unwrap();
    assert!(shows(&mut s, "List").is_some());
    assert!(shows(&mut s, "Detail Salmon").is_some());
    assert!(shows(&mut s, "More").is_none());
}

#[test]
fn a_screen_tapped_in_a_panel_opens_from_the_panels_layout() {
    // A feed in a panel whose rows and button open its own screens by
    // address (a post, a New post): home has no such screens.
    let mut s = session(WIDE);
    s.layouts.insert(
        "detail".into(),
        node(json!({ "type": "core://Page", "id": "detail", "title": "Feed",
            "root": { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Body", "value_expr": "Feed" },
                { "type": "core://Tappable", "tag": "post",
                  "on_click": { "intent": "action.navigation.open", "presentation": "Push",
                                "target_uri": "feed://posts/7" },
                  "content": { "type": "core://Text", "style": "Body", "value_expr": "A post" } },
                { "type": "core://Tappable", "tag": "compose",
                  "on_click": { "intent": "action.navigation.open", "presentation": "Modal",
                                "target_uri": "feed://compose" },
                  "content": { "type": "core://Text", "style": "Body", "value_expr": "New post" } } ] },
            "screens": [
                { "id": "post", "route": "feed://posts/{id}", "title": "Post",
                  "root": { "type": "core://Text", "style": "Body", "value_expr": "Post $route.id" } },
                { "id": "compose", "route": "feed://compose", "title": "New post",
                  "root": { "type": "core://Text", "style": "Body", "value_expr": "Write a caption" } } ] })),
    );
    s.tap(&Target::Id("salmon".into())).unwrap();
    assert!(shows(&mut s, "A post").is_some(), "the feed is a panel");

    let out = s.tap(&Target::Id("post".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::OpenedScreen {
            screen: "post".into()
        }]
    );
    assert!(shows(&mut s, "Post 7").is_some());
    // Back returns to home with the feed still beside the list.
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert!(shows(&mut s, "Post 7").is_none());
    assert!(shows(&mut s, "A post").is_some());

    s.tap(&Target::Id("compose".into())).unwrap();
    assert!(shows(&mut s, "Write a caption").is_some());
}

#[test]
fn on_a_phone_a_panel_host_pushes_as_before() {
    for width in [390.0, 700.0] {
        let mut s = session(Viewport {
            width,
            height: 844.0,
            scale: 1.0,
        });
        s.tap(&Target::Id("salmon".into())).unwrap();
        assert_eq!(s.current(), "detail", "{width}");
    }
}

#[test]
fn the_mouse_hovers_a_tappable_lighter_than_a_press() {
    let mut s = session(WIDE);
    let scene = s.scene().unwrap();
    let before = scene.quads().count();
    let row = scene.find_node("salmon").unwrap().bounds;
    let (x, y) = (row[0] + 2.0, row[1] + 2.0);
    assert!(s.hover(x, y));
    assert!(s.hovering());
    let scene = s.scene().unwrap();
    assert_eq!(scene.quads().count(), before + 1);
    let wash = scene.quads().find(|q| q.rect == row).expect("a wash");
    assert!(
        wash.bg_color[3] > 0.0 && wash.bg_color[3] < 0.08,
        "{wash:?}"
    );
    // Moving within it changes nothing; off it, the wash goes.
    assert!(!s.hover(x + 1.0, y));
    assert!(s.hover(1400.0, 880.0));
    assert!(!s.hovering());
    assert_eq!(s.scene().unwrap().quads().count(), before);
    assert!(s.hover(x, y));
    assert!(s.hover_end());
    assert_eq!(s.scene().unwrap().quads().count(), before);
}
