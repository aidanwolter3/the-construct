//! Feature `bars_rows` (decision `device-day-grid`): a `core://Bars` with
//! `row` is a grid, a row per `row` value and a cell per bar, the cell's
//! items side by side, each `value` wide out of `scale_min`.

use construct_render::Viewport;
use construct_render::scene::Draw;
use construct_render::style::Rgba;
use protocol::{DataBundle, UiNode};
use serde_json::json;
use std::collections::BTreeMap;

use crate::common::build;

const BLUE: &str = "#2A78D6"; // chart_1 (light)
const ORANGE: &str = "#D95926"; // chart_2

/// Two devices over two days: `a` updated halfway through day 2, `b`
/// wasn't seen on day 1.
fn grid() -> (UiNode, DataBundle) {
    let spec = json!({ "type": "core://Bars", "query_key": "q", "row": "dev",
        "row_label": "$item.name", "bar": "day", "label": "$item.day", "value": "hours",
        "scale_min": 24, "color": "${local://theme#chart_$item.series}", "hatched": "hatched",
        "field": "cell" });
    let items = [
        ("a", "1", "2", 24.0, false),
        ("a", "2", "2", 12.0, false),
        ("a", "2", "1", 12.0, false),
        ("b", "1", "other", 24.0, true),
        ("b", "2", "1", 24.0, false),
    ];
    let mut data = json!({ "entities": {}, "queries": { "q": [] } });
    for (i, (dev, day, series, hours, hatched)) in items.iter().enumerate() {
        let uri = format!("x://cells/{i}");
        data["entities"][&uri] = json!({ "dev": dev, "name": format!("Device {dev}"),
            "day": day, "series": series, "hours": hours, "hatched": hatched });
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

/// The filled quads in `colors`: (colour, x, y, width).
fn fills(root: &UiNode, data: &DataBundle) -> Vec<(&'static str, f32, f32, f32)> {
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
    let colors = [BLUE, ORANGE];
    scene
        .draws
        .iter()
        .filter_map(|d| match d {
            Draw::Quad(q) => colors
                .iter()
                .find(|c| Rgba::hex(c).unwrap().0 == q.bg_color)
                .map(|c| (*c, q.rect[0], q.rect[1], q.rect[2])),
            _ => None,
        })
        .collect()
}

#[test]
fn a_cell_splits_its_width_by_value_and_rows_line_up() {
    let (root, data) = grid();
    let f = fills(&root, &data);
    // a: day 1 orange, day 2 orange then blue; b: day 2 blue (day 1 an
    // outline, no fill).
    let names: Vec<&str> = f.iter().map(|x| x.0).collect();
    assert_eq!(names, [ORANGE, ORANGE, BLUE, BLUE], "{f:?}");
    let (whole, half_a, half_b, b_day2) = (f[0], f[1], f[2], f[3]);
    // The halves share the day: each about half of a whole day's width,
    // the orange one first.
    assert!(half_a.1 < half_b.1, "{f:?}");
    assert!((half_a.3 + half_b.3 + 1.0 - whole.3).abs() < 1.5, "{f:?}");
    assert!((half_a.3 - half_b.3).abs() <= 1.5, "{f:?}");
    // Day 2 starts at the same x in both rows, the second row below.
    assert!((half_a.1 - b_day2.1).abs() < 0.5, "{f:?}");
    assert!(b_day2.2 > half_a.2, "{f:?}");
}

#[test]
fn a_tap_names_the_row_and_the_bar_and_rows_need_the_feature() {
    let (root, data) = grid();
    let scene = build(
        &root,
        &data,
        &BTreeMap::new(),
        Viewport {
            width: 390.0,
            height: 400.0,
            scale: 1.0,
        },
    );
    let cell = scene
        .semantics
        .iter()
        .find(|s| s.text_content.as_deref() == Some("Device a 2"))
        .expect("a cell named by its row and bar");
    assert_eq!(
        serde_json::to_value(cell.tap.as_ref().unwrap()).unwrap()["value"],
        "a/2"
    );
    let features = protocol::required_features(&root);
    assert!(
        features.contains(&protocol::feature::BARS_ROWS),
        "{features:?}"
    );
}
