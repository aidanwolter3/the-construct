//! `core://Text` `weight`, `caps`, `max_lines` and `strike`, and
//! `core://Conditional` `not` and `count` (decision
//! `text-weight-caps-lines`).

use std::collections::BTreeMap;

use construct_render::Viewport;
use construct_render::cpu::CpuRenderer;
use construct_render::scene::{Scene, TextRun};
use construct_render::style::Face;
use construct_render::text::TextEngine;
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};

use crate::common::build;

fn node(v: Value) -> UiNode {
    serde_json::from_value(v).unwrap()
}

fn column(children: Value) -> UiNode {
    node(json!({"type": "core://Column", "children": children}))
}

fn runs(root: &UiNode, data: &DataBundle) -> Vec<TextRun> {
    build(root, data, &BTreeMap::new(), Viewport::PHONE)
        .texts()
        .cloned()
        .collect()
}

fn read_out(root: &UiNode, data: &DataBundle) -> Vec<String> {
    build(root, data, &BTreeMap::new(), Viewport::PHONE)
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect()
}

/// The weights of the fonts the glyphs of `run` are drawn with.
fn drawn_weights(engine: &mut TextEngine, run: &TextRun) -> Vec<u16> {
    let buffer = engine.buffer(&run.text, run.ty(), Some(run.rect[2] + 0.5), 2.0);
    let ids: Vec<_> = buffer
        .layout_runs()
        .flat_map(|r| r.glyphs.iter().map(|g| g.font_id))
        .collect();
    ids.iter()
        .map(|id| {
            engine
                .font_system()
                .db()
                .face(*id)
                .expect("a face")
                .weight
                .0
        })
        .collect()
}

#[test]
fn a_weight_draws_the_face_at_that_weight_from_its_own_font() {
    let text = |style: &str, weight: Option<&str>| {
        let mut t = json!({"type": "core://Text", "style": style, "value_expr": "Weekly plan"});
        if let Some(w) = weight {
            t["weight"] = json!(w);
        }
        t
    };
    let root = column(json!([
        text("Body", None),
        text("Body", Some("Medium")),
        text("Body", Some("SemiBold")),
        text("Body", Some("Bold")),
        text("Label", Some("Regular")),
        text("Display", Some("SemiBold")),
    ]));
    let runs = runs(&root, &DataBundle::default());
    let faces: Vec<Face> = runs.iter().map(|r| r.face).collect();
    assert_eq!(
        faces,
        [
            Face::Sans,
            Face::SansMedium,
            Face::SansSemiBold,
            Face::SansBold,
            Face::Sans,
            Face::SerifSemiBold,
        ]
    );
    // Each weight is a real font file at that weight, not the nearest one.
    let mut engine = TextEngine::new();
    for (run, want) in runs.iter().zip([400, 500, 600, 700, 400, 600]) {
        let weights = drawn_weights(&mut engine, run);
        assert!(
            !weights.is_empty() && weights.iter().all(|w| *w == want),
            "{:?}: {weights:?}",
            run.face
        );
    }
    // Heavier is wider.
    let widths: Vec<f32> = runs[..4]
        .iter()
        .map(|r| engine.measure(&r.text, r.ty(), None, 1.0).width)
        .collect();
    assert!(widths.windows(2).all(|w| w[0] <= w[1]), "{widths:?}");
    assert!(widths[3] > widths[0], "{widths:?}");
}

#[test]
fn caps_draws_capitals_spaced_out_and_reads_the_text_as_written() {
    let root = column(json!([
        {"type": "core://Text", "style": "Caption", "value_expr": "Move often · this week"},
        {"type": "core://Text", "style": "Caption", "value_expr": "Move often · this week",
         "caps": true, "weight": "SemiBold"}
    ]));
    let data = DataBundle::default();
    let runs = runs(&root, &data);
    assert_eq!(runs[0].text, "Move often · this week");
    assert_eq!(runs[1].text, "MOVE OFTEN · THIS WEEK");
    assert_eq!(runs[0].tracking, 0.0);
    assert!(
        (0.06..=0.08).contains(&runs[1].tracking),
        "{}",
        runs[1].tracking
    );
    // The spacing is drawn: wider than the same capitals without it.
    let mut engine = TextEngine::new();
    let spaced = engine.measure(&runs[1].text, runs[1].ty(), None, 1.0).width;
    let mut tight = runs[1].ty();
    tight.tracking = 0.0;
    let unspaced = engine.measure(&runs[1].text, tight, None, 1.0).width;
    assert!(spaced > unspaced + 5.0, "{spaced} vs {unspaced}");
    let said = read_out(&root, &data);
    assert_eq!(
        said.iter()
            .filter(|t| *t == "Move often · this week")
            .count(),
        2
    );
}

#[test]
fn max_lines_ends_a_longer_text_in_an_ellipsis() {
    let long = "A long walk along the river every morning before work, then a swim at the \
                lido on Thursdays, stretching on Sundays and a rest day whenever it rains.";
    let root = column(json!([
        {"type": "core://Text", "style": "Body", "value_expr": long},
        {"type": "core://Text", "style": "Body", "value_expr": long, "max_lines": 2},
        {"type": "core://Text", "style": "Body", "value_expr": long, "max_lines": 1},
        {"type": "core://Text", "style": "Body", "value_expr": "Short", "max_lines": 2},
        {"type": "core://Text", "style": "Body", "value_expr": "One\nTwo\nThree", "max_lines": 2}
    ]));
    let data = DataBundle::default();
    let runs = runs(&root, &data);
    let mut engine = TextEngine::new();
    let mut shaped = |run: &TextRun| -> Vec<String> {
        let buffer = engine.buffer(&run.text, run.ty(), Some(run.rect[2] + 0.5), 2.0);
        buffer
            .layout_runs()
            .map(|r| {
                let start = r.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
                let end = r.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
                r.text[start..end].trim_end().to_owned()
            })
            .collect()
    };
    let all = shaped(&runs[0]);
    assert!(all.len() > 2, "{all:?}");
    for (run, lines) in [(&runs[1], 2), (&runs[2], 1), (&runs[4], 2)] {
        let drawn = shaped(run);
        assert_eq!(drawn.len(), lines, "{drawn:?}");
        assert!(drawn.last().unwrap().ends_with('\u{2026}'), "{drawn:?}");
        // The box is that many lines tall.
        assert_eq!(run.rect[3], lines as f32 * run.line_height, "{drawn:?}");
    }
    assert_eq!(shaped(&runs[4])[0], "One");
    // Kept whole: as much as fits, then the ellipsis.
    let two = shaped(&runs[1]).join(" ");
    assert!(
        long.starts_with(two.trim_end_matches('\u{2026}').trim_end()),
        "{two}"
    );
    assert!(two.len() > all[0].len() + 10, "{two}");
    // A text that fits is untouched.
    assert_eq!(shaped(&runs[3]), ["Short"]);
    // Screen readers read it whole.
    assert!(read_out(&root, &data).iter().filter(|t| *t == long).count() == 3);
}

/// The longest stretch of dark pixels along any one row of `run`'s box in
/// `frame`, and that row (device pixels).
pub fn longest_dark_row(frame: &image::RgbaImage, run: &TextRun, scale: f32) -> (u32, u32) {
    let [x, y, w, h] = run.rect.map(|v| (v * scale).round() as u32);
    let mut best = (0, 0);
    for row in y..(y + h).min(frame.height()) {
        let mut stretch = 0;
        for col in x..(x + w).min(frame.width()) {
            if frame.get_pixel(col, row).0[0] < 128 {
                stretch += 1;
                if stretch > best.0 {
                    best = (stretch, row);
                }
            } else {
                stretch = 0;
            }
        }
    }
    best
}

/// A plain and a struck-through "Milk and eggs", built and drawn by the CPU.
pub fn struck_scene() -> (Scene, TextEngine) {
    let root = column(json!([
        {"type": "core://Text", "style": "Body", "value_expr": "Milk and eggs"},
        {"type": "core://Text", "style": "Body", "value_expr": "Milk and eggs", "strike": true},
        {"type": "core://Text", "style": "Body", "value_expr": "Milk and eggs", "strike": true,
         "max_lines": 1, "weight": "Bold"}
    ]));
    let scene = build(
        &root,
        &DataBundle::default(),
        &BTreeMap::new(),
        Viewport::PHONE,
    );
    (scene, TextEngine::new())
}

#[test]
fn strike_draws_a_line_through_the_middle_of_the_text() {
    let (scene, mut engine) = struck_scene();
    let runs: Vec<TextRun> = scene.texts().cloned().collect();
    assert_eq!(
        runs.iter().map(|r| r.strike).collect::<Vec<_>>(),
        [false, true, true]
    );
    let s = scene.scale;
    let frame = CpuRenderer::new().render(&scene, &mut engine);
    let wide = |run: &TextRun, engine: &mut TextEngine| {
        let buffer = engine.buffer(&run.text, run.ty(), Some(run.rect[2] + 0.5), s);
        construct_render::text::strike_lines(&buffer, run.ty(), s)
    };
    // Plain text has gaps along every row (between letters and words).
    let lines = wide(&runs[1], &mut engine);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let [_, _, line_w, line_h] = lines[0];
    assert_eq!(line_h, s.round() as i32);
    let (plain, _) = longest_dark_row(&frame, &runs[0], s);
    assert!((plain as i32) < line_w / 3, "{plain} of {line_w}");
    for run in &runs[1..] {
        // Struck through: one row is dark across the whole text...
        let (dark, row) = longest_dark_row(&frame, run, s);
        let width = wide(run, &mut engine)[0][2];
        assert!(dark as i32 >= width - 2, "{dark} of {width}");
        // ...through its middle: between the baseline and the top of its
        // capitals, not at the top or bottom of the line.
        let top = run.rect[1] * s;
        let mid = (row as f32 - top) / (run.line_height * s);
        assert!((0.35..0.75).contains(&mid), "{mid}");
    }
    // The text reads as written.
    assert!(
        scene
            .semantics
            .iter()
            .filter(|n| n.text_content.as_deref() == Some("Milk and eggs"))
            .count()
            == 3
    );
}

fn inbox(unread: &str) -> DataBundle {
    serde_json::from_value(json!({"entities": {"local://inbox": {"unread": unread}}})).unwrap()
}

#[test]
fn not_inverts_a_conditional() {
    let root = column(json!([
        {"type": "core://Conditional", "value": "${local://inbox#unread}", "equals": "0",
         "content": {"type": "core://Text", "style": "Body", "value_expr": "All read"}},
        {"type": "core://Conditional", "value": "${local://inbox#unread}", "equals": "0",
         "not": true,
         "content": {"type": "core://Text", "style": "Body", "value_expr": "Something is unread"}},
        {"type": "core://Conditional", "value": "${local://inbox#unread}", "not": true,
         "content": {"type": "core://Text", "style": "Body", "value_expr": "Nothing at all"}},
        {"type": "core://Conditional", "value": "${local://inbox#unread}", "date": "Past",
         "not": true,
         "content": {"type": "core://Text", "style": "Body", "value_expr": "Not a past date"}}
    ]));
    assert_eq!(
        read_out(&root, &inbox("0")),
        ["All read", "Nothing at all", "Not a past date"]
    );
    assert_eq!(
        read_out(&root, &inbox("3")),
        ["Something is unread", "Not a past date"]
    );
    assert_eq!(
        read_out(&root, &inbox("2020-01-01")),
        ["Something is unread"]
    );
}

fn todos(done: &[bool]) -> DataBundle {
    let mut entities = serde_json::Map::new();
    let mut uris = Vec::new();
    for (i, d) in done.iter().enumerate() {
        let uri = format!("x://todos/{i}");
        entities.insert(
            uri.clone(),
            json!({"title": format!("Task {i}"), "done": d}),
        );
        uris.push(uri);
    }
    serde_json::from_value(json!({"entities": entities, "queries": {"todos": uris}})).unwrap()
}

#[test]
fn count_shows_a_conditional_while_its_query_has_a_match() {
    let open = json!({"query_key": "todos", "where": [{"field": "done", "equals": "false"}]});
    let root = column(json!([
        {"type": "core://Conditional", "count": open,
         "content": {"type": "core://Text", "style": "Label", "value_expr": "Still to do"}},
        {"type": "core://Conditional", "count": open, "not": true,
         "content": {"type": "core://Text", "style": "Label", "value_expr": "All done"}},
        {"type": "core://Conditional", "count": {"query_key": "todos"},
         "content": {"type": "core://Text", "style": "Label", "value_expr": "Has tasks"}},
        {"type": "core://Conditional", "count": {"query_key": "todos"}, "not": true,
         "content": {"type": "core://Text", "style": "Label", "value_expr": "No tasks"}}
    ]));
    assert_eq!(
        read_out(&root, &todos(&[true, false])),
        ["Still to do", "Has tasks"]
    );
    assert_eq!(
        read_out(&root, &todos(&[true, true])),
        ["All done", "Has tasks"]
    );
    assert_eq!(read_out(&root, &todos(&[])), ["All done", "No tasks"]);
    // Not loaded: neither shows.
    assert!(read_out(&root, &DataBundle::default()).is_empty());
}
