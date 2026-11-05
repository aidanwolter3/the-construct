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
| `construct.baseUrl` / `CONSTRUCT_BASE_URL` | the menu's saved choice, else staging | The server at launch. Picking a server in Settings still switches the running app. |
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

## The shell

Opened on `home` (the default), the client runs the layout-first shell (decision `blueprints-and-home`):

- **Before a pod is open** it draws a bundled sign-in layout: a pod code, or **Try a temporary pod**. It needs no
  server to draw, and shows the sign-in error under the button.
- **With a pod** it draws the pod's `home` layout, which is the user's whole interface. A new pod's home is the
  `shell` blueprint: a blank off-white page saying "The Construct" and "Ask your agent to build anything". The agent
  builds the rest into home, only as much as the user asks for: one app filling the screen, then a drawer or dock
  of apps, notifications or settings pages, from small pieces (`describe_vocabulary` snippets).
- **An app framed in home** (`core://AppFrame`): its layout, screens and Back stay inside the frame. The client's
  one engine follows the framed app; the home layout is kept on the device while it does.
- **The native menu and inbox** below are still in the app, opened only by a layout that asks for them
  (`action.device` `open_settings`, `open_inbox`); the version picker is there.
- **If the pod has no home this build can draw**, a bundled screen offers Try again and Leave pod. Ask the agent to
  repair the home layout (`restore_version`); clearing the app's data also starts over.

## The app menu

Opened on any other app, the screen shows only the layout, with a **menu button** at the top left and a **bell** at
the top right. The menu
button gets a dot when the device is offline, when actions wait to be sent, or when the version on screen has a
newer one.

- **Apps:** every app of the pod. A row's history button opens the **version picker**: "Latest", then every version
  newest first with its note. Picking one shows that layout over your current data, on this device only; "Latest"
  follows new versions again. Versions this build can't draw are greyed out. A row's **⋮** hides the app (on every
  device) or deletes a custom app.
- **Settings:**
  - **Your pod:** the connection, the server picker, and **Leave pod**, which forgets the pod code on this device.
  - **Sign-ins:** every upstream sign-in on the pod (e.g. "Pixelfed · @you · pixelfed.social"), with **Sign out**,
    and a **Sign in** row for each one the pod offers.
  - **Notifications:** each app's channels with switches.
  - **Server:** Staging, Local (desktop `http://127.0.0.1:8080`; Android emulator `http://10.0.2.2:8080` or USB
    `http://127.0.0.1:8080`), or a custom URL. Switching starts a new engine on that server, in place; each server has
    its own cache and outbox.

## Offline behaviour

The last workspace and every loaded screen are cached, so the app opens and scrolls offline. A tap changes the screen
at once and queues the request; the menu shows "N pending". Queued actions are sent in order when the server is
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
