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
| `accessibility` | The semantic tree as an AccessKit tree for screen readers (roles, labels, values, states), and their requests back as taps, scrolls and field values |
| `cpu` | The CPU rasterizer: `sd_rounded_box` and `shade` (the shader's maths), 8-bit premultiplied target, glyphs placed as glyphon places them |
| `gpu` | `Renderer` (any texture view), `HeadlessSurface` (offscreen + readback); feature `gpu` (default) |
| `interact` | `Session`: a layout with form values, scroll offsets and a back stack; `tap`, `long_press`, `scroll_by`, `type_text`, `dispatch`; text editing (cursor, selection, handles, the edit bar); confirm dialogs; sheet drags and carousel paging; writes through its `Loader` |
| `text` | Fonts, shaping and measuring (cosmic-text); caret stops for editing |
| `images` | Images by URL, cut to their box (on a background thread for the GPU) |
| `viewer` | `Controller`: platform events (resize, tap, long press, wheel, text, editing keys, Back) applied to a session; what windows feed |
| `window` | winit + wgpu surface (feature `window`): touch (drags, flings, swipes, long presses, handles), the clipboard, the `Platform` a window runs on; used by `construct-viewer` and Android |
| `engine` | `PodEngine`: a live session over a pod run by the engine (`client-core`: cache, outbox, event stream), the session's `Loader` (feature `remote`); `FileStore`, a desktop's secure store without a keychain |
| `remote` | `PodClient`: a session fetched straight from the pod (`construct-sim`'s live mode), and the device data builders both use (feature `remote`) |
| `pods` | A device's own pod: the pod it opened last, its engine, the pod's sign-in screen, leaving and reloading (features `remote` and `window`); shared by Android and the desktop viewer |
| `platform` | `Platform`: what the app needs from the system it runs on (keyboard, insets, browser, clipboard, photo picker, events, the pod's saved session), implemented per platform |
| `android` | `android_main` for the NativeActivity (Android builds with `window`) |
| `bridge` | The Rust side of the Android bridge (`android/java`): links, the photo picker's result, messages |
| `push` | Push while the app is closed: Web Push keys and decryption, endpoints and notification taps (feature `remote`); on Android the UnifiedPush receiver's native methods, registration and notifications |

The pod builds the crate with `default-features = false`: no wgpu, CPU only.

Fonts are bundled in `fonts/` and nothing else is loaded: Instrument Sans (regular, bold), Lora (the Display
style) and `DejaVuSans-Symbols.ttf`, DejaVu Sans cut to punctuation, arrows, shapes, symbols and dingbats.
cosmic-text falls back to any loaded font with the glyph, so ♥, ✓ and ★ come from the last one.

## Semantic ids

What agents and tests address: a node's `tag` (bound, e.g. `task_$item.id`); `<field>=<value>` for a
`core://ChoiceChips` option or a `core://Dock` tab (`filter=started`, `filter=` for the empty value);
`field:<name>` for a `core://TextField`; `list:<query_key>`, `chips:<field>`, `layout:<id>`; otherwise the node's
path from the root (`root/0/2/i3/0`, `i3` being a list's fourth item). Bounds are logical points.

## Screen readers

The window serves screen readers from the semantic tree through AccessKit (`accessibility`, decision
`rust-client-accessibility`): VoiceOver on macOS and Narrator on Windows (`accesskit_winit`). Not Orca on Linux yet:
AccessKit's AT-SPI adapter depends on `zvariant`, unmaintained since it moved into zbus 6 (RUSTSEC-2026-0370), so it
is left off until AccessKit moves too. Each frame's tree is sent while one is listening. Roles follow the node type:
a button, icon button, tappable or dock tab is a button (a tab is selected when on); a switch is a switch with its
state; a `core://ChoiceChips` is a radio group of its options; a text field is a text input with its value, named by
its placeholder, and focused while typed into; a scroll is a scroll view; an image is named by its
`content_description`; other text is a label. As in Compose, a tappable's texts fold into it: it is read as one
element, named by its own description, else by its texts. Under a dialog's or panel's scrim nothing is listed.
Activating an element taps it by its semantic id (`Session::tap`), Focus on a field focuses it, a value set on a
field replaces it, and Scroll up or down moves a scroll view by most of its height. AccessKit's ids are hashes of
the semantic ids, so a screen reader keeps its place from frame to frame.

Android isn't served yet: AccessKit's Android adapter needs a Java class (a prebuilt dex, or the app's own Java)
and a GameActivity, and the app is a NativeActivity with no Java (issue #172).

## Coverage

Drawn like the app: Page, Workspace (title and slots), Column, Row, Container, Box, Text (styles, `count`), Button,
IconButton (`active`), Chip, Badge, Spacer, Divider, Conditional, Container (children 8 points apart; horizontal
ones wrap), Tappable, Gesture (tap), Scroll (clipped,
offsets), List (`where`, `filter`, `group_by`, `columns`, `focus`, `indent`, `empty_text`), ChoiceChips, TextField
(with its Send button beside it when it has `on_submit`), Switch, Progress, Dock, Layout (inline, from the layouts
given), Carousel (pages side by side, an "n/N" badge and dots; a sideways drag pages it, snapping to a page), and
every named icon (`icons.rs`: the shared UI's Material path data, rasterized with zeno; on the GPU they are glyphon
custom glyphs). Images draw once loaded (over a placeholder in the blurhash's colour; only live sessions load them;
in a window they are cut to their box on a background thread). Approximated: MediaPicker (the add tile),
Drawer (the selected page), AppFrame and unknown nodes (a labelled box). Not drawn: `format` dates.
Presented layouts are drawn as the shared UI draws them: a sheet (85% of the height, with a handle), a side panel
(86% of the width, at most 340 points) or an overlay from its edge, over a 32% black scrim that closes it when
tapped; a screen a sheet or panel opens stays inside it. A sheet dragged by its handle strip, or a panel dragged
sideways toward its edge, follows the finger and closes past a quarter of its size or on a flick faster than
500 points per second, else settles back. In a window, layouts and screens slide in and out over
280 ms (`FastOutSlowInEasing`); a fling follows Android's `OverScroller` spline from the release velocity, estimated
over the last 100 ms with Android's impulse strategy (`VelocityTracker`'s and Compose's default), which weighs the
speed at release over the average. A drag's scrolling is applied once per frame, and the GPU renderer keeps shaped text between frames. `action.device` `show_versions` fills `local://versions` and `local:versions` from
`/v1/apps/<id>/versions`; `pin_version` and `follow_latest` mark the choice there, but the layout still shows its
latest version. `core://Sticky` pins at the top of its scrolling view, over the page
background, until its parent scrolls away; one that rests within 24 points of the view's top (a column's padding)
pins where it rests, so it never moves (the shared UI moves it up to the top first).

## The desktop viewer

```bash
cargo run --release -p construct-render --features viewer --bin construct-viewer               # your pod
cargo run --release -p construct-render --features viewer --bin construct-viewer -- layout.json  # a file
cargo run --release -p construct-render --features viewer --bin construct-viewer -- --demo       # the demo
```

With no file it opens your pod as the phone app does (`pods`, shared with Android): the pod opened last
(`pod.json`, in `~/Library/Application Support/The Construct` on macOS, else `$XDG_CONFIG_HOME/the-construct` or
`~/.config/the-construct`), with the engine's cache next to it (`engine/<server>/`) and its session in the login
Keychain on macOS, else in `secure.json` there (readable by you only); without a session, the pod's sign-in screen
(staging, or `CONSTRUCT_POD_URL`). Click, scroll, type, Escape for Back; each interaction's effects are logged. Copy and paste use
the system clipboard through `pbcopy`/`pbpaste` on macOS and `wl-copy`, `xclip` or `xsel` on Linux. It draws with
Metal on macOS and Vulkan (or GL) on Linux; on a headless Linux box, `Xvfb` plus Mesa's `mesa-vulkan-drivers`
(lavapipe) is enough.

Any workspace bundle (what `GET /v1/workspaces/<id>` answers) can be drawn to PNGs without a window:
`cargo run --release -p construct-render --no-default-features --example render_bundle -- a.json b.json --out dir`
(390 × 844 at 3×, the simulator's phone).

## Android

`scripts/android_native.sh` (no options) cross-compiles `libconstruct_render.so` (cargo-ndk, arm64-v8a), packages
a NativeActivity APK with aapt2 (no Gradle, no Kotlin), signs it with the debug key and installs it on the adb device
**as The Construct itself** (`org.theconstruct.android`), over a debug build of the main app signed with the same
machine's debug key, so the app's data stays. An install over a build signed with another key fails, and the script
says so rather than uninstall (which would sign you out). `cd app && ./gradlew :androidApp:installDebug` goes back to
the Compose app with the same data.

The app opens a pod live (below), with the engine's cache in its private files (`engine/<server>/`). It keeps its
pod there too (`pod.json`: the server and the last pod code used), else uses the main app's server
(`shared_prefs/server.xml`, else staging). Sessions are kept as the main app keeps them (`KeystoreStore`, through
JNI: AES/GCM with the Android keystore key `construct_session_key`, in the private SharedPreferences file
`construct_secure`), so the main app's sign-in carries over, and a session token an older build kept in `pod.json`
moves there on the first start. Without a session the pod answers its sign-in screen (a pod code box, Open pod and
Try a temporary pod), with the last code used filled in. Signing in (`action.auth.session`) keeps the session;
Leave pod (`leave_pod`, after its `action.confirm` dialog) forgets it and shows the sign-in screen again, as does a
session the pod no longer knows. A `layout.json` pushed to the external files directory (`{"layout", "data"}`) is
drawn instead.

winit makes one event loop per process while Android keeps the process after the activity goes, so the process ends
with its activity (when it is finishing or destroyed) and every launch starts afresh. The app targets SDK 34, so the
system keeps its window below the status bar and camera cutout and resizes it for the keyboard; a tapped text field
opens the keyboard, and Back pops the stack. The layout stays inside the window insets Android reports
(`getRootWindowInsets`: system bars, camera cutout and keyboard), logged as they change. Its theme draws light system
bars with dark icons.

### The Android bridge

A NativeActivity gets none of the Android callbacks that arrive through Java: a link opening the app, an activity
result, a broadcast while the app is closed. `android/java` holds a small bridge that passes them to Rust and holds no
app logic: `ConstructActivity` (NativeActivity plus `onNewIntent` and `onActivityResult`, and `pickImages` and
`askPermission` that Rust calls), `ConstructReceiver` (a message, also while the app is closed), `PushReceiver` and
`NativePush` (push, below) and `Native` (the
native methods, `Java_org_theconstruct_android_Native_*` in `src/bridge.rs`). The script compiles them with `javac`
and `d8` into the APK's `classes.dex`. Events queue in `bridge.rs`, wake the window and reach it through
`Platform::events`; for now each shows as a notice. Decision:
[`android-native-bridge`](decisions/android-native-bridge.md). Try it with the app installed:

```bash
adb shell am start -a android.intent.action.VIEW -d "construct://bridge/hello"     # a link opens the app
# Pixelfed → New post → the Add photos tile: Android's photo picker; the count and size come back to Rust
# Swipe the app away, then (only adb and the system may send this):
adb shell am broadcast -n org.theconstruct.android/.ConstructReceiver --es message "Hello"   # a notification
```

#### Push

The pod's Web Push reaches the app while it is closed through a UnifiedPush distributor (ntfy, ...): `PushReceiver`
extends the UnifiedPush connector's receiver (2.5.0; the script adds its classes and the Kotlin standard library's
to the APK, downloaded once from Maven Central and checked by hash) and forwards each callback to `NativePush`
(`src/push.rs`). Rust keeps a key pair per server (`push.json`), sends the endpoint to the pod, decrypts messages,
posts them (channel `<app_id>.<channel>`) and opens a tapped one's layout and screen; a tap link only opens a
notification this app posted. Nothing is posted while the app shows that pod. `request_notification_permission`
asks for the permission. Decision: [`android-native-push-unifiedpush`](decisions/android-native-push-unifiedpush.md).
Without a distributor (no FCM fallback yet), notifications show only while the app runs. To try it: install ntfy,
open the app signed in to a pod (logcat: `push: asked the distributor`, then `push: new endpoint`), swipe it away,
and make the pod notify (a matched event); the notification opens the event.

The platform interface (`platform.rs`: `Platform`, `PlatformEvent`, `Reopen`) is what each platform implements
(Android, the desktop viewer; a web build later).

## Live sessions (`remote`)

Feature `remote` (on in the viewer and the APK) runs a session over a pod with the engine the Compose clients run
(`client-core`, decision `rust-clients-on-the-engine`): `PodEngine` is the session's `Loader`. Home shows as the
engine has it, from its cache at once (offline too; "Opening your pod…" with the connection's state on a first
start, until the pod answers), and the device's list of layouts (`local:layouts`, `local:apps`) comes from
`GET /v1/apps?all=1`, kept in `apps.json` next to the cache for offline starts. A layout a tap opens is the engine's
cached copy at once, fetched again behind it (`open_layout`; a layout it never had waits for the pod), and a page
screen whose route matches a tap's `target_uri` opens with its `$route` bound and its `item`, and slides in at once
while its `data` path loads through the engine (`action.data.load`, cached; `local://screen#loading`). Every
snapshot the engine sends reaches the session on its next frame (`Loader::changes`; the window is woken): data,
`local://connection` (online, offline or connecting, and how many writes wait), and the new root of every layout
the event stream announced, so a layout edited over MCP, or a change from another device, shows without opening it
again. The simulator's live mode (`construct-sim --base-url`) still fetches straight from the pod (`PodClient`). `local://nav` and
`local://screen` are set as the shared UI sets them. Images (`core://Image`) download on four background threads, newest request first,
without the session (the pod serves media from public paths), are cut to their box (cropped to fill, corners
rounded) and drawn over their placeholder once they arrive; the window redraws then.

Writes go to the engine's outbox, as the shared UI sends them (`Loader::take`): `$form.<f>`, `$form?.<f>` and one
`$nonce` per tap are bound when the tap runs (a blank `$form.<f>` sends nothing). `action.mutation.optimistic`
changes the entity at once (every `value_expr` against the entity as it was) and the engine keeps the write until
the pod has it, offline or through a failed POST, with `$value` as the new value; a refusal puts the fields back.
`action.mutation.create` adds its entity to its list at once, and the engine loads its list again once the pod has
it (a refusal takes it out). A loader without an outbox (`PodClient`) POSTs on a background thread instead
(`Loader::send`) and the session undoes a refusal itself. `action.device`
`open_notification` marks it read (`POST /v1/notifications/read`), closes the inbox's sheet or panel and opens the
notification's layout and `target_uri` screen; `mark_read` and `mark_all_read` mark them here and on the pod.
`action.link.open` opens an `http(s)` address in the browser (an `ACTION_VIEW` intent on Android, `open` or
`xdg-open` on desktops). `action.confirm` shows the app's own dialog (title, message, Cancel and the confirm label,
tagged `confirm:cancel` and `confirm:ok`) and runs `then`, with the steps of a sequence after it, only on confirm;
Cancel, Back or a tap outside do nothing. `action.auth.session` goes to the engine, which opens the session and
keeps it in its secure store; the platform then remembers the code and shows the pod. `leave_pod` (the engine
forgets the session) and `refresh` (home fetched again) go to the platform too. The device data the shared UI builds
(`DeviceData`) is built the same way: `local://connection`, `local://session` (from the engine's sign-in state),
`local://inbox` (the engine's unread count), the layouts with their versions, and, for a layout that binds them,
`local:notifications` (the engine's list, cached; `when` and `day` labels in UTC; loaded again when the count
changes), `local:channels` and `local:sign_ins` / `local:providers`. Marking notifications read goes through the
engine, which sends it again when the pod is back.

Windows place a laid-out frame again on scroll without laying it out (`Laid::emit`), so dragging is cheap. A drag
that scrolls flings on release and slows down each frame; one that scrolls nothing, past 48 points, is a swipe that
runs the `core://Gesture` under its start (`on_swipe_*`). Text fields are edited by the engine itself (a NativeActivity gets key events, not Android's text input, so the
keyboard's own paste and suggestions don't reach them), as Android's own fields behave: a tap puts the cursor where
it lands; the cursor blinks (500 ms); a long press or a double tap selects the word there, with handles to drag and a
Cut / Copy / Paste / Select all bar over it (dragging on from a long press moves the selection's end); a tap on the
cursor shows its handle and the bar (Paste, Select all). Typing and pasting replace the selection and are cut to the
field's `max_length`. With a hardware keyboard: arrows, Home and End (Shift selects), Delete and Ctrl or Command+A, C,
X and V. On Android the clipboard is `ClipboardManager` through the SDK.
The headless simulator, `construct-sim` ([simulator.md](simulator.md)), drives a session from a script on fixture
bundles or a live pod and writes PNGs drawn on the CPU.

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
- `writes`: optimistic changes and creates (sent, refused and undone), form binding, notifications, links, dragging a
  sheet closed, carousel paging, confirm dialogs, signing in, and editing text (cursor, selection, the edit bar,
  `max_length`).

The MCP round trip is `mcp::agents_preview_a_layout_and_tap_it` in the server's integration tests.

Timings for the task-list fixture at 390 × 844 @2x, release build, on a 4-core cloud container: layout 0.4 ms;
CPU raster 9 ms and lossless WebP 3 ms (about 12.5 ms in all); GPU on lavapipe (software Vulkan) 15.6 ms including
readback and WebP. A real GPU has not been measured yet.
