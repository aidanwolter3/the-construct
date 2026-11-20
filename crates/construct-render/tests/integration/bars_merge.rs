//! Feature `bars_merge` and chart colours (decision `chart-colours`): a
//! bar's neighbouring segments with the same `merge` value draw as one,
//! coloured from the theme's `chart_<series>`.

use construct_render::Viewport;
use construct_render::scene::Draw;
use construct_render::style::Rgba;
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::common::build;

/// One bar of `items` (`(build, series, hatched)`), with `merge` if given.
fn chart(items: &[(&str, &str, bool)], merge: Option<&str>) -> (UiNode, DataBundle) {
    let mut spec = json!({ "type": "core://Bars", "query_key": "q", "bar": "day",
        "color": "${local://theme#chart_$item.series}", "hatched": "hatched", "scale_min": 4 });
    if let Some(m) = merge {
        spec["merge"] = json!(m);
    }
    let mut data = json!({ "entities": {}, "queries": { "q": [] } });
    for (i, (build, series, hatched)) in items.iter().enumerate() {
        let uri = format!("x://days/{i}");
        data["entities"][&uri] =
            json!({ "day": "d", "build": build, "series": series, "hatched": hatched });
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

/// The filled segments' colours and heights, top to bottom.
fn segments(root: &UiNode, data: &DataBundle, color: &[&str]) -> Vec<(String, f32)> {
    let wanted: Vec<[f32; 4]> = color.iter().map(|c| Rgba::hex(c).unwrap().0).collect();
    let scene = build(
        root,
        data,
        &BTreeMap::new(),
        Viewport {
            width: 390.0,
            height: 400.0,
            scale: 1.0,
        },
    );
    scene
        .draws
        .iter()
        .filter_map(|d| match d {
            Draw::Quad(q) => wanted
                .iter()
                .position(|w| *w == q.bg_color)
                .map(|i| (color[i].to_owned(), q.rect[3])),
            _ => None,
        })
        .collect()
}

const BLUE: &str = "#2A78D6"; // chart_1 (light)
const ORANGE: &str = "#D95926"; // chart_2

fn items() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("old", "2", false),
        ("old", "2", false),
        ("new", "1", false),
    ]
}

#[test]
fn segments_with_the_same_merge_value_draw_as_one() {
    let (root, data) = chart(&items(), None);
    let apart = segments(&root, &data, &[BLUE, ORANGE]);
    assert_eq!(apart.len(), 3, "{apart:?}");
    let (root, data) = chart(&items(), Some("build"));
    let merged = segments(&root, &data, &[BLUE, ORANGE]);
    assert_eq!(merged.len(), 2, "{merged:?}");
    assert_eq!(merged[0].0, BLUE);
    assert_eq!(merged[1].0, ORANGE);
    // As tall as the two it replaces with the gap between them.
    let one = apart[0].1;
    assert!(
        (merged[1].1 - (2.0 * one + 2.0)).abs() < 0.01,
        "{merged:?} {one}"
    );
}

#[test]
fn hatched_and_solid_segments_stay_apart() {
    let mut list = items();
    list[1].2 = true;
    let (root, data) = chart(&list, Some("build"));
    // The hatched one is an outline, not a fill: one orange fill is left.
    let fills = segments(&root, &data, &[BLUE, ORANGE]);
    assert_eq!(fills.len(), 2, "{fills:?}");
}

#[test]
fn an_unmerged_layout_parses_and_a_merge_needs_the_feature() {
    let (root, _) = chart(&items(), Some("build"));
    let features = protocol::required_features(&root);
    assert!(
        features.contains(&protocol::feature::BARS_MERGE),
        "{features:?}"
    );
    let plain: Value = serde_json::to_value(chart(&items(), None).0).unwrap();
    assert!(plain.get("merge").is_none());
    let (root, _) = chart(&items(), None);
    assert!(!protocol::required_features(&root).contains(&protocol::feature::BARS_MERGE));
}
