//! A scrolling view whose content is taller than it draws a thin thumb
//! along its right edge: as long as the share of the content showing, and
//! moving down as the view scrolls. A view whose content fits draws none.

use construct_render::Viewport;
use construct_render::interact::Session;
use construct_render::scene::{Draw, QuadInstance, Scene};
use serde_json::{Value, json};

fn session(rows: usize) -> Session {
    let rows: Vec<Value> = (0..rows)
        .map(
            |i| json!({ "type": "core://Text", "value_expr": format!("Row {i}"), "style": "Body" }),
        )
        .collect();
    let page: protocol::UiNode = serde_json::from_value(json!({ "type": "core://Page", "id": "p",
        "root": { "type": "core://Scroll", "content": { "type": "core://Column", "children": rows } } }))
    .unwrap();
    Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 300.0,
            height: 400.0,
            scale: 1.0,
        },
    )
}

/// The thumb: a narrow, rounded, see-through bar by the view's right edge.
fn thumb(scene: &Scene) -> Option<QuadInstance> {
    let view = scene
        .semantics
        .iter()
        .find(|n| n.node_type == "core://Scroll")?
        .bounds;
    let right = view[0] + view[2];
    scene.draws.iter().find_map(|d| match d {
        Draw::Quad(q)
            if q.rect[2] <= 4.0
                && q.rect[3] >= 24.0
                && q.rect[0] + q.rect[2] > right - 8.0
                && q.radii[0] > 0.0
                && q.bg_color[3] < 1.0 =>
        {
            Some(*q)
        }
        _ => None,
    })
}

#[test]
fn a_long_view_shows_a_thumb_that_moves_down_as_it_scrolls() {
    let mut s = session(60);
    let start = thumb(s.scene().unwrap()).expect("a thumb at the top");
    assert!(start.rect[1] <= 4.0, "at the top: {:?}", start.rect);
    assert!(
        start.rect[3] < 400.0 / 2.0,
        "a share of the view: {:?}",
        start.rect
    );
    s.scroll_by(None, 300.0).unwrap();
    let mid = thumb(s.scene().unwrap()).unwrap();
    assert!(mid.rect[1] > start.rect[1], "moved down");
    assert_eq!(mid.rect[3], start.rect[3], "the same length");
    s.scroll_by(None, 100_000.0).unwrap();
    let end = thumb(s.scene().unwrap()).unwrap();
    assert!(
        (end.rect[1] + end.rect[3] - 398.0).abs() < 0.5,
        "at the bottom: {:?}",
        end.rect
    );
}

#[test]
fn more_content_makes_a_shorter_thumb() {
    let mut short = session(30);
    let mut long = session(120);
    let a = thumb(short.scene().unwrap()).unwrap();
    let b = thumb(long.scene().unwrap()).unwrap();
    assert!(b.rect[3] < a.rect[3], "{:?} {:?}", a.rect, b.rect);
}

#[test]
fn a_view_whose_content_fits_has_no_thumb() {
    let mut s = session(3);
    assert!(thumb(s.scene().unwrap()).is_none());
}
