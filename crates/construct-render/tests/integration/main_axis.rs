//! Main-axis placement (`justify` on `core://Row` and `core://Column`) and
//! inline `core://Layout` embeds that size to their content.

use construct_render::{Input, Scene, SceneBuilder, Viewport};
use protocol::UiNode;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::close;

fn tile(tag: &str) -> Value {
    json!({ "type": "core://Tappable", "tag": tag,
        "on_click": { "intent": "action.navigation.back" },
        "content": { "type": "core://Box", "width_dp": 80, "height_dp": 84, "background": "#EEF0F4",
            "content": { "type": "core://Text", "value_expr": tag, "style": "Caption" } } })
}

fn row(justify: &str, width: f32) -> Scene {
    let spec = json!({ "type": "core://Row", "justify": justify, "spacing_dp": 6, "padding_h_dp": 16,
        "children": [tile("a"), tile("b"), tile("c"), tile("d")] });
    SceneBuilder::build_from_spec(&spec, width, 400.0, 1.0).unwrap()
}

fn x(s: &Scene, tag: &str) -> (f32, f32) {
    let b = s.find_node(tag).unwrap().bounds;
    (b[0], b[0] + b[2])
}

#[test]
fn a_centred_row_keeps_its_tiles_in_the_middle_at_any_width() {
    for width in [380.0, 390.0, 800.0] {
        let s = row("center", width);
        let (left, _) = x(&s, "a");
        let (_, right) = x(&s, "d");
        assert!(close(left, width - right), "{width}: {left} {right}");
        assert!(close(x(&s, "b").0, x(&s, "a").1 + 6.0), "the spacing stays");
    }
}

#[test]
fn a_row_spreads_its_children_evenly_or_to_its_ends() {
    let s = row("space_between", 600.0);
    assert!(close(x(&s, "a").0, 16.0));
    assert!(close(x(&s, "d").1, 600.0 - 16.0));
    let s = row("space_evenly", 600.0);
    // Equal room before, between (with the spacing) and after: what the
    // padding, the tiles and the spacing leave, in five.
    let gap = (600.0 - 32.0 - 320.0 - 18.0) / 5.0;
    assert!(close(x(&s, "a").0, 16.0 + gap), "{:?}", x(&s, "a"));
    assert!(close(x(&s, "b").0 - x(&s, "a").1, gap + 6.0));
    assert!(close(600.0 - 16.0 - x(&s, "d").1, gap));
    let s = row("end", 600.0);
    assert!(close(x(&s, "d").1, 600.0 - 16.0));
}

#[test]
fn a_row_too_narrow_for_its_tiles_starts_them_at_its_padding() {
    // Four 80 dp tiles and their gaps don't fit 360 dp: centred, they would
    // hang off both edges; they start at the padding instead.
    for justify in ["center", "end", "space_evenly"] {
        let s = row(justify, 360.0);
        assert!(close(x(&s, "a").0, 16.0), "{justify}: {:?}", x(&s, "a"));
    }
}

#[test]
fn a_column_places_its_children_down_a_fixed_height() {
    let spec = json!({ "type": "core://Box", "height_dp": 300, "content": {
        "type": "core://Column", "justify": "end", "children": [tile("a")] } });
    let s = SceneBuilder::build_from_spec(&spec, 390.0, 844.0, 1.0).unwrap();
    let b = s.find_node("a").unwrap().bounds;
    assert!(close(b[1] + b[3], 300.0), "{b:?}");
}

/// A page embedding `inner` inline in its scrolling column, under a header
/// and between two texts (as a home with its priorities), its scroll
/// `scrolled` points down.
fn embedded(inner: Value, scrolled: f32) -> Scene {
    let root: UiNode = serde_json::from_value(json!({ "type": "core://Page", "id": "home", "root": {
        "type": "core://Gesture", "on_swipe_down": { "intent": "action.navigation.back" },
        "content": { "type": "core://Box", "fill": true, "content": { "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Home", "style": "Headline" },
            { "type": "core://Divider" },
            { "type": "core://Scroll", "content": { "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Before", "style": "Body" },
                { "type": "core://Layout", "layout": "inner" },
                { "type": "core://Text", "value_expr": "After", "style": "Body" } ] } } ] } } } }))
    .unwrap();
    let layouts = BTreeMap::from([("inner".to_owned(), serde_json::from_value(inner).unwrap())]);
    SceneBuilder::new()
        .build(&Input {
            root: &root,
            data: &Default::default(),
            form: &BTreeMap::new(),
            route: &BTreeMap::new(),
            layouts: &layouts,
            scroll: &BTreeMap::from([("root/0/0/0/2".to_owned(), scrolled)]),
            // Shorter than the page, as a phone under a longer home.
            viewport: Viewport {
                width: 390.0,
                height: 400.0,
                scale: 1.0,
            },
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
        })
        .unwrap()
}

fn card(tag: &str) -> Value {
    json!({ "type": "core://Tappable", "tag": tag, "on_click": { "intent": "action.navigation.back" },
        "content": { "type": "core://Box", "padding_h_dp": 16, "padding_v_dp": 14, "background": "#E3F1E0",
            "content": { "type": "core://Row", "spacing_dp": 12, "children": [
                { "type": "core://Text", "value_expr": "1", "style": "Headline" },
                { "type": "core://Column", "spacing_dp": 2, "children": [
                    { "type": "core://Text", "value_expr": tag, "style": "Title" },
                    { "type": "core://Text", "value_expr": "A line under the title", "style": "Secondary" } ] },
                { "type": "core://Spacer" },
                { "type": "core://Text", "value_expr": "›", "style": "Headline" } ] } } })
}

#[test]
fn an_inline_layout_in_a_scroll_is_as_tall_as_its_content() {
    let cards: Vec<Value> = ["a", "b", "c", "d", "e"].into_iter().map(card).collect();
    let inner = json!({ "type": "core://Page", "id": "inner", "root": {
        "type": "core://Column", "padding_h_dp": 16, "spacing_dp": 10, "children": cards } });
    let top = embedded(inner.clone(), 0.0);
    assert!(top.find_text("After").is_none(), "below the screen");
    // Scrolled to the end, the last card is whole and "After" right under
    // it: the embed held all five rather than the room the screen had left.
    let s = embedded(inner, 1000.0);
    let a = top.find_node("a").unwrap().bounds;
    let e = s.find_node("e").unwrap().bounds;
    assert!(close(e[3], a[3]), "{a:?} {e:?}");
    let after = s.find_text("After").unwrap().bounds;
    assert!(close(after[1], e[1] + e[3]), "{e:?} {after:?}");
    // The scroll stops at its content's end: "After" at the bottom.
    assert!(close(after[1] + after[3], 400.0), "{after:?}");
}

#[test]
fn an_inline_layout_in_a_short_scroll_leaves_no_gap() {
    let s = embedded(
        json!({ "type": "core://Page", "id": "inner", "root": {
        "type": "core://Column", "children": [card("a")] } }),
        0.0,
    );
    // Room to spare: the embed stays as tall as its card, and "After"
    // follows it, on any screen height.
    let a = s.find_node("a").unwrap().bounds;
    let after = s.find_text("After").unwrap().bounds;
    assert!(close(after[1], a[1] + a[3]), "{a:?} {after:?}");
}
