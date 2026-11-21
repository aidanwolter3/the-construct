//! Client hardening (#459): a tree nested far deeper than any page is
//! refused, and one huge text is cut.

use construct_render::layout::{MAX_BUILD_DEPTH, MAX_TEXT_CHARS};
use construct_render::{Input, SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::build;

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

#[test]
fn a_tree_deeper_than_the_cap_is_cut_not_built() {
    let mut deep = node(json!({ "type": "core://Text", "style": "Body", "value_expr": "bottom" }));
    for _ in 0..(MAX_BUILD_DEPTH * 3) {
        let mut col = node(json!({ "type": "core://Column", "children": [] }));
        if let UiNode::Column { children, .. } = &mut col {
            children.push(deep);
        }
        deep = col;
    }
    assert_eq!(deep.depth(), MAX_BUILD_DEPTH * 3 + 1);
    let built = SceneBuilder::new().build(&Input {
        root: &deep,
        data: &DataBundle::default(),
        form: &BTreeMap::new(),
        route: &BTreeMap::new(),
        layouts: &BTreeMap::new(),
        scroll: &BTreeMap::new(),
        viewport: Viewport::PHONE,
        dark: false,
        item: None,
        insets: [0.0; 4],
        focused: None,
        panels: &construct_render::NO_PANELS,
    });
    assert!(built.is_err());
}

#[test]
fn text_beyond_the_cap_is_cut() {
    let long = "x".repeat(MAX_TEXT_CHARS * 3);
    let root = node(json!({ "type": "core://Text", "style": "Body", "value_expr": long }));
    let scene = build(
        &root,
        &DataBundle::default(),
        &BTreeMap::new(),
        Viewport::PHONE,
    );
    let total: usize = scene.texts().map(|t| t.text.chars().count()).sum();
    assert!(total > 0 && total <= MAX_TEXT_CHARS + 1, "{total}");
}
