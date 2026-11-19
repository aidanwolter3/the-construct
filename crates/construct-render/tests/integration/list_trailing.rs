//! Feature `list_trailing`: a list's `trailing` cell, drawn after its items
//! as the next cell (in a grid, after the last item in its row).

use construct_render::Viewport;
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::{build, close};

/// A list of `n` tiles (`tile_<i>`) in a query `q`, laid out with `list`'s
/// fields, ending with an add tile (`add`).
fn tiles(n: usize, list: Value) -> (UiNode, DataBundle) {
    let tile = |tag: &str, text: &str| {
        json!({ "type": "core://Tappable", "tag": tag,
            "on_click": { "intent": "action.navigation.back" },
            "content": { "type": "core://Box", "padding_dp": 12,
                "content": { "type": "core://Text", "style": "Body", "value_expr": text } } })
    };
    let mut spec = json!({ "type": "core://List", "query_key": "q",
        "item_template": tile("tile_$item.id", "$item.title"),
        "trailing": tile("add", "+") });
    for (k, v) in list.as_object().unwrap() {
        spec[k] = v.clone();
    }
    let mut data = json!({ "entities": {}, "queries": { "q": [] } });
    for i in 0..n {
        let uri = format!("x://tiles/{i}");
        data["entities"][&uri] = json!({ "id": i.to_string(), "title": format!("Tile {i}") });
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

fn phone() -> Viewport {
    Viewport {
        width: 390.0,
        height: 800.0,
        scale: 1.0,
    }
}

/// Each tagged node's bounds, `[x, y, w, h]`.
fn bounds(root: &UiNode, data: &DataBundle, tags: &[&str]) -> Vec<Option<[f32; 4]>> {
    let scene = build(root, data, &BTreeMap::new(), phone());
    tags.iter()
        .map(|t| scene.find_node(t).map(|n| n.bounds))
        .collect()
}

#[test]
fn a_grid_s_trailing_cell_follows_the_last_item_in_its_row() {
    let (root, data) = tiles(4, json!({ "columns": 3, "spacing_dp": 8 }));
    let b = bounds(&root, &data, &["tile_1", "tile_3", "add"]);
    let (t1, t3, add) = (b[0].unwrap(), b[1].unwrap(), b[2].unwrap());
    // The second row: tile 3 in the first column, the add tile in the second.
    assert!(close(add[1], t3[1]), "{add:?} {t3:?}");
    assert!(close(add[0], t1[0]), "{add:?} {t1:?}");
    assert!(close(add[2], t1[2]), "as wide as a tile: {add:?} {t1:?}");
}

#[test]
fn a_full_row_puts_the_trailing_cell_on_the_next() {
    let (root, data) = tiles(3, json!({ "columns": 3 }));
    let b = bounds(&root, &data, &["tile_0", "tile_2", "add"]);
    let (t0, t2, add) = (b[0].unwrap(), b[1].unwrap(), b[2].unwrap());
    assert!(
        add[1] >= t2[1] + t2[3] - 0.5,
        "below the row: {add:?} {t2:?}"
    );
    assert!(
        close(add[0], t0[0]) && close(add[2], t0[2]),
        "{add:?} {t0:?}"
    );
}

#[test]
fn automatic_columns_take_the_trailing_cell_as_the_next() {
    // 390 wide, columns at least 120: three of them.
    let (root, data) = tiles(4, json!({ "min_column_dp": 120, "spacing_dp": 8 }));
    let b = bounds(&root, &data, &["tile_1", "tile_3", "add"]);
    let (t1, t3, add) = (b[0].unwrap(), b[1].unwrap(), b[2].unwrap());
    assert!(close(add[1], t3[1]), "{add:?} {t3:?}");
    assert!(
        close(add[0], t1[0]) && close(add[2], t1[2]),
        "{add:?} {t1:?}"
    );
}

#[test]
fn a_plain_list_ends_with_its_trailing_cell() {
    let (root, data) = tiles(2, json!({}));
    let b = bounds(&root, &data, &["tile_1", "add"]);
    let (t1, add) = (b[0].unwrap(), b[1].unwrap());
    assert!(add[1] >= t1[1] + t1[3] - 0.5, "{add:?} {t1:?}");
    assert!(close(add[2], t1[2]), "the list's width: {add:?} {t1:?}");
}

#[test]
fn an_empty_list_still_draws_its_trailing_cell() {
    let (root, data) = tiles(0, json!({ "columns": 3, "empty_text": "No tiles yet" }));
    let scene = build(&root, &data, &BTreeMap::new(), phone());
    let add = scene.find_node("add").expect("drawn").bounds;
    assert!(add[2] < 390.0 / 2.0, "one column of three: {add:?}");
    let s = format!("{scene:?}");
    assert!(s.contains("No tiles yet"));
}

#[test]
fn a_list_still_loading_draws_no_trailing_cell() {
    let (root, _) = tiles(2, json!({ "columns": 3 }));
    let none: DataBundle = serde_json::from_value(json!({ "entities": {} })).unwrap();
    assert!(bounds(&root, &none, &["add"])[0].is_none());
}
