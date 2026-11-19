//! A scrolling view's content fades where it passes under a header or a
//! bottom bar, and not at the screen's own edges.

use construct_render::Viewport;
use construct_render::interact::Session;
use construct_render::scene::{Draw, Scene, SemanticNode};
use construct_render::{Input, SceneBuilder};
use protocol::UiNode;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn session(header: bool) -> Session {
    session_with(|scroll| {
        if header {
            json!({ "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Title", "style": "Headline" },
                scroll,
                { "type": "core://Text", "value_expr": "Tabs", "style": "Body" } ] })
        } else {
            scroll
        }
    })
}

fn session_with(wrap: impl Fn(Value) -> Value) -> Session {
    let rows: Vec<Value> = (0..40)
        .map(
            |i| json!({ "type": "core://Text", "value_expr": format!("Row {i}"), "style": "Body" }),
        )
        .collect();
    let scroll = json!({ "type": "core://Scroll", "content": { "type": "core://Column", "children": rows } });
    let root = wrap(scroll);
    let page: protocol::UiNode =
        serde_json::from_value(json!({ "type": "core://Page", "id": "p", "root": root })).unwrap();
    Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 300.0,
            height: 400.0,
            scale: 1.0,
        },
    )
}

fn scroll_view(scene: &Scene) -> &SemanticNode {
    scene
        .semantics
        .iter()
        .find(|n| n.node_type == "core://Scroll")
        .unwrap()
}

/// The fade strips drawn at the view's top and at its bottom.
fn fades(s: &mut Session) -> (usize, usize) {
    strips(s.scene().unwrap())
}

fn strips(scene: &Scene) -> (usize, usize) {
    let view = scroll_view(scene).bounds;
    let mid = view[1] + view[3] / 2.0;
    let strips = scene.draws.iter().filter_map(|d| match d {
        Draw::Quad(q) if q.rect[3] <= 1.0 && q.rect[2] == view[2] && q.bg_color[3] < 1.0 => {
            Some(q.rect[1])
        }
        _ => None,
    });
    strips.fold(
        (0, 0),
        |(t, b), y| if y < mid { (t + 1, b) } else { (t, b + 1) },
    )
}

#[test]
fn content_fades_under_a_header_and_a_bottom_bar_while_there_is_more_that_way() {
    let mut s = session(true);
    let (top, bottom) = fades(&mut s);
    assert_eq!(top, 0, "nothing above yet");
    assert!(bottom > 0, "more below");
    s.scroll_by(None, 200.0).unwrap();
    let (top, bottom) = fades(&mut s);
    assert!(top > 0 && bottom > 0, "{top} {bottom}");
    s.scroll_by(None, 100_000.0).unwrap();
    let (top, bottom) = fades(&mut s);
    assert!(top > 0, "scrolled past the top");
    assert_eq!(bottom, 0, "at the end");
}

#[test]
fn an_embedded_layout_above_a_tab_bar_fades_into_it() {
    // As a home whose tab under an embedded layout: the embed clips to its
    // own box, which ends where the tab bar starts.
    let rows: Vec<Value> = (0..40)
        .map(
            |i| json!({ "type": "core://Text", "value_expr": format!("Row {i}"), "style": "Body" }),
        )
        .collect();
    let inner = json!({ "type": "core://Scroll", "content": { "type": "core://Column", "children": rows } });
    let root: UiNode = serde_json::from_value(json!({ "type": "core://Page", "id": "home", "root": {
        "type": "core://Column", "children": [
            { "type": "core://Box", "height_dp": 300, "content": { "type": "core://Layout", "layout": "inner" } },
            { "type": "core://Text", "value_expr": "Tabs", "style": "Body" } ] } }))
    .unwrap();
    let layouts = BTreeMap::from([("inner".to_owned(), serde_json::from_value(inner).unwrap())]);
    let build = |scroll: &BTreeMap<String, f32>| {
        SceneBuilder::new()
            .build(&Input {
                root: &root,
                data: &Default::default(),
                form: &BTreeMap::new(),
                route: &BTreeMap::new(),
                layouts: &layouts,
                scroll,
                viewport: Viewport {
                    width: 300.0,
                    height: 400.0,
                    scale: 1.0,
                },
                dark: false,
                item: None,
                insets: [0.0; 4],
                focused: None,
                panels: &construct_render::NO_PANELS,
            })
            .unwrap()
    };
    let id = scroll_view(&build(&BTreeMap::new())).id.clone();
    let scene = build(&BTreeMap::from([(id, 100.0)]));
    let (top, bottom) = strips(&scene);
    assert_eq!(top, 0, "at the screen's top edge");
    assert!(bottom > 0, "above the tab bar");
}

#[test]
fn a_view_filling_the_screen_does_not_fade_at_its_edges() {
    let mut s = session(false);
    s.scroll_by(None, 200.0).unwrap();
    assert_eq!(fades(&mut s), (0, 0));
    // Nor under the system bars, inside which the screen's content sits.
    s.set_insets([24.0, 0.0, 16.0, 0.0]);
    assert_eq!(fades(&mut s), (0, 0));
}
