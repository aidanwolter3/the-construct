# Clients

The desktop and Android apps draw layouts with the Rust renderer (`crates/construct-render`) over the Rust engine
(`crates/client-core`), decision `rust-clients`: `construct-viewer` on the desktop (`The Construct.app` on macOS) and
a NativeActivity APK on Android. Both open your pod: the one opened last, else the pod's sign-in screen (a pod code,
or **Try a temporary pod**) on the staging server, `https://the-construct-pod.fly.dev`. Renderer details, the
Android bridge and push are in [rendering.md](rendering.md).

## Desktop

```bash
cargo run --release -p construct-render --features viewer --bin construct-viewer               # your pod
CONSTRUCT_POD_URL=http://127.0.0.1:8080 \
  cargo run --release -p construct-render --features viewer --bin construct-viewer               # a local server
cargo run --release -p construct-render --features viewer --bin construct-viewer -- layout.json  # a layout file
```

`CONSTRUCT_POD_URL` is the server the sign-in screen opens (default staging); once a pod is open, the window reopens
it (`pod.json`). Settings' server picker (`local:servers`, `set_server`) offers Staging, Local
(`http://127.0.0.1:8080`) and `CONSTRUCT_POD_URL` when it is another; each server has its own cache and outbox. Plain
`http://` is accepted only for this machine and private network addresses.

The viewer keeps its pod and the engine's cache in `~/Library/Application Support/The Construct` on macOS (else
`$XDG_CONFIG_HOME/the-construct` or `~/.config/the-construct`), and sessions in the login Keychain on macOS (else
`secure.json` there, readable by you only).

**The Mac app:** `scripts/macos_app.sh` (on a Mac) builds `build/macos/The Construct.app` and
`build/macos/The-Construct.dmg`; copy the app to Applications. It is signed ad hoc, not notarized: a copy
downloaded from a release opens only through Open in the Finder's context menu the first time (or after
`xattr -dr com.apple.quarantine "/Applications/The Construct.app"`).

## Android

Prerequisites once: an Android SDK with an NDK and build-tools (`ANDROID_HOME`, else `~/android-sdk`), a JDK
(`javac`; Android Studio's bundled one is found), then:

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android
```

Build and install on the adb device (a phone over USB, or an arm64 emulator):

```bash
scripts/android_native.sh               # build/android-native/construct-render.apk, installed and started
adb logcat -s construct-render          # the app's log (panics too, with a backtrace)
```

If the app closed by itself, the next start says why in a notice when a panic caused it
([rendering.md](rendering.md#when-something-fails)); otherwise the reason is in logcat (`adb logcat -b crash` for
native crashes, `ActivityManager` lines for one Android ended for memory).

A development build is debuggable and signed with this machine's debug key; it installs over an earlier build signed
with the same key, keeping the app's data and sign-in. A build signed with another key (a release, or another
machine's) can't be updated in place: `adb uninstall org.theconstruct.android` first, then sign in again.

| Environment variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_APP_LINK_HOST` | the staging host | The pod whose App Link carries the sign-in redirect (in the manifest and the Rust code). |
| `CONSTRUCT_VERSION_CODE`, `CONSTRUCT_VERSION_NAME` | `1`, `0.1` | The APK's version. |

The server is picked on the sign-in screen and in Settings (`local:servers`: Staging, Local emulator
`http://10.0.2.2:8080`, Local USB `http://127.0.0.1:8080` with `adb reverse tcp:8080 tcp:8080`, and the saved
server when it is another). The APK is built for arm64-v8a only.

## Releases

`.github/workflows/release.yml` builds the release APK (`scripts/android_native.sh --release`: not debuggable) and the
Mac app's `.dmg` on every tag `v<version>` and publishes them as a GitHub release (a manual run of the workflow
uploads them as its artifacts). The version code is the workflow's run number, so each release installs over the
previous one. The APK is signed with the release key from these repository secrets (without them, with a throwaway
debug key and a warning):

| Secret | What |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | The keystore file, base64 on one line. |
| `ANDROID_KEYSTORE_PASSWORD` | Its password. |
| `ANDROID_KEY_ALIAS` | The key's alias. |
| `ANDROID_KEY_PASSWORD` | The key's password (optional: the keystore's when unset). |

Make the key once and keep the keystore and passwords somewhere safe outside the repository: a lost key means users
reinstall.

```bash
keytool -genkeypair -v -keystore construct-release.jks -storetype PKCS12 -alias construct \
  -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=The Construct"
base64 < construct-release.jks | tr -d '\n'     # the value of ANDROID_KEYSTORE_BASE64
keytool -list -v -keystore construct-release.jks -alias construct | grep SHA256   # the cert fingerprint
```

(PKCS12 keystores use one password for the store and the key.) Add the fingerprint in front of the pod's
`CONSTRUCT_ANDROID_CERT_SHA256` ([server.md](server.md)) so the release build's App Link sign-in redirect verifies;
the workflow also prints it. To build a signed APK locally, set `CONSTRUCT_ANDROID_KEYSTORE` (the file),
`CONSTRUCT_ANDROID_KEYSTORE_PASSWORD`, `CONSTRUCT_ANDROID_KEY_ALIAS` and `CONSTRUCT_ANDROID_KEY_PASSWORD` and run
`scripts/android_native.sh --release`.

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
  others; each server has its own cache and outbox), so a layout can add a picker with `set_server`; the desktop
  also takes `CONSTRUCT_POD_URL`.
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
`.icns`). It writes `assets/icon/` (the Mac app's `construct.icns` among them) and the Android launcher icon in
`crates/construct-render/android/res/`. Don't edit the generated files by hand.
