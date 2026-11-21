//! Decision `screen-sizes`: a column's `max_width_dp`, a list's
//! `min_column_dp` and the window's size in `local://device`.

use construct_render::interact::{Session, size_class};
use construct_render::{SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::{build, close};

/// A list of `n` cards (`card_<i>`) in a query `q`, laid out with `list`'s
/// fields.
fn cards(n: usize, list: Value) -> (UiNode, DataBundle) {
    let mut spec = json!({ "type": "core://List", "query_key": "q",
        "item_template": { "type": "core://Tappable", "tag": "card_$item.id",
            "on_click": { "intent": "action.navigation.back" },
            "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.title" } } });
    for (k, v) in list.as_object().unwrap() {
        spec[k] = v.clone();
    }
    let mut data = json!({ "entities": {}, "queries": { "q": [] } });
    for i in 0..n {
        let uri = format!("x://cards/{i}");
        data["entities"][&uri] = json!({ "id": i.to_string(), "title": format!("Card {i}") });
        data["queries"]["q"]
            .as_array_mut()
            .unwrap()
            .push(json!(uri));
    }
    (
        serde_json::from_value(spec).unwrap(),
        serde_json::from_value(data).unwrap(),
    )
}

fn at(width: f32) -> Viewport {
    Viewport {
        width,
        height: 800.0,
        scale: 1.0,
    }
}

#[test]
fn a_column_with_a_max_width_is_centred_at_that_width() {
    let spec = json!({ "type": "core://Column", "max_width_dp": 600, "children": [
        { "type": "core://Tappable", "tag": "row", "on_click": { "intent": "action.navigation.back" },
          "content": { "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Body", "value_expr": "Wide" } ] } } ] });
    // Wider than the limit: 600 wide, the rest shared on both sides.
    let scene = SceneBuilder::build_from_spec(&spec, 1280.0, 800.0, 1.0).unwrap();
    let row = scene.find_node("row").unwrap().bounds;
    assert!(close(row[2], 600.0), "{row:?}");
    assert!(close(row[0], 340.0), "{row:?}");
    // Narrower: the whole width, as a column without one.
    let scene = SceneBuilder::build_from_spec(&spec, 390.0, 800.0, 1.0).unwrap();
    let row = scene.find_node("row").unwrap().bounds;
    assert!(close(row[0], 0.0) && close(row[2], 390.0), "{row:?}");
}

#[test]
fn automatic_columns_follow_the_width() {
    let (root, data) = cards(7, json!({ "min_column_dp": 280, "spacing_dp": 12 }));
    let form = BTreeMap::new();
    let columns = |width: f32| {
        let scene = build(&root, &data, &form, at(width));
        let first = scene.find_node("card_0").unwrap().bounds;
        let xs: std::collections::BTreeSet<i64> = (0..7)
            .map(|i| scene.find_node(&format!("card_{i}")).unwrap().bounds[0] as i64)
            .collect();
        (xs.len(), first[2])
    };
    // A phone: one column, the whole width.
    let (n, w) = columns(390.0);
    assert_eq!(n, 1);
    assert!(close(w, 390.0), "{w}");
    // A tablet: two (2 × 280 + 12 fits in 820, three don't).
    let (n, w) = columns(820.0);
    assert_eq!(n, 2);
    assert!(close(w, (820.0 - 12.0) / 2.0), "{w}");
    // A desktop: four, sharing the width equally.
    let (n, w) = columns(1280.0);
    assert_eq!(n, 4);
    assert!(close(w, (1280.0 - 3.0 * 12.0) / 4.0), "{w}");
}

#[test]
fn automatic_columns_keep_group_headers_across_the_width() {
    let (root, mut data) = cards(
        4,
        json!({ "min_column_dp": 200,
            "group_by": { "field": "group", "header": { "type": "core://Tappable", "tag": "h_$item.group",
                "on_click": { "intent": "action.navigation.back" },
                "content": { "type": "core://Text", "style": "Title", "value_expr": "$item.group" } } } }),
    );
    for (i, e) in data.entities.values_mut().enumerate() {
        e.set("group", if i < 2 { "A" } else { "B" });
    }
    let scene = build(&root, &data, &BTreeMap::new(), at(1000.0));
    let (ha, hb) = (
        scene.find_node("h_A").unwrap().bounds,
        scene.find_node("h_B").unwrap().bounds,
    );
    assert!(
        close(ha[2], 1000.0) && close(hb[2], 1000.0),
        "{ha:?} {hb:?}"
    );
    // Each group's cards sit side by side below its header, B's below A's.
    let c: Vec<[f32; 4]> = (0..4)
        .map(|i| scene.find_node(&format!("card_{i}")).unwrap().bounds)
        .collect();
    assert!(close(c[0][1], c[1][1]) && c[1][0] > c[0][0]);
    assert!(c[0][1] > ha[1] && hb[1] > c[0][1] && c[2][1] > hb[1]);
}

#[test]
fn the_window_size_is_device_data() {
    assert_eq!(size_class(390.0), "compact");
    assert_eq!(size_class(600.0), "medium");
    assert_eq!(size_class(839.0), "medium");
    assert_eq!(size_class(840.0), "expanded");
    let spec = json!({ "type": "core://Column", "children": [
        { "type": "core://Text", "style": "Body",
          "value_expr": "${local://device#size_class} ${local://device#width_dp}" },
        { "type": "core://Conditional", "value": "${local://device#size_class}", "equals": "expanded",
          "content": { "type": "core://Text", "style": "Body", "value_expr": "Side panel" } } ] });
    let root: UiNode = serde_json::from_value(spec).unwrap();
    let mut s = Session::new("home", root, DataBundle::default(), Viewport::PHONE);
    let scene = s.scene().unwrap();
    assert!(scene.find_text("compact 390").is_some());
    assert!(scene.find_text("Side panel").is_none());
    // A resize changes it on the next frame.
    s.resize(at(1280.0));
    let scene = s.scene().unwrap();
    assert!(scene.find_text("expanded 1280").is_some());
    assert!(scene.find_text("Side panel").is_some());
}

/// The window's size is written into the device data on each frame; a
/// screen that didn't change is not laid out again for it (a window that
/// animates would otherwise hold its slides back forever).
#[test]
fn the_window_size_doesnt_lay_a_still_screen_out_again() {
    let spec = json!({ "type": "core://Text", "style": "Body",
        "value_expr": "${local://device#size_class}" });
    let root: UiNode = serde_json::from_value(spec).unwrap();
    let mut s = Session::new("home", root, DataBundle::default(), Viewport::PHONE);
    s.animate = true;
    s.scene().unwrap();
    let _ = s.take_work();
    let built = s.scenes_built();
    for _ in 0..3 {
        s.scene().unwrap();
    }
    assert_eq!(s.take_work().laid_out, 0);
    assert_eq!(s.scenes_built(), built, "nothing changed: no new scene");
}

/// `headers_only` draws one header per distinct value of the field, wherever
/// its items come in the list (docs/layouts.md), so a settings list of apps
/// names each app once even when its channels aren't next to each other.
#[test]
fn headers_only_gives_one_header_per_distinct_value() {
    let (root, mut data) = cards(
        5,
        json!({ "group_by": { "field": "group", "headers_only": true,
            "header": { "type": "core://Text", "style": "Label", "value_expr": "Group $item.group" } } }),
    );
    for (i, e) in data.entities.values_mut().enumerate() {
        e.set("group", ["A", "B", "A", "C", "B"][i]);
    }
    let scene = build(&root, &data, &BTreeMap::new(), at(390.0));
    let headers: Vec<&str> = scene
        .semantics
        .iter()
        .filter_map(|n| n.text_content.as_deref())
        .filter(|t| t.starts_with("Group "))
        .collect();
    assert_eq!(headers, ["Group A", "Group B", "Group C"]);
    assert!(
        scene.find_text("Card 0").is_none(),
        "headers only, no items"
    );
}
