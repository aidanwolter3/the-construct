//! `core://Text` `markdown` (decision `text-markdown`): the blocks drawn
//! from the renderer's own boxes, inline styles as spans shaped in one
//! buffer, and a preview flattened onto its lines.

use std::collections::BTreeMap;

use construct_render::Viewport;
use construct_render::scene::{Draw, Scene};
use construct_render::text::TextEngine;
use protocol::{DataBundle, UiNode};
use serde_json::json;

use crate::common::build;

const REPORT: &str = "## Summary\n\nFixed the **redirect** in `auth.rs`; see [the PR](https://x.example/1).\n\n- one\n- [x] done\n\n```\nlet a = 1;\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |";

fn scene(extra: serde_json::Value) -> Scene {
    let mut text = json!({"type": "core://Text", "style": "Body", "markdown": true,
        "value_expr": "${agents://sessions/r1#summary}"});
    for (k, v) in extra.as_object().unwrap() {
        text[k] = v.clone();
    }
    let root: UiNode =
        serde_json::from_value(json!({"type": "core://Column", "children": [text]})).unwrap();
    let mut data = DataBundle::default();
    data.entities.insert(
        "agents://sessions/r1".into(),
        serde_json::from_value(json!({"summary": REPORT})).unwrap(),
    );
    build(&root, &data, &BTreeMap::new(), Viewport::PHONE)
}

#[test]
fn blocks_are_drawn_with_spans_and_read_without_markup() {
    let s = scene(json!({}));
    let texts: Vec<_> = s.texts().collect();
    let said: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
    // The heading, the paragraph, each item's marker-less text, the code,
    // the table's cells: no markup left anywhere.
    for want in [
        "Summary",
        "Fixed the redirect in auth.rs; see the PR.",
        "one",
        "done",
        "let a = 1;",
        "a",
        "2",
    ] {
        assert!(said.contains(&want), "{want:?} in {said:?}");
    }
    assert!(
        said.iter()
            .all(|t| !t.contains("**") && !t.contains('`') && !t.contains('|'))
    );
    let heading = texts.iter().find(|t| t.text == "Summary").unwrap();
    let body = texts.iter().find(|t| t.text.starts_with("Fixed")).unwrap();
    assert!(heading.size > body.size, "a heading is larger");
    let bold = body.spans.iter().find(|s| s.bold).unwrap();
    assert_eq!(&body.text[bold.start..bold.end], "redirect");
    let code = body.spans.iter().find(|s| s.mono).unwrap();
    assert_eq!(&body.text[code.start..code.end], "auth.rs");
    let link = body.spans.iter().find(|s| s.color.is_some()).unwrap();
    assert_eq!(&body.text[link.start..link.end], "the PR");
    let block = texts.iter().find(|t| t.text == "let a = 1;").unwrap();
    assert!(block.spans.iter().all(|s| s.mono));
    // The code card, the table's border and its header, the dot.
    let quads = s
        .draws
        .iter()
        .filter(|d| matches!(d, Draw::Quad(_)))
        .count();
    assert!(quads >= 4, "{quads} quads");
    // One node for screen readers and tests, without markup.
    let read: Vec<String> = s
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect();
    assert!(
        read.iter()
            .any(|t| t.starts_with("Summary Fixed the redirect in auth.rs")),
        "{read:?}"
    );
}

#[test]
fn spans_change_what_is_measured_and_drawn() {
    let s = scene(json!({}));
    let body = s
        .texts()
        .find(|t| t.text.starts_with("Fixed"))
        .unwrap()
        .clone();
    let mut engine = TextEngine::new();
    let plain = engine.measure(&body.text, body.ty(), None, 2.0);
    let rich = engine.measure_rich(&body.text, &body.spans, body.ty(), None, 2.0);
    assert_ne!(
        plain.width, rich.width,
        "bold and monospace take other widths"
    );
    let buffer = engine.buffer_rich(&body.text, &body.spans, body.ty(), None, 2.0);
    let coloured = buffer
        .layout_runs()
        .flat_map(|r| r.glyphs.iter())
        .filter(|g| g.color_opt.is_some())
        .count();
    // The link's glyphs (its space may be one), and nothing else.
    assert!((5..=6).contains(&coloured), "{coloured} coloured glyphs");
}

#[test]
fn a_preview_keeps_inline_styles_on_its_lines() {
    let s = scene(json!({"max_lines": 2}));
    let texts: Vec<_> = s.texts().collect();
    assert_eq!(texts.len(), 1, "one text");
    assert!(
        texts[0].text.starts_with("Summary Fixed the redirect"),
        "{}",
        texts[0].text
    );
    assert!(texts[0].spans.iter().any(|s| s.bold));
    let mut engine = TextEngine::new();
    let lines = engine
        .buffer_rich(
            &texts[0].text,
            &texts[0].spans,
            texts[0].ty(),
            Some(texts[0].rect[2] + 0.5),
            2.0,
        )
        .layout_runs()
        .count();
    assert!(lines <= 2);
}
