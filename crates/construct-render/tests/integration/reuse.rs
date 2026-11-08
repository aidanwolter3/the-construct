//! Laying a frame out again over the one before (`SceneBuilder::relayout`):
//! list items whose data didn't change are taken over, the rest are built,
//! and the frame is the same as one laid out from scratch.

use std::collections::BTreeMap;

use construct_render::layout::{Laid, Reuse};
use construct_render::{Input, SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};

const ROWS: usize = 150;

/// A staples page: five lists over one collection, each filtered to a
/// storage place, every row hidden while it has no count, and a row that
/// shows a muted state while `added` is set (and reads a setting).
fn page() -> UiNode {
    let row = json!({ "type": "core://Conditional", "value": "$item.count", "content": {
        "type": "core://Column", "children": [
            { "type": "core://Conditional", "value": "$item.added", "equals": "0", "content": {
                "type": "core://Tappable", "content_description": "Add $item.name",
                "on_click": { "intent": "action.local.set", "entity_uri": "$item.uri",
                    "field": "added", "value_expr": "1" },
                "content": { "type": "core://Row", "children": [
                    { "type": "core://Text", "value_expr": "$item.name", "style": "Body" },
                    { "type": "core://Spacer" },
                    { "type": "core://Text", "value_expr": "$item.count", "style": "Caption" } ] } } },
            { "type": "core://Conditional", "value": "$item.added", "content": {
                "type": "core://Text", "style": "Secondary",
                "value_expr": "$item.name ${x://settings/s#added_label}" } }
        ] } });
    let mut children = Vec::new();
    for place in ["Produce", "Fridge", "Freezer", "Pantry", "Spices"] {
        children.push(json!({ "type": "core://Text", "value_expr": place, "style": "Title" }));
        children.push(json!({ "type": "core://List", "query_key": "rows", "item_template": row,
            "where": [{ "field": "deleted", "equals": "false" }, { "field": "place", "equals": place }] }));
    }
    serde_json::from_value(json!({ "type": "core://Scroll", "content": {
        "type": "core://Column", "spacing_dp": 4, "children": children } }))
    .unwrap()
}

fn data() -> DataBundle {
    let places = ["Produce", "Fridge", "Freezer", "Pantry", "Spices"];
    let mut entities = serde_json::Map::new();
    let mut uris = Vec::new();
    for i in 0..ROWS {
        let uri = format!("x://rows/r{i}");
        entities.insert(
            uri.clone(),
            json!({ "name": format!("Item {i}"), "place": places[i % 5], "deleted": false,
                "count": i % 4, "added": 0 }),
        );
        uris.push(uri);
    }
    entities.insert(
        "x://settings/s".into(),
        json!({ "added_label": "in groceries" }),
    );
    serde_json::from_value(json!({ "entities": entities, "queries": { "rows": uris } })).unwrap()
}

fn set(data: &mut DataBundle, uri: &str, field: &str, v: Value) {
    data.entities.get_mut(uri).unwrap().set(field, v);
}

fn lay(
    b: &mut SceneBuilder,
    root: &UiNode,
    data: &DataBundle,
    form: &BTreeMap<String, String>,
    prev: Option<Laid>,
) -> Laid {
    let empty = BTreeMap::new();
    let layouts = BTreeMap::new();
    b.relayout(
        &Input {
            root,
            data,
            form,
            route: &empty,
            layouts: &layouts,
            scroll: &BTreeMap::new(),
            viewport: Viewport::PHONE,
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
        },
        prev,
    )
    .unwrap()
}

/// The frame is the one a layout from scratch draws, nodes and paint alike.
fn same_as_fresh(laid: &Laid, root: &UiNode, data: &DataBundle, form: &BTreeMap<String, String>) {
    let fresh = lay(&mut SceneBuilder::new(), root, data, form, None);
    for y in [0.0, 1200.0] {
        let scroll = BTreeMap::from([("root".to_owned(), y)]);
        let (a, b) = (laid.emit(&scroll), fresh.emit(&scroll));
        assert_eq!(a.semantics, b.semantics, "semantics at scroll {y}");
        assert_eq!(a.draws, b.draws, "draws at scroll {y}");
    }
}

#[test]
fn a_changed_row_is_the_only_one_built_again() {
    let root = page();
    let mut data = data();
    let form = BTreeMap::new();
    let mut b = SceneBuilder::new();
    let laid = lay(&mut b, &root, &data, &form, None);
    assert_eq!(
        laid.reuse,
        Reuse {
            built: ROWS,
            reused: 0
        }
    );

    // Nothing changed: every row is taken over.
    let laid = lay(&mut b, &root, &data, &form, Some(laid));
    assert_eq!(
        laid.reuse,
        Reuse {
            built: 0,
            reused: ROWS
        }
    );
    same_as_fresh(&laid, &root, &data, &form);

    // A tap marks one row: it alone is built again, and shows it.
    set(&mut data, "x://rows/r5", "added", json!(1));
    let laid = lay(&mut b, &root, &data, &form, Some(laid));
    assert_eq!(
        laid.reuse,
        Reuse {
            built: 1,
            reused: ROWS - 1
        }
    );
    same_as_fresh(&laid, &root, &data, &form);
    let scene = laid.emit(&BTreeMap::new());
    assert!(
        scene
            .semantics
            .iter()
            .any(|n| n.text_content.as_deref() == Some("Item 5 in groceries"))
    );

    // A row the list filters out now (and the rows after it move up).
    set(&mut data, "x://rows/r1", "deleted", json!(true));
    let laid = lay(&mut b, &root, &data, &form, Some(laid));
    same_as_fresh(&laid, &root, &data, &form);
    assert!(laid.reuse.reused > 0);

    // An entity the rows read besides their own: those that read it are
    // built again (only the marked row shows the setting).
    set(
        &mut data,
        "x://settings/s",
        "added_label",
        json!("on the list"),
    );
    let laid = lay(&mut b, &root, &data, &form, Some(laid));
    assert_eq!(laid.reuse.built, 1);
    same_as_fresh(&laid, &root, &data, &form);

    // Something every part may read (the form): nothing is taken over.
    let form = BTreeMap::from([("q".to_owned(), "x".to_owned())]);
    let laid = lay(&mut b, &root, &data, &form, Some(laid));
    assert_eq!(laid.reuse.reused, 0);
    same_as_fresh(&laid, &root, &data, &form);
}
