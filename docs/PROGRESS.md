# Progress Log

Running log of implementation work for the first demo (Milestones 1-4 of DESIGN.md, plus a headless simulation mode).
Process: orchestrator coordinates separate implementation, code-review and QA passes.
Note: the environment restarted twice mid-work; work is now committed in small wip commits.

## Milestone 1: Protocol crate

- protocol crate implemented: `ast.rs` (UiNode, SlotSpec, Orientation, TextStyle, ListBinding),
  `intent.rs` (ActionIntent, PresentationMode), `bundle.rs` (WorkspaceSpec, EntityRecord).
- `fixtures/pixelfed_home_bundle.json` is the exact §2.2C bundle; `tests/roundtrip.rs` has 10 tests (pass).
- client-core, client-ffi, server are stubs so the workspace builds.
- Decisions recorded in docs/DECISIONS.md.
- Review/QA: pending.

- Review (independent agent): wire format matches DESIGN.md; added counter-fixture test and f32 aspect_ratio
  semantics test; silenced clippy large_enum_variant. Deferred: unknown fields are ignored (lenient), root node
  not forced to be core://Workspace.
- Status: DONE (12 tests).

## Milestone 2: Axum server

- crates/server: lib `app()` + bin; routes GET /healthz, /v1/workspaces/{home,pixelfed_demo}, 404 JSON for unknown,
  POST /v1/counters/home/{increment,reset} (stateless ack). Fixtures embedded with include_str! and validated at startup.
- Review+QA (independent agent): fixed HTTP/2 (h2c) not enabled, empty-body POST with JSON content-type
  rejected, non-JSON error bodies, path-vs-body error ordering, port-in-use message, ANSI colors in logs.
  Curl evidence: home returns 200 application/json; h2c works; unknown id returns 404 JSON.
- Status: DONE (10 tests).

## Milestone 3: macOS desktop shell

- client-core CanvasEngine: background fetch thread (reqwest blocking + rustls), observers called without locks,
  D4 expression evaluator (expr.rs), per-workspace offline JSON cache at ~/.canvas/workspace_<id>.json,
  retry every 5s while the server is unreachable, HTTP errors keep the engine online. Snapshot
  workspace_json = layout node only; entities_json = URI-keyed map.
- client-ffi: UniFFI proc-macro EngineHandle {new, new_with_workspace, subscribe, dispatch, snapshot, refresh},
  FfiStateSnapshot, StateObserver callback interface; `uniffi-bindgen` bin. scripts/build_ffi.sh builds the
  aarch64-apple-darwin dylib, copies it to target/release, and regenerates
  kmp-app/desktopApp/src/main/kotlin/uniffi/client_ffi/client_ffi.kt. The Gradle task :desktopApp:buildRustFfi runs it
  before compileKotlin (skip with -PskipRustBuild).
- kmp-app: Gradle 9.3.1, Kotlin 2.3.20, Compose MP 1.11.0-beta01, material3 1.11.0-alpha05, kotlinx-serialization
  1.10.0, JNA 5.17.0. :shared (jvm("desktop") target only; ready for androidTarget later) contains Model.kt
  (hand-written tagged-union serializers with Unknown fallback), Interpolate.kt, DynamicRenderer.kt (all 9 node
  types plus an Unknown placeholder), CanvasApp.kt (CanvasController + StateFlow). :desktopApp contains Main.kt and
  EngineBridge.kt (JNA/UniFFI).
- Review (independent agents): client-core reviewed. FFI/Kotlin review fixed embedded `$item` tokens in refs,
  added keyed FeedList items, and corrected the README JDK note. Deferred items are listed below.
- QA round 1 (real launch, System Events clicks, window-only screenshots): all checks passed. It found an
  offline cache shared across workspaces (medium), 404 shown as Offline, and no reconnect. All three are fixed.
- QA round 2: 41 Rust tests and 27 Kotlin tests pass. Clicking Increment twice shows "Count: 2"; Reset returns
  to 0. The offline cache is per workspace, a 404 keeps the app Online, and reconnect happens within about 2s of
  the server starting. Screenshots are in the session scratchpad (qa/ and qa2/).
- Status: DONE.

### Deferred / known issues
- Clicks are applied locally only; no server sync until the M5 outbox. rollback_value is the fixture's static value.
- An unknown workspace (404) leaves the UI on "Loading..." with no "not found" message.
- A bad enum value or missing field in any node fails the whole layout decode. This is strict, like Rust serde;
  only unknown `type`/`intent` values degrade to placeholders.
- Snapshots can in rare cases be applied out of order (a fetch notification racing a click). Fix: add a sequence
  number to FfiStateSnapshot.
- FeedList ignores query_key and shows every `/posts/` entity, sorted by created_at.
- client-ffi enables uniffi/cli by default (revisit for Android cross-builds in M4).
- build_ffi.sh is aarch64-apple-darwin only.

## Phase A: Simulation mode (headless screenshots)

- `:desktopApp:simulate` (JavaExec, `sim` source set) renders the real shared `CanvasApp` with
  `runSkikoComposeUiTest` from a plain main, headless, and writes PNGs plus steps.log/steps.json. It has a
  fixture mode (in-memory FakeEngineBridge) and a live mode (the real JNA engine). See README "Simulation mode"
  and D8.
- Review (independent agent) fixed: fake engine accepted unquoted literals expr.rs rejects; JVM could hang on
  unexpected exceptions in live mode; a parser index crash; buildRustFfi's onlyIf broke the configuration cache.
- QA (independent agent): 9/9 checks pass (fixture + live, both workspaces, phone/tablet/desktop, ~1.4s per run,
  no window, 10 PNGs viewed). Low findings fixed: unwritable -Pout now exit 2; live-mode offline/online rejected
  before running; NN-FAILED.png on failure; warning for unknown -P flags; wait capped at 600000 ms; README notes.
- Deferred (low): fake engine can deliver snapshots out of order under concurrent emits; live engine is never
  shut down (process exits); default out dir is per-second; no golden-image diffing (Roborazzi) yet.
- Tests: shared 26, desktopApp 12. Status: DONE.

## Milestone 4: Android

- Rust: client-ffi `bindgen` feature now opt-in (no CLI deps by default); scripts/build_android.sh (cargo-ndk,
  arm64-v8a + x86_64 into kmp-app/androidApp/src/main/jniLibs; deps only liblog/libdl/libc); Rust stdout/stderr
  forwarded to logcat tag CanvasEngine on Android (client-ffi/src/android_log.rs). D9.
- Gradle: AGP 9.1.1 `com.android.kotlin.multiplatform.library` on :shared; new `jvmCommonMain` source set holds
  the UniFFI bindings and `UniffiEngineBridge` (JNA jar on desktop, aar on Android); :shared owns buildRustFfi.
  D5 amended, D10. Tests: cargo 42, shared 26, desktopApp 12.

- :androidApp (AGP 9.1.1 application, activity-compose 1.12.4): MainActivity + CanvasViewModel (engine survives
  rotation, closed in onCleared), BuildConfig/intent-extra baseUrl (default http://10.0.2.2:8080) and workspace,
  filesDir storage, debug-only cleartext config, `buildRustAndroid` task before preBuild. assembleDebug builds an
  APK with libclient_ffi.so + libjnidispatch.so for arm64-v8a and x86_64. D11.
- Review fixed: release builds ignore intent extras (exported launcher), buildRustFfi inputs, doc corrections.
- QA round 1 found a BLOCKER: launch crash, because Interpolate.kt's regex had an unescaped `}` that Android ICU
  rejects. Fixed. Added scripts/android_smoke.sh (install, launch, check for FATAL, assert text through
  uiautomator, screencap) and an instrumented InterpolateDeviceTest (3 tests, connectedDebugAndroidTest).
  With the fix, QA steps 5-11 passed (counter, offline/online, pixelfed Like, rotation, cache, ripple).

- QA round 2 on the committed build (emulator Medium_Phone_API_36, arm64): all checks pass. Fetch over 10.0.2.2
  is visible in `logcat -s CanvasEngine:V` (Rust and Kotlin lines), with no FATAL. Count 0->2->0. Offline shows within
  2s of stopping the server and Online returns within 5s of restarting it. pixelfed_demo renders and Like toggles.
  Rotation keeps one engine. A fresh install with the server down recovers. android_smoke.sh passes for both
  workspaces, connectedDebugAndroidTest passes 3/3, and the desktop simulator still works.
- Finding (documented, not fixed): the network security config does not restrict the engine's cleartext traffic,
  because requests go through Rust reqwest. See D11 caveat.
- Tests: cargo 42, shared 26, desktopApp 12, androidApp instrumented 3. Status: DONE.

### Deferred / known issues (M4)
- Enforce HTTPS in the engine for release builds; the network security config doesn't apply to Rust traffic.
- An unknown workspace (404) shows "Online" + "Loading..." forever with no on-screen error (desktop and Android).
- The CanvasEngine logcat tag also carries other native stderr from the process (e.g. GL lines); the fd redirect is
  process-wide. Log lines over ~4 KB are truncated by logcat.
- Android builds use the committed UniFFI bindings; if the Rust API changes without regenerating them, the
  checksum check fails at load.
- :shared has no Android host tests; Android-only behaviour is covered by the smoke script + instrumented test.
- Deprecated `compose.runtime/foundation/ui` accessors in shared/build.gradle.kts.

## Milestone 5: SQLite cache + outbox (in progress)

- Contract for the M5 units: DESIGN §2.3 plus D12 (server persistence) and D13 (client storage).
- Done: server counters persist in SQLite (`CANVAS_DATA_DIR`, decrement rejects below zero with 422, test fault
  hook `CANVAS_TEST_HOOKS=1`). client-core uses `<storage>/cache.db` with transactional optimistic mutate +
  outbox insert, `seq`/`pending_actions` in snapshots, FFI bindings regenerated, Android .so builds.

## Next steps

1. M5 remaining: sync worker (FIFO flush, 2xx delete, 4xx rollback + re-apply, 5xx/network backoff) and
   refetch-merge (`State::apply_fetched`) in client-core; Kotlin: snapshot seq drop + "N pending" indicator,
   FakeEngineBridge outbox, simulator `-Pstorage`; then review + QA (desktop sqlite3 check, Android).
2. M6 Pixelfed + OAuth PKCE against a fake upstream; M7 AI layout engine (rules provider + Anthropic provider).
3. Small follow-ups: 404 "workspace not found" UI state; HTTPS enforcement for release; golden-image diffing for
   the simulator (Roborazzi or a pixel diff on sim PNGs); Android host tests for :shared.
