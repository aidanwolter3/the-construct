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

- client-core ConstructEngine: background fetch thread (reqwest blocking + rustls), observers called without locks,
  D4 expression evaluator (expr.rs), per-workspace offline JSON cache at ~/.construct/workspace_<id>.json,
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
  types plus an Unknown placeholder), ConstructApp.kt (ConstructController + StateFlow). :desktopApp contains Main.kt and
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

- `:desktopApp:simulate` (JavaExec, `sim` source set) renders the real shared `ConstructApp` with
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
  forwarded to logcat tag ConstructEngine on Android (client-ffi/src/android_log.rs). D9.
- Gradle: AGP 9.1.1 `com.android.kotlin.multiplatform.library` on :shared; new `jvmCommonMain` source set holds
  the UniFFI bindings and `UniffiEngineBridge` (JNA jar on desktop, aar on Android); :shared owns buildRustFfi.
  D5 amended, D10. Tests: cargo 42, shared 26, desktopApp 12.

- :androidApp (AGP 9.1.1 application, activity-compose 1.12.4): MainActivity + ConstructViewModel (engine survives
  rotation, closed in onCleared), BuildConfig/intent-extra baseUrl (default http://10.0.2.2:8080) and workspace,
  filesDir storage, debug-only cleartext config, `buildRustAndroid` task before preBuild. assembleDebug builds an
  APK with libclient_ffi.so + libjnidispatch.so for arm64-v8a and x86_64. D11.
- Review fixed: release builds ignore intent extras (exported launcher), buildRustFfi inputs, doc corrections.
- QA round 1 found a BLOCKER: launch crash, because Interpolate.kt's regex had an unescaped `}` that Android ICU
  rejects. Fixed. Added scripts/android_smoke.sh (install, launch, check for FATAL, assert text through
  uiautomator, screencap) and an instrumented InterpolateDeviceTest (3 tests, connectedDebugAndroidTest).
  With the fix, QA steps 5-11 passed (counter, offline/online, pixelfed Like, rotation, cache, ripple).

- QA round 2 on the committed build (emulator Medium_Phone_API_36, arm64): all checks pass. Fetch over 10.0.2.2
  is visible in `logcat -s ConstructEngine:V` (Rust and Kotlin lines), with no FATAL. Count 0->2->0. Offline shows within
  2s of stopping the server and Online returns within 5s of restarting it. pixelfed_demo renders and Like toggles.
  Rotation keeps one engine. A fresh install with the server down recovers. android_smoke.sh passes for both
  workspaces, connectedDebugAndroidTest passes 3/3, and the desktop simulator still works.
- Finding (documented, not fixed): the network security config does not restrict the engine's cleartext traffic,
  because requests go through Rust reqwest. See D11 caveat.
- Tests: cargo 42, shared 26, desktopApp 12, androidApp instrumented 3. Status: DONE.

### Deferred / known issues (M4)
- Enforce HTTPS in the engine for release builds; the network security config doesn't apply to Rust traffic.
- An unknown workspace (404) shows "Online" + "Loading..." forever with no on-screen error (desktop and Android).
- The ConstructEngine logcat tag also carries other native stderr from the process (e.g. GL lines); the fd redirect is
  process-wide. Log lines over ~4 KB are truncated by logcat.
- Android builds use the committed UniFFI bindings; if the Rust API changes without regenerating them, the
  checksum check fails at load.
- :shared has no Android host tests; Android-only behaviour is covered by the smoke script + instrumented test.
- Deprecated `compose.runtime/foundation/ui` accessors in shared/build.gradle.kts.

## Milestone 5: SQLite cache + outbox

- Server counters persist in SQLite (`CONSTRUCT_DATA_DIR`, default ~/.construct-server); decrement below zero is a 422;
  test fault hook with `CONSTRUCT_TEST_HOOKS=1` (D12). client-core stores entities/workspace/outbox in
  `<storage>/cache.db` (rusqlite bundled, user_version migrations, D13); optimistic mutate updates the entity and
  inserts the outbox row in one transaction; `seq`/`pending_actions` in snapshots; "N pending" in the UI.
- Sync worker thread (D14): FIFO flush while online, 2xx delete, 4xx rollback to prev value + re-apply later rows,
  5xx/408/429/network capped exponential backoff; refetch re-applies pending rows; engines on one file share a
  flush lock; dropping the engine stops new POSTs.
- Review fixed: double-send by two engines on one cache.db (HIGH), clock-skew stall, worker not stopping on drop,
  "Reset" squeezed on phone (horizontal containers now FlowRow). QA fixes: read-only cache.db falls back to an
  in-memory copy, busy timeout 250 ms (locked DB no longer freezes the UI 5 s), 408/429 retried, flaky drop test
  made deterministic, fetch-failure log throttled.
- QA desktop: real app + real ~/.construct/cache.db: 3 offline clicks -> 3 PENDING rows + "3 pending", server
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
  or `construct://oauth`), session in a secure store, 401 policy (pause, never roll back a like).
- UI (D18): Coil 3.4 images, PostCard, carousel with a fixed clamped aspect, page dots, CW handling.
- Platform glue (D21): macOS Keychain + browser, Android Keystore store + `construct://oauth` (`singleTask`), simulator
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
  (`adb reverse tcp:8090`, real browser login, `construct://oauth` return, like reaching Pixelfed); e2e suite against
  the real instance (alice) passed with upstream state restored.
- Status: DONE.

## Milestone 7: AI layout engine

- Versioned layouts (D22): migration v5 `layout_versions`, append-only (v1 = built-in layout, rollback appends a
  copy), `X-Construct-Layout-Version`, `GET .../versions`, `POST .../rollback`, SSE `GET .../events` (broadcast,
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
    restores of the active layout and switches. `X-Construct-Layout-Version` and SSE `version` carry it, so client-core
    is unchanged; new `X-Construct-Layout-Id` / `X-Construct-Layout-Id-Version` headers and `layout_id` /
    `layout_version` fields in the SSE event and `layout_id` / `layout_id_version` in the agent response.
  - Endpoints: `GET /v1/layouts[?workspace_id=]`, `GET /v1/layouts/:id`, `GET /v1/layouts/:id/versions[?limit,
    before]`, `GET /v1/layouts/:id/versions/:n`, `POST /v1/layouts/:id/activate {version?}`, `POST
    /v1/layouts/:id/restore {version}` (append-only copy; equal content -> `unchanged`). Reads follow the workspace
    GET's auth (pixelfed_home needs a session; the unfiltered list hides it from anonymous callers), writes need a
    session unless `CONSTRUCT_AGENT_OPEN=1`. `/v1/workspaces/:id/versions` and `/rollback` act on the active layout.
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
  - `ConstructEngine::{list_layouts, layout_versions(id, before, limit), layout_version(id, n), activate_layout(id,
    version?), restore_layout_version(id, n)}`: blocking (callers use a background thread), engine base URL + pod
    session, 15 s timeout, the pod's JSON passed through. Errors: `LayoutApiError::{Offline, Unauthorized, NotFound,
    Server{status}}` with a readable `reason` ("Sign in to change layouts.", "That version doesn't exist.", ...); a
    401 for the current session signs out and refetches (D19 policy). An activate / restore that changed this
    workspace's screen refetches before returning, so the caller's next snapshot already shows the new layout
    (pending outbox rows re-applied by the merge; the SSE echo is a no-op).
  - Snapshot: `layout_json` = `{"layout_id","layout_version","revision"}` from the new headers (and the agent
    response), `"{}"` when unknown / unversioned. FFI: `EngineHandle` methods returning `Result<String,
    LayoutApiError>` (Kotlin `uniffi.client_ffi.LayoutApiException`), `FfiStateSnapshot.layout_json`.
  - Kotlin: `ConstructEngineBridge.{listLayouts, layoutVersions, layoutVersion, activateLayout, restoreLayoutVersion}`
    (blocking; default implementations throw), `LayoutApiException(message, kind, status)`,
    `EngineSnapshot.layoutJson` -> `ConstructState.activeLayout` (`ActiveLayout`). `UniffiEngineBridge` maps the UniFFI
    error; `FakeEngineBridge` serves an in-memory `FakeLayoutCatalog` (home: counter/big_counter; pixelfed_demo:
    classic with 3 versions, editorial, compact) with the pod's JSON shapes.
  - Tests: `client-core/tests/layouts.rs` against the in-process pod (list + snapshot layout_json, version paging
    and fetch, activate applies before returning, restore of active/inactive layouts, 401 without / with a stale
    session (signed out), offline), unit tests for error sentences; FFI offline mapping; shared `LayoutCatalogTest`,
    desktop `FakeLayoutCatalogTest`. cargo 280, :shared 51, :desktopApp 56.
- Status: A2 DONE (client API); the browser UI (A3) is next.
- A3 (shared Compose UI + simulator, D30): the layout browser on every surface (desktop, Android, simulator; the
  same commonMain code a web shell would use).
  - Layouts button (`layouts-button`, "Browse layouts") above the AI FAB opens an in-tree full-screen browser:
    list with LIVE thumbnails (the real `RenderUiNode` of each layout's current version against the current entities,
    rendered at 390 dp and scaled into 120x200 dp, non-interactive: no-op dispatch, pointer shield, cleared
    semantics), "Active" + version badges; details with the paged version history (source chips Built-in / AI prompt
    with the prompt / Rollback / Restore / Manual, relative + absolute time, "Current · on screen"); full-size version
    preview with "Restore this version" (active layout) or "Use this version" (other layouts); "Use this layout".
  - In-tree confirmation dialog; on success the browser closes and a snackbar ("Switched to Editorial journal",
    "Restored v2 as v5") offers Undo for 8 s; undo is append-only (re-activate the previous layout at its previous
    version, or restore the previous version as a new copy). Errors inline with Try again (offline, 401 "Sign in to
    change layouts."); a failed write keeps the dialog open with the reason.
  - Navigation is a pure saveable value (`LayoutNav`): Back closes the innermost level; rotation keeps level,
    selection and an open dialog (verified on the emulator). Calls run on `Dispatchers.IO` via `LayoutRepository`
    (spec cache per (id, version), owned by the controller), loads are cancelled with their level. A switch keeps the
    carousel page, the agent prompt text and the outbox (tested).
  - Fake catalog: the pod's real built-ins from `fixtures/layouts/` with the pod's titles/descriptions/hints; classic
    has seed, two AI prompts and a rollback; `pixelfed_home` built-ins for the feed bundle.
  - Simulator: `open-layouts`, `select-layout`, `select-version`, `use-layout`, `restore-version`, `confirm`,
    `cancel`, `undo`, `close-layouts`, `back` (wait for idle and the catalog calls, fail with a hint). Run in fixture
    mode (pixelfed_demo phone + desktop, pixelfed_home, home) and live against a local pod; Android emulator: list,
    details, preview, switch to Editorial, undo, Back per level, rotation.
  - Tests: shared `LayoutBrowserModelTest` (navigation, offered change, undo bookkeeping, messages, labels, relative
    time, lenient parsers), desktop `LayoutBrowserTest` (switch keeps the carousel page + undo, restore + undo is
    append-only, previews never dispatch and the browser blocks the feed, Back per level, offline error + retry,
    401 write keeps the dialog, the agent prompt survives a switch, the open level and Undo survive a new composition,
    command failures, command parsing). :shared 59, :desktopApp 66.
  - Review/QA pass: the browser state moved into `ConstructController` (rotation keeps a running write and the Undo
    snackbar; nav still saved for process death), writes run `NonCancellable`, Back is ignored while a change runs,
    Undo hides once the screen moved on elsewhere (`undoStillApplies`), "Load older" can't stay spinning or append a
    stale page, previews render at most 3 feed items per query (no N x timeline image loads), `select-layout` of a
    missing id fails at once. Re-verified in fixture + live (SSE refreshes the open list's Active badge; 401 path)
    and on the emulator (Undo after rotation).
- Status: A3 DONE (layout browser + version history on every surface).

## App icon and staging default

- App icon "the construct" (D28): a white space with a perspective grid floor and one cube, generated by
  `scripts/gen_icons.sh` into `assets/icon` (SVG, PNG 16-1024, icns, ico). Android adaptive icon with a monochrome
  layer for themed icons; desktop window + dock icon and packaged-app icons. Checked on the emulator launcher
  (normal and themed) and the macOS window. Review: reproducible byte for byte, safe zone respected; follow-up
  added a `tiny` level so 16/32 px read on white.
- Staging default (D29): all surfaces default to `https://the-construct-pod.fly.dev`, with local overrides and a
  debug-only Android server picker (per-server storage). `android_smoke.sh --base-url` passes the local URL
  explicitly. Verified on the emulator with no override: sign-in on staging (fake Pixelfed) and the feed with
  images. Review fixed: shared cache/outbox across servers, `-Dconstruct.baseUrl` not reaching the app JVM, custom URL
  validation, picker state on rotation.
- Status: DONE.

## Rename to The Construct

- Product renamed from Project Canvas to The Construct everywhere, identifiers included (D31): Kotlin packages
  `org.theconstruct.*` (Android `org.theconstruct.android`, desktop bundle `org.theconstruct.desktop`, package name
  "The Construct"), `Construct*` types in Rust/UniFFI/Kotlin (bindings regenerated), logcat tag `ConstructEngine`,
  threads `construct-engine-*`, env vars `CONSTRUCT_*` (incl. Fly `pod.toml` and the pod Dockerfile), system/Gradle
  properties `construct.*` / `-PconstructBaseUrl`, headers `X-Construct-*`, redirect `construct://oauth`, paths
  `~/.construct` / `~/.construct-server`, Keychain/keystore/prefs names, the pod's Pixelfed client name and user
  agent, fixtures (`The Construct Counter`, `constructdemo`, `#construct`).
- No migration (old caches, sessions and the old Android package are not read); staging's `server.db` opens
  unchanged (no schema identifier contained "canvas") and the first `construct://oauth` sign-in registers a new app.
- Verified: cargo clippy + tests, `:shared:allTests`, `:desktopApp:test`, `assembleDebug`, simulator (counter title,
  layout browser script), desktop window title, Android smoke, launcher label, and emulator sign-in via
  `construct://oauth` against a local pod + fake Pixelfed and against staging.
- Status: DONE.

## One app, layouts as code, hamburger menu

Product rule: the pod serves one app (Pixelfed, `pixelfed_home`) and its latest layout version. New versions come
only from the developer, as files in the repo plus a deploy. No client can change layouts, and no AI is reachable
from a client (D32, D33).

- **Server (D32):**
  - Layouts as code: `layouts/<app>/NNNN.json` (`{"note","layout"}`), embedded through `layouts::FILES`. The
    startup sync is append-only and idempotent. It refuses to start, naming the file, on a changed, missing,
    out-of-order, gapped or invalid file (the agent's structural validator).
  - Each 0001 is the former built-in default. The Editorial/Compact/Minimal/Big counter alternatives
    (`fixtures/layouts/`) are deleted.
  - Migration v7 (`app_versions`): workspace revision R -> app version R, showing what was on screen, with a note
    ("Switched to <title>", "Restored <title> vN", the agent prompt, ...). The v6 catalog tables are kept but
    inert. Tested on a DB built with migrations 1..=6.
  - Public read-only API: `GET /v1/apps` (dev apps with `CONSTRUCT_SHOW_DEV_APPS=1`) and
    `GET /v1/apps/{id}/versions[?limit&before]`, `.../versions/{n}`.
  - Removed: the agent route, rollback, workspace versions and all `/v1/layouts` routes, plus
    `CONSTRUCT_AGENT_OPEN` and the `X-Construct-Layout-Id(-Version)` headers. SSE carries `{workspace_id, version}`.
  - The agent module stays in the library, unrouted, with its tests.
- **Protocol:** `ActionIntent::PromptAgent` removed. Unknown intent tags deserialize to `ActionIntent::Unknown`, so
  legacy AI-edited versions still serve and render.
- **client-core / FFI (D33):** `list_apps`, `app_versions(app_id, before, limit)`, `app_version(app_id, n)` (blocking,
  public, pass-through JSON, `LayoutApiError` sentences). Snapshot `layout_json` = `{"app_id","version"}`.
  - Removed: `PromptAgent` dispatch, the agent thread, `agent_json`, catalog writes and old reads.
  - Kotlin bindings regenerated. `UniffiEngineBridge` implements the apps calls and `close()`.
  - Tests: `client-core/tests/apps.rs` against the in-process pod; `tests/events.rs` (SSE); `EngineBridgeTest`
    over JNA.
- **Shared UI (D33):** the default view is the layout plus a hamburger button, badged when offline, when actions
  are pending, or when a pinned version has a newer one.
  - Material3 drawer: Apps (with each app's version history), Connection, Account, Server.
  - Version picker: Latest, then versions paged by 20. A selection is client-local and not persisted; the pin
    survives rotation in the controller and gets "Newer version available" when the pod publishes a newer
    version.
  - The AI FAB/panel and the layout browser are removed.
  - Simulator: `open-menu`, `close-menu`, `open-app`, `open-history`, `select-version`, `set-server`,
    `publish-version` (fixture `FakeAppCatalog`); the browser/AI commands are removed.
- **Platforms (D33):** default app `pixelfed_home` everywhere. Android, desktop and both simulator modes open other
  apps through an engine factory.
  - Android: the debug server chip is replaced by the drawer's Server section in all builds (SharedPreferences
    `server`, in-place switch, per-server storage). Cleartext to the loopback hosts is now allowed in release too.
  - Desktop: Staging/Local picker in `java.util.prefs`; `-Dconstruct.baseUrl` / `CONSTRUCT_BASE_URL` still win at
    launch; switches happen in place; the engine is closed on window close.
  - `android_smoke.sh` checks the Menu button.
- **Tests:** cargo 270, :shared 59, :desktopApp 58 (all passing). README simulator examples re-run in fixture mode
  (phone and desktop) and live against a local pod + fake Pixelfed (sign-in, history, select-version, open-app).
- Review/QA: two review rounds and two QA rounds (simulator phone + desktop; emulator against a local pod + fake
  Pixelfed, including a new layout file appearing live as "Newer version available"; the in-place server switch;
  staging sign-in + like).
  - Fixed: stable file hash (migration v8), unknown intents rejected in files, missing files tolerated, the
    closed-controller race, custom URL https policy, connectivity badge, pinned row, offline drawer (stale apps
    error only while online), the WAL busy race at startup (`PRAGMA journal_mode` retried on SQLITE_BUSY), failed
    server switch feedback, the launch `baseUrl` extra no longer winning after a switch, and a clear "This server
    doesn't look like a Construct pod." for a non-pod custom URL.
  - Deferred (LOW): Android creates the new engine on the main thread during a server switch; the simulator's server
    list uses the desktop URL policy; staging's duplicate `pixelfed_home` v2 "Classic feed" is by design (D32).
  - Tests: cargo 281, :shared 63, :desktopApp 63 (all passing).
- Status: DONE.

## Fake Pixelfed comments
- `fixtures/pixelfed/comments.json`: threaded comments (34, nested replies included) matching each post's
  `replies_count`; embedded in the binary like the other fixtures.
- Mastodon endpoints on the fake: `GET /api/v1/statuses/:id/context`, `POST /api/v1/statuses` (replies only,
  `Idempotency-Key` honoured), `GET` / `DELETE /api/v1/statuses/:id` (delete: own only). Counts stay consistent;
  reset restores the threads. `_admin/state` gains `counts.comments` and `created`. README "Fake Pixelfed upstream".
- Not yet wired into the pod or clients: the comment button still navigates to `pixelfed://posts/:id/comments`.
- Tests: `crates/fake-pixelfed/tests/comments.rs` (13).

## Comments: pod endpoints; client approach backed out (D34)
- Pod: `GET /v1/pixelfed/posts/{id}/comments` (thread order, depth, `is_own`) and `POST /v1/pixelfed/posts/comment`
  (`client_id` -> upstream `Idempotency-Key`), with integration tests against the fake.
- A client-side comments sheet was built and then backed out: it put Pixelfed logic into the engine and the app.
  Next: the generic screens / nodes / form / create-action work listed in D34, then comments as layout files.

## Generic screens; comments from the pod (D35)
- Protocol: `Workspace.screens`, `core://List`, `core://Image`, `core://TextField`, `action.data.load`,
  `action.mutation.create`, `DataBundle`, schema versions (`X-Construct-Schema`).
- Pod: schema-aware serving; comments screen data path; `layouts/pixelfed_home/0002.json` (comments screen).
- Engine: cached data loads (`loads_json`, store v3), create rows in the outbox, schema header.
- Clients: route matching and back stack, generic nodes, `$route` / `$form` / `$nonce` binding; simulator fake data
  paths and creates; fixtures generated from the pod.
- Tests: cargo (pod, engine end to end against the fake), :shared + :desktopApp (137, incl. simulator screens).
- Needs a device check on the Mac: Android Back and keyboard on the comments screen, the desktop window.

## Layout schema compatibility (D36)
- Layout files declare `"schema"` (added to the four existing files: 1, 1, 1 and 2 for `pixelfed_home/0002`);
  the pod records it (store v9), serves the newest compatible version and reports `schema` / `compatible` /
  `incompatible` per version. `protocol::RETIRED` retires old layout schemas after a breaking change (empty today).
- Clients grey out versions they can't draw and won't pin them.
- Needs a device check on the Mac: the greyed-out rows on Android and the desktop window.

## "Latest" is the newest drawable version (D38, issue #5)
- The SSE stream announces the newest version the client can draw (engine sends `X-Construct-Schema` there too), so
  versions for newer clients cause no refetch. The apps API adds `latest_compatible_version`.
- The menu never calls an undrawable version the latest; it says "Update the app to see vN" instead.
- Needs a device check on the Mac: the hint on Android and the desktop window.

## Pod core and app adapters (D40)
- The pod is split: `pod-core` (generic; names no app), `app-pixelfed` (the Pixelfed adapter + `pixelfed_demo` as a
  `StaticAdapter`), and `server` (the binary that composes them). The `AppAdapter` trait is documented for
  implementors in `crates/pod-core/src/adapter.rs`.
- Sessions are pod-wide and collect sign-ins; apps declare the sign-in they need (`pixelfed_home` needs
  `pixelfed`). Migration v11 adds `adapter_migrations` and `session_sign_ins` and keeps existing sessions signed in.
- No wire, layout or schema change: no client build or device check needed.
- **Not yet:** a second adapter; the media proxy is still Pixelfed-only.

## Recipes app (D41, issue #13)
- `crates/app-recipes`: a second adapter with typed `recipes_*` tables in the pod's SQLite. Its migration imports
  the family collection (81 recipes, 8 categories) from `data/import-0001.json`, generated from the recipe PDF by
  `scripts/recipes_from_pdf.py`.
- `layouts/recipes/0001.json`: recipes by category, each opening a screen with ingredients (grouped), numbered
  steps (grouped), notes and the source.
- Existing vocabulary only: no client build. Checked with the simulator against a local pod.
- **Not yet:** editing, search, serving scaling, sign-in to read; tappable list rows instead of the View button
  (issue #15, schema 4).

## List search (D43, issue #17)
- Schema 5: `core://List` `filter` (client-side, as you type, case/accent-insensitive, every word must match) and
  `group_by` (the list draws group headers, so they stay right while filtering). A `core://TextField` without
  `on_submit` gets a Clear button.
- Recipes: each recipe entity has `search_text` (title, category, ingredient names); `layouts/recipes/0003.json`
  adds a search box. Simulator against a local pod: "rhubarb" leaves only Rhubarb Crunch; Clear restores all 81.
- Needs a client build: Android device check on the developer's Mac.
- **Not yet:** server-side search for very large lists (needs a debounce primitive).

## Prettier recipes (D44)
- Schema 6: `core://Box` (coloured, rounded surface), `core://ChoiceChips` (chips editing a form value), List
  `where` (exact match), `group_count` in group headers, Column/Row `align`, the `Display` text style, `flame` and
  `timer` icons.
- Recipes: colour letter tiles, a meta line and category chips in the list; a tinted serif title, a summary strip,
  a quantity column with fraction characters and oven/timer chips on the recipe (`layouts/recipes/0004.json`).
- Needs a client build: Android device check on the developer's Mac.
- **Not yet:** photos, ingredient tick boxes, a grid list.

## Replies to comments (D47)

- Replies to comments now show on real Pixelfed: its `context` lists only direct replies, so the pod also asks each
  comment with replies for its own (capped, concurrent; a failure only loses that branch).
- The fake Pixelfed answers `context` like Pixelfed (one level each way); `FAKE_PIXELFED_THREADS=mastodon` keeps the
  whole-thread answer. Tests cover both, the call count, a reply to a reply and a failed branch.

## Sign in to any Pixelfed instance (D45)

- The signed-out Pixelfed screen has a server box when `PIXELFED_ALLOWED_INSTANCES` is set; staging sets `*`, so
  any public HTTPS Pixelfed works there. Each session uses the instance it signed in to
  (`pixelfed_session_instance`, the adapter's first own migration).
- Typed-in instances are contacted only on public IP addresses (guarded resolver, no IP literals, redirects only to
  public HTTPS names); their media may come from any public HTTPS host.
- No client build needed (the box is a `core://TextField` + `$form.instance`).
- Needs a check: sign-in against a public instance from the Android app or the macOS window.

## One server, many pods (D46)

- `crates/pod-host`: pods with their own SQLite files, pod tokens (shown once, stored hashed), temporary pods with a
  sweeper, the "Open your pod" screen, public media paths per pod. `server pods create|adopt|rotate|delete|list`.
- Pod tokens are pinned sessions; a valid session without an app's sign-in gets 200/403 instead of 401, so clients
  keep their token. Sign-out keeps the pod; "Leave this pod" forgets it on one device.
- Schema 7: `action.auth.session`. The engine sends its session on sign-in and catalog calls.
- Staging: `CONSTRUCT_TEMP_PODS=1`; existing data moves into an admin pod with `server pods adopt` (README "Pods").
- Needs a client build and a device check (Android: open a pod with a code, Pixelfed sign-in, sign out, leave).

## Pixelfed cards view (D48)

- Schema 8: `core://List` `columns`, `spacing_dp`, `padding_h_dp` (a grid); `core://Conditional` on `$form.<field>`
  and with `equals`.
- `layouts/pixelfed_home/0004.json`: a List/Cards switch over the feed; two-column cards (cover, author, like) open a
  new post screen (`pixelfed://posts/{id}`, `GET /v1/pixelfed/screens/post?id=`). Posts carry `cover_url`,
  `cover_blurhash` and `media_count`. Schema-7 apps keep `0003`.
- Needs a client build and a device check (Android: switch to Cards, tap a card, like from a card and the post).

### Deferred / known issues (M6/M7)
- No idempotency keys on outbox POSTs: a request applied upstream but timed out client-side is re-sent (likes and
  bookmarks are idempotent upstream, comments are not).
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
- Not yet exercised: sign-in against a public Pixelfed instance (staging now accepts one, D45).

## Fully generic protocol, engine and clients (D37)

- The `pixelfed://` widgets (FeedList, PostCard, MediaCarousel, AuthorHeader, ActionBar) are gone from the
  protocol, the engine and the shared UI. Schema 3 adds generic building blocks (Row, Column, Spacer, Divider,
  Conditional, Carousel, IconButton, Chip, Badge); the post card is `layouts/pixelfed_home/0003.json` (and
  `pixelfed_demo/0002.json`). Old versions are still served to old clients as opaque nodes.
- The pod sends the strings the widgets used to compute (author name and handle, avatar URL, "boosted by", the
  content-warning label). Sign-in paths come from the layout; sign-out purges generically (store v4).
- The parked agent module is deleted.
- Needs a device check (Android, macOS window): the feed should look as before.

## Counter app removed (D39)

- The `home` "Counter" app is gone from the pod (layouts, `/v1/counters`, the `counters` table via migration v10), the
  apps list, the simulator catalog and the docs. Pixelfed and the `pixelfed_demo` dev app cover what it showed.
- `fixtures/counter_home_bundle.json` stays as test-only data; the outbox tests run against an in-test counter server.
- CI's fixture-mode simulator run and the staging smoke test use `pixelfed_demo` now.

## Next steps

1. Simulator live mode: make `set-server` recreate the engine on the new server (today it is recorded and shown in
   the menu, but the engine stays).
2. HTTPS enforcement in the engine for release builds (now more relevant: the server picker with custom URLs ships
   in release); a cap on SSE subscribers.
3. Feed as a `LazyColumn` with pagination (`max_id` / `Link: next`).
4. Outbox idempotency keys (client-generated, honoured by the pod).
5. Real-Pixelfed contract test in scheduled CI on a throwaway instance (`scripts/e2e.sh --upstream real`).
6. Web target for the shared UI. Small follow-ups: a 404 "workspace not found" UI state, golden-image diffing for
   the simulator, Android host tests for :shared, Android `testTagsAsResourceId` for instrumented menu tests, a
   zstd dictionary for layout blobs; drop `CONSTRUCT_LLM_PROVIDER` from `deploy/fly/pod.toml` (a harmless no-op
   now).

Parked: a layout agent (deleted in D37; it can come back from git history), e.g. as developer tooling that writes
new layout files; diffs between versions.
