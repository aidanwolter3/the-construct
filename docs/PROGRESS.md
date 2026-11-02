# Progress Log

Running log of implementation work for Milestones 1-7 of DESIGN.md, plus a headless simulation mode.
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

## Milestone 5: SQLite cache + outbox

- Server counters persist in SQLite (`CANVAS_DATA_DIR`, default ~/.canvas-server); decrement below zero is a 422;
  test fault hook with `CANVAS_TEST_HOOKS=1` (D12). client-core stores entities/workspace/outbox in
  `<storage>/cache.db` (rusqlite bundled, user_version migrations, D13); optimistic mutate updates the entity and
  inserts the outbox row in one transaction; `seq`/`pending_actions` in snapshots; "N pending" in the UI.
- Sync worker thread (D14): FIFO flush while online, 2xx delete, 4xx rollback to prev value + re-apply later rows,
  5xx/408/429/network capped exponential backoff; refetch re-applies pending rows; engines on one file share a
  flush lock; dropping the engine stops new POSTs.
- Review fixed: double-send by two engines on one cache.db (HIGH), clock-skew stall, worker not stopping on drop,
  "Reset" squeezed on phone (horizontal containers now FlowRow). QA fixes: read-only cache.db falls back to an
  in-memory copy, busy timeout 250 ms (locked DB no longer freezes the UI 5 s), 408/429 retried, flaky drop test
  made deterministic, fetch-failure log throttled.
- QA desktop: real app + real ~/.canvas/cache.db: 3 offline clicks -> 3 PENDING rows + "3 pending", server
  restart flushes in order, counts match; rollback, 503 retry 2/4/8 s, relaunch merge; latency median 0.5 ms.
  QA Android (emulator): offline rows, force-stop persistence, flush, rollback, rotation, instrumented 3/3.
- Tests: cargo 80, shared 29, desktopApp 17, android instrumented 3.
- Deferred: idempotency keys (a POST applied server-side but timed out client-side is re-sent); toggle payload
  "$value" is not rewritten on re-apply; Android emulator latency 1-6 ms (desktop well under).

## Milestone 6: Pixelfed + OAuth PKCE

- Protocol: `queries`, `OptimisticMutate.also` (multi-field like + count), ternary expressions (D4, D7, D15).
- Fake upstream `crates/fake-pixelfed` (D16): Mastodon API + real S256 PKCE, `{{BASE}}` fixtures, generated media.
- Pod adapter `server::pixelfed` (D17): pod owns the upstream token, client holds only a pod session; feed
  normalization (reblogs, HTML captions, lenient types); like/bookmark actions; token refresh; media proxy with an
  origin allowlist; migrations v2-v4.
- client-core (D19): `queries` + multi-field outbox rows, PKCE sign-in on a background thread (loopback port 47823
  or `canvas://oauth`), session in a secure store, 401 policy (pause, never roll back a like).
- UI (D18): Coil 3.4 images, PostCard, carousel with a fixed clamped aspect, page dots, CW handling.
- Platform glue (D21): macOS Keychain + browser, Android Keystore store + `canvas://oauth` (`singleTask`), simulator
  auto-approve and scripted Pixelfed login browsers.
- Real local Pixelfed 0.15.2 (D20): Colima stack in `dev/pixelfed`, `scripts/pixelfed_dev.sh`, recorded fixtures,
  pod adapted to its differences (`include_reblogs`, `localhost`-only web routes, Passport token shape).
- M6-X: the fake is the primary e2e target (D16 amended, D20 optional): embedded fixtures (single binary,
  Dockerfile), `/_admin/{reset,state,faults}` + `X-Fake-Fault`, `crates/e2e` (pod + engine in-process, one suite for
  the fake or a real instance, upstream fault tests), `scripts/e2e.sh`.
- Review/QA fixes: owner pinning (`account_not_owner`), session TTL, media SSRF allowlist + redirect allowlist +
  25 MB cap, redirect-URI allowlist, stale upstream app recovery (`client_credentials` check), compare-and-delete on
  upstream 401, restart-safe custom-scheme sign-in (`pending_auth:`), private cache purge on sign-out with
  `secure_delete`, secure-store I/O off the UI thread, readable sign-in errors, "Loading your feed…" until the feed
  lands, CW label and "Photo n of N" alt fallback.
- QA evidence: desktop simulator vs the fake and vs the real local Pixelfed (sign-in, feed with boost, carousel
  swipe, like and offline like -> flush -> `favourited` upstream); Android emulator against the real instance
  (`adb reverse tcp:8090`, real browser login, `canvas://oauth` return, like reaching Pixelfed); e2e suite against
  the real instance (alice) passed with upstream state restored.
- Status: DONE.

## Milestone 7: AI layout engine

- Versioned layouts (D22): migration v5 `layout_versions`, append-only (v1 = built-in layout, rollback appends a
  copy), `X-Canvas-Layout-Version`, `GET .../versions`, `POST .../rollback`, SSE `GET .../events` (broadcast,
  keepalive 15 s, lag -> current version, shutdown on Ctrl-C).
- Agent endpoint (D25): `POST /v1/agent/modify_layout` -> RFC 6902 patch -> `json-patch` applied op by op ->
  validation (deserializes as `UiNode`, same root, 1..=50 unique slots, depth/size/op bounds, no invented intents)
  -> compare-and-append commit (re-applied on a concurrent commit, else 409) -> SSE push. Providers: offline `rules`
  (composable, idempotent phrases) and Anthropic (structured outputs with a schema registry derived from the
  protocol types, prompt caching, retries, 170 s total bound). Unchanged results commit nothing.
- client-core (D24): `action.agent.prompt`, `agent_json` status, layout applied from the response without a
  refetch, layout version tracking, SSE subscriber thread (async reqwest on a private runtime, backoff, 45 s
  silence reconnect, 64 KB event cap) that refetches on newer versions with pending outbox rows re-applied.
- UI (D23): FAB + in-tree bottom panel with chips, status line, Back closes it, `adjustResize`/IME insets;
  slot-keyed recomposition so the carousel keeps its page; simulator `type` command and fake rules.
- Review/QA fixes: patch validation bounds (depth, size, op count, aspect/padding ranges), no invented
  endpoints/targets, compare-and-append commits, prompt panel not painted over sign-in, total provider timeout,
  double-submit guard, readable agent errors and "No change needed (vN)", stale status reset on a pushed version,
  padded weather slot, workspace id limits, SSE line/event size cap.
- QA evidence: curl runs of prompt -> version -> rollback with the SSE stream receiving each version; desktop
  simulator prompts (weather added, no change needed, unsupported prompt, panel closed); Android emulator against
  the pod + fake: sign-in, prompt with the IME (portrait/landscape), layout updated in place, queued like flushed
  after the change, rollback pushed to the device. The Anthropic provider is covered by tests against a mock
  server only (no API key in this environment).
- Tests: cargo 257 (includes the e2e suite against the in-process fake), shared 49, desktopApp 54, Android
  instrumented 6. e2e: the suite runs against the fake (in-process in `cargo test`, or the binary/Docker image via
  `scripts/e2e.sh [--faults]`) and optionally against a real Pixelfed (`scripts/e2e.sh --upstream real`, passed
  against the local instance in M6-X).
- Status: DONE.

## Layout browser + version history

- A1 (server, D26): layout catalog. Each versioned workspace has several layouts (`<ws>.<name>`), each with its own
  append-only, gapless history, and exactly one active layout.
  - Built-ins seeded idempotently (`fixtures/layouts/`, `include_str!`): `pixelfed_home` / `pixelfed_demo`:
    Classic feed (default), Editorial journal, Compact, Minimal; `home`: Counter (default), Big counter. New
    built-ins reach existing databases without touching history or the active layout. Every seed validates against
    its workspace's default layout with the agent validator (tested).
  - Workspace revision log (`workspace_revisions`): one monotonic number per workspace, bumped by agent commits,
    restores of the active layout and switches. `X-Canvas-Layout-Version` and SSE `version` carry it, so client-core
    is unchanged; new `X-Canvas-Layout-Id` / `X-Canvas-Layout-Id-Version` headers and `layout_id` /
    `layout_version` fields in the SSE event and `layout_id` / `layout_id_version` in the agent response.
  - Endpoints: `GET /v1/layouts[?workspace_id=]`, `GET /v1/layouts/:id`, `GET /v1/layouts/:id/versions[?limit,
    before]`, `GET /v1/layouts/:id/versions/:n`, `POST /v1/layouts/:id/activate {version?}`, `POST
    /v1/layouts/:id/restore {version}` (append-only copy; equal content -> `unchanged`). Reads follow the workspace
    GET's auth (pixelfed_home needs a session; the unfiltered list hides it from anonymous callers), writes need a
    session unless `CANVAS_AGENT_OPEN=1`. `/v1/workspaces/:id/versions` and `/rollback` act on the active layout.
  - Agent commits to the active layout; compare-and-append also detects a switch (re-applies the patch on the new
    active layout, else 409).
  - Storage (migration v6): specs are content-addressed zstd-19 blobs (`layout_blobs`), so restores, rollbacks and
    re-seeds store no new blob. Measured: a distinct agent edit ~1.16 KB vs ~2.08 KB before; a restore ~0.18 KB vs
    ~2.08 KB; 100k versions ~64 MB (50/50 edits/restores) vs ~198 MB. Every version is kept (no retention knob).
    v5 history becomes the history of each workspace's default layout (same numbers, prompts, patches, times), so
    revisions continue from the old version and connected clients never see the number go backwards (tested on a
    v5 db built with migrations 1..=5, and by hand with a database written by the previous server binary).
  - Tests: store unit tests (seeding, compare-and-append across switches, dedupe, activate/restore, v5 -> v6
    migration, blob codec), `tests/catalog.rs` (list/get/versions/activate/restore over HTTP, SSE on switch and
    restore, auth, agent on a non-default layout, a switch during generation, restart idempotency). cargo 272.
  - Measurement tool: `cargo run -p server --release --example layout_storage [N]`.
- Status: A1 DONE (server); the surfaces' layout browser UI is the client half.
- A2 (client-core + FFI, D27): catalog API for the browser UI.
  - `CanvasEngine::{list_layouts, layout_versions(id, before, limit), layout_version(id, n), activate_layout(id,
    version?), restore_layout_version(id, n)}`: blocking (callers use a background thread), engine base URL + pod
    session, 15 s timeout, the pod's JSON passed through. Errors: `LayoutApiError::{Offline, Unauthorized, NotFound,
    Server{status}}` with a readable `reason` ("Sign in to change layouts.", "That version doesn't exist.", ...); a
    401 for the current session signs out and refetches (D19 policy). An activate / restore that changed this
    workspace's screen refetches before returning, so the caller's next snapshot already shows the new layout
    (pending outbox rows re-applied by the merge; the SSE echo is a no-op).
  - Snapshot: `layout_json` = `{"layout_id","layout_version","revision"}` from the new headers (and the agent
    response), `"{}"` when unknown / unversioned. FFI: `EngineHandle` methods returning `Result<String,
    LayoutApiError>` (Kotlin `uniffi.client_ffi.LayoutApiException`), `FfiStateSnapshot.layout_json`.
  - Kotlin: `CanvasEngineBridge.{listLayouts, layoutVersions, layoutVersion, activateLayout, restoreLayoutVersion}`
    (blocking; default implementations throw), `LayoutApiException(message, kind, status)`,
    `EngineSnapshot.layoutJson` -> `CanvasState.activeLayout` (`ActiveLayout`). `UniffiEngineBridge` maps the UniFFI
    error; `FakeEngineBridge` serves an in-memory `FakeLayoutCatalog` (home: counter/big_counter; pixelfed_demo:
    classic with 3 versions, editorial, compact) with the pod's JSON shapes.
  - Tests: `client-core/tests/layouts.rs` against the in-process pod (list + snapshot layout_json, version paging
    and fetch, activate applies before returning, restore of active/inactive layouts, 401 without / with a stale
    session (signed out), offline), unit tests for error sentences; FFI offline mapping; shared `LayoutCatalogTest`,
    desktop `FakeLayoutCatalogTest`. cargo 280, :shared 51, :desktopApp 56.
- Status: A2 DONE (client API); the browser UI (A3) is next.

### Deferred / known issues (M6/M7)
- No idempotency keys on outbox POSTs: a request applied upstream but timed out client-side is re-sent (likes and
  bookmarks are idempotent upstream, counters are not).
- Toggle payloads (`"$value"`) are not rewritten on re-apply (D14); local and server state can differ until the
  next fetch.
- The client's layout version is in memory only (refetched at startup, D24).
- Anonymous SSE subscriptions are allowed and the number of open streams is not capped (events carry only a
  version number, D22).
- The media proxy is unauthenticated by design (opaque keys over public upstream media, D17).
- The feed is a plain scrolling `Column` (every post composed), not a `LazyColumn`; fine for a 20-post page.
- The stored session is read from the secure store (Keychain) synchronously while constructing the engine, i.e. on
  the caller's thread; writes are already off the UI thread (D19).
- Single tenant: one pinned Pixelfed account per pod; one timeline page (no pagination).
- Engine traffic is not HTTPS-enforced on Android release builds (D11).
- Not yet exercised: the Anthropic provider against the live API, and sign-in against a public Pixelfed instance.

## Next steps

1. Layout browser UI on every surface (A3) on top of the D27 bridge calls (`listLayouts`, `layoutVersions`,
   `layoutVersion`, `activateLayout`, `restoreLayoutVersion` on `Dispatchers.IO`; `CanvasState.activeLayout` marks
   the active layout/version): preview by `preview_hint` or the version's `layout`, activate, history with restore.
   Optional later: user-created layouts (copy a layout), a zstd dictionary for
   blobs (measured ~6x smaller edits), a retention policy.
2. Feed as a `LazyColumn` with pagination (`max_id` / `Link: next`).
3. Outbox idempotency keys (client-generated, honoured by the pod).
4. Live test of the Anthropic provider once a key exists (prompt quality, latency, refusals).
5. Real-Pixelfed contract test in scheduled CI on a throwaway instance (`scripts/e2e.sh --upstream real`).
6. HTTPS enforcement in the engine for release builds; a cap on SSE subscribers.
7. Web target for the shared UI; small follow-ups: 404 "workspace not found" UI state, golden-image diffing for
   the simulator, Android host tests for :shared.
