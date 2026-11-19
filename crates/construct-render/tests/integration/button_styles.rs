//! `core://Button` `style` and `size`: every style of a size is drawn at one
//! height and corner (decision `one-button-node`).

use std::collections::BTreeMap;

use construct_render::Viewport;
use construct_render::scene::Scene;
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};

use crate::common::{build, close};

fn scene(children: Value) -> Scene {
    let root: UiNode = serde_json::from_value(json!({
        "type": "core://Row", "spacing_dp": 8, "children": children
    }))
    .unwrap();
    build(
        &root,
        &DataBundle::default(),
        &BTreeMap::new(),
        Viewport::PHONE,
    )
}

fn button(label: &str, extra: Value) -> Value {
    let mut b = json!({"type": "core://Button", "label": label,
        "on_click": {"intent": "action.navigation.back"}});
    b.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    b
}

fn bounds(s: &Scene, label: &str) -> [f32; 4] {
    s.semantics
        .iter()
        .find(|n| n.text_content.as_deref() == Some(label))
        .unwrap_or_else(|| panic!("no {label}"))
        .bounds
}

#[test]
fn every_style_of_a_size_has_one_height() {
    let s = scene(json!([
        button("Open in Claude", json!({})),
        button("Stop", json!({"style": "Secondary"})),
        button("Tonal", json!({"style": "Tonal"})),
        button("Text", json!({"style": "Text"})),
    ]));
    for label in ["Open in Claude", "Stop", "Tonal", "Text"] {
        let b = bounds(&s, label);
        assert!(close(b[3], 40.0), "{label} is {b:?}");
        assert!(close(b[1], bounds(&s, "Stop")[1]), "{label} is {b:?}");
    }
    let small = scene(json!([
        button("Add", json!({"size": "Small"})),
        button("Skip", json!({"size": "Small", "style": "Secondary"})),
    ]));
    for label in ["Add", "Skip"] {
        assert!(close(bounds(&small, label)[3], 32.0));
    }
}

#[test]
fn styles_differ_only_in_paint() {
    // The quads of a primary and a secondary button: the same size and
    // corner, a fill on one and an outline on the other.
    let s = scene(json!([
        button("Go", json!({})),
        button("Stop", json!({"style": "Secondary"})),
    ]));
    let at = |label: &str| {
        let b = bounds(&s, label);
        s.quads()
            .find(|q| q.rect == b)
            .copied()
            .unwrap_or_else(|| panic!("no quad for {label}"))
    };
    let (go, stop) = (at("Go"), at("Stop"));
    assert_eq!(go.radii, stop.radii);
    assert!(close(go.radii[0], 20.0));
    assert_eq!(go.border_width, 0.0);
    assert!(stop.border_width > 0.0);
    assert_ne!(go.bg_color, stop.bg_color);
}
