# Project Canvas

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

Milestones 1–3 are implemented: the protocol crate, the Axum server and the macOS desktop app.
Milestone 4 (the Android app, see [Android](#android)) is in progress.

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord
crates/server        Axum server: GET /v1/workspaces/{home,pixelfed_demo}
crates/client-core   CanvasEngine: fetches the workspace, holds state, applies optimistic mutations
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
fixtures/            counter_home_bundle.json, pixelfed_home_bundle.json (DESIGN.md §2.2C)
scripts/build_ffi.sh builds libclient_ffi.dylib and regenerates the Kotlin bindings
scripts/build_android.sh  cross-compiles libclient_ffi.so for Android (cargo-ndk)
scripts/android_smoke.sh  installs and launches the Android app on an emulator, checks it renders (Tests)
kmp-app/             Gradle project: :shared (model + Compose renderer), :desktopApp (JVM, JNA),
                     :androidApp (Android, JNA aar)
```

## Prerequisites

- macOS on Apple Silicon, with Rust installed through rustup (target `aarch64-apple-darwin`; add it with
  `rustup target add aarch64-apple-darwin` if needed).
- JDK 21 (`:desktopApp` sets `jvmToolchain(21)`). This repo uses Android Studio's bundled JBR, which is set in
  `kmp-app/gradle.properties` (`org.gradle.java.home`). Set `JAVA_HOME` to match:

```bash
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
```

## Run the demo

Terminal 1: start the server on 127.0.0.1:8080 (set `CANVAS_ADDR` to use another address).

```bash
cargo run -p server
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq .
```

Terminal 2: build the FFI library and bindings, then run the desktop app.

```bash
scripts/build_ffi.sh            # optional: :shared:buildRustFfi runs it before the desktop compile
                                # (dylib built to target/aarch64-apple-darwin/release, copied to target/release)
cd kmp-app
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
./gradlew :desktopApp:run
```

A "Project Canvas" window opens with the counter layout served by the server. Clicking
**Increment** or **Reset** prints the intent to stdout and the count updates:

```
[desktop] dispatch: {"intent":"action.mutation.optimistic",...}
[CanvasEngine] dispatch: {...}
[CanvasEngine] core://counters/home#count = 1
```

Options for the desktop app (set as `-D` JVM properties or environment variables):

- `canvas.baseUrl` / `CANVAS_BASE_URL` (default `http://127.0.0.1:8080`)
- `canvas.workspace` / `CANVAS_WORKSPACE` (default `home`; `pixelfed_demo` shows the Pixelfed feed fixture)

Separately, `-PskipRustBuild` is a Gradle property (`./gradlew -PskipRustBuild :desktopApp:run`) that skips
the cargo build step.

Clicks are applied locally only: there is no server sync until the action outbox lands in M5, so the count
resets when the app restarts. The engine caches the last fetched bundle per workspace in
`~/.canvas/workspace_<id>.json` (e.g. `workspace_home.json`) and uses it when the server is unreachable;
it retries the fetch every 5 seconds until the server responds.

## Simulation mode

A headless feedback loop: `:desktopApp:simulate` renders the real shared `CanvasApp` to an offscreen
virtual screen (no window), runs a small script against it and writes PNG screenshots. A run takes about
2 seconds. Run it from `kmp-app` with `JAVA_HOME` set as above:

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
  -Pscript="shot start; click Increment; click Increment; assert-text Count: 2; shot after; offline; shot offline"

./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pscreen=desktop
./gradlew -PskipRustBuild :desktopApp:simulate -PscriptFile=my_script.txt -Pdensity=1 -Pout=/tmp/shots
./gradlew -PskipRustBuild :desktopApp:simulate -PbaseUrl=http://127.0.0.1:8080 -Pscript="shot live"   # live mode
```

Flags (all optional):

| Flag | Default | Meaning |
|---|---|---|
| `-Pworkspace` | `home` | Workspace id. In fixture mode it selects the default fixture. |
| `-Pscreen` | `phone` | `phone` (390x844 dp), `tablet` (820x1180), `desktop` (1280x800), or `WxH` in dp. |
| `-Pdensity` | per screen | Pixels per dp: phone 3, tablet 2, desktop 2, `WxH` 2. PNG size = dp x density. |
| `-Pscript` | `shot initial` | Script text (see below). |
| `-PscriptFile` | | Script read from a file. Use either this or `-Pscript`, not both. |
| `-Pmode` | `fixture` | `fixture` or `live`. Live mode is selected automatically when `-PbaseUrl` is given. |
| `-Pfixture` | per workspace | Bundle JSON in fixture mode. Defaults: `home` uses `fixtures/counter_home_bundle.json`, `pixelfed_demo` uses `fixtures/pixelfed_home_bundle.json`. Other workspaces need `-Pfixture`. |
| `-PbaseUrl` | `http://127.0.0.1:8080` | Server for live mode. |
| `-PtimeoutMs` | `10000` | Live mode only: how long to wait for the workspace to load before the first step. |
| `-Pout` | `kmp-app/desktopApp/build/sim/<yyyyMMdd-HHmmss>` | Output directory. |

Relative paths (`-Pfixture`, `-PscriptFile`, `-Pout`) resolve against the **repo root**, the task's working
directory, so `-Pfixture=fixtures/counter_home_bundle.json` works.

**Fixture mode** (the default) needs no server, and no Rust build when run with `-PskipRustBuild` (without
it, the desktop compile triggers `:shared:buildRustFfi`, i.e. cargo). A fake in-memory engine
(`FakeEngineBridge`) loads the bundle, applies `OptimisticMutate` clicks with the D4 expression subset, and
toggles connectivity for `offline`/`online`. **Live mode** runs the real Rust engine through JNA against
the server. It uses a fresh temp storage dir, so it neither reads nor writes `~/.canvas`. If the workspace
does not load within `timeoutMs`, it logs a warning and still runs the script (shots show Loading/Offline).
`offline`/`online` are not supported in live mode; a live-mode script containing them is rejected before any
step runs.

Script commands are separated by `;` or newlines. Each command is trimmed, blank entries are skipped, and
entries starting with `#` are comments. An argument cannot contain `;`, and since `;` separates commands, a
`#` comment ends at the next `;` (the rest of the line is parsed as commands).

| Command | Effect |
|---|---|
| `shot <name>` | Writes `<NN>-<name>.png`, where NN is the step number. |
| `click <text>` | Clicks the node with that text: an exact match first (clickable nodes preferred), then a substring match. Fails if no node matches. |
| `wait <ms>` | Sleeps, advances the UI clock, then waits for idle. At most 600000 (10 minutes). |
| `idle` | Waits until the UI is idle. |
| `assert-text <text>` | Fails unless some node's text contains `<text>`. Retries for up to 2 s. |
| `assert-no-text <text>` | The inverse of `assert-text`. |
| `offline` / `online` | Fixture mode only: flips the engine's connectivity. |
| `screen <WxH or preset>` | Restarts the renderer at the new size. Engine state carries over; UI state such as scroll position does not. |

The output directory holds the PNGs, `steps.log` (human-readable) and `steps.json` (one entry per step:
`index`, `command`, `arg`, `status` (`ok`/`fail`), `message`, `file`, `elapsed_ms`). Its absolute path is
printed at the end. The run stops at the first failing step; the logs are still written, and the screen at that
point is saved (best effort) as `<NN>-FAILED.png`, recorded as that step's `file` and printed with the error.
The simulator's exit codes: 0 for success, 1 for a failed step, 2 for bad flags, a bad script or an unusable
`-Pout` directory. Unknown commands, bad arguments and `offline`/`online` in live mode are rejected before any
step runs. `./gradlew` itself exits 1 for any simulator failure; the simulator's own code appears in Gradle's
message `Process ... finished with non-zero exit value N`. An unrecognized `-P` property (e.g. a typo such as
`-Pscren`) is ignored, with a warning.

## Android

`:androidApp` runs the same shared `CanvasApp` on Android, with the Rust engine cross-compiled to
`libclient_ffi.so` (DECISIONS D9, D11).

Prerequisites (once): an Android SDK with an NDK (Android Studio SDK Manager; the SDK path comes from
`kmp-app/local.properties` `sdk.dir=...` or `ANDROID_HOME`), plus

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android x86_64-linux-android
```

Build (from `kmp-app`, `JAVA_HOME` set as above). `assembleDebug` runs `:androidApp:buildRustAndroid`, i.e.
`scripts/build_android.sh`, first; running the script by hand is optional:

```bash
../scripts/build_android.sh          # optional; writes androidApp/src/main/jniLibs/{arm64-v8a,x86_64}/libclient_ffi.so
./gradlew :androidApp:assembleDebug  # -> androidApp/build/outputs/apk/debug/androidApp-debug.apk
```

Gradle properties for `:androidApp`:

- `-PskipRustBuild`: don't run cargo; package the `.so` files already in `jniLibs`.
- `-PandroidAbis=arm64-v8a,x86_64` (default): ABIs to build and package (`CANVAS_ANDROID_ABIS` for the script,
  and the APK's `abiFilters`).
- `-PcanvasBaseUrl=http://10.0.2.2:8080` (default) and `-PcanvasWorkspace=home` (default): the built-in
  defaults (`BuildConfig`).

Run on an emulator. Start the server first (`cargo run -p server`, on the host's 127.0.0.1:8080, which the
emulator reaches as `10.0.2.2:8080`):

```bash
export PATH="$HOME/Library/Android/sdk/platform-tools:$HOME/Library/Android/sdk/emulator:$PATH"
emulator -list-avds
emulator -avd Medium_Phone_API_36 -no-window -no-audio -no-boot-anim -no-snapshot-save &   # headless
adb wait-for-device
adb shell 'while [ "$(getprop sys.boot_completed)" != 1 ]; do sleep 1; done'

./gradlew :androidApp:installDebug
adb shell am start -n org.genui.canvas.android/.MainActivity
adb shell am start -S -n org.genui.canvas.android/.MainActivity --es workspace pixelfed_demo   # -S restarts it
adb shell am start -S -n org.genui.canvas.android/.MainActivity --es baseUrl http://10.0.2.2:9090
adb logcat -s CanvasEngine:V
adb exec-out screencap -p > screen.png
```

Intent extras `baseUrl` and `workspace` override the `BuildConfig` defaults (debug builds only; release builds
ignore them). They are read when the engine is
created, so use `am start -S` (force-stop first) to switch workspace on a running app. Rotation keeps the
same engine. Logcat tag `CanvasEngine` carries both the Kotlin log lines (`starting base_url=...`,
`dispatch: ...`) and the Rust engine's stdout/stderr. The stdout/stderr redirect is process-wide, so the tag also
shows other native output from the app process (e.g. GL/EGL driver lines). The workspace cache is in the app's
`filesDir`.

On a physical phone, use `adb reverse tcp:8080 tcp:8080` and `--es baseUrl http://127.0.0.1:8080`. Debug
builds allow cleartext HTTP only to `10.0.2.2`, `localhost` and `127.0.0.1`; release builds allow none.

## Tests

```bash
cargo test --workspace
cd kmp-app && ./gradlew :shared:allTests :desktopApp:test
./gradlew :androidApp:assembleDebug   # Android build check only: it doesn't launch anything
```

`assembleDebug` and the JVM host tests can't catch Android-only runtime failures (for example, Android's ICU regex
engine rejects patterns that `java.util.regex` accepts). Two checks run on a booted emulator or device:

```bash
# Launch check: installs the debug app, launches it per workspace, then fails on a FATAL EXCEPTION in logcat,
# a dead process, or missing text in a `uiautomator dump` ("Count: <n>" and "Online" for home, "Sunrise over
# Boulder" for pixelfed_demo). Writes <ws>.png, <ws>.xml and <ws>.logcat.txt to --out
# (default kmp-app/androidApp/build/smoke).
../scripts/android_smoke.sh --pixelfed                    # device and server already running
../scripts/android_smoke.sh --boot --server --pixelfed    # boots Medium_Phone_API_36 headless and starts the
                                                          # server, then stops both at the end
../scripts/android_smoke.sh --help                        # --workspaces, --skip-install, --skip-rust, --wait, --timeout

# Instrumented tests (androidApp/src/androidTest): runs the shared interpolation regexes on the device.
./gradlew :androidApp:connectedDebugAndroidTest
```
