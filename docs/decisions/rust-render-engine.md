# A Rust rendering engine next to the Compose UI, starting with agent previews

Date: 2026-11-07. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md), [mcp-connector](mcp-connector.md),
[layouts-not-apps](layouts-not-apps.md).

Layouts are drawn only by the shared Compose UI (`app/shared`), on the JVM (desktop, the simulator) and on Android.
That leaves three gaps. An agent editing a layout over MCP can't see what it made: drawing it means a JVM with
Skiko, seconds per frame and hundreds of megabytes, which the pod can't host. Every UI primitive is written once
for Compose and its behaviour (sizes, bindings) lives only in Kotlin. And a web client would need another UI
target. Considered: a headless Compose renderer in the pod (too heavy, and the pod image is distroless), HTML
rendered by a headless browser (another layout implementation that drifts from the app's), and a Rust renderer
over the engine's own types.

Decision: a Rust rendering engine, crate `construct-render`, drawing the plumbing vocabulary
(`protocol::UiNode`) with its data:

- **Layout** with taffy (flexbox): each node type maps to a flex style with the Compose renderer's Material 3
  metrics; text is measured with cosmic-text over fonts bundled in the crate (Instrument Sans, Lora, and a
  symbol subset of DejaVu Sans for characters such as ♥ and ✓; no system fonts), so a frame measures the same on
  every machine.
- **Scene**: instanced rounded-box quads (`QuadInstance`: rect, fill, border, per-corner radii, drop shadow,
  clip), text runs in paint order, and a semantic tree (`SemanticNode`: id, type, bounds in points, text, what a
  tap does). Ids are a node's `tag`, `<field>=<value>` for chip options and dock tabs, `field:<name>` for text
  fields, else the node's path.
- **Two rasterizers of one distance function**: wgpu (feature `gpu`; `quad.wgsl` evaluates the rounded-box SDF
  per fragment, glyphon draws text) and a CPU rasterizer (always built) evaluating the same function in device
  pixels. The distance is computed in pixels, so the edge ramp is one pixel at any scale without `fwidth`, and
  both paths agree to the pixel on quads. CPU frames are the goldens; GPU frames must match them within 0.2%.
- **Interaction** (`Session`): device state only, as the app keeps it: form values, chips, `action.local.set`,
  opening a layout onto a back stack, scrolling. Intents that would reach the pod or the platform (mutations,
  sign-in, device commands) are reported, never sent.
- **Agents first**: the pod's MCP tools `render_layout_preview` and `dispatch_virtual_interaction` fetch a layout
  as a client does (`GET /v1/workspaces/<id>` through the pod's router, as the owner), draw it on the CPU and answer
  a lossless WebP with the semantic tree. A preview's state is kept in memory (the pod's last four).
- **Native clients as a prototype**: a winit window (`construct-viewer`, Metal on macOS, Vulkan or GL on Linux)
  and a NativeActivity APK (`scripts/android_native.sh`) draw the same scenes through the same input controller.

The Compose clients stay the product. Nothing on the wire changes and no client update is needed: the engine
draws today's plumbing, and new vocabulary still lands in Compose first.

Icons are the shared UI's own Material path data (`icons.rs`, generated from `ConstructIcons.kt`).

The viewer and the APK can run a pod live but read-only (feature `remote`): layouts, screens, their data and images
are fetched as they open; nothing is sent back.

Not yet: dialogs are not drawn, a pinned version is not shown, carousels show their first item, and dates are not formatted. The native window and
APK talk to no engine (`client-core`): no cache, outbox, event stream or upstream sign-in, so taps that change data
are not sent. Moving the clients onto this engine, the
web target and a shared golden suite with the simulator are later decisions.
