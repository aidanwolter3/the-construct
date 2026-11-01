# Progress Log

Running log of implementation work for the first demo (Milestones 1-3 of DESIGN.md).
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

## Next steps

1. Phase B (M4 Android): Rust android targets and cargo-ndk are installed. Steps: client-ffi `cli` feature
   off by default; Android target in :shared + :androidApp; move UniFFI bindings/EngineBridge to a shared
   JVM+Android source set; cargo-ndk build task; emulator QA.
3. Then M5 (SQLite + outbox).
