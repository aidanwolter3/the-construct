# Project Canvas

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

Milestones 1–4 are implemented: the protocol crate, the Axum server, the macOS desktop app and the Android
app (see [Android](#android)). A headless simulation mode renders the shared UI to PNGs (see Simulation mode).

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord
crates/server        Axum server: GET /v1/workspaces/{home,pixelfed_demo,pixelfed_home}, persistent counters
                     (SQLite), Pixelfed adapter (auth, feed normalization, actions, media proxy)
crates/client-core   CanvasEngine: fetches the workspace, holds state, applies optimistic mutations (SQLite cache + outbox)
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
crates/fake-pixelfed local fake Pixelfed (Mastodon API + OAuth PKCE) upstream for development and tests
fixtures/            counter_home_bundle.json, pixelfed_home_bundle.json (DESIGN.md §2.2C);
                     pixelfed/ (Mastodon API JSON + media served by fake-pixelfed)
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

Server details:

- State lives in SQLite at `$CANVAS_DATA_DIR/server.db`; `CANVAS_DATA_DIR` defaults to `~/.canvas-server`
  (created if missing). The path is logged at startup. Delete the file to start from a zero count.
- `GET /v1/workspaces/home` returns the counter fixture with `core://counters/home`.count set to the stored count.
- `POST /v1/counters/home/{increment,decrement,reset}` (optional JSON body, ignored) returns
  `{"ok":true,"count":N}`. Decrementing at 0 is refused with 422 `{"error":"count_below_zero","count":0}` and
  changes nothing. Unknown counters/ops are 404. Every mutation is logged at info with the resulting count.
- Test hook, only when started with `CANVAS_TEST_HOOKS=1` (otherwise 404): `POST /v1/test/fault` with
  `{"status":503,"count":3}` makes the next 3 POSTs (other than `/v1/test/*`) return that status (400-599)
  without being applied; `"count":0` clears it. Each injected fault is logged.

```bash
CANVAS_TEST_HOOKS=1 cargo run -p server
curl -s -X POST http://127.0.0.1:8080/v1/counters/home/increment          # {"count":1,"ok":true}
curl -s -X POST -H 'content-type: application/json' \
  -d '{"status":503,"count":2}' http://127.0.0.1:8080/v1/test/fault       # next 2 mutations -> 503
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

The engine keeps a SQLite cache at `~/.canvas/cache.db` (tables `entities`, `active_workspace`,
`action_outbox`; see docs/DECISIONS.md D13). Every fetched workspace (layout + entities) is written there and
loaded on startup, so the last workspace shows when the server is unreachable; the fetch is retried every
2 seconds until the server responds. Each click updates the entity and queues a PENDING `action_outbox` row in
one transaction (`sqlite3 ~/.canvas/cache.db "SELECT * FROM action_outbox;"`). The old
`workspace_<id>.json` cache files are ignored.

Queued rows are sent by a background sync worker (docs/DECISIONS.md D14), oldest first and one at a time,
as `POST <base_url><endpoint>`. Clicks made while offline (or with the app closed before they were sent) stay
in the queue and the UI shows "N pending"; they are flushed as soon as the server is reachable again,
including on the next launch. Each accepted row is deleted. A row the server rejects with a 4xx (e.g.
decrementing below 0) is deleted and its field rolled back, with later queued clicks re-applied on top. A 5xx,
408, 429 or network error keeps the row and retries it with exponential backoff (2 s, 4 s, ... capped at 60 s). A
re-fetched workspace has the still-pending clicks re-applied, so they never visibly disappear. The engine logs
each attempt, e.g. `[CanvasEngine] outbox #3 POST http://127.0.0.1:8080/v1/counters/home/increment -> 200,
deleted (0 pending)`.

## Fake Pixelfed upstream

`crates/fake-pixelfed` is a local Mastodon-compatible stand-in for a Pixelfed instance (no real account is
needed; docs/DECISIONS.md D16). It serves `fixtures/pixelfed/` with in-memory state that resets on restart.

```bash
cargo run -p fake-pixelfed                       # listens on 127.0.0.1:8081
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/timelines/home | jq '.[].id'
                                                 # (started with FAKE_PIXELFED_TOKEN=dev)
```

Environment:

- `FAKE_PIXELFED_ADDR` (default `127.0.0.1:8081`).
- `FAKE_PIXELFED_PUBLIC_URL` (default `http://<bound addr>`): every `{{BASE}}` in the fixture JSON is replaced
  with it, so emitted media/profile/status URLs point at it. Use `http://10.0.2.2:8081` when the Android
  emulator must load them directly.
- `FAKE_PIXELFED_AUTO_APPROVE=1`: `GET /oauth/authorize` redirects straight back with a code (same as adding
  `auto=1` to the query); otherwise it shows an HTML page with an **Authorize** button.
- `FAKE_PIXELFED_TOKEN=<tok>`: a pre-seeded valid access token, to skip OAuth in QA.
- `FAKE_PIXELFED_FIXTURES`: fixture directory (default `fixtures/pixelfed`).

Endpoints: `POST /api/v1/apps` and `POST /oauth/token` (form-urlencoded or JSON bodies), `GET|POST
/oauth/authorize` (authorization code + PKCE; only `S256` accepted, the verifier is checked, codes are single-use
and expire after 10 minutes), `GET /api/v1/accounts/verify_credentials`, `GET /api/v1/timelines/home` (`limit`,
`max_id`, `Link: rel="next"`), `POST /api/v1/statuses/{id}/{favourite,unfavourite,bookmark,unbookmark}`
(idempotent, returns the updated status; a reblog id acts on the reblogged status; unknown id -> 404
`{"error":"Record not found"}`) and media under `/storage/<file>`. The `/api/v1` endpoints other than apps need
`Authorization: Bearer <token>`, else 401 `{"error":"The access token is invalid"}`. Each request, app
registration, code/token issuance and favourite/bookmark change is logged at info.

The timeline has five entries: a 3-image carousel, a reblog of a single-image post, a landscape single image
(already favourited, no alt text), a text-only post, and a content-warning (sensitive) post with a portrait
image. Media are small generated PNGs with a big digit (1-6) or avatar letter so screenshots show which image is
visible.

## Pixelfed (pod adapter)

The server (`server::pixelfed`, docs/DECISIONS.md D17) adapts one Pixelfed instance: it owns the upstream
access token, normalizes the home timeline into Canvas entities and serves the `pixelfed_home` workspace. The
client only holds a pod session token and sends `Authorization: Bearer <session>`.

Environment:

- `PIXELFED_INSTANCE_URL` (default `http://127.0.0.1:8081`, the fake): base for pod->upstream calls. The only
  instance accepted in auth calls.
- `PIXELFED_PUBLIC_URL` (default = instance URL): base for browser-facing links (the `authorize_endpoint`),
  e.g. `http://10.0.2.2:8081` for the Android emulator while the pod calls `127.0.0.1:8081`.

Endpoints:

- `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}` -> `{client_id, scopes, authorize_endpoint}`
  (registers an app upstream once per redirect_uri). Another instance -> 400 `instance_not_allowed`.
- `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}` -> `{session_token,
  account:{id, username, display_name, avatar_url}}`. Upstream rejection -> 400 `oauth_exchange_failed`.
- `POST /v1/auth/logout` (bearer) -> `{ok:true}`; deletes the session (401 if it is not valid).
- `GET /v1/workspaces/pixelfed_home`: with a valid session, the home feed bundle (`queries["timeline:home"]` +
  posts/users/media entities, PostCard layout with like/bookmark optimistic mutations); without one (or when the
  upstream rejects the stored token) a signed-out bundle with a "Sign in with Pixelfed" `action.auth.login`
  button; upstream unreachable -> 502.
- `POST /v1/pixelfed/posts/like {id, liked}` and `POST /v1/pixelfed/posts/bookmark {id, bookmarked}` -> `{ok, id,
  is_liked, like_count, is_bookmarked}`. No session 401; upstream 4xx passed through; upstream 5xx/unreachable 502.
- `GET /v1/pixelfed/media/{key}`: unauthenticated media/avatar proxy. Every image URL in a bundle is rewritten to
  `http://<request Host>/v1/pixelfed/media/<key>`; unknown keys are 404.
- `POST /v1/demo/ack`: stateless 200 used by the static `pixelfed_demo` fixture's like button.

Try it with curl against the fake (auto-approve skips the browser):

```bash
FAKE_PIXELFED_ADDR=127.0.0.1:8081 FAKE_PIXELFED_AUTO_APPROVE=1 cargo run -p fake-pixelfed &
cargo run -p server &
curl -s -X POST -H 'content-type: application/json' http://127.0.0.1:8080/v1/auth/pixelfed/app \
  -d '{"instance_url":"http://127.0.0.1:8081","redirect_uri":"http://127.0.0.1:9/cb"}'
# GET <authorize_endpoint>?response_type=code&client_id=..&redirect_uri=..&scope=read+write&state=s
#   &code_challenge=<S256(verifier)>&code_challenge_method=S256  -> 302 to redirect_uri?code=..
curl -s -X POST -H 'content-type: application/json' http://127.0.0.1:8080/v1/auth/pixelfed/token \
  -d '{"instance_url":"http://127.0.0.1:8081","code":"..","code_verifier":"..","redirect_uri":"http://127.0.0.1:9/cb"}'
curl -s -H "Authorization: Bearer $SESSION" http://127.0.0.1:8080/v1/workspaces/pixelfed_home | jq .queries
```

## Simulation mode

A headless feedback loop: `:desktopApp:simulate` renders the real shared `CanvasApp` to an offscreen
virtual screen (no window), runs a small script against it and writes PNG screenshots. A run takes about
2 seconds. Run it from `kmp-app` with `JAVA_HOME` set as above:

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
  -Pscript="shot start; click Increment; click Increment; assert-text Count: 2; shot after; offline; shot offline"

./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pscreen=desktop
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home \
  -Pscript="wait-images; shot feed; swipe-left tag:carousel:; assert-text Page 2 of 3; shot page2; click Like; assert-text 28; shot liked"
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
| `-Pfixture` | per workspace | Bundle JSON in fixture mode. Defaults: `home` uses `fixtures/counter_home_bundle.json`, `pixelfed_demo` uses `fixtures/pixelfed_home_bundle.json` (one post), `pixelfed_home` uses `fixtures/pixelfed_home_feed_bundle.json` (the fake upstream's five-post timeline, normalized as the pod does, with `queries`). Other workspaces need `-Pfixture`. |
| `-PbaseUrl` | `http://127.0.0.1:8080` | Server for live mode. |
| `-PtimeoutMs` | `10000` | Live mode only: how long to wait for the workspace to load before the first step. |
| `-Pout` | `kmp-app/desktopApp/build/sim/<yyyyMMdd-HHmmss>` | Output directory. |
| `-Pstorage` | fresh temp dir | Live mode only: engine storage dir (holds `cache.db`). It is kept after the run, so a second run with the same dir exercises restart persistence. The temp default is deleted on exit. |

Relative paths (`-Pfixture`, `-PscriptFile`, `-Pout`, `-Pstorage`) resolve against the **repo root**, the task's working
directory, so `-Pfixture=fixtures/counter_home_bundle.json` works.

**Fixture mode** (the default) needs no server, and no Rust build when run with `-PskipRustBuild` (without
it, the desktop compile triggers `:shared:buildRustFfi`, i.e. cargo). A fake in-memory engine
(`FakeEngineBridge`) loads the bundle, applies `OptimisticMutate` clicks with the D4 expression subset, and
toggles connectivity for `offline`/`online`. It models the outbox: clicks made while offline are applied
locally and queued (the status line shows "N pending"), and `online` flushes them FIFO to an in-memory fake
server. That server rejects a `.../decrement` that would take the count below 0; the fake then rolls the
field back and re-applies later queued rows (D8). **Live mode** runs the real Rust engine through JNA against
the server. By default it uses a fresh temp storage dir, so it neither reads nor writes `~/.canvas`; pass
`-Pstorage=<dir>` to keep a persistent one. If the workspace
does not load within `timeoutMs`, it logs a warning and still runs the script (shots show Loading/Offline).
`offline`/`online` are not supported in live mode; a live-mode script containing them is rejected before any
step runs.

**Images** (Coil, DECISIONS D18) are deterministic. In fixture mode the network is never used: any image URL whose
path ends in `/storage/<file>` or `/v1/pixelfed/media/<file>` is served from `fixtures/pixelfed/media/<file>` (the
`pixelfed_home` fixture points at `http://127.0.0.1:8081/storage/...`, the fake upstream's URLs); every other URL
fails at once and the widget shows its error state (e.g. `pixelfed_demo`'s pixelfed.social URL). In live mode
images load over HTTP from wherever the URLs point (the pod's media proxy). Every `shot` first waits up to 5 s for
on-screen images to finish loading (and their crossfade to end); `wait-images` waits explicitly and fails on
timeout.

Script commands are separated by `;` or newlines. Each command is trimmed, blank entries are skipped, and
entries starting with `#` are comments. An argument cannot contain `;`, and since `;` separates commands, a
`#` comment ends at the next `;` (the rest of the line is parsed as commands).

| Command | Effect |
|---|---|
| `shot <name>` | Writes `<NN>-<name>.png`, where NN is the step number. |
| `click <target>` | Clicks the node `<target>` selects (below). Fails if no node matches. |
| `wait <ms>` | Sleeps, advances the UI clock, then waits for idle. At most 600000 (10 minutes). |
| `idle` | Waits until the UI is idle. |
| `assert-text <text>` | Fails unless some node's text or content description contains `<text>`. Retries for up to 2 s. |
| `assert-no-text <text>` | The inverse of `assert-text`. |
| `wait-text <ms> <text>` | Like `assert-text` with a custom timeout (at most 600000), e.g. `wait-text 15000 Count: 3` while a live sync worker retries. |
| `wait-images [ms]` | Waits until no image is loading (default 10000 ms, at most 600000). Works in both modes. |
| `swipe-left <target>` / `swipe-right <target>` | Touch-swipes the selected node; `swipe-left` on a carousel shows the next page. |
| `scroll-to <target>` | Scrolls the feed until the selected node is visible (e.g. `scroll-to Kept my distance`). |
| `offline` / `online` | Fixture mode only: flips the engine's connectivity. |
| `screen <WxH or preset>` | Restarts the renderer at the new size. Engine state carries over; UI state such as scroll position does not. |

A `<target>` is either `tag:<testTag>` (an exact test tag, else the first tag containing it) or text: an exact text
match, then an exact content description (clickable nodes preferred for both), then the same as substrings; the
first match wins. Pixelfed widgets carry content descriptions `Like`/`Unlike`, `Bookmark`/`Remove bookmark`,
`Comment`, the image alt text, and `Page n of N` on the carousel dots (plus an `n/N` badge text), and test tags
`carousel:<post uri>`, `carousel-dots:<post uri>`, `like:<post uri>`, `bookmark:<post uri>`, `comment:<post uri>`,
e.g. `click tag:like:pixelfed://posts/781254316723458049` or `swipe-left tag:carousel:` (the first carousel).

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
shows other native output from the app process (e.g. GL/EGL driver lines). The SQLite cache (`cache.db`) is in the app's
`filesDir`.

On a physical phone, use `adb reverse tcp:8080 tcp:8080` and `--es baseUrl http://127.0.0.1:8080`.

Cleartext caveat: the debug `network_security_config.xml` (cleartext only to `10.0.2.2`, `localhost`,
`127.0.0.1`) governs Java/Kotlin networking only. All engine traffic goes through Rust (`reqwest`), which that
config does not cover, so the engine will talk plain HTTP to any `baseUrl` it is given, in debug and release.
The real controls are the base URL itself (`BuildConfig`, and intent extras that only debug builds honour).
Enforcing HTTPS in the engine for release builds is a follow-up.

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
# Note: AGP uninstalls the app afterwards, so don't follow this with android_smoke.sh --skip-install.
./gradlew :androidApp:connectedDebugAndroidTest
```
