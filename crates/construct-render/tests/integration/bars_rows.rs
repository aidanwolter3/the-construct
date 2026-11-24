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

#[test]
fn a_mark_is_a_dashed_line_at_its_moment_down_every_row() {
    let (mut root, mut data) = grid();
    if let UiNode::Bars { marks, .. } = &mut root {
        *marks = Some(protocol::BarsMarks {
            query_key: "m".into(),
            at: "at".into(),
            color: "#00FF00".into(),
        });
    }
    // Noon on day 2.
    data.entities.insert(
        "x://m/0".into(),
        serde_json::from_value(json!({"day": "2", "at": 12.0})).unwrap(),
    );
    data.queries.insert("m".into(), vec!["x://m/0".into()]);
    let day2 = fills(&root, &data)[3];
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
    let green = Rgba::hex("#00FF00").unwrap().0;
    let dashes: Vec<[f32; 4]> = scene
        .draws
        .iter()
        .filter_map(|d| match d {
            Draw::Quad(q) if q.bg_color == green => Some(q.rect),
            _ => None,
        })
        .collect();
    // Dashes in both rows, all down the middle of day 2's cell.
    let middle = day2.1 + day2.3 / 2.0;
    assert!(dashes.len() >= 6, "{dashes:?}");
    assert!(
        dashes.iter().all(|d| (d[0] - middle).abs() < 2.0),
        "{dashes:?} {middle}"
    );
    assert!(dashes.iter().any(|d| d[1] > day2.2), "{dashes:?}");
    // The segments are where they were.
    assert_eq!(fills(&root, &data), fills(&grid().0, &grid().1));
}

#[test]
fn neighbouring_cells_of_one_merge_value_join_without_resizing() {
    let (plain, data) = grid();
    let mut root = plain.clone();
    if let UiNode::Bars { merge, .. } = &mut root {
        *merge = Some("series".into());
    }
    let apart = fills(&plain, &data);
    let joined = fills(&root, &data);
    // a's day 1 ends orange and its day 2 starts orange: one join piece
    // between them, from under day 1's end to past day 2's start.
    assert_eq!(joined.len(), apart.len() + 1, "{joined:?}");
    let join = joined[1];
    assert_eq!(join.0, ORANGE);
    let (day1, day2) = (apart[0], apart[1]);
    assert!(
        join.1 < day1.1 + day1.3 && join.1 + join.3 > day2.1 + 3.0,
        "{joined:?}"
    );
    // Every segment keeps its place and size.
    let mut rest = joined.clone();
    rest.remove(1);
    assert_eq!(rest, apart);
}
