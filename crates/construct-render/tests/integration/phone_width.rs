//! A layout that doesn't adapt to the window draws at most phone width,
//! centred; one that adapts uses all the width (decision
//! `phone-width-layouts`).

use construct_render::{
    Input, NO_PANELS, PHONE_WIDTH_DP, Scene, SceneBuilder, Viewport, adapts_to_window,
};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::close;

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

/// A column holding one tagged tappable row.
fn phone(tag: &str) -> Value {
    json!({ "type": "core://Column", "children": [
        { "type": "core://Tappable", "tag": tag,
          "on_click": { "intent": "action.navigation.back" },
          "content": { "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": tag } ] } } ] })
}

fn draw(root: &UiNode, layouts: &BTreeMap<String, UiNode>, width: f32) -> Scene {
    SceneBuilder::new()
        .build(&Input {
            root,
            data: &DataBundle::default(),
            form: &BTreeMap::new(),
            route: &BTreeMap::new(),
            layouts,
            scroll: &BTreeMap::new(),
            viewport: Viewport {
                width,
                height: 800.0,
                scale: 1.0,
            },
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
            panels: &NO_PANELS,
        })
        .unwrap()
}

fn bounds(s: &Scene, tag: &str) -> [f32; 4] {
    s.find_node(tag).unwrap().bounds
}

#[test]
fn a_phone_layout_is_phone_wide_and_centred_on_a_wide_window() {
    let root = node(phone("row"));
    let b = bounds(&draw(&root, &BTreeMap::new(), 1280.0), "row");
    assert!(close(b[2], PHONE_WIDTH_DP), "{b:?}");
    assert!(close(b[0], (1280.0 - PHONE_WIDTH_DP) / 2.0), "{b:?}");
    // A phone's window is narrower than the cap: nothing changes there.
    let b = bounds(&draw(&root, &BTreeMap::new(), 390.0), "row");
    assert!(close(b[0], 0.0) && close(b[2], 390.0), "{b:?}");
}

#[test]
fn a_layout_that_tests_the_window_uses_all_of_it() {
    let root = node(json!({ "type": "core://Column", "children": [
        { "type": "core://Conditional", "value": "${local://device#size_class}",
          "equals": "expanded", "content": { "type": "core://Text", "style": "Body", "value_expr": "Wide" } },
        phone("row") ] }));
    assert!(adapts_to_window(&root));
    let b = bounds(&draw(&root, &BTreeMap::new(), 1280.0), "row");
    assert!(close(b[0], 0.0) && close(b[2], 1280.0), "{b:?}");
}

#[test]
fn what_counts_as_adapting() {
    assert!(!adapts_to_window(&node(phone("row"))));
    assert!(adapts_to_window(&node(
        json!({ "type": "core://Column", "max_width_dp": 900, "children": [] })
    )));
    assert!(adapts_to_window(&node(
        json!({ "type": "core://Conditional",
        "value": "${local://device#platform}", "equals": "web",
        "content": { "type": "core://Text", "style": "Body", "value_expr": "Web" } })
    )));
    // An embed answers for itself: embedding a layout isn't adapting.
    assert!(!adapts_to_window(&node(
        json!({ "type": "core://Layout", "layout": "meals" })
    )));
}

#[test]
fn an_embedded_phone_layout_is_phone_wide_in_a_wide_home() {
    // Home adapts (it tests the window), so it takes the whole width; the
    // layout it embeds was written for a phone and is capped, centred in
    // the room the embed gives it.
    let home = node(json!({ "type": "core://Column", "children": [
        { "type": "core://Conditional", "value": "${local://device#size_class}",
          "equals": "expanded", "content": { "type": "core://Text", "style": "Body", "value_expr": "Wide" } },
        phone("home-row"),
        { "type": "core://Layout", "layout": "meals" } ] }));
    let layouts = BTreeMap::from([("meals".to_owned(), node(phone("meal")))]);
    let s = draw(&home, &layouts, 1280.0);
    let h = bounds(&s, "home-row");
    assert!(close(h[2], 1280.0), "{h:?}");
    let m = bounds(&s, "meal");
    assert!(close(m[2], PHONE_WIDTH_DP), "{m:?}");
    assert!(close(m[0], (1280.0 - PHONE_WIDTH_DP) / 2.0), "{m:?}");
}
