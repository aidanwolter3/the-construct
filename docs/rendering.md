# The Rust rendering engine

`crates/construct-render` draws layouts without the JVM: a plumbing `UiNode` tree and its data are laid out with
taffy, turned into a scene of rounded-box quads, text runs and a semantic tree, and drawn by wgpu or by a CPU
rasterizer of the same distance function. The pod uses it for agents' layout previews; a desktop window and an
Android NativeActivity show it as a prototype client. The Compose UI (`app/shared`) is still the app. Decision:
[`rust-render-engine`](decisions/rust-render-engine.md).

```
layout (plumbing) + entities/queries + form, route, theme
        │  layout.rs: taffy flexbox; text measured with cosmic-text (bundled fonts only)
        ▼
Scene { draws: [Quad(QuadInstance) | Text(TextRun)], semantics: [SemanticNode] }
        │                                   │
        ▼                                   ▼
cpu.rs: SDF per pixel, swash glyphs    gpu/: quad.wgsl (instanced SDF) + glyphon
        │                                   │
        ▼                                   ▼
PNG / WebP (encode.rs)                 HeadlessSurface (readback) or a window / Android surface
```

## Modules

| Module | What it does |
|---|---|
| `layout` | `SceneBuilder::build(&Input)`: every plumbing node to a taffy style (Material 3 metrics, as the Compose renderer), bindings resolved (`bind`), lists filtered (`where`, `filter`, `group_by`, grids), `$count`, theme tokens |
| `scene` | `QuadInstance` (the GPU vertex layout), `TextRun`, `SemanticNode`, `hit_test`, `find_node`, `find_text` |
| `cpu` | The CPU rasterizer: `sd_rounded_box` and `shade` (the shader's maths), 8-bit premultiplied target, glyphs placed as glyphon places them |
| `gpu` | `Renderer` (any texture view), `HeadlessSurface` (offscreen + readback); feature `gpu` (default) |
| `interact` | `Session`: a layout with form values, scroll offsets and a back stack; `tap`, `scroll_by`, `type_text`, `dispatch` |
| `viewer` | `Controller`: platform events (resize, tap, wheel, text, Back) applied to a session; what windows feed |
| `window` | winit + wgpu surface (feature `window`); used by `construct-viewer` and Android |
| `android` | `android_main` for the NativeActivity (Android builds with `window`) |

The pod builds the crate with `default-features = false`: no wgpu, CPU only.

Fonts are bundled in `fonts/` and nothing else is loaded: Instrument Sans (regular, bold), Lora (the Display
style) and `DejaVuSans-Symbols.ttf`, DejaVu Sans cut to punctuation, arrows, shapes, symbols and dingbats.
cosmic-text falls back to any loaded font with the glyph, so ♥, ✓ and ★ come from the last one.

## Semantic ids

What agents and tests address: a node's `tag` (bound, e.g. `task_$item.id`); `<field>=<value>` for a
`core://ChoiceChips` option or a `core://Dock` tab (`filter=started`, `filter=` for the empty value);
`field:<name>` for a `core://TextField`; `list:<query_key>`, `chips:<field>`, `layout:<id>`; otherwise the node's
path from the root (`root/0/2/i3/0`, `i3` being a list's fourth item). Bounds are logical points.

## Coverage

Drawn like the app: Page, Workspace (title and slots), Column, Row, Container, Box, Text (styles, `count`), Button,
IconButton (`active`), Chip, Badge, Spacer, Divider, Conditional, Tappable, Gesture (tap), Scroll (clipped,
offsets), List (`where`, `filter`, `group_by`, `columns`, `focus`, `indent`, `empty_text`), ChoiceChips, TextField,
Switch, Progress, Dock, Layout (inline, from the layouts given), and every named icon (`icons.rs`: the shared UI's
Material path data, rasterized with zeno; on the GPU they are glyphon custom glyphs). Images draw once loaded
(over a placeholder in the blurhash's colour; only live sessions load them). Approximated: Carousel (first item and an "n/N" badge), MediaPicker (the add tile),
Drawer (the selected page), AppFrame and unknown nodes (a labelled box). Not drawn: dialogs, `format` dates.
Presented layouts are drawn as the shared UI draws them: a sheet (85% of the height, with a handle), a side panel
(86% of the width, at most 340 points) or an overlay from its edge, over a 32% black scrim that closes it when
tapped; a screen a sheet or panel opens stays inside it. In a window, layouts and screens slide in and out over
280 ms (`FastOutSlowInEasing`); a fling follows Android's `OverScroller` spline with the release velocity of the
last 100 ms. `action.device` `show_versions` fills `local://versions` and `local:versions` from
`/v1/apps/<id>/versions`; `pin_version` and `follow_latest` mark the choice there, but the layout still shows its
latest version. `core://Sticky` pins at the top of its scrolling view, over the page
background, until its parent scrolls away.

## The desktop viewer

```bash
cargo run -p construct-render --features viewer --bin construct-viewer                      # the bundled demo
cargo run -p construct-render --features viewer --bin construct-viewer -- layout.json       # {"layout", "data"}
CONSTRUCT_POD_TOKEN=… cargo run -p construct-render --features viewer --bin construct-viewer -- \
  --pod http://127.0.0.1:8080 --layout home                                                 # live, read-only
```

Click, scroll, type into a focused field, Escape for Back; each interaction's effects are logged. It draws with
Metal on macOS and Vulkan (or GL) on Linux; on a headless Linux box, `Xvfb` plus Mesa's `mesa-vulkan-drivers`
(lavapipe) is enough.

Any workspace bundle (what `GET /v1/workspaces/<id>` answers) can be drawn to PNGs without a window:
`cargo run --release -p construct-render --no-default-features --example render_bundle -- a.json b.json --out dir`
(390 × 844 at 3×, the simulator's phone, so the two can be compared side by side with `-Pfixture`).

## Android

`scripts/android_native.sh` cross-compiles `libconstruct_render.so` (cargo-ndk, arm64-v8a), packages a
NativeActivity APK with aapt2 (no Gradle, no Kotlin) and signs it with the debug key. With an adb device it then
installs it, pushes the task-list fixture, launches, checks logcat for `surface initialized successfully` and
`Frame 0 drawn`, takes a screenshot, and compares the frame the device drew offscreen at 390 × 844 @2x with the
desktop golden (at most 1% of pixels may differ). `--build-only` stops after the APK. The activity opens
`layout.json` from its external files directory, or the bundled demo.

`scripts/android_native.sh --pod <url>` instead installs it and opens that pod **live** (below): it asks for the pod
code (or reads `CONSTRUCT_POD_CODE`), checks it with `POST /v1/pods/open`, and writes the session token the pod
answers (on today's server, the pod code itself) to `pod.json` in the app's private storage with `run-as` (the APK is
debuggable). It is stored unencrypted, unlike the main app's keystore. `--no-pod` deletes it and goes back to the
demo. The app targets SDK 34, so the system keeps its window below the status bar and camera cutout and resizes it
for the keyboard; a tapped text field opens the keyboard, and Back pops the stack. The layout stays inside the
window insets Android reports (`getRootWindowInsets`: system bars, camera cutout and keyboard), logged as they change.

## Live sessions (`remote`)

Feature `remote` (on in the viewer and the APK) runs a session over a pod, read-only: `PodClient` fetches home as a
client does (`GET /v1/workspaces/home`), builds the device's list of layouts (`local:layouts`, `local:apps`) from
`GET /v1/apps?all=1`, and is the session's `Loader`: a layout a tap opens is fetched then, a page screen whose route
matches a tap's `target_uri` opens with its `$route` bound, its `item` and its `data` path loaded. `local://nav` and
`local://screen` are set as the shared UI sets them. Images (`core://Image`) download on four background threads
without the session (the pod serves media from public paths), are cut to their box (cropped to fill, corners
rounded) and drawn over their placeholder once they arrive; the window redraws then. What a tap would send to the
pod (likes, adds, edits) is logged as not sent: there is no outbox, cache or event stream. The device data the
shared UI builds (`DeviceData`) is built from the pod the same way: `local://connection`, `local://session`,
`local://inbox`, the layouts with their versions, and, for a layout that binds them, `local:notifications`
(`/v1/notifications`, with `when` and `day` labels in UTC), `local:channels` and `local:sign_ins` /
`local:providers`.

Windows place a laid-out frame again on scroll without laying it out (`Laid::emit`), so dragging is cheap. A drag
that scrolls flings on release and slows down each frame; one that scrolls nothing, past 48 points, is a swipe that
runs the `core://Gesture` under its start (`on_swipe_*`). A focused text field shows a caret and a primary outline.
`pod_shots` (feature `remote`) drives a live session from a script and writes PNGs, to compare with the simulator's
live mode.

## Agent previews (MCP)

`render_layout_preview` (`id`, or a draft `layout` drawn with `data_from`'s data and every feed of home's;
`viewport`, `form`, `dark`) answers an image (`image/webp`, lossless) and a text: a summary line, then JSON with
`preview_id`, `state`, timing and the nodes (id, type, bounds, text, tap, selected). `dispatch_virtual_interaction`
(`preview_id`, `action` tap / scroll / type / back, with `node_id`, `text` or `x`/`y`, `dy`) changes that preview's
device state and answers the same, with the interaction's effects; a tap that would reach the pod is reported as
`not sent`. Layouts the previewed one embeds inline or opens are loaded with it (up to 8). Code: `pod_core::preview`.

## Tests

`cargo test -p construct-render` (`--no-default-features` for the CPU only):

- `layout_math`: positions and sizes (padding, gaps, flex growth, wrapping, semantic bounds equal to quads).
- `golden`: CPU frames against `tests/data/goldens/*.png` (at most 0.2% of pixels off by more than 16 levels), the
  SDF's anti-aliased edges and shadows, and snapshot latency. After a deliberate change:
  `UPDATE_GOLDENS=1 cargo test -p construct-render --test integration golden::`, then look at the PNGs.
- `gpu`: GPU frames against the same goldens (0.2%, 24 levels), and one quad pixel for pixel. Without an adapter they
  skip; `CONSTRUCT_REQUIRE_GPU=1` (CI, with lavapipe) fails instead.
- `interaction`: hit-testing, taps, `action.local.set`, opening and Back, scrolling, resize reflow, window events.

The MCP round trip is `mcp::agents_preview_a_layout_and_tap_it` in the server's integration tests.

Timings for the task-list fixture at 390 × 844 @2x, release build, on a 4-core cloud container: layout 0.4 ms;
CPU raster 9 ms and lossless WebP 3 ms (about 12.5 ms in all); GPU on lavapipe (software Vulkan) 15.6 ms including
readback and WebP. A real GPU has not been measured yet.
