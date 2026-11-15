//! A scroll benchmark for the GPU renderer: a list shaped like the Construct
//! Tasks layout (`task_scroll`), scrolled a step per frame through the real
//! session and `gpu::Renderer`, timing each frame's CPU side as the windows
//! do (`perf`). Ignored by default; run it with
//!
//! ```text
//! cargo test --release -p construct-render --test integration bench -- --ignored --nocapture
//! ```
//!
//! On a software adapter (lavapipe) the milliseconds aren't a phone's, but
//! the split between stages and how it changes are.

use construct_render::Viewport;
use construct_render::gpu::{Gpu, Renderer};
use construct_render::interact::Session;
use std::time::{Duration, Instant};

use crate::common::fixture;

const FRAMES: usize = 600;
/// A phone's frame: 412 × 915 points at 2.625× (1082 × 2402 pixels).
const VIEWPORT: Viewport = Viewport {
    width: 412.0,
    height: 915.0,
    scale: 2.625,
};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn spread(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    format!("{:.2}/{:.2}/{:.2}", at(0.5), at(0.95), v[v.len() - 1])
}

#[test]
#[ignore = "a benchmark: run with --ignored --nocapture"]
fn bench_scroll_task_list() {
    bench_scroll("task_scroll");
}

#[test]
#[ignore = "a benchmark: run with --ignored --nocapture"]
fn bench_scroll_task_details() {
    bench_scroll("task_details");
}

fn bench_scroll(name: &str) {
    let gpu = match pollster::block_on(Gpu::headless()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    eprintln!("GPU: {}", gpu.describe());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = VIEWPORT.pixels();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = Renderer::new(&gpu, format);
    let (root, data) = fixture(name);
    let mut session = Session::new(name, root, data, VIEWPORT);

    let (mut scene_t, mut render_t, mut work_t) = (Vec::new(), Vec::new(), Vec::new());
    let (mut prep_t, mut sub_t) = (Vec::new(), Vec::new());
    let mut dir = 1.0f32;
    let at = (VIEWPORT.width / 2.0, VIEWPORT.height / 2.0);
    let mut last = renderer.stats;
    for i in 0..FRAMES {
        // An uneven drag: 18–30 points a frame, back and forth.
        let v = session.scroll_view_at(at.0, at.1).expect("a scroll view");
        let step = 18.0 + (i % 7) as f32 * 2.0;
        if (v.offset + dir * step) > v.max || (v.offset + dir * step) < 0.0 {
            dir = -dir;
        }
        session.set_scroll(&v.id, (v.offset + dir * step).clamp(0.0, v.max));

        let t0 = Instant::now();
        let (scene, text) = session.scene_and_text().unwrap();
        let t1 = Instant::now();
        renderer.render(&gpu, &view, (w, h), scene, text).unwrap();
        let t2 = Instant::now();
        // The GPU's own work (lavapipe runs it on this CPU) stays out of
        // the timings, as vsync does on a phone.
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        if i >= 30 {
            scene_t.push(ms(t1 - t0));
            render_t.push(ms(t2 - t1));
            work_t.push(ms(t2 - t0));
            prep_t.push(ms(renderer.stats.prepare));
            sub_t.push(ms(renderer.stats.submit));
        }
        last = renderer.stats;
    }
    eprintln!(
        "bench {name}: {} frames; work {}; scene {}; render {} = prepare {} + submit {} + rest (p50/p95/max ms); runs {} quads {} layers {}",
        work_t.len(),
        spread(work_t),
        spread(scene_t),
        spread(render_t),
        spread(prep_t),
        spread(sub_t),
        last.runs,
        last.quads,
        last.layers,
    );
}
