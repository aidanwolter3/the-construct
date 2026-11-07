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
| `interact` | `Session`: a layout with form values, scroll offsets and a back stack; `tap`, `long_press`, `scroll_by`, `type_text`, `dispatch`; text editing (cursor, selection, handles, the edit bar); confirm dialogs; sheet drags and carousel paging; writes through its `Loader` |
| `text` | Fonts, shaping and measuring (cosmic-text); caret stops for editing |
| `dates` | `core://Text` `format`: `YYYY-MM-DD` values as "tomorrow" or "Nov 5", against the device's day (its time zone through jiff) |
| `images` | Images by URL, cut to their box (on a background thread for the GPU) |
| `viewer` | `Controller`: platform events (resize, tap, long press, wheel, text, editing keys, Back) applied to a session; what windows feed |
| `window` | winit + wgpu surface (feature `window`): touch (drags, flings, swipes, long presses, handles), the clipboard, the `Platform` a window runs on; used by `construct-viewer` and Android |
| `remote` | `PodClient`: a live session over a pod (feature `remote`) |
| `pods` | A device's own pod: its saved session, the pod's sign-in screen, leaving and reloading (features `remote` and `window`); shared by Android and the desktop viewer |
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
IconButton (`active`), Chip, Badge, Spacer, Divider, Conditional, Container (children 8 points apart; horizontal
ones wrap), Tappable, Gesture (tap), Scroll (clipped, offsets; with `follow_end`, a tap outside it, such as a
footer's Send, makes it follow its content to the end once that has grown, while a tap inside it, a like, doesn't), Text
`format` (bound `YYYY-MM-DD` values as "today", "in 3 days" or "Nov 5", or `past`, against the device's own day; a
`SceneBuilder` can be given another with `today`), List (`where`, `filter`, `group_by`, `columns`, `focus`, `indent`, `empty_text`), ChoiceChips, TextField
(with its Send button beside it when it has `on_submit`), Switch, Progress, Dock, Layout (inline, from the layouts
given), Carousel (pages side by side, an "n/N" badge and dots; a sideways drag pages it, snapping to a page), and
every named icon (`icons.rs`: the shared UI's Material path data, rasterized with zeno; on the GPU they are glyphon
custom glyphs). Images draw once loaded (over a placeholder in the blurhash's colour; only live sessions load them;
in a window they are cut to their box on a background thread). Removed vocabulary (`core://AppFrame`,
`core://Drawer`) and nodes this build doesn't know are named as the shared UI names them: "Unsupported component:
<type>" in small grey text. Approximated: MediaPicker (the add tile; the picker is its own ticket, #170).
Presented layouts are drawn as the shared UI draws them: a sheet (85% of the height, with a handle), a side panel
(86% of the width, at most 340 points) or an overlay from its edge, over a 32% black scrim that closes it when
tapped; a screen a sheet or panel opens stays inside it. A sheet dragged by its handle strip, or a panel dragged
sideways toward its edge, follows the finger and closes past a quarter of its size or on a flick faster than
500 points per second, else settles back. In a window, layouts and screens slide in and out over
280 ms (`FastOutSlowInEasing`); a fling follows Android's `OverScroller` spline from the release velocity, estimated
over the last 100 ms with Android's impulse strategy (`VelocityTracker`'s and Compose's default), which weighs the
speed at release over the average. A drag's scrolling is applied once per frame, and the GPU renderer keeps shaped text between frames. `action.device` `show_versions` fills `local://versions` and `local:versions` from
`/v1/apps/<id>/versions`; `pin_version` draws that version (`/v1/apps/<id>/versions/<n>`'s layout over the
layout's latest data) wherever the layout shows, home included, until `follow_latest`; pins are kept in `pod.json`
across restarts and forgotten on Leave pod. The session follows the platform's light or dark mode
(`Session::set_dark`). `core://Sticky` pins at the top of its scrolling view, over the page
background, until its parent scrolls away; one that rests within 24 points of the view's top (a column's padding)
pins where it rests, so it never moves (the shared UI moves it up to the top first).

## The desktop viewer

```bash
cargo run --release -p construct-render --features viewer --bin construct-viewer               # your pod
cargo run --release -p construct-render --features viewer --bin construct-viewer -- layout.json  # a file
cargo run --release -p construct-render --features viewer --bin construct-viewer -- --demo       # the demo
```

With no file it opens your pod as the phone app does (`pods`, shared with Android): the pod opened last, its
session kept in `pod.json` (`~/Library/Application Support/The Construct` on macOS, else
`$XDG_CONFIG_HOME/the-construct` or `~/.config/the-construct`); without one, the pod's sign-in screen (staging, or
`CONSTRUCT_POD_URL`). `local:servers` offers Staging, Local (`http://127.0.0.1:8080`) and `CONSTRUCT_POD_URL` when
it is another; `set_server` opens that server's saved session, else its sign-in screen, and `pod.json` keeps each
server's session, code and pins for coming back. The window follows the system's light or dark mode (winit's
theme). Click, scroll, type, Escape for Back; each interaction's effects are logged. Copy and paste use
the system clipboard through `pbcopy`/`pbpaste` on macOS and `wl-copy`, `xclip` or `xsel` on Linux. It draws with
Metal on macOS and Vulkan (or GL) on Linux; on a headless Linux box, `Xvfb` plus Mesa's `mesa-vulkan-drivers`
(lavapipe) is enough.

Any workspace bundle (what `GET /v1/workspaces/<id>` answers) can be drawn to PNGs without a window:
`cargo run --release -p construct-render --no-default-features --example render_bundle -- a.json b.json --out dir`
(390 × 844 at 3×, the simulator's phone, so the two can be compared side by side with `-Pfixture`).

## Android

`scripts/android_native.sh` (no options) cross-compiles `libconstruct_render.so` (cargo-ndk, arm64-v8a), packages
a NativeActivity APK with aapt2 (no Gradle, no Kotlin), signs it with the debug key and installs it on the adb device
**as The Construct itself** (`org.theconstruct.android`), over a debug build of the main app signed with the same
machine's debug key, so the app's data stays. An install over a build signed with another key fails, and the script
says so rather than uninstall (which would sign you out). `cd app && ./gradlew :androidApp:installDebug` goes back to
the Compose app with the same data.

The app opens a pod live (below). It keeps its pod in its private files (`pod.json`: the server, the session token,
the last pod code used, the versions pinned, the device's id and the other servers' sessions; unencrypted, unlike the main app's keystore) and opens, in order: that pod; the main
app's own sign-in, which it reads (`shared_prefs/server.xml` for the server, else staging;
`shared_prefs/construct_secure.xml` for the session, decrypted with the main app's Android keystore key through the
SDK); else the pod's sign-in screen, which the pod answers without a session (a pod code box, Open pod and Try a
temporary pod), with the last code used filled in. Signing in (`action.auth.session`) saves the session; Leave pod
(`leave_pod`, after its `action.confirm` dialog) forgets it and shows the sign-in screen again, as does a session the
pod no longer knows. A pod that can't be reached shows what went wrong with Try again (`refresh`) and Sign in with
another code. A `layout.json` pushed to the external files directory (`{"layout", "data"}`) is drawn instead.

winit makes one event loop per process while Android keeps the process after the activity goes, so the process ends
with its activity (when it is finishing or destroyed) and every launch starts afresh. The app targets SDK 34, so the
system keeps its window below the status bar and camera cutout and resizes it for the keyboard; a tapped text field
opens the keyboard, and Back pops the stack. The layout stays inside the window insets Android reports
(`getRootWindowInsets`: system bars, camera cutout and keyboard), logged as they change. Its theme draws light system
bars with dark icons; in dark mode (`values-night`), dark bars with light icons. The layouts follow the system's mode:
`Configuration.uiMode` read through JNI after each batch of events (the activity handles `uiMode` changes itself,
so the system bars keep the mode the app started in until the next launch). `local:servers` offers the main app's
servers: Staging, Local emulator (`10.0.2.2:8080`), Local USB (`127.0.0.1:8080`, with `adb reverse`) and the main
app's own server when it is another; `set_server` switches as on the desktop.

## Live sessions (`remote`)

Feature `remote` (on in the viewer and the APK) runs a session over a pod: `PodClient` fetches home as a
client does (`GET /v1/workspaces/home`), builds the device's list of layouts (`local:layouts`, `local:apps`) from
`GET /v1/apps?all=1`, and is the session's `Loader`: a layout a tap opens is fetched then, a page screen whose route
matches a tap's `target_uri` opens with its `$route` bound and its `item`, and slides in at once while its `data`
path loads in the background (`local://screen#loading`). `local://nav` and
`local://screen` are set as the shared UI sets them. Images (`core://Image`) download on four background threads, newest request first,
without the session (the pod serves media from public paths), are cut to their box (cropped to fill, corners
rounded) and drawn over their placeholder once they arrive; the window redraws then.

Writes go to the pod as the shared UI sends them, without an outbox: `$form.<f>`, `$form?.<f>` and one `$nonce`
per tap are bound when the tap runs (a blank `$form.<f>` sends nothing). `action.mutation.optimistic` changes the
entity at once (every `value_expr` against the entity as it was), POSTs its payload with `$value` as the new value
on a background thread, and puts the fields back if the pod refuses. `action.mutation.create` adds its entity to its
list at once, POSTs, then loads the layouts on the stack again (a refusal takes it out). `action.device`
`open_notification` marks it read (`POST /v1/notifications/read`), closes the inbox's sheet or panel and opens the
notification's layout and `target_uri` screen; `mark_read` and `mark_all_read` mark them here and on the pod;
`load_older_notifications` adds the next page under the newer ones (`local://inbox` keeps `has_older` and
`next_before`). `open_settings` and `open_inbox` present the `settings` layout as a panel and `inbox` as a sheet.
`toggle_channel` flips the switch at once and tells the pod (`PUT /v1/devices/<id>/channels`, registering the device
with `PUT /v1/devices/<id>` the first time; its id is kept in `pod.json`), flipping it back when refused.
`hide_layout`, `unhide_layout` and `delete_layout` go to the pod (`PUT /v1/apps/<id>/hidden`,
`DELETE /v1/custom-apps/<id>`) and load its list of layouts again; a deleted layout leaves the stack and its pin is
forgotten; home is never hidden or deleted. `set_server` hands the platform another of `local:servers`
(`Effect::SwitchServer`), below. A refused write or device command shows the shared UI's notice bar at the bottom
(the pod's `detail`, else its error, with Dismiss; tagged `notice` and `notice-dismiss`) until dismissed. These
commands block the session while the pod answers, as `show_versions` does. Left to their own tickets: `sign_in` and
`sign_out` (#169), `request_notification_permission` (#168).
`action.link.open` opens an `http(s)` address in the browser (an `ACTION_VIEW` intent on Android, `open` or
`xdg-open` on desktops). `action.confirm` shows the app's own dialog (title, message, Cancel and the confirm label,
tagged `confirm:cancel` and `confirm:ok`) and runs `then`, with the steps of a sequence after it, only on confirm;
Cancel, Back or a tap outside do nothing. `action.auth.session` POSTs its payload and hands the session token to the
platform, which saves it and opens the pod; `leave_pod` and `refresh` go to the platform too. There is no cache or event stream: what other devices change shows on the next load. The
device data the
shared UI builds (`DeviceData`) is built from the pod the same way: `local://connection`, `local://session`,
`local://inbox`, the layouts with their versions, and, for a layout that binds them, `local:notifications`
(`/v1/notifications`, with `when` and `day` labels in UTC), `local:channels` and `local:sign_ins` /
`local:providers`.

Windows place a laid-out frame again on scroll without laying it out (`Laid::emit`), so dragging is cheap. A drag
that scrolls flings on release and slows down each frame; one that scrolls nothing, past 48 points, is a swipe that
runs the `core://Gesture` under its start (`on_swipe_*`). Text fields are edited by the engine itself (a NativeActivity gets key events, not Android's text input, so the
keyboard's own paste and suggestions don't reach them), as Android's own fields behave: a tap puts the cursor where
it lands; the cursor blinks (500 ms); a long press or a double tap selects the word there, with handles to drag and a
Cut / Copy / Paste / Select all bar over it (dragging on from a long press moves the selection's end); a tap on the
cursor shows its handle and the bar (Paste, Select all). Typing and pasting replace the selection and are cut to the
field's `max_length`. With a hardware keyboard: arrows, Home and End (Shift selects), Delete and Ctrl or Command+A, C,
X and V. On Android the clipboard is `ClipboardManager` through the SDK.
`pod_shots` (feature `remote`) drives a live session from a script and writes PNGs, to compare with the simulator's
live mode; `device <command> [app] [id]` runs an `action.device` command and `dark` / `light` set the mode.

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
- `device`: Settings and the inbox presented, notification switches (sent, refused and flipped back), older
  notifications, hiding and deleting layouts, `set_server`, pinned versions, the notice bar, `follow_end`, dark mode,
  `format` dates and unsupported nodes. `pods`' own unit test (switching servers) runs with `--features viewer`.
- `writes`: optimistic changes and creates (sent, refused and undone), form binding, notifications, links, dragging a
  sheet closed, carousel paging, confirm dialogs, signing in, and editing text (cursor, selection, the edit bar,
  `max_length`).

The MCP round trip is `mcp::agents_preview_a_layout_and_tap_it` in the server's integration tests.

Timings for the task-list fixture at 390 × 844 @2x, release build, on a 4-core cloud container: layout 0.4 ms;
CPU raster 9 ms and lossless WebP 3 ms (about 12.5 ms in all); GPU on lavapipe (software Vulkan) 15.6 ms including
readback and WebP. A real GPU has not been measured yet.
