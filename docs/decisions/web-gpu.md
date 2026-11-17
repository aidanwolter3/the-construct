# The web client draws on the GPU through WebGL2, the CPU only as a fallback

Date: 2026-11-17. Related: [web-client](web-client.md), [rust-render-engine](rust-render-engine.md).

The web client drew every frame on the CPU and copied it into a 2D canvas: about 50 ms for a 1280 × 800 window and
90 ms at twice the density, so a long page scrolled at 10 to 20 frames a second. The renderer already has a GPU
pipeline (wgpu, glyphon), which the desktop and Android apps use.

Considered:

- **Faster CPU frames** (redraw only what moved, draw at 1× while scrolling): more code in the rasterizer for every
  client, and still bound by the CPU at high density.
- **WebGPU first** (`new_instance_with_webgpu_detection`): headless Chromium's software WebGPU loses its device on
  the first buffer it makes, so it can't be tested here, and not every browser has WebGPU.
- **The renderer's wgpu pipeline over WebGL2.** Taken.

Decision: **in a browser, construct-render builds with `gpu` and wgpu's `webgl` backend, and the client draws each new
scene with `gpu::Renderer` into a surface on its canvas.**

- `Gpu::with_instance` asks for `Limits::downlevel_webgl2_defaults()` in a browser (WebGL2's limits are lower);
  the GPU module uses `crate::time`, as the rest of the renderer does in a browser.
- The surface is a non-sRGB format, as on the desktop, so colours match the CPU path and the goldens.
- **Fallback**: where no adapter comes, the canvas is replaced with a fresh one (a canvas keeps the first kind of
  context it gave out) and the CPU rasterizer draws into its 2D context. `?cpu` in the address forces the CPU;
  `?webgpu` tries WebGPU first.
- **Size**: the client builds with the `web-release` profile (size-optimized, full LTO) and wasm-bindgen drops the
  name and producers sections, so with the GPU pipeline it is about 2.0 MB gzipped, under CI's 2.5 MiB budget.
- A panic is written to the console with its message (else a browser shows only `unreachable`).

Measured in headless Chromium on a software GPU (SwiftShader), one wheel step a frame on a long page: at 1× the GPU
holds 60 frames a second (slowest tenth of frames 17 ms, the CPU's 50 ms); at 2× the slowest tenth take 83 ms against
the CPU's 133 ms. A real GPU does the drawing itself, so this is the floor.

Not yet: WebGPU by default (once it is checked on real browsers and hardware).
