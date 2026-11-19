//! A scrolling view's content fades where it passes under a header or a
//! bottom bar, and not at the screen's own edges.

use construct_render::Viewport;
use construct_render::interact::Session;
use construct_render::scene::Draw;
use serde_json::{Value, json};

fn session(header: bool) -> Session {
    let rows: Vec<Value> = (0..40)
        .map(
            |i| json!({ "type": "core://Text", "value_expr": format!("Row {i}"), "style": "Body" }),
        )
        .collect();
    let scroll = json!({ "type": "core://Scroll", "content": { "type": "core://Column", "children": rows } });
    let root = if header {
        json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Title", "style": "Headline" },
            scroll,
            { "type": "core://Text", "value_expr": "Tabs", "style": "Body" } ] })
    } else {
        scroll
    };
    let page: protocol::UiNode =
        serde_json::from_value(json!({ "type": "core://Page", "id": "p", "root": root })).unwrap();
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

/// The fade strips drawn at the view's top and at its bottom.
fn fades(s: &mut Session) -> (usize, usize) {
    let scene = s.scene().unwrap();
    let view = scene
        .semantics
        .iter()
        .find(|n| n.node_type == "core://Scroll")
        .unwrap()
        .bounds;
    let mid = view[1] + view[3] / 2.0;
    let strips = scene.draws.iter().filter_map(|d| match d {
        Draw::Quad(q) if q.rect[3] <= 1.0 && q.rect[2] == view[2] && q.bg_color[3] < 1.0 => {
            Some(q.rect[1])
        }
        _ => None,
    });
    strips.fold(
        (0, 0),
        |(t, b), y| if y < mid { (t + 1, b) } else { (t, b + 1) },
    )
}

#[test]
fn content_fades_under_a_header_and_a_bottom_bar_while_there_is_more_that_way() {
    let mut s = session(true);
    let (top, bottom) = fades(&mut s);
    assert_eq!(top, 0, "nothing above yet");
    assert!(bottom > 0, "more below");
    s.scroll_by(None, 200.0).unwrap();
    let (top, bottom) = fades(&mut s);
    assert!(top > 0 && bottom > 0, "{top} {bottom}");
    s.scroll_by(None, 100_000.0).unwrap();
    let (top, bottom) = fades(&mut s);
    assert!(top > 0, "scrolled past the top");
    assert_eq!(bottom, 0, "at the end");
}

#[test]
fn a_view_filling_the_screen_does_not_fade_at_its_edges() {
    let mut s = session(false);
    s.scroll_by(None, 200.0).unwrap();
    assert_eq!(fades(&mut s), (0, 0));
}
