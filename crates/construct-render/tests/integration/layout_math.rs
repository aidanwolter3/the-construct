//! Phase 1: layout math, no GPU.

use construct_render::scene::Draw;
use construct_render::{SceneBuilder, Viewport};
use serde_json::json;
use std::collections::BTreeMap;

use crate::common::{build, close, fixture};

fn pill(tag: &str, label: &str) -> serde_json::Value {
    json!({ "type": "core://Tappable", "tag": tag,
        "on_click": { "intent": "action.navigation.back" },
        "content": { "type": "core://Box", "corner_dp": 16, "padding_h_dp": 12, "padding_v_dp": 6,
            "background": "#EEF0F4",
            "content": { "type": "core://Text", "value_expr": label, "style": "Body" } } })
}

#[test]
fn pill_row_layout_math() {
    let spec = json!({ "type": "core://Row", "spacing_dp": 8, "padding_h_dp": 16,
        "children": [pill("btn_1", "Save"), pill("btn_2", "Cancel")] });
    let scene = SceneBuilder::build_from_spec(&spec, 390.0, 844.0, 2.0).unwrap();
    let b1 = scene.find_node("btn_1").unwrap();
    let b2 = scene.find_node("btn_2").unwrap();
    assert_eq!(b1.bounds[0], 16.0, "left padding");
    assert!(
        close(b2.bounds[0], b1.bounds[0] + b1.bounds[2] + 8.0),
        "x + width + gap"
    );
    // "Cancel" is wider than "Save"; both are as tall as their text plus padding.
    assert!(b2.bounds[2] > b1.bounds[2]);
    assert!(close(b1.bounds[3], 20.0 + 12.0));
}

#[test]
fn flexbox_expansion() {
    // A spacer takes the rest of the row: the trailing text ends at the padding.
    let spec = json!({ "type": "core://Row", "padding_h_dp": 16, "children": [
        { "type": "core://Text", "value_expr": "Left", "style": "Body" },
        { "type": "core://Spacer" },
        { "type": "core://Text", "value_expr": "Right", "style": "Body" } ] });
    let scene = SceneBuilder::build_from_spec(&spec, 400.0, 300.0, 1.0).unwrap();
    let right = scene.find_text("Right").unwrap();
    assert!(
        close(right.bounds[0] + right.bounds[2], 400.0 - 16.0),
        "{:?}",
        right.bounds
    );

    // Columns in a row are text-like (as Compose's `NodeRow`): a short one
    // keeps its width, a long one takes what is left and wraps.
    let col = |t: &str| json!({ "type": "core://Column", "children": [{ "type": "core://Text", "value_expr": t, "style": "Body" }] });
    let long = "Much longer text that cannot possibly fit on one line of this row, so it has to wrap onto a second one";
    let spec = json!({ "type": "core://Row", "spacing_dp": 10, "children": [
        { "type": "core://Tappable", "tag": "a", "on_click": { "intent": "action.navigation.back" }, "content": col("A") },
        { "type": "core://Tappable", "tag": "b", "on_click": { "intent": "action.navigation.back" }, "content": col(long) } ] });
    let scene = SceneBuilder::build_from_spec(&spec, 410.0, 300.0, 1.0).unwrap();
    let (a, b) = (scene.find_node("a").unwrap(), scene.find_node("b").unwrap());
    assert!(a.bounds[2] < 20.0, "{:?}", a.bounds);
    assert!(close(b.bounds[0], a.bounds[2] + 10.0));
    assert!(b.bounds[0] + b.bounds[2] <= 410.0 + 0.01, "{:?}", b.bounds);
    let t = scene.find_text(long).unwrap().bounds;
    assert!(t[3] >= 40.0, "wraps: {t:?} in {:?}", b.bounds);
}

#[test]
fn padding_and_gap_math() {
    let boxed = |h: u16| {
        json!({ "type": "core://Box", "height_dp": h, "background": "#000000",
        "content": { "type": "core://Spacer" } })
    };
    let spec = json!({ "type": "core://Column", "padding_h_dp": 16, "padding_v_dp": 12, "spacing_dp": 10,
        "children": [boxed(40), boxed(30), boxed(20)] });
    let scene = SceneBuilder::build_from_spec(&spec, 300.0, 600.0, 1.0).unwrap();
    let ys: Vec<[f32; 4]> = scene.quads().map(|q| q.rect).collect();
    assert_eq!(ys.len(), 3);
    assert_eq!(ys[0][0], 16.0);
    assert_eq!(ys[0][1], 12.0);
    assert_eq!(ys[1][1], 12.0 + 40.0 + 10.0);
    assert_eq!(ys[2][1], 12.0 + 40.0 + 10.0 + 30.0 + 10.0);
    // Padding never pushes a child out of the column.
    for r in &ys {
        assert!(r[0] + r[2] <= 300.0 - 16.0 + 0.01);
    }
}

#[test]
fn semantic_bounds_match_the_quads() {
    let (root, data) = fixture("task_list");
    let scene = build(&root, &data, &BTreeMap::new(), Viewport::PHONE);
    let card = scene.find_node("task_1").unwrap();
    // The card's Box is drawn exactly where its tappable wrapper is.
    let quad = scene
        .quads()
        .find(|q| close(q.rect[1], card.bounds[1]) && close(q.rect[0], card.bounds[0]))
        .expect("the card's quad");
    for i in 0..4 {
        assert!(
            close(quad.rect[i], card.bounds[i]),
            "{:?} vs {:?}",
            quad.rect,
            card.bounds
        );
    }
    assert!(close(card.bounds[0], 16.0) && close(card.bounds[2], 390.0 - 32.0));
}

#[test]
fn list_binds_items_filters_and_counts() {
    let (root, data) = fixture("task_list");
    let all = build(&root, &data, &BTreeMap::new(), Viewport::PHONE);
    for t in [
        "Repaint the Millbrook fence",
        "Call the plumber",
        "Due Nov 12",
        "5 open",
    ] {
        assert!(all.find_text(t).is_some(), "{t}");
    }
    let started = BTreeMap::from([("filter".to_owned(), "started".to_owned())]);
    let some = build(&root, &data, &started, Viewport::PHONE);
    assert!(some.find_node("task_1").is_some() && some.find_node("task_4").is_some());
    assert!(some.find_node("task_2").is_none());
    assert!(some.find_text("2 open").is_some());
    assert!(some.find_node("filter=started").unwrap().selected);
    assert!(!some.find_node("filter=").unwrap().selected);
    let none = BTreeMap::from([("filter".to_owned(), "blocked".to_owned())]);
    let empty = build(&root, &data, &none, Viewport::PHONE);
    assert!(empty.find_text("No tasks").is_some());
    assert!(empty.find_text("Nothing here").is_some());
}

#[test]
fn text_wraps_inside_its_row() {
    let (root, data) = fixture("task_list");
    let scene = build(&root, &data, &BTreeMap::new(), Viewport::PHONE);
    let long = scene
        .find_text("Book the van for the move to Oakridge Lane")
        .unwrap();
    let card = scene.find_node("task_4").unwrap();
    assert!(long.bounds[0] + long.bounds[2] <= card.bounds[0] + card.bounds[2]);
    assert!(
        long.bounds[3] >= 40.0,
        "wrapped onto two lines: {:?}",
        long.bounds
    );
    // Every text run lies inside the frame's width.
    for d in &scene.draws {
        if let Draw::Text(t) = d {
            assert!(
                t.rect[0] >= 0.0 && t.rect[0] + t.rect[2] <= 390.0 + 0.01,
                "{t:?}"
            );
        }
    }
}

#[test]
fn theme_bindings_colour_boxes() {
    let (root, data) = fixture("task_list");
    let scene = build(&root, &data, &BTreeMap::new(), Viewport::PHONE);
    let card = scene.find_node("task_1").unwrap();
    let quad = scene
        .quads()
        .find(|q| close(q.rect[1], card.bounds[1]))
        .unwrap();
    // muted_surface #EEF0F4 with a card_border #E4E4EA border.
    let c = construct_render::style::Rgba(quad.bg_color).to_u8();
    assert_eq!(c, [0xEE, 0xF0, 0xF4, 0xFF]);
    assert_eq!(quad.border_width, 1.0);
}

/// Two columns side by side, each scrolling on its own: a row in a filling
/// box, a filling box and a fixed-width one, each with a scrolling view.
/// The row takes the height the box leaves, each column takes the row's
/// height (whatever the row's alignment), and each view scrolls its own
/// content.
#[test]
fn side_by_side_scrolling_columns_get_the_row_s_height() {
    let long = |n: usize, label: &str| {
        let lines: Vec<_> = (0..n)
            .map(|i| json!({ "type": "core://Text", "value_expr": format!("{label} {i}"), "style": "Body" }))
            .collect();
        json!({ "type": "core://Scroll", "content": { "type": "core://Column", "children": lines } })
    };
    let spec = json!({ "type": "core://Column", "children": [
        { "type": "core://Text", "value_expr": "Groceries", "style": "Title" },
        { "type": "core://Box", "fill": true, "content": {
            "type": "core://Row", "align": "Start", "children": [
                { "type": "core://Box", "fill": true, "content": long(80, "Left") },
                { "type": "core://Box", "width_dp": 150, "content": {
                    "type": "core://Box", "fill": true, "content": long(60, "Right") } }
            ] } }
    ] });
    let root: protocol::UiNode = serde_json::from_value(spec).unwrap();
    let data = protocol::DataBundle::default();
    let empty = BTreeMap::new();
    let laid = SceneBuilder::new()
        .layout(&construct_render::Input {
            root: &root,
            data: &data,
            form: &empty,
            route: &empty,
            layouts: &BTreeMap::new(),
            scroll: &BTreeMap::new(),
            viewport: Viewport::PHONE,
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
        })
        .unwrap();
    let scene = laid.emit(&BTreeMap::new());
    let views: Vec<_> = scene
        .semantics
        .iter()
        .filter(|n| n.node_type == "core://Scroll")
        .collect();
    assert_eq!(views.len(), 2);
    let (left, right) = (views[0], views[1]);
    let title = scene.find_text("Groceries").unwrap();
    let below = title.bounds[1] + title.bounds[3];
    for v in [left, right] {
        assert!(
            close(v.bounds[1], below),
            "starts under the title: {:?}",
            v.bounds
        );
        assert!(
            close(v.bounds[1] + v.bounds[3], Viewport::PHONE.height),
            "reaches the bottom: {:?}",
            v.bounds
        );
        assert!(laid.scroll_max[&v.id] > 0.0, "{} scrolls", v.id);
    }
    assert!(
        close(right.bounds[2], 150.0),
        "the fixed column: {:?}",
        right.bounds
    );
    assert!(close(left.bounds[2], Viewport::PHONE.width - 150.0));

    // Scrolling one moves only its own content.
    let scrolled = laid.emit(&BTreeMap::from([(left.id.clone(), 300.0)]));
    let y = |s: &construct_render::Scene, t: &str| s.find_text(t).unwrap().bounds[1];
    assert!(close(y(&scrolled, "Left 20"), y(&scene, "Left 20") - 300.0));
    assert!(close(y(&scrolled, "Right 0"), y(&scene, "Right 0")));
}
