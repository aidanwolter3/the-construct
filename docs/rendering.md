# The Rust rendering engine

`crates/construct-render` draws layouts without the JVM: a plumbing `UiNode` tree and its data are laid out with
taffy, turned into a scene of rounded-box quads, text runs and a semantic tree, and drawn by wgpu or by a CPU
rasterizer of the same distance function. It is The Construct's only renderer: the desktop app (`construct-viewer`),
the Android app (a NativeActivity), the headless simulator and the pod's layout previews for agents all draw with
it. Decisions: [`rust-render-engine`](decisions/rust-render-engine.md), [`rust-clients`](decisions/rust-clients.md).

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
| `layout` | `SceneBuilder::build(&Input)`, and `relayout` over the frame before (list items whose data didn't change are taken over, nodes and measured layout alike: decision `held-local-sets-and-list-item-reuse`): every plumbing node to a taffy style (Material 3 metrics, as the Compose renderer had), bindings resolved (`bind`), lists filtered (`where`, `filter`, `group_by`, grids), `$count`, theme tokens |
| `scene` | `QuadInstance` (the GPU vertex layout), `TextRun`, `SemanticNode`, `hit_test`, `find_node`, `find_text` |
| `accessibility` | The semantic tree as an AccessKit tree for screen readers (roles, labels, values, states), and their requests back as taps, scrolls and field values |
| `cpu` | The CPU rasterizer: `sd_rounded_box` and `shade` (the shader's maths), 8-bit premultiplied target, glyphs placed as glyphon places them |
| `gpu` | `Renderer` (any texture view), `HeadlessSurface` (offscreen + readback); feature `gpu` (default) |
| `interact` | `Session`: a layout with form values, scroll offsets and a back stack; `tap`, `long_press`, `scroll_by`, `type_text`, `dispatch`; text editing (cursor, selection, handles, the edit bar); confirm dialogs; sheet drags, carousel paging and chip rows scrolled sideways (`slide_at`, `drag_slide`); writes through its `Loader`; photos for a media picker (`interact/media.rs`: staged through the `Loader`, the desktop's file dialog) |
| `text` | Fonts, shaping and measuring (cosmic-text); caret stops for editing |
| `dates` | `core://Text` `format`: `YYYY-MM-DD` values as "tomorrow" or "Nov 5", against the device's day (its time zone through jiff) |
| `images` | Images by URL, cut to their box (on a background thread for the GPU) |
| `perf` | Frame diagnostics: each window frame's timings and counts, one log line per burst of frames ([below](#frame-diagnostics)) |
| `viewer` | `Controller`: platform events (resize, tap, long press, wheel, text, editing keys, Back) applied to a session; what windows feed |
| `window` | winit + wgpu surface (feature `window`): touch (drags, flings, swipes, long presses, handles), the clipboard, the `Platform` a window runs on; used by `construct-viewer` and Android |
| `engine` | `PodEngine`: a live session over a pod run by the engine (`client-core`: cache, outbox, event stream), the session's `Loader` (feature `remote`); `FileStore`, a desktop's secure store without a keychain |
| `remote` | `PodClient`: a session fetched straight from the pod (`construct-sim`'s live mode), and the device data builders both use (feature `remote`) |
| `pods` | A device's own pod: the pod it opened last, its engine, the pod's sign-in screen, leaving and reloading (features `remote` and `window`); shared by Android and the desktop viewer |
| `signin` | `SignIn`: how a platform signs in to an upstream (its browser, and the redirect for a server: loopback on desktops, the App Link or `construct://oauth` on Android) (feature `remote`) |
| `platform` | `Platform`: what the app needs from the system it runs on (keyboard, insets, browser, clipboard, photo picker, events, the pod's saved session), implemented per platform |
| `android` | `android_main` for the NativeActivity (Android builds with `window`) |
| `bridge` | The Rust side of the Android bridge (`android/java`): links (the sign-in redirect), the photo picker's result, messages |
| `push` | Push while the app is closed: Web Push keys and decryption, endpoints and notification taps (feature `remote`); on Android the UnifiedPush receiver's native methods, registration and notifications |
| `ime` | The keyboard's input connection: the focused field as a keyboard sees it (`TextState`: value, selection, the word being composed), the `InputConnection` edits over it in UTF-16 offsets, and the hand-off between the keyboard and the window (`Connection`); on Android, `NativeText`'s natives |
| `picked` | The photos Android's picker returned, read through the content resolver into files the engine stages (HEIC and other formats it can't decode become JPEG through `ImageDecoder`) |

The pod builds the crate with `default-features = false`: no wgpu, CPU only.

Fonts are bundled in `fonts/` and nothing else is loaded: Instrument Sans (regular, bold), Lora (the Display
style), and three fallbacks: `DejaVuSans-Symbols.ttf`, DejaVu Sans cut to punctuation, arrows, shapes, symbols,
dingbats and number forms (fractions such as ⅓ and ⅝, which Instrument Sans lacks); `DejaVuSans-Bold-Fractions.ttf`,
the bold cut of just the fractions, superscripts and subscripts, so bold text keeps bold fractions; and
`NotoEmoji-Regular.ttf`, monochrome Noto Emoji cut to what DejaVu lacks (⏰, 📅). cosmic-text falls back to a loaded
font with the glyph, DejaVu Sans of the matching weight first, so ♥, ✓, ★ and ⅓ come from DejaVu. To change a cut,
subset the font with fontTools' `pyftsubset` and keep its license next to it.

## Semantic ids

What agents and tests address: a node's `tag` (bound, e.g. `task_$item.id`); `<field>=<value>` for a
`core://ChoiceChips` option or a `core://Dock` tab (`filter=started`, `filter=` for the empty value);
`field:<name>` for a `core://TextField` (`clear:<name>` its x, `send:<name>` its Send); `list:<query_key>`, `chips:<field>`, `layout:<id>`; otherwise the node's
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
ones wrap), Tappable (while a finger or the mouse button is down on it, a wash of the text colour at 10% darkens it on
a light page and lightens it on a dark one, rounded as its one child is; it goes when the finger lifts or the touch
becomes a scroll or swipe: `Session::press` / `release_press`), Gesture (tap), Scroll (clipped, offsets; with `follow_end`, a tap outside it, such as a
footer's Send, makes it follow its content to the end once that has grown, while a tap inside it, a like, doesn't), Text
`format` (bound `YYYY-MM-DD` values as "today", "in 3 days" or "Nov 5", or `past`, against the device's own day; a
`SceneBuilder` can be given another with `today`), List (`where`, `filter`, `group_by`, `columns`, `focus`, `indent`, `empty_text`), ChoiceChips (a row as wide as its place; chips past its
edge scroll sideways under a finger, a sideways wheel or trackpad, or Shift and the wheel), TextField
(with its Send button beside it when it has `on_submit`), Switch, Progress, Dock, Layout (inline, from the layouts
given), Carousel (pages side by side, an "n/N" badge and dots; a sideways drag pages it, snapping to a page), and
every named icon (`icons.rs`: Material path data, rasterized with zeno; on the GPU they are glyphon
custom glyphs). Images draw once loaded (over a placeholder in the blurhash's colour; only live sessions load them;
in a window they are cut to their box on a background thread; `file://` ones, staged photos, are read from disk, only
from the engine's attachments folder: another local file a layout or a pod's data names draws its placeholder).
MediaPicker as the Compose UI drew it: a thumbnail per staged photo with a remove button, then the add tile while there is
room, wrapping. Its tile opens the platform's photo picker (Android's, through the bridge) or the desktop's file
dialog (`osascript` on macOS, `zenity` or `kdialog` on Linux); each photo is staged with the engine
(`stage_attachment`, D49) and its id kept in the field, and a create's `attachments` hands them to the engine, which
uploads them before the POST ([posting.md](posting.md)). Removed vocabulary (`core://AppFrame`, `core://Drawer`) and
nodes this build doesn't know are named as the Compose UI named them: "Unsupported component: <type>" in small grey
text.
Presented layouts are drawn as the Compose UI drew them: a sheet (85% of the height, with a handle), a side panel
(86% of the width, at most 340 points) or an overlay from its edge, over a 32% black scrim that closes it when
tapped; a screen a sheet or panel opens stays inside it. A sheet dragged by its handle strip, or a panel dragged
sideways toward its edge, follows the finger and closes past a quarter of its size or on a flick faster than
500 points per second, else settles back. In a window, layouts and screens slide in and out over
280 ms (`FastOutSlowInEasing`). Scrolling's physics are `scroll.rs` (decision `scroll-physics`), which reads no
clock, so gestures replay in its unit tests: the release velocity is a least-squares fit over the last 100 ms (at
least 3 samples; none after a 40 ms pause), counting moves that arrive in one burst as one sample, since Android
delivers the moves queued during a frame together; a fling follows Android's `OverScroller` curve (Flutter's closed
form, starting at the release speed), 50 to 8000 points per second; a finger landing on a fling faster than 50 points
per second stops it without tapping. Past an end, a view whose content scrolls stretches as Android 12's do: a pull
stretches it (pushing back undoes that first), letting go springs it back, and a fling that meets the end bumps it;
the content draws scaled along the axis from that end (`stretch:<id>` among the scroll offsets, up to about 10%;
images move but keep their size, since each size is cut anew). A mouse wheel notch glides 64 points over about
120 ms; trackpad deltas apply as they come (macOS adds its own momentum). A drag's scrolling is applied once per frame, and the GPU renderer keeps shaped text between frames. `action.device` `show_versions` fills `local://versions` and `local:versions` from
`/v1/apps/<id>/versions`; `pin_version` draws that version (`/v1/apps/<id>/versions/<n>`'s layout over the
layout's latest data) wherever the layout shows, home included, until `follow_latest`, and new versions published
meanwhile don't replace it; the engine keeps pins across restarts (`set_pinned_version`) and forgets them on Leave
pod. The session follows the platform's light or dark mode (`Session::set_dark`). Dark mode draws the theme's dark palette
(decision `dark-palette`): the background is the `page` token, dialogs, sheets, panels and the edit bar draw on
`card`, and each mode takes only its own token values from `core://theme` (`<token>`, `<token>_dark`), any token
the pod sends included. `core://Sticky` pins at the top of its scrolling view, over the page
background, until its parent scrolls away; one that rests within 24 points of the view's top (a column's padding)
pins where it rests, so it never moves (the Compose UI moved it up to the top first).

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
(staging, or `CONSTRUCT_POD_URL`). `local:servers` offers Staging, Local (`http://127.0.0.1:8080`) and
`CONSTRUCT_POD_URL` when it is another; `set_server` opens that server (its session and last pod code are in the
secure store already). `pod.json` keeps only the server: a pod code is the login itself, so it lives in the secure store
and never in `pod.json` or a log. Plain `http://` is refused unless the server is this machine, 10.0.2.2 or a private
network address (10/8, 172.16/12, 192.168/16, link-local, IPv6 unique local): a refused `CONSTRUCT_POD_URL` or saved
server falls back to staging, and `set_server` to one stays put. The window follows the system's light or dark mode (winit's theme).
Signing in to Pixelfed from Settings opens the system browser, and the redirect comes back to a loopback listener
the engine runs (`http://127.0.0.1:47823/callback`, another port when that one is busy; below).
Click, scroll, type, Escape for Back; each interaction's effects are logged by name only (never a typed value or a
token). Copy and paste use
the system clipboard through `pbcopy`/`pbpaste` on macOS and `wl-copy`, `xclip` or `xsel` on Linux. It draws with
Metal on macOS and Vulkan (or GL) on Linux; on a headless Linux box, `Xvfb` plus Mesa's `mesa-vulkan-drivers`
(lavapipe) is enough.

Any workspace bundle (what `GET /v1/workspaces/<id>` answers) can be drawn to PNGs without a window:
`cargo run --release -p construct-render --no-default-features --example render_bundle -- a.json b.json --out dir`
(390 × 844 at 3×, the simulator's phone).

## Android

`scripts/android_native.sh` (no options) cross-compiles `libconstruct_render.so` (cargo-ndk, arm64-v8a), packages
a NativeActivity APK with aapt2 (no Gradle, no Kotlin), signs it with the debug key and installs it on the adb device
as The Construct (`org.theconstruct.android`), keeping the data of a build signed with the same key (an earlier one,
or the Compose app it replaced). An install over a build signed with another key fails, and the script says so
rather than uninstall (which would sign you out). `--release` builds a release APK instead (not debuggable, signed
with the release key; [clients.md](clients.md#releases)).

The app opens a pod live (below), with the engine's cache in its private files (`engine/<server>/`). It keeps its
pod's server there too (`pod.json`), else uses the Compose app's saved server (`shared_prefs/server.xml`, else
staging). Sessions, and the last pod code used with each server, are kept as the Compose app kept them
(`KeystoreStore`, through JNI: AES/GCM with the Android keystore key `construct_session_key`, in the private
SharedPreferences file `construct_secure`), so its sign-in carries over; a session token or pod codes an older build kept in
`pod.json` move there on the first start. Without a session the pod answers its sign-in screen (a pod code box, Open pod and
Try a temporary pod), with the last code used filled in. Signing in (`action.auth.session`) keeps the session;
Leave pod (`leave_pod`, after its `action.confirm` dialog) forgets it and shows the sign-in screen again, as does a
session the pod no longer knows. A `layout.json` (`{"layout", "data"}`) in the app's private `files/dev/` is drawn
instead (development; with `frame-request` next to it, it also saves `frame.png` there). It is never read from external
storage, which other apps can write on Android 9 and 10. Copy one in with
`adb push layout.json /data/local/tmp/ && adb shell run-as org.theconstruct.android sh -c 'mkdir -p files/dev && cp /data/local/tmp/layout.json files/dev/'`
(a development build is debuggable; a release build is not, so this needs one). It allows no backups or
device-to-device transfers (`allowBackup="false"` and `android/res/xml/data_extraction_rules.xml`): `pod.json`, the
engine's cache and the push keys stay on the device.

winit makes one event loop per process while Android keeps the process after the activity goes, so the process ends
with its activity (when it is finishing or destroyed) and every launch starts afresh. The app targets SDK 34, so the
system keeps its window below the status bar and camera cutout and resizes it for the keyboard; a tapped text field
opens the keyboard, and Back pops the stack. The layout stays inside the window insets Android reports
(`getRootWindowInsets`: system bars, camera cutout and keyboard), logged as they change. Its theme draws light system
bars with dark icons; in dark mode (`values-night`), dark bars with light icons. The layouts follow the system's
mode: `Configuration.uiMode` read through JNI after each batch of events. The activity handles `uiMode` changes
itself (no restart), so it sets the system bars' colour and icons again in `onConfigurationChanged`, as the theme
would. `local:servers` offers Staging, Local emulator (`10.0.2.2:8080`), Local USB (`127.0.0.1:8080`, with
`adb reverse`) and the saved server when it is another; `set_server` switches as on the desktop.

### The Android bridge

A NativeActivity gets none of the Android callbacks that arrive through Java: a link opening the app, an activity
result, a broadcast while the app is closed. `android/java` holds a small bridge that passes them to Rust and holds no
app logic: `ConstructActivity` (NativeActivity plus `onNewIntent` and `onActivityResult`, and `pickImages` and
`askPermission` that Rust calls), `ConstructReceiver` (a message, also while the app is closed), `PushReceiver` and
`NativePush` (push, below), `Native` (the
native methods, `Java_org_theconstruct_android_Native_*` in `src/bridge.rs`), and the keyboard's `TextInputView`,
`TextInput` and `NativeText` (`src/ime.rs`, above). The script compiles them with `javac`
and `d8` into the APK's `classes.dex`. Events queue in `bridge.rs`, wake the window and reach it through
`Platform::events`: a sign-in redirect goes to the engine's sign-in (below); picked photos go to the media picker
that asked (read by `picked.rs`); other links and messages show as a notice for now. Decision:
[`android-native-bridge`](decisions/android-native-bridge.md). Try it with the app installed:

```bash
adb shell am start -a android.intent.action.VIEW -d "construct://bridge/hello"     # a link opens the app
# Pixelfed → New post → the Add photos tile: Android's photo picker; the photos show as thumbnails, Share posts them
# Swipe the app away, then (only adb and the system may send this):
adb shell am broadcast -n org.theconstruct.android/.ConstructReceiver --es message "Hello"   # a notification
```

#### Push

The pod's Web Push reaches the app while it is closed through a UnifiedPush distributor: an app such as ntfy if one
is installed, else the embedded FCM distributor (Play services, with the pod's VAPID key; no Firebase project).
`PushReceiver` extends the UnifiedPush connector's receiver (3.3.5; the script adds its classes, the embedded FCM
distributor's and the Kotlin libraries' to the APK, downloaded once from Maven Central and checked by hash) and
forwards each callback to `NativePush` (`src/push.rs`); `PushKeys` is the connector's key store, forwarding to Rust
too. Rust keeps a key pair per server (`push.json`), sends the endpoint to the pod, decrypts messages,
posts them (channel `<app_id>.<channel>`) and opens a tapped one's layout and screen; a tap link only opens a
notification this app posted. Nothing is posted while the app shows that pod. `request_notification_permission`
asks for the permission. Decisions: [`android-native-push-unifiedpush`](decisions/android-native-push-unifiedpush.md),
[`android-native-push-embedded-fcm`](decisions/android-native-push-embedded-fcm.md). On a phone without a distributor
app or Play services, notifications show only while the app runs. To try it: install ntfy,
open the app signed in to a pod (logcat: `push: asked the distributor`, then `push: new endpoint`), swipe it away,
and make the pod notify (a matched event); the notification opens the event. Without ntfy, on a phone with Play services, the embedded FCM
distributor takes over (logcat `push: distributor org.theconstruct.android`, then `push: new endpoint for … at
fcm.googleapis.com`; the distributor logs under tag `UP-FCMD`).

The platform interface (`platform.rs`: `Platform`, `PlatformEvent`, `Reopen`) is what each platform implements
(Android, the desktop viewer; a web build later).

## Live sessions (`remote`)

Feature `remote` (on in the viewer and the APK) runs a session over a pod with the engine
(`client-core`, decision `rust-clients-on-the-engine`): `PodEngine` is the session's `Loader`. Home shows as the
engine has it, from its cache at once (offline too; "Opening your pod…" with the connection's state on a first
start, until the pod answers), and the device's list of layouts (`local:layouts`, `local:apps`) comes from
`GET /v1/apps?all=1`, kept in `apps.json` next to the cache for offline starts. A layout a tap opens is the engine's
cached copy at once, fetched again behind it (`open_layout`; a layout it never had waits for the pod), and a page
screen whose route matches a tap's `target_uri` opens with its `$route` bound and its `item`, and slides in at once
while its `data` path loads through the engine (`action.data.load`, cached; `local://screen#loading`). Every
snapshot the engine sends reaches the session on its next frame (`Loader::changes`, snapshots since the last frame merged into one; the window is woken; only entities and queries that differ are taken, and news with nothing new draws nothing again): data,
`local://connection` (online, offline or connecting, and how many writes wait), and the new root of every layout
the event stream announced, so a layout edited over MCP, or a change from another device, shows without opening it
again. Each new frame tells the engine what it draws, topmost first (`Loader::visible`: every drawn entry's layout and
data path, and the layouts drawn inline in them), so an `event: data` reloads what's on screen first and leaves the
rest stale until it's shown again (decision `scoped-data-events`). The simulator's live mode (`construct-sim --base-url`) still fetches straight from the pod (`PodClient`). `local://nav` and
`local://screen` are set as the Compose UI set them. Images (`core://Image`) download on four background threads, newest request first,
without the session (the pod serves media from public paths), are cut to their box (cropped to fill, corners
rounded) and drawn over their placeholder once they arrive; the window redraws then.

`action.local.set` changes the session's entity and the engine's (`Loader::local_set`), so the engine's next snapshot doesn't put the old value back. Writes go to the engine's outbox, as the Compose UI sent them (`Loader::take`): `$form.<f>`, `$form?.<f>` and one
`$nonce` per tap are bound when the tap runs (a blank `$form.<f>` sends nothing). `action.mutation.optimistic`
changes the entity at once (every `value_expr` against the entity as it was, in the expression language client-core
uses, `protocol::expr`; one that fails changes nothing) and the engine keeps the write until
the pod has it, offline or through a failed POST, with `$value` as the new value; a refusal puts the fields back.
`action.mutation.create` adds its entity to its list at once, and the engine loads its list again once the pod has
it (a refusal takes it out). A loader without an outbox (`PodClient`) POSTs on a background thread instead
(`Loader::send`) and the session undoes a refusal itself. `action.device`
`open_notification` marks it read (`POST /v1/notifications/read`), closes the inbox's sheet or panel and opens the
notification's layout and `target_uri` screen; `mark_read` and `mark_all_read` mark them here and on the pod;
`load_older_notifications` adds the next page under the newer ones (`local://inbox` keeps `has_older` and
`next_before`; older pages stay when the inbox is listed again). `open_settings` and `open_inbox`, retired from new
layouts, still present the `settings` layout as a panel and `inbox` as a sheet for versions written before.
`toggle_channel` flips the switch at once and the engine tells
the pod (registering the device first), flipping it back when refused. `hide_layout`, `unhide_layout` and
`delete_layout` go to the pod through the engine and load its list of layouts again; a deleted layout leaves the
stack and its pin is forgotten; home is never hidden or deleted. `set_server` hands the platform another of
`local:servers` (`Effect::SwitchServer`). A refused write or device command shows a notice bar at the
bottom (the pod's `detail`, else its error, with Dismiss; tagged `notice` and `notice-dismiss`) until dismissed.
These commands (`Loader::device_command`) block the session while the pod answers, as `show_versions` does. Left to
its own ticket: `request_notification_permission` (#168).
`action.link.open` opens an `http(s)` address in the browser (an `ACTION_VIEW` intent on Android, `open` or
`xdg-open` on desktops, `rundll32 url.dll,FileProtocolHandler` on Windows). Desktops hand the opener only `http`,
`https` and `mailto` addresses, as one argument and never through a shell (`platform::openable`). `action.confirm` shows the app's own dialog (title, message, Cancel and the confirm label,
tagged `confirm:cancel` and `confirm:ok`) and runs `then`, with the steps of a sequence after it, only on confirm;
Cancel, Back or a tap outside do nothing. `action.auth.session` goes to the engine, which opens the session and
keeps it in its secure store; the platform then remembers the code and shows the pod.
Signing in to an upstream (D19) is the engine's flow too (`client_core::auth`), started by `action.auth.login` or
Settings' `sign_in` (a provider of `local:providers`, on the server typed for it when it takes one): the engine
registers the client with the pod (`app_path`) and the platform opens the authorize page in the browser (`signin`).
The desktop viewer gets the redirect on the engine's loopback listener. The Android app registers the verified App
Link `https://the-construct-pod.fly.dev/oauth/android` for the staging pod once Android has verified it (decision
[`android-app-link-sign-in-redirect`](decisions/android-app-link-sign-in-redirect.md); the APK's debug key must be
in the pod's `CONSTRUCT_ANDROID_CERT_SHA256`), else `construct://oauth`; either opens the app (`onNewIntent`, or
`onCreate` when Android had ended it) and goes through the bridge to the engine, which matches its `state` and
exchanges the code (`token_path`). Its progress and errors show in `local://session` (`status`), the session lands in
the secure store and home, the sign-ins and the feed load again. `sign_out` signs out through the pod
(`POST /v1/auth/logout`, which keeps a pod code's session) and loads the sign-ins again. Each server's engine
picks its own redirect, so `set_server` to another pod signs in there the same way. `leave_pod` (the engine
forgets the session) and `refresh` (home fetched again) go to the platform too. The device data (`DeviceData`) is built from the
engine: `local://connection`, `local://session` (from the engine's sign-in state),
`local://inbox` (the engine's unread count), the layouts with their versions, and, for a layout that binds them,
`local:notifications` (the engine's list, cached; `when` and `day` labels in UTC; loaded again when the count
changes), `local:channels` and `local:sign_ins` / `local:providers`. Marking notifications read goes through the
engine, which sends it again when the pod is back.

Windows place a laid-out frame again on scroll without laying it out (`Laid::emit`), so dragging is cheap. A drag
that scrolls flings on release and slows down each frame; one that scrolls nothing, past 48 points, is a swipe that
runs the `core://Gesture` under its start (`on_swipe_*`). A `core://Progress` spinner is an arc with a fading tail
that turns once a second; a window draws every frame while one is on screen, and previews and the simulator draw it
still. Text fields are edited by the engine itself, as Android's
own fields behave, and on Android the keyboard edits them too (below): a tap puts the cursor where
it lands; the cursor blinks (500 ms); a long press or a double tap selects the word there, with handles to drag and a
Cut / Copy / Paste / Select all bar over it (dragging on from a long press moves the selection's end); a tap on the
cursor shows its handle and the bar (Paste, Select all). Typing and pasting replace the selection and are cut to the
field's `max_length`. With a hardware keyboard: arrows, Home and End (Shift selects), Delete and Ctrl or Command+A, C,
X and V. On Android the clipboard is `ClipboardManager` through the SDK.

On Android the keyboard connects to the focused field (`ime.rs`), so its suggestions, autocorrect, swipe and voice
typing, emoji and its own paste work. The bridge's `TextInputView` is an invisible view over the NativeActivity,
focusable only while the keyboard is up (with the activity's `stateAlwaysHidden`, so returning to the app never opens
the keyboard by itself); its `InputConnection` (`TextInput`) forwards each call to `NativeText`'s natives, which run on
Android's main thread and answer at once from a copy of the field (`Connection`). The keyboard's edits change the
copy and wake the window, which applies them to the session first (`Session::set_text_state`, cut to `max_length`);
the engine's own changes (a tap, a handle, the edit bar, a hardware key) then reach the keyboard: a moved selection
with `InputMethodManager.updateSelection`, changed text or another field with `restartInput`. The word being
composed is underlined. The keyboard's Done (or Enter) ends editing, as Enter does; its Cut, Copy, Paste and Select
all run the edit bar's. Fields ask for text with capitals at sentences and autocorrect. Elsewhere `Platform::text_input`
is `None` and the window types what key events bring.

The headless simulator, `construct-sim` ([simulator.md](simulator.md)), drives a session from a script on fixture
bundles or a live pod and writes PNGs drawn on the CPU.

## Frame diagnostics

Windows (Android and the desktop viewer) measure every frame they draw (`perf`) and log one line per burst of frames:
a drag and its fling, a screen sliding in, a tap. A burst ends when nothing is left to draw; a long one (a spinner on
screen) is summed up every 5 seconds, and windows with no touches and no frame over 25 ms aren't logged (the next
line says how many were skipped). Recording a frame is a few clock reads and a copy into a buffer allocated once; the
summary is worked out after the burst's last frame, and the frame after a mid-burst summary doesn't count as back to
back, so the diagnostics don't show up in what they measure. Only timings and counts are logged: no text or data from
the pod, only the layout's id (and the page screen's) on top. On Android: `adb logcat -s construct-render | grep perf`.

```
perf agents/task: 96 frames in 1580 ms, 60 fps; display 8.3 ms (120 Hz); slow 31 (worst: interval 48.2 work 41.0 =
scene 38.1 [layout 37.9 x1, data 1] + render 2.6 [shape 1.9 x14, atlas 0.0 x0] + present 0.3; acquire 6.1; moves 3);
interval 16.6/25.0/48.2; work 1.9/9.8/41.0; ...; moves/frame 0:3 1:60 2:28 3+:4; drag step 21.3 pt ±38%
```

An example, wrapped here (it is one line). Spreads are p50/p95/max in milliseconds.

| Field | Meaning |
|---|---|
| frames, fps | Frames in the burst, and their rate over the back-to-back ones (idle gaps between taps don't count) |
| display | The display's frame time: Android's `Display.getRefreshRate()`, the desktop monitor's, else estimated from the shortest steady intervals |
| slow | Frames more than 1.5 display frames after the one before (or, the first of a burst, whose work took over one); `worst` breaks the slowest one down |
| interval | Time between back-to-back frame starts |
| work | The frame's own time: scene + render + present (not waiting for the surface) |
| scene | `Session::scene`: placing the laid-out frame again (a scroll), or laying it out (`layouts`: how many, how long; `data`: engine snapshots that changed what is drawn) |
| render | The GPU renderer's CPU side; `shaped` text runs not kept from earlier frames, `atlas` images and icons drawn into the glyph atlas (count, KB) |
| acquire | Waiting for the surface's next image: vsync and the GPU's backlog |
| present | Handing the image to the compositor |
| latency | From the first touch a frame shows to the frame starting |
| moves/frame | During a drag, frames by how many touch moves arrived since the one before; uneven counts mean uneven steps |
| drag step | During a drag, the scroll per frame: mean and variation (standard deviation over mean); a high variation judders |
| runs, quads | The most text runs (with icons and images) and quads a frame drew |

## Agent previews (MCP)

`render_layout_preview` (`id`, or a draft `layout` drawn with `data_from`'s data and every feed of home's;
`viewport`, `form`, `dark`) answers an image (`image/webp`, lossless) and a text: a summary line, then JSON with
`preview_id`, `state`, timing and the nodes (id, type, bounds, text, tap, selected). `dispatch_virtual_interaction`
(`preview_id`, `action` tap / scroll / type / back, with `node_id`, `text` or `x`/`y`, `dy`) changes that preview's
device state and answers the same, with the interaction's effects.

A preview behaves as a device with the user's data, minus anything that writes (decision
`previews-read-like-a-device`). Its session has a read-only `Loader` that asks the pod's own router as the owner:

- **Device data** is built as the clients build it (`construct_render::device_data`): `local:layouts`,
  `local:all_layouts`, `local:apps` and their `local://layouts/<id>` rows, `local://connection` (online),
  `local://session`, `local://servers/current`, `local://inbox`, and, when a layout binds them, the notifications,
  channels and sign-ins. Device-only facts get defaults: `local://device` is an Android phone with notifications
  allowed.
- **Data paths**: a screen's `data` and `action.data.load` are read when the path matches a data path a data
  source declares (`SourceManifest::data_paths`, what `list_data_sources` lists); any other path is refused
  (`couldn't load …: not a data path`). Layouts a tap opens, and a layout's versions (`show_versions`,
  `pin_version`, `follow_latest`, which stay in the preview), are read too.
- **Writes are not sent**: mutations, creates, sign-ins, `action.auth.*` and every device command that changes
  the pod come back in `effects` as `not sent`.

Arguments for real screens: `target_uri` opens the layout's screen whose route matches (as a tap with that
`target_uri` does, loading its data; no match answers `no_screen` with the layout's routes), and `params` sets
the layout's own `$route.<name>`. `item` binds `$item` to an entity URI, or to an object of made-up fields
(stored as `preview://item`), and `entities` (`{uri: {field: value}}`) and `queries` (`{key: [uri]}`) add or
replace data, so a row template or a `pod:` component can be drawn against one entity. `full_height` grows the
frame until the screen's scrolling content fits (up to 10,000 points; the scale drops to stay within the pixel
budget), and `scroll_to` scrolls a node into view first. `nodes` is `text` (the default: id, type, bounds, text
and a short tap summary such as `action.navigation.open layout=inbox presentation=Sheet`), `full` (each tap's whole
intent) or `none`; `dispatch_virtual_interaction` takes `nodes` and `full_height` too, for that preview from then
on.

Layouts the previewed one embeds inline or opens are loaded with it (up to 8), and others when a tap opens them. What is drawn is held to
the value checks a saved layout gets (sizes, icons, tints; `validate::check_node_values`), a draft's and a saved
layout's alike, since a component updated after a layout was saved isn't checked against it: a preview that fails
them answers `invalid_layout`, and a linked layout that fails them isn't loaded. The renderer also draws no icon
mask past 2048 px a side. An `id`, `data_from` or linked layout that isn't shaped like a layout id (lowercase
letters, digits, `_` and `-`, at most 64) answers `unknown_layout` before any request is built. Code:
`pod_core::preview`.

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
- `engine` (feature `remote`, against a stub pod): offline starts and the outbox, layout events, a pinned home that
  stays on its version until `follow_latest`, and hiding a layout.
- `writes`: optimistic changes and creates (sent, refused and undone), form binding, notifications, links, dragging a
  sheet closed, carousel paging, scrolling a chip row, confirm dialogs, signing in, and editing text (cursor, selection, the edit bar,
  `max_length`).

The MCP round trip is `mcp::agents_preview_a_layout_and_tap_it` in the server's integration tests.

Timings for the task-list fixture at 390 × 844 @2x, release build, on a 4-core cloud container: layout 0.4 ms;
CPU raster 9 ms and lossless WebP 3 ms (about 12.5 ms in all); GPU on lavapipe (software Vulkan) 15.6 ms including
readback and WebP. A real GPU has not been measured yet.
