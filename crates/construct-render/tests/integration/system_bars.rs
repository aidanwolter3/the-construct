//! Edge to edge: the window's insets (the system bars over them) show the
//! screen's own page colour, and the scene says how light it is there.

use construct_render::Viewport;
use construct_render::interact::Session;
use construct_render::style::Rgba;
use serde_json::{Value, json};

const STATUS: f32 = 24.0;
const NAV: f32 = 16.0;

fn session(root: Value) -> Session {
    let page: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "p", "root": root }))
    .unwrap();
    let mut s = Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 100.0,
            height: 100.0,
            scale: 1.0,
        },
    );
    s.set_insets([STATUS, 0.0, NAV, 0.0]);
    s
}

fn page_box(background: &str, content: Value) -> Value {
    json!({ "type": "core://Box", "fill": true, "background": background, "content": content })
}

fn text() -> Value {
    json!({ "type": "core://Text", "value_expr": "Hello", "style": "Body" })
}

fn rgb(hex: &str) -> [u8; 3] {
    let c = Rgba::hex(hex).unwrap().to_u8();
    [c[0], c[1], c[2]]
}

#[test]
fn the_page_background_fills_the_insets_and_sets_the_bars() {
    let cream = "#F6F5F1";
    let mut s = session(page_box(cream, text()));
    let img = s.render_cpu().unwrap();
    // Under the status bar, under the navigation bar, and the page itself.
    for y in [2, 12, 50, 90, 98] {
        assert_eq!(img.get_pixel(50, y).0[..3], rgb(cream), "y = {y}");
    }
    let bars = s.scene().unwrap().bars;
    assert_eq!(bars, [Rgba::hex(cream).unwrap().0; 2]);
    assert!(bars.iter().all(|&c| Rgba(c).is_light()));

    // A dark page takes light icons; the content stays below the status bar.
    let ink = "#1B2030";
    let mut s = session(page_box(ink, text()));
    let img = s.render_cpu().unwrap();
    assert_eq!(img.get_pixel(50, 5).0[..3], rgb(ink));
    assert_eq!(img.get_pixel(50, 95).0[..3], rgb(ink));
    let scene = s.scene().unwrap();
    assert!(scene.bars.iter().all(|&c| !Rgba(c).is_light()));
    let hello = scene.find_text("Hello").unwrap();
    assert!(hello.bounds[1] >= STATUS, "{:?}", hello.bounds);
}

#[test]
fn each_edge_takes_what_spans_it() {
    // A coloured header across the top, on a cream page: the status bar
    // shows the header, the navigation bar the page.
    let header = "#234E8C";
    let cream = "#F6F5F1";
    let mut s = session(page_box(
        cream,
        json!({ "type": "core://Column", "children": [
            { "type": "core://Box", "background": header, "padding_v_dp": 8,
              "content": { "type": "core://Column", "children": [ text() ] } },
            text() ] }),
    ));
    let img = s.render_cpu().unwrap();
    assert_eq!(img.get_pixel(50, 5).0[..3], rgb(header));
    assert_eq!(img.get_pixel(50, 95).0[..3], rgb(cream));
    let bars = s.scene().unwrap().bars;
    assert!(!Rgba(bars[0]).is_light());
    assert!(Rgba(bars[1]).is_light());
}

#[test]
fn without_a_page_box_the_theme_background_shows() {
    let mut s = session(json!({ "type": "core://Column", "children": [ text() ] }));
    let img = s.render_cpu().unwrap();
    let scene = s.scene().unwrap();
    let bg = Rgba(scene.background).to_u8();
    assert_eq!(img.get_pixel(50, 5).0[..3], bg[..3]);
    assert_eq!(scene.bars, [scene.background; 2]);
}

#[test]
fn dark_mode_takes_the_dark_page_and_light_icons() {
    // The page token follows the mode: in dark mode the insets take the
    // dark page and the bars get light icons.
    let page = || page_box("${local://theme#page}", text());
    let mut light = session(page());
    let light_bars = light.scene().unwrap().bars;
    assert!(light_bars.iter().all(|&c| Rgba(c).is_light()));

    let mut s = session(page());
    s.set_dark(true);
    let img = s.render_cpu().unwrap();
    let bars = s.scene().unwrap().bars;
    assert_ne!(bars, light_bars);
    assert!(bars.iter().all(|&c| !Rgba(c).is_light()), "{bars:?}");
    let dark = Rgba(bars[0]).to_u8();
    for y in [2, 50, 98] {
        assert_eq!(img.get_pixel(50, y).0[..3], dark[..3], "y = {y}");
    }
}

/// A home on a dark page with a layout on a cream page, opened over it as
/// `presentation` (from `edge`).
fn over_home(intent: Value) -> Session {
    let mut s = session(page_box("#202833", text()));
    let shade: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "shade", "root": page_box("#F6F5F1", text()) }))
    .unwrap();
    s.layouts.insert("shade".into(), shade);
    s.dispatch(&intent).unwrap();
    s
}

#[test]
fn a_shade_from_the_top_is_its_page_to_the_handle_and_under_the_status_bar() {
    let cream = "#F6F5F1";
    let mut s = over_home(
        json!({ "intent": "action.navigation.open", "layout": "shade",
        "presentation": "Overlay", "edge": "top" }),
    );
    let img = s.render_cpu().unwrap();
    // The shade is 85 points tall: under the status bar, its page, and the
    // handle strip at its bottom (beside the handle) are all cream.
    for y in [2, 12, 50, 72, 82] {
        assert_eq!(img.get_pixel(10, y).0[..3], rgb(cream), "y = {y}");
    }
    // The handle shows on it, darker than the page.
    let pill = img.get_pixel(50, 85 - 12 + 2).0;
    assert!(pill[0] < rgb(cream)[0] - 10, "{pill:?}");
    let bars = s.scene().unwrap().bars;
    assert_eq!(bars[0], Rgba::hex(cream).unwrap().0);
}

#[test]
fn a_bottom_sheet_and_a_panel_take_their_page_under_the_bars() {
    let cream = "#F6F5F1";
    let mut s = over_home(
        json!({ "intent": "action.navigation.open", "layout": "shade",
        "presentation": "Sheet" }),
    );
    let img = s.render_cpu().unwrap();
    // The handle strip at the sheet's top (15 points down), its page and
    // the navigation bar under it.
    for y in [20, 50, 90, 98] {
        assert_eq!(img.get_pixel(10, y).0[..3], rgb(cream), "y = {y}");
    }
    assert_eq!(s.scene().unwrap().bars[1], Rgba::hex(cream).unwrap().0);

    let mut s = over_home(
        json!({ "intent": "action.navigation.open", "layout": "shade",
        "presentation": "Panel" }),
    );
    let img = s.render_cpu().unwrap();
    // The panel (86 points wide, from the start): its page under both bars.
    for y in [2, 50, 98] {
        assert_eq!(img.get_pixel(40, y).0[..3], rgb(cream), "y = {y}");
    }
}
