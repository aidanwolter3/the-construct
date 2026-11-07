//! Phase 2 on wgpu: frames drawn by the GPU pipeline (lavapipe in CI, Metal
//! or Vulkan locally) match the CPU goldens. Without any adapter the tests
//! say so and pass; set CONSTRUCT_REQUIRE_GPU=1 (CI does) to fail instead.

use construct_render::encode::diff_ratio;
use construct_render::gpu::HeadlessSurface;
use construct_render::scene::{Draw, QuadInstance, Scene};
use construct_render::style::Rgba;
use construct_render::text::TextEngine;
use construct_render::{Input, SceneBuilder, Viewport};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::common::fixture;
use crate::golden::golden;

/// One device for every test: creating many lavapipe devices is slow.
fn surface() -> Option<std::sync::MutexGuard<'static, HeadlessSurface>> {
    static SURFACE: OnceLock<Option<Mutex<HeadlessSurface>>> = OnceLock::new();
    let s = SURFACE.get_or_init(|| match pollster::block_on(HeadlessSurface::new(8, 8)) {
        Ok(s) => {
            eprintln!("GPU: {}", s.gpu.describe());
            Some(Mutex::new(s))
        }
        Err(e) => {
            assert!(std::env::var_os("CONSTRUCT_REQUIRE_GPU").is_none(), "{e}");
            eprintln!("skipping GPU tests: {e}");
            None
        }
    });
    s.as_ref()
        .map(|m| m.lock().unwrap_or_else(|p| p.into_inner()))
}

fn gpu_frame(name: &str, form: &BTreeMap<String, String>, dark: bool) -> Option<image::RgbaImage> {
    let mut surface = surface()?;
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
        })
        .unwrap();
    Some(surface.render(&scene, &mut builder.text).unwrap())
}

/// The GPU frame against the CPU golden: anti-aliasing and glyph blending
/// differ by a few levels at edges, so compare with a tolerance.
fn matches_golden(name: &str, frame: &image::RgbaImage) {
    let ratio = diff_ratio(frame, &golden(name), 24);
    if ratio >= 0.002 {
        let out = std::env::temp_dir().join(format!("{name}.gpu.png"));
        std::fs::write(&out, construct_render::encode::png(frame)).unwrap();
        panic!(
            "{name}: {:.4}% of GPU pixels differ from the golden; frame: {}",
            ratio * 100.0,
            out.display()
        );
    }
}

#[test]
fn golden_visual_regression_gpu() {
    let empty = BTreeMap::new();
    for (name, form, dark) in [
        ("task_list", empty.clone(), false),
        (
            "task_list_started_dark",
            BTreeMap::from([("filter".to_owned(), "started".to_owned())]),
            true,
        ),
        ("home", empty.clone(), false),
    ] {
        let fixture_name = name.trim_end_matches("_started_dark");
        let Some(frame) = gpu_frame(fixture_name, &form, dark) else {
            return;
        };
        matches_golden(name, &frame);
    }
}

#[test]
fn gpu_quad_matches_the_cpu_sdf() {
    let Some(mut surface) = surface() else {
        return;
    };
    let q = QuadInstance::fill([10.0, 10.0, 80.0, 60.0], Rgba([0.2, 0.3, 0.8, 1.0]), 18.0)
        .border(Rgba([0.0, 0.0, 0.0, 1.0]), 2.0)
        .shadow(Rgba([0.0, 0.0, 0.0, 0.4]), [0.0, 3.0], 6.0, 1.0);
    let scene = Scene {
        width: 100.0,
        height: 90.0,
        scale: 2.0,
        background: [1.0, 1.0, 1.0, 1.0],
        draws: vec![Draw::Quad(q)],
        semantics: Vec::new(),
    };
    let mut text = TextEngine::new();
    let gpu = surface.render(&scene, &mut text).unwrap();
    let cpu = construct_render::cpu::CpuRenderer::new().render(&scene, &mut text);
    assert_eq!(gpu.dimensions(), (200, 180));
    let ratio = diff_ratio(&gpu, &cpu, 3);
    assert!(ratio < 0.001, "{:.4}% differ", ratio * 100.0);
}

#[test]
fn gpu_snapshot_latency() {
    let Some(mut surface) = surface() else {
        return;
    };
    let (root, data) = fixture("task_list");
    let mut builder = SceneBuilder::new();
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
    };
    let scene = builder.build(&input).unwrap();
    surface.render(&scene, &mut builder.text).unwrap();
    let t = std::time::Instant::now();
    let scene = builder.build(&input).unwrap();
    let frame = surface.render(&scene, &mut builder.text).unwrap();
    let bytes = construct_render::encode::webp(&frame);
    eprintln!(
        "GPU snapshot (layout + render + readback + WebP) on {}: {:?}",
        surface.gpu.describe(),
        t.elapsed()
    );
    assert_eq!(&bytes[0..4], b"RIFF");
}
