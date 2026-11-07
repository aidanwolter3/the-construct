# Clients

Desktop and Android run the same shared Compose UI (`app/shared`) over the same Rust engine
(`crates/client-core`). Every client defaults to the staging server, `https://the-construct-pod.fly.dev`, and to the
pod's `home` app (the shell, below). Build prerequisites are in [CONTRIBUTING.md](../CONTRIBUTING.md#setup).

## Desktop

```bash
cd app
./gradlew :desktopApp:run                                              # the staging server
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080    # a local server
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080 -Dconstruct.workspace=pixelfed_sample_no_login
```

`:desktopApp:run` builds the engine first (`scripts/build_ffi.sh`, macOS); `-PskipRustBuild` skips that and uses the
library already in `target/release`.

| Option (`-D` property or environment variable) | Default | Meaning |
|---|---|---|
| `construct.baseUrl` / `CONSTRUCT_BASE_URL` | the menu's saved choice, else staging | The server at launch. A layout with a server picker (`local:servers`, `set_server`) still switches the running app. |
| `construct.workspace` / `CONSTRUCT_WORKSPACE` | `home` | The app opened at launch (`home` runs the shell; any other id runs that app alone with the native menu). |

The engine keeps its cache in `~/.construct/cache.db` for staging and `~/.construct/servers/<url>/` for any other
server, so data and queued actions never cross servers. Sessions are kept in the macOS Keychain (service
`org.theconstruct.client`).

## Android

Prerequisites once: an Android SDK with an NDK (the path from `app/local.properties` `sdk.dir=` or
`ANDROID_HOME`), then:

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android x86_64-linux-android
```

Build and install from `app`; `assembleDebug` cross-compiles the engine first (`scripts/build_android.sh`):

```bash
./gradlew :androidApp:assembleDebug     # androidApp/build/outputs/apk/debug/androidApp-debug.apk
./gradlew :androidApp:installDebug
adb shell am start -n org.theconstruct.android/.MainActivity
adb logcat -s ConstructEngine:V         # Kotlin log lines and the engine's output
```

| Gradle property | Default | Meaning |
|---|---|---|
| `-PskipRustBuild` | | Package the `.so` files already in `jniLibs` without running cargo. |
| `-PandroidAbis` | `arm64-v8a,x86_64` | ABIs to build and package. |
| `-PconstructBaseUrl` | staging | The built-in server (`http://10.0.2.2:8080` is the host machine from the emulator). |
| `-PconstructWorkspace` | `home` | The app opened at launch. |
| `-PconstructAppLinkHost` | the staging host | The host whose App Link the sign-in redirect uses. |
| `-PnoFcmFallback` | | Leave out the embedded FCM push distributor. |

Debug builds also take intent extras, read when the engine starts (use `am start -S` to restart):
`--es baseUrl http://10.0.2.2:8080`, `--es workspace pixelfed_sample_no_login`. On a phone over USB, run
`adb reverse tcp:8080 tcp:8080` and use `http://127.0.0.1:8080`.

**Cleartext:** the app allows plain HTTP only to `localhost`, `127.0.0.1` and `10.0.2.2`, and a custom server in
Settings must be `https://` otherwise. That rule covers images (loaded by Kotlin); the engine's own traffic goes
through Rust and is not restricted by Android's network security config.

## Rust-native prototype

`crates/construct-render` draws layouts without the JVM: `construct-viewer` is a desktop window (Metal or Vulkan)
and `scripts/android_native.sh` builds a NativeActivity APK. They draw a layout file, or a pod live (`--pod`, or
`--as-app` over the main Android app with its sign-in), sending writes straight to the pod, with no engine behind
them (no cache, outbox or event stream). See [rendering.md](rendering.md).

## Home and the layouts it shows

The client opens the pod's `home` layout (the default) and draws everything from it (decision `layouts-not-apps`):

- **Before a pod is open** it draws a bundled sign-in layout: a pod code, or **Try a temporary pod**. It needs no
  server to draw, and shows the sign-in error under the button.
- **With a pod** it draws `home`, the user's whole interface. A new pod's home is the `shell` blueprint: a blank
  off-white page saying "The Construct", with Notifications and Settings buttons. The agent builds the rest into home,
  only as much as the user asks for.
- **Other layouts** show inside home (`core://Layout`: in place, in a card-sized region, or as a sheet or side panel
  while a condition holds), or are presented on the back stack (`action.navigation.open {layout}`: pushed over the
  whole screen, or as a sheet or panel). An overlay can come from any edge: a sheet from the top or bottom, a panel
  from the start or end (decision `edge-overlays-and-gestures`). One engine serves them all and shares their data.
- **Back:** every presented layout and screen is one entry of the back stack. System Back (and the Back arrow of a
  pushed layout or screen, or a tap on a sheet's or panel's scrim) pops the top one; with nothing left, the platform's
  own Back runs. An overlay also closes when dragged back toward its edge: a sheet by its handle (on its inner side),
  or from its content once that can't scroll further that way (a bottom sheet's at its top); a panel anywhere.
  Released past a quarter of its size, or flung that way, it closes; otherwise it springs back.
- **Gestures:** `core://Gesture` runs an intent on a tap, long press or swipe (up, down, left, right). A pull the
  content can't scroll for counts as a swipe, so a home that opens the inbox from the top on a swipe down still
  scrolls. The swipe feel and Android's own edge gestures (Back from the sides, the notification shade from the top)
  are checked on a phone.
- **Screens inside a sheet or panel** (decision `screens-inside-frames`): a screen of the sheet's or panel's own layout
  opens inside that frame, not over the whole screen. The frame's content slides left as the screen slides in from the
  end, with a Back arrow and the screen's title at its top; Back (the arrow or system Back) slides it out again, and
  a scrim tap or dragging a sheet down closes the frame with whatever is open in it. Screens of pushed layouts and of
  home cover the whole screen, as before.
- **Settings and the inbox are layouts** (the `settings` and `inbox` blueprints), presented as a side panel and a
  sheet. Settings shows the connection, the pod's layouts (a tap lists a layout's versions and offers Hide; picking one
  shows it on this device only, and the pin is kept across restarts until Show latest or leaving the pod), the
  hidden layouts with a Show button, sign-ins, notifications (one row per layout that sends them; a tap opens that layout's
  switches inside the panel), and **Leave pod**. On Android the notification permission row sits above it. The
  blueprint no longer lists servers: the platform still offers them in `local:servers` (Staging, Local, or its
  others; each server has its own cache and outbox), so a layout can add a picker with `set_server`; desktop and
  the simulator also take `CONSTRUCT_BASE_URL` / `-PbaseUrl`.
- **The inbox** lists the newest page of notifications and a **Load older** button while there are more.
- **If the pod has no home this build can draw**, a bundled screen offers Try again and Leave pod. Ask the agent to
  repair the home layout (`restore_version`); clearing the app's data also starts over.

## Offline behaviour

The last workspace and every loaded screen are cached, so the app opens and scrolls offline. A tap changes the screen
at once and queues the request; Settings shows "N pending" (and `local://connection` lets a layout show it too). Queued actions are sent in order when the server is
reachable again, including after a restart. If the server refuses one, its change is rolled back. Details:
[architecture.md](architecture.md#the-engine).

## Signing in to an upstream

Signing in to Pixelfed happens inside the pod: the engine runs OAuth2 PKCE in the system browser, and the pod keeps
the upstream tokens. Clients hold only a pod session.

- **Desktop:** the browser redirects to a loopback listener on `http://127.0.0.1:47823/callback` (another port if
  that one is busy).
- **Android:** the redirect is a verified App Link, `https://<server>/oauth/android`, when the server is the build's
  App Link host and Android has verified it; otherwise `construct://oauth`. The session is encrypted with an
  AndroidKeyStore key. Check verification with `adb shell pm get-app-links org.theconstruct.android`.
- **Simulator:** an in-memory session and a scripted browser; see [simulator.md](simulator.md#live-mode).

Signing out (or a 401 from the pod) removes cached private data from the device. More in [pixelfed.md](pixelfed.md).

## App icon

A white space with a grey perspective grid and one cube. Every asset is generated from one geometry definition in
`scripts/gen_icons.py`: run `scripts/gen_icons.sh` (python3 with Pillow and numpy; `iconutil` on macOS for the
`.icns`). It writes `assets/icon/`, the Android adaptive icon and the desktop window icon. Don't edit the generated
files by hand.
