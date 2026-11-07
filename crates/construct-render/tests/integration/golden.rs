//! Phase 2 on the CPU: golden frames and the SDF's edges and shadows. Set
//! UPDATE_GOLDENS=1 to rewrite `tests/data/goldens/` after a deliberate
//! change (and look at the new PNGs before committing them).

use construct_render::cpu::CpuRenderer;
use construct_render::encode::{diff_ratio, png, webp};
use construct_render::scene::{Draw, QuadInstance, Scene};
use construct_render::style::Rgba;
use construct_render::text::TextEngine;
use construct_render::{Input, SceneBuilder, Viewport};
use image::RgbaImage;
use std::collections::BTreeMap;

use crate::common::{data_dir, fixture};

/// The frame of a fixture, drawn on the CPU.
pub fn cpu_frame(name: &str, form: &BTreeMap<String, String>, dark: bool) -> RgbaImage {
    let (root, data) = fixture(name);
    let mut builder = SceneBuilder::new();
    let scene = builder
        .build(&Input {
            root: &root,
            data: &data,
            form,
            route: &BTreeMap::new(),
            layouts: &BTreeMap::new(),
            scroll: &BTreeMap::new(),
            viewport: Viewport::PHONE,
            dark,
            item: None,
            insets: [0.0; 4],
            focused: None,
        })
        .unwrap();
    CpuRenderer::new().render(&scene, &mut builder.text)
}

pub fn golden(name: &str) -> RgbaImage {
    image::open(data_dir().join("goldens").join(format!("{name}.png")))
        .unwrap_or_else(|e| panic!("golden {name}: {e} (run with UPDATE_GOLDENS=1)"))
        .to_rgba8()
}

/// Compares with the golden (or rewrites it), saving the frame next to the
/// test binary when it differs.
pub fn check(name: &str, frame: &RgbaImage, max_ratio: f64) {
    let path = data_dir().join("goldens").join(format!("{name}.png"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, png(frame)).unwrap();
        return;
    }
    let ratio = diff_ratio(frame, &golden(name), 16);
    if ratio >= max_ratio {
        let out = std::env::temp_dir().join(format!("{name}.actual.png"));
        std::fs::write(&out, png(frame)).unwrap();
        panic!(
            "{name}: {:.4}% of pixels differ (limit {:.2}%); this frame: {}",
            ratio * 100.0,
            max_ratio * 100.0,
            out.display()
        );
    }
}

#[test]
fn golden_task_list() {
    check(
        "task_list",
        &cpu_frame("task_list", &BTreeMap::new(), false),
        0.002,
    );
}

#[test]
fn golden_task_list_started_dark() {
    let form = BTreeMap::from([("filter".to_owned(), "started".to_owned())]);
    check(
        "task_list_started_dark",
        &cpu_frame("task_list", &form, true),
        0.002,
    );
}

#[test]
fn golden_home() {
    check("home", &cpu_frame("home", &BTreeMap::new(), false), 0.002);
}

fn one_quad(q: QuadInstance, w: f32, h: f32, scale: f32) -> RgbaImage {
    let scene = Scene {
        width: w,
        height: h,
        scale,
        background: [1.0, 1.0, 1.0, 1.0],
        draws: vec![Draw::Quad(q)],
        semantics: Vec::new(),
    };
    CpuRenderer::new().render(&scene, &mut TextEngine::new())
}

#[test]
fn quad_sdf_anti_aliasing() {
    // A black card with 20pt corners on white, at 2x.
    let q = QuadInstance::fill([10.0, 10.0, 80.0, 80.0], Rgba([0.0, 0.0, 0.0, 1.0]), 20.0);
    let img = one_quad(q, 100.0, 100.0, 2.0);
    // Along the diagonal through the top-left corner the value falls from
    // white to black through in-between greys (no stair steps).
    let diag: Vec<u8> = (0..80).map(|i| img.get_pixel(i, i).0[0]).collect();
    let greys = diag.iter().filter(|v| **v > 10 && **v < 245).count();
    assert!((1..=4).contains(&greys), "edge ramp: {diag:?}");
    assert!(diag.windows(2).all(|w| w[1] <= w[0]), "monotonic: {diag:?}");
    // Straight edges ramp over about one pixel too.
    let row: Vec<u8> = (15..25).map(|x| img.get_pixel(x, 100).0[0]).collect();
    assert_eq!(row.first(), Some(&255));
    assert_eq!(row.last(), Some(&0));
    // The corner's own pixel stays white: it is cut by the radius.
    assert_eq!(img.get_pixel(21, 21).0[0], 255);
}

#[test]
fn drop_shadow_extends_past_the_quad() {
    let q = QuadInstance::fill([30.0, 30.0, 40.0, 40.0], Rgba([1.0, 1.0, 1.0, 1.0]), 8.0).shadow(
        Rgba([0.0, 0.0, 0.0, 0.5]),
        [0.0, 4.0],
        8.0,
        0.0,
    );
    let img = one_quad(q, 100.0, 100.0, 1.0);
    let at = |x, y| img.get_pixel(x, y).0[0];
    // Below the card (offset and blurred) is darker than far away, and not clipped at the card's edge.
    assert!(at(50, 74) < 230, "{}", at(50, 74));
    assert!(at(50, 80) < at(50, 95));
    assert_eq!(at(50, 99), 255);
    assert!(at(26, 50) < 255, "the blur reaches the left side too");
    // The card itself stays white.
    assert_eq!(at(50, 50), 255);
    assert!(q.extent()[3] > 40.0 + 4.0);
}

#[test]
fn snapshot_latency_and_webp() {
    let (root, data) = fixture("task_list");
    let mut builder = SceneBuilder::new();
    let mut cpu = CpuRenderer::new();
    let empty = BTreeMap::new();
    let input = Input {
        root: &root,
        data: &data,
        form: &empty,
        route: &empty,
        layouts: &BTreeMap::new(),
        scroll: &BTreeMap::new(),
        viewport: Viewport::PHONE,
        dark: false,
        item: None,
        insets: [0.0; 4],
        focused: None,
    };
    // Warm the glyph caches, then time one full snapshot.
    let scene = builder.build(&input).unwrap();
    let _ = cpu.render(&scene, &mut builder.text);
    let t = std::time::Instant::now();
    let scene = builder.build(&input).unwrap();
    let laid_out = t.elapsed();
    let frame = cpu.render(&scene, &mut builder.text);
    let drawn = t.elapsed();
    let bytes = webp(&frame);
    let took = t.elapsed();
    println!(
        "CPU snapshot, 390x844 @2x: layout {laid_out:?}, raster {:?}, WebP {:?}: {took:?}",
        drawn - laid_out,
        took - drawn
    );
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
    assert_eq!(frame.dimensions(), (780, 1688));
    // Generous: debug builds are many times slower than release.
    assert!(took.as_secs_f64() < 5.0, "{took:?}");
}
