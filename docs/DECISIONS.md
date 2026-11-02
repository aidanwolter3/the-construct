# Design Decisions

Short records of places where the implementation resolves ambiguity or deviates from DESIGN.md.

## D1: `MediaCarousel.media_refs` accepts a binding expression or a literal list

DESIGN.md §2.2A types `media_refs` as `Vec<String>`, but the hydration bundle in §2.2C sends
`"media_refs": "$item.media_uris"`, a template binding string. Item templates need bindings,
and hand-written layouts want literal lists, so both forms are accepted.

Decision: `media_refs` has type `protocol::ListBinding`, an untagged enum:

- `ListBinding::Expr(String)` for a JSON string such as `"$item.media_uris"`
- `ListBinding::List(Vec<String>)` for a JSON array of entity URIs

It is untagged, so each form serializes back exactly as it arrived. The renderer resolves
`Expr` against the current item or entity at render time.

## D2: `WorkspaceSpec` is the hydration bundle

DESIGN.md names `WorkspaceSpec` without defining it. In this codebase `protocol::WorkspaceSpec`
is the §2.2C bundle: `{ version, workspace: UiNode, entities: { uri -> EntityRecord } }`.
`EntityRecord` is a schemaless JSON object (`serde(transparent)` over a JSON map), so any
entity fields round-trip unchanged. Typed domain views can come later.

## D4: Counter demo uses `OptimisticMutate`, plus a minimal expression subset

The M2/M3 counter layout (`fixtures/counter_home_bundle.json`, served at `GET /v1/workspaces/home`)
reuses `ActionIntent::OptimisticMutate` on the entity `core://counters/home`, field `count`. It adds
no new intent variant. Increment uses `value_expr: "entity.count + 1"` and Reset uses `"0"`. The
endpoints `/v1/counters/home/{increment,reset}` are informational for now: there is no outbox
until M5.

Until a real CEL evaluator exists, a small expression subset stands in:

- **Mutation `value_expr`**, evaluated in client-core against the target entity's current
  payload: a JSON literal (`0`, `true`, `"x"`); `entity.<field>`; `!entity.<field>` (boolean
  negation); `entity.<field> + N` and `entity.<field> - N` (integer arithmetic). Since M6 also the
  ternary `<cond> ? <expr> : <expr>`, where `<cond>` is `entity.<field>` or `!entity.<field>` and
  each branch is one of the forms above (no nesting), e.g.
  `entity.is_liked ? entity.like_count - 1 : entity.like_count + 1`. The cond field must be a JSON
  bool: a missing or non-bool field is an error (as for `!entity.<field>`), not "false". Anything
  else logs a warning and changes nothing. The desktop simulator's `FakeEngineBridge.evalMutation`
  mirrors this subset.
- **Text `value_expr` interpolation**, done by the renderer: each `${<entity_uri>#<field>}` is
  replaced by that entity field's value, or by an empty string when it is missing. Inside an
  item template, an expression that is exactly `entity.<field>` or `$item.<field>` resolves
  against the current item. Any other text renders as-is.

## D3: Absent optional fields are omitted

`AuthorHeader.on_click` is `Option<ActionIntent>`. It is left out of the JSON when `None` and
defaults to `None` when missing, which matches the §2.2C fixture.

## D5: UniFFI Kotlin bindings live in `:shared`'s `jvmCommonMain` source set

UniFFI's Kotlin bindings load the library through JNA, so they run on the JVM and on Android but not in
`commonMain`. Until M4 they lived in `:desktopApp`. Since M4 (Phase B) they are generated into
`kmp-app/shared/src/jvmCommonMain/kotlin/uniffi/client_ffi/` (committed). `jvmCommonMain` is an intermediate
source set between `commonMain` and the `desktop` (JVM) and `android` targets, declared with
`applyDefaultHierarchyTemplate { common { group("jvmCommon") { ... } } }`.

- `org.genui.canvas.engine.UniffiEngineBridge` (also in `jvmCommonMain`) implements `CanvasEngineBridge` over
  `EngineHandle`. `UniffiEngineBridge.create(storageDir, baseUrl, workspace)` builds the engine. The bridge
  holds the handle and observers, so callers only need to keep the bridge reachable.
- Logging goes through `internal expect fun engineLog`: stdout `[desktop] ...` on desktop (Rust already
  prints `[CanvasEngine] ...` there) and `android.util.Log` tag `CanvasEngine` on Android.
- The storage directory, base URL and system properties stay in the apps (`desktopApp` `Main.kt`).
- JNA is declared per platform: the plain `net.java.dev.jna:jna` jar in `desktopMain`, and
  `net.java.dev.jna:jna:<ver>@aar` (which bundles `libjnidispatch.so`) in `androidMain`. Both use `api`,
  because the bindings expose JNA types. Neither is declared in `jvmCommonMain`, so Android never gets the jar
  and desktop never gets the aar.
- `:shared:buildRustFfi` (`scripts/build_ffi.sh`) regenerates the bindings and the macOS dylib.
  `compileKotlinDesktop` depends on it. Android compiles only `mustRunAfter` it: they use the committed
  bindings and don't need cargo. `-PskipRustBuild` skips it.

## D6: Engine snapshot shape

`FfiStateSnapshot.workspace_json` holds only the layout node (the bundle's `workspace`), or `"null"` before the
first load. `entities_json` holds the URI-keyed entity map. The offline cache stores the raw bundle per workspace
at `<storage>/workspace_<id>.json`.

## D7: FeedList item selection (temporary)

Until query resolution exists, `FeedList` ignores `query_key`. It renders `item_template` once for each entity
whose URI contains `/posts/`, ordered by `created_at` (newest first), and binds the item's URI as `uri`.

Resolved in M6 by `WorkspaceSpec.queries` (`query_key` -> ordered entity URIs; omitted from the JSON when empty, so
older bundles are unchanged): a `FeedList` renders `queries[query_key]` in order and falls back to the behavior
above when the key is absent. client-core persists the queries (`active_workspace.queries_json`, D13) and exposes
them as the snapshot's `queries_json` (`EngineSnapshot.queriesJson` in Kotlin). The Kotlin renderer (`feedItems`, read through
`LocalCanvasQueries`) skips listed URIs that have no entity and drops duplicates.

## D8: Simulation mode harness

`:desktopApp:simulate` (README, "Simulation mode") renders `CanvasApp` headlessly for screenshot-driven
feedback.

- **Harness:** the Compose UI test harness, `runSkikoComposeUiTest(size, density)` from
  `org.jetbrains.compose.ui:ui-test` (matching the Compose Multiplatform version). It is called from a plain
  `main`, with no JUnit. It renders to a Skia raster surface, so `java.awt.headless=true` works. Clicks go
  through `performMouseInput` (move, click, move off the node). `performClick()` injects touch events that
  leave a Material button with a hover tint in later screenshots. Screenshots come from
  `onRoot().captureToImage()` and are encoded to PNG with Skia. `ImageComposeScene` was not needed.
- **Source set:** the simulator and fake engine live in a separate `sim` source set of `:desktopApp`. It sees
  `main`, and tests see `sim`. The test harness stays off the shipping app's runtime classpath.
- **Fake engine:** `FakeEngineBridge` implements `CanvasEngineBridge` in memory. It emits D6-shaped snapshots
  from a bundle and applies `OptimisticMutate` with the D4 subset, mirroring `client-core/src/expr.rs`. That
  lets fixture runs exercise real interactions without Rust or a server. It also models the M5 outbox (D13):
  every mutation is applied locally and queued. While online the queue flushes at once to an in-memory fake
  server, so the pending count stays 0. While offline the rows stay queued. Going online flushes them FIFO. The
  fake server rejects a `.../decrement` whose result would be below 0, like the real server's 422. The row is then
  dropped, the field is restored to its previous value (or `rollback_value` when that is null), and later
  queued rows for the same field are re-applied. There is no 5xx/backoff simulation. Snapshots carry an
  increasing `seq`. Live mode uses the real JNA engine with temp storage, or a persistent `-Pstorage` dir.
- **Screen sizes and density:** presets are in dp: phone 390x844 at density 3, tablet 820x1180 at 2, desktop
  1280x800 at 2. A custom `WxH` uses density 2. `-Pdensity` overrides any of these. The PNG is dp x density
  pixels, so text is legible at phone-like scale.

## D9: Bindgen feature and Android logging

- **Bindgen feature:** uniffi's `cli` feature (clap, askama, and other CLI deps) is behind the opt-in
  `client-ffi` feature `bindgen`, and the `uniffi-bindgen` binary has `required-features = ["bindgen"]`. The
  cdylib builds for desktop and Android don't compile the CLI. To generate bindings:
  `cargo run -p client-ffi --release --features bindgen --bin uniffi-bindgen -- generate --library <lib> --language kotlin --out-dir <dir> --no-format`.
- **Android build:** `scripts/build_android.sh` runs `cargo ndk` and writes
  `kmp-app/androidApp/src/main/jniLibs/<abi>/libclient_ffi.so`. The ABIs default to `arm64-v8a x86_64`
  (`CANVAS_ANDROID_ABIS` overrides them), and the NDK defaults to the newest complete one (`ANDROID_NDK_HOME`
  overrides it). The `.so` needs only `liblog`, `libdl` and `libc`, not `libc++_shared.so`. reqwest's rustls
  (ring) cross-compiles cleanly.
- **Android logging:** client-core logs with `println!`/`eprintln!`, which Android discards. client-core is
  unchanged. Instead, `client-ffi/src/android_log.rs` (`cfg(target_os = "android")`, run once from the
  `EngineHandle` constructors) uses `pipe2` and `dup2` to point fds 1 and 2 at pipes. A thread per pipe
  forwards each line to `__android_log_write` with tag `CanvasEngine`: stdout at INFO and stderr at WARN. View
  them with `adb logcat -s CanvasEngine:V`. Desktop builds don't compile this module. The redirect applies
  to the whole process, so the tag also carries any other native stdout/stderr from it, such as GL/EGL driver
  lines, and not only engine output.

## D10: Android target uses AGP 9.1.1's `com.android.kotlin.multiplatform.library` plugin

`:shared` gets its Android target from the KMP-native Android library plugin
(`com.android.kotlin.multiplatform.library`, AGP 9.1.1), configured with
`kotlin { android { namespace = "org.genui.canvas.shared"; compileSdk = 36; minSdk = 26 } }`. It does not
use the classic `androidTarget()` with `com.android.library`.

- **Why this plugin:** it is Google's supported path for KMP libraries on AGP 9. Combining
  `com.android.library` with the Kotlin Multiplatform plugin is the legacy setup that AGP 9 phases out. That
  combination was not tried here. The plugin builds a single-variant library with no build types or
  flavors, which is all `:shared` needs. Host and device tests are off unless enabled.
- **Why AGP 9.1.1:** it works with Gradle 9.3.1, Kotlin 2.3.20 and Compose Multiplatform 1.11.0-beta01, and it
  supports compileSdk 36. With `--warning-mode all` it shows no Gradle or AGP deprecation warnings. Verified with
  `:shared:assemble` (which produces `shared.aar`) and the desktop tests. Newer 9.x releases exist but were
  not needed.
- **DSL:** the target block is `android { }` inside `kotlin { }`. AGP 9.1.1 reports `androidLibrary { }` as
  deprecated with "use 'android' instead". The target's source sets are `androidMain`/`androidHostTest`/`androidDeviceTest`.
- **Hierarchy:** `withAndroidTarget()` in the hierarchy template only matches the legacy `androidTarget()`.
  The `jvmCommon` group (D5) matches this plugin's target with
  `withCompilations { it.target.platformType == KotlinPlatformType.androidJvm }`.
- The plugin id and version are in `gradle/libs.versions.toml` (`agp`,
  `android-kotlin-multiplatform-library`) and applied with `apply false` in the root build. The SDK path comes
  from the gitignored `kmp-app/local.properties` (`sdk.dir=...`) or `ANDROID_HOME`.

## D11: `:androidApp` shell (base URL, cleartext, storage, engine lifetime)

`kmp-app/androidApp` is a plain `com.android.application` module (AGP 9.1.1) with the Kotlin Compose
compiler plugin. AGP 9 compiles Kotlin itself (built-in Kotlin), so `org.jetbrains.kotlin.android` is not
applied. namespace/applicationId `org.genui.canvas.android`, compileSdk/targetSdk 36, minSdk 26, Java/Kotlin
target 21 (same toolchain as `:shared`). Dependencies: `:shared`, `androidx.activity:activity-compose` 1.12.4,
and the same `org.jetbrains.compose.material3` as `:shared`. The JNA aar (`libjnidispatch.so`) arrives through
`:shared`'s `api` dependency; there is no duplicate-class conflict with the desktop jar (D5).

- **Base URL / workspace:** an intent extra (`baseUrl`, `workspace`) wins over `BuildConfig.CANVAS_BASE_URL` /
  `BuildConfig.CANVAS_WORKSPACE`, whose defaults are `http://10.0.2.2:8080` (the emulator's alias for the
  host loopback) and `home`, overridable at build time with `-PcanvasBaseUrl=...` / `-PcanvasWorkspace=...`.
  Extras are read only from the intent that creates the engine (see lifetime), and only in debug builds
  (`BuildConfig.DEBUG`): the launcher activity is exported, so release builds ignore them rather than let
  another app redirect the engine to an arbitrary server.
- **Cleartext:** only debug builds allow HTTP, and only to `10.0.2.2`, `localhost` and `127.0.0.1`
  (`src/debug/AndroidManifest.xml` sets `android:networkSecurityConfig`, `src/debug/res/xml/network_security_config.xml`).
  `localhost`/`127.0.0.1` cover a physical phone with `adb reverse tcp:8080 tcp:8080`. Release builds keep the
  platform default (cleartext blocked), so they need an HTTPS server or a config change.
  **Caveat, found in QA:** the network security config applies only to the Java networking stack. The engine
  makes its requests from Rust (`reqwest`), so the config does not restrict them: the engine will use cleartext
  to any base URL, in debug and release. The effective control is the base URL (BuildConfig, plus extras that only
  debug builds read). A follow-up is to make the engine reject `http://` outside debug builds.
- **Storage:** `Context.filesDir` (app-private, `/data/data/org.genui.canvas.android/files`); the engine's
  workspace cache lands there as `workspace_<id>.json` (D6).
- **Engine lifetime:** `CanvasViewModel` (an androidx `ViewModel`) creates the `UniffiEngineBridge` and
  `CanvasController`. It survives configuration changes, so rotation reuses the engine instead of creating a
  second one. `onCleared()` (activity finished for good) calls `EngineHandle.close()`; dropping the Rust engine
  also stops its background fetch thread (it holds only a `Weak`). The ViewModel keeps the bridge and its JNA
  observers strongly reachable while the UI exists. A process kill simply ends everything.
- **Insets:** `enableEdgeToEdge` with dark (light-style) bar icons, since the shared UI is the Material3 light
  scheme; the root `Box` uses `Modifier.safeDrawingPadding()` so the status line is not under the status bar.
  Theme: `@android:style/Theme.Material.Light.NoActionBar` (no AppCompat).
- **Rust `.so`:** `:androidApp:buildRustAndroid` (Exec, `scripts/build_android.sh`, `~/.cargo/bin` prepended to
  PATH) runs before `preBuild`; inputs are the crates' sources/`Cargo.toml`/`build.rs`, the workspace
  `Cargo.toml`/`Cargo.lock` and the script; outputs are `src/main/jniLibs/<abi>/libclient_ffi.so` (gitignored).
  `-PskipRustBuild` skips it (read at configuration time; configuration-cache safe). `-PandroidAbis=a,b`
  (default `arm64-v8a,x86_64`) is passed to the script as `CANVAS_ANDROID_ABIS` and also sets
  `ndk.abiFilters`, so the APK contains only ABIs that have `libclient_ffi.so` (the JNA aar ships more).
- **Minify:** off for debug and release. `androidApp/proguard-rules.pro` already keeps `com.sun.jna.**` and
  `uniffi.**` (JNA uses reflection and native lookups) for when release minify is turned on.

## D12: Server persistence (SQLite counters, fault-injection hook)

- **Store:** one SQLite file, `$CANVAS_DATA_DIR/server.db` (default `~/.canvas-server`), via `rusqlite`
  (`bundled`, workspace dependency shared with client-core), WAL mode. A single `Connection` behind a `Mutex`
  in `AppState` (an `Arc` passed to `server::app(state)`); handlers run store calls in `spawn_blocking`. Tests
  use `AppState::in_memory()` / `server::app_in_memory()` or `AppState::open_dir(tempdir)`.
- **Migrations:** `PRAGMA user_version`; `store::MIGRATIONS[i]` upgrades v`i` to v`i+1` in one transaction.
  v1 = `counters(id PK, count)`. Later milestones append steps (layout versions, Pixelfed tokens).
- **Counters:** the fixture is still the layout source; `GET /v1/workspaces/home` overlays the stored count on
  `core://counters/home`. Decrement is a conditional `UPDATE ... WHERE count > 0`, so the floor check and write
  are atomic; at 0 it answers 422 `count_below_zero` with the unchanged count (the client rolls back on 4xx).
- **Fault hook:** `CANVAS_TEST_HOOKS=1` registers `POST /v1/test/fault {status,count}`; otherwise the route
  does not exist (404). A route layer answers the next `count` POSTs (except `/v1/test/*`) with `status`
  before they reach a handler, so nothing is applied. In-memory only; it does not survive a restart.

## D13: Client SQLite cache and outbox (schema extensions, write-through)

- **File:** `<storage_path>/cache.db` (desktop `~/.canvas/cache.db`, Android `filesDir`); empty `storage_path`
  (simulation, tests) uses an in-memory DB, and so does a file that cannot be opened (logged). A file that opens
  but cannot be written (read-only open, or a rolled-back write probe at open fails with `SQLITE_READONLY`)
  falls back to an in-memory copy of its workspaces and entities, logged as `ERROR`; its outbox rows are not
  copied (they could be flushed but never deleted from the file, so they would be re-sent later). A probe that
  only hits another connection's lock keeps the file. `busy_timeout` is 250 ms (`store::BUSY_TIMEOUT`), not
  SQLite's usual seconds: `dispatch` runs on the UI thread on both platforms, so a locked DB costs a click at most
  ~250 ms, after which the mutate fails atomically. `rusqlite`
  `bundled`, WAL with `synchronous=NORMAL` (a crash never corrupts the DB; a power loss can drop the last
  commits). Schema behind `PRAGMA user_version` (v2; `store::MIGRATIONS[i]` upgrades v`i` to v`i+1`, one
  transaction each, so M5 v1 files upgrade in place). The bundled SQLite adds no Android `.so` dependencies.
- **Schema extensions over DESIGN §2.3:** `entities.domain`/`entity_type` are derived from the URI
  `<domain>://<type>/...`. `action_outbox` adds `field`, `value_expr`, `prev_value_json`,
  `rollback_value_json`, `next_attempt_at` (unix ms, default 0) and `last_error`, so the sync worker can roll a
  rejected row back and re-apply later rows without the original intent. `active_workspace` adds
  `entity_uris_json` (URIs the bundle carried); startup loads those entities plus any entity with a PENDING row.
  v2 (M6) adds `active_workspace.queries_json` (the bundle's `queries`, default `'{}'`; startup also loads every
  URI they name) and `action_outbox.also_json` (`[{"field","value_expr","prev_value"}]` for a mutation's `also`
  updates, NULL for single-field rows; D15). The read-only fallback copies `queries_json` only from a v2 file.
- **In-memory state + write-through:** `State` stays the snapshot source; every change is written to SQLite
  inside the state lock. An optimistic mutate commits entity update + outbox insert in one transaction before
  touching memory; if it fails (logged as `ERROR`), nothing changes and no snapshot is sent. Dispatch->notify is
  logged in microseconds; target < 5 ms. Measured on a file DB: tens of us in tests and a median of ~0.5 ms in
  the desktop app on macOS; on the Android emulator typically 1-6 ms, with a cold-start outlier of ~18 ms on the
  first click.
- **Timestamps:** `created_at`/`updated_at` are unix **seconds** (DESIGN §2.3); `action_outbox.next_attempt_at`
  is unix **milliseconds** (D14). Intentional, not a bug. Fetched bundles are persisted in one
  transaction; a persist failure is logged and the fetched state is still shown.
- **JSON cache ignored:** `workspace_<id>.json` / `last_workspace.json` from M3/M4 are neither read nor deleted.
- **`"$value"` token:** any JSON string exactly `"$value"` inside an intent's `payload` is replaced by the new
  optimistic field value before the row is stored, so toggles can send the desired state.
- **Snapshot:** `StateSnapshot`/`FfiStateSnapshot` carry `seq` (strictly increasing per engine) and
  `pending_actions` (PENDING outbox rows), and since M6 `queries_json` (`"{}"` when none) and `auth_json`
  (`{"state":"signed_out"|"pending"|"signed_in"|"error","username"?,"error"?}`; always signed_out until the auth
  unit). Kotlin receives them as `EngineSnapshot(seq: Long, pendingActions: Int, queriesJson, authJson)`. `CanvasController` drops a snapshot whose seq is <= the last applied one. The check and the write are one
  atomic `MutableStateFlow.update`, because observers fire from several engine threads. The status line shows
  "N pending" when N > 0.

## D14: Outbox sync worker and refetch merge

- **Threads, not Tokio:** DESIGN suggests a Tokio worker; client-core keeps its blocking-reqwest + thread
  pattern. A `canvas-engine-sync` thread waits on a `Condvar` (woken after every dispatch and whenever the engine
  goes online: successful fetch or probe) and holds only a `Weak` to the engine between passes; dropping the
  engine signals it to exit. HTTP never runs with the state mutex held. A separate `net` mutex serializes a
  workspace fetch+merge with an outbox POST+apply, so a bundle never races a flush (which could count a row
  twice or drop it). That lock is shared by every engine in the process on the same `cache.db`, and dropping
  a `CanvasEngine` stops its worker before the next row, so a closing engine that is mid-POST and its
  successor (Android activity relaunch) never send the same row twice. The shutdown flag set in
  `CanvasEngine::drop` is checked by the worker right before each POST and by the fetch thread after its HTTP
  call (a fetch that completes after the drop is discarded and wakes no one), so no new POST starts once the
  handle is dropped; one already in flight may complete. A `next_attempt_at` further out than
  the backoff cap (wall clock moved backwards) is treated as due.
- **Pass:** while online, take the lowest-id PENDING row; if its `next_attempt_at` (unix **ms**, so test
  backoffs can be sub-second) is in the future, sleep until then (FIFO never skips ahead). POST
  `base_url + endpoint` with `payload_json`. 2xx: delete. 4xx: delete + roll back (below), except 408 and 429,
  which are transient and treated like 5xx. 5xx, 408, 429 or anything else non-2xx/4xx, and network errors: `retry_count += 1`, `next_attempt_at = now + min(base * 2^retry_count, cap)`
  (defaults 1 s and 60 s, so 2, 4, ... 60 s), `last_error`, stop the pass. A network error also marks the engine
  offline; the 2 s `/healthz` probe flips it back and wakes the worker. Deletes and rollbacks bump `seq` and
  notify; a retry does not (nothing visible changes) unless it took the engine offline. Each attempt is logged
  (`outbox #id POST url -> 200, deleted (N pending)`, 4xx with the response body). The fetch retry loop (server
  down at cold start) logs its first failure, then only when the error changes or every 30th attempt (about once
  a minute), and the eventual success says how many attempts failed.
- **Rollback:** rewind every later PENDING row for the same entity (newest first, each field to its stored prev),
  then restore the rejected row: `entity[field]` to `prev_value_json` (or `rollback_value_json` when prev is
  null/absent) and each `also` field to its `prev_value`; then re-apply the later rows FIFO (all fields
  re-evaluated, prevs updated). Rewinding first keeps relative `also` fields of later rows (a like count) from
  being applied twice. Delete + entity + prev updates are one transaction; memory changes only if it commits.
- **Refetch merge:** a fetched bundle replaces its entities, then every PENDING row for those entities is
  re-applied FIFO (same re-evaluation of every field, prevs rebased on the fetched value), persisted with the layout
  in one transaction. Entities outside the bundle with pending rows keep their local value. A flush does not
  trigger a refetch: the displayed value already includes the flushed row.
- **Toggle edge case:** re-applying re-evaluates `value_expr` against the new base, but the stored payload is
  not rewritten (a `"$value"` was substituted at dispatch). For relative expressions (`entity.count + 1`) the
  server applies the same relative op, so both agree. For a toggle (`!entity.liked` with payload
  `{"liked": "$value"}`) a rollback or refetch can flip the local value while the queued payload still carries
  the old desired state; local and server state can then differ until the next fetch.
- **Config:** `EngineConfig { poll_interval, backoff_base, backoff_cap }` via
  `CanvasEngine::init_with_config` (Rust only; `init` uses the defaults; FFI unchanged). Tests shorten them.

## D15: `OptimisticMutate.also` (multi-field optimistic updates)

A like must flip `is_liked` and adjust `like_count` together. `ActionIntent::OptimisticMutate` gains
`also: Vec<FieldUpdate>` (`FieldUpdate { field, value_expr }`), default empty and omitted from the JSON when empty,
so existing intents round-trip byte for byte. Rules: the primary `value_expr` and every `also` expression are
evaluated against the PRE-mutation entity, then all are applied together; a rollback restores every field; a
re-apply re-evaluates all of them. `"$value"` in the payload is still the primary field's new value. Kotlin
mirrors `also` in `Model.kt` and keeps it through item binding and the re-encode before dispatch; the desktop
simulator's `FakeEngineBridge` implements the full semantics. client-core implements them too: if any expression
fails nothing changes (no row, no snapshot); the row stores the `also` updates with their prev values in
`action_outbox.also_json` (D13); rollback and re-apply follow D14.

## D16: Fake Pixelfed upstream (`crates/fake-pixelfed`)

No Pixelfed account exists, so M6 runs against a local Mastodon-compatible fake serving `fixtures/pixelfed/`.
Fixture JSON carries a `{{BASE}}` placeholder replaced with `FAKE_PIXELFED_PUBLIC_URL` at load time, so the same
files work for pod->fake (127.0.0.1) and emulator (10.0.2.2) URLs. State (apps, codes, tokens, favourites,
bookmarks) is in memory and resets on restart. The token endpoint really checks S256 PKCE, the client secret and
redirect_uri; a code is consumed by any exchange attempt (also a failed one). Acting on a reblog acts on the
reblogged status (Mastodon behaviour); favourite/bookmark are idempotent. Media are generated PNGs (solid colour +
big digit/letter) so screenshots show which carousel page is visible; their `meta.original` sizes are the
advertised sizes (pixels are 2x smaller to keep files tiny) and `blurhash` is a valid 1x1 (DC-only) hash.

## D17: Pixelfed token ownership and the pod adapter (`server::pixelfed`)

- **Token ownership:** the pod holds the upstream access token; private data stays on the pod. The client runs
  PKCE against the instance's authorize page, then hands `code` + `code_verifier` to `POST
  /v1/auth/pixelfed/token`; the pod exchanges them with its `client_secret`, calls verify_credentials, stores the
  token in server SQLite and returns a random 256-bit **pod session token**. The client keeps only that, in a
  secure store, and sends `Authorization: Bearer <session>` on every pod request.
- **Store (migration v2):** `pixelfed_apps(instance, redirect_uri, client_id, client_secret)` (one app per
  redirect_uri, registered lazily with scopes `read write`; deleted when the token endpoint answers 401
  invalid_client so the next `/app` call re-registers), `pixelfed_account(instance PK, access_token, ...)`,
  `sessions(token_hash PK, created_at)` holding SHA-256(token) hex only, and `pixelfed_media(key, url)`.
- **Single tenant:** one account per instance; any valid session uses it; a new sign-in replaces it. Logout deletes
  only the session. Only the configured instance is accepted in auth calls (`PIXELFED_INSTANCE_URL` or
  `PIXELFED_PUBLIC_URL`, trailing slash ignored), so the pod never calls a client-chosen host. Sessions do not
  expire yet.
- **Status mapping:** `GET /v1/workspaces/pixelfed_home` without a bearer -> 200 signed-out bundle (AuthLogin
  button). With a bearer that gets no feed (unknown session, no stored account, or upstream 401, which also deletes
  the stored account) -> 401 + `WWW-Authenticate: Bearer` + `X-Canvas-Session: invalid`, body = the same signed-out
  bundle, so the client learns its session is stale (D19 fetch-401 path). Unreachable/5xx -> 502. Actions: no session 401, upstream 4xx passed through (client rolls back), 5xx/unreachable 502 (retry).
- **Normalization** is pure (`server::pixelfed`): lenient serde types (absent/null optional fields default; an
  unparseable status is skipped and logged); a reblog becomes the inner post plus `reblogged_by_uri`, with the
  outer order kept and duplicates dropped; captions are HTML -> text (`<br>` newline, paragraphs separated by a
  blank line, entities decoded, link text kept); `aspect_ratio` = `meta.original.aspect`, else width/height,
  else `meta.small`, else 1.0.
- **Media proxy:** every image URL becomes `<scheme>://<Host>/v1/pixelfed/media/<key>` (scheme from
  `X-Forwarded-Proto`, else http), key = first 128 bits of SHA-256(url) in hex, persisted so keys survive a pod
  restart. `GET` streams the upstream bytes with no auth (keys are opaque and the media are public upstream).
  The URLs come from upstream content, so the reply is inert: the upstream content type only if it is
  `image/*` (not SVG), `video/*` or `audio/*`, else `application/octet-stream`, plus `nosniff` and
  `CSP: default-src 'none'; sandbox`. URLs under `PIXELFED_PUBLIC_URL` are fetched via `PIXELFED_INSTANCE_URL`.
- **Demo:** the static `pixelfed_demo` like intent posts to `POST /v1/demo/ack` (stateless 200).
- **Real Pixelfed (M6-RA):** the timeline request adds `include_reblogs=true` (Pixelfed hides boosts otherwise; the
  pod reads one page, so Pixelfed's Link headers, which drop the flag, are not followed). Every optional field is
  type-lenient (counts as numbers or numeric strings, flags as bools/0/1/strings, `aspect` integer or float,
  malformed `meta`/`reblog`/attachments dropped rather than the whole status); `reply_count` (Pixelfed's `_pe`
  name, used for a boost's inner status) backs `replies_count`. The token response is parsed leniently (only
  `access_token` required; Pixelfed's ISO `created_at` is ignored). **Refresh:** migration v3 adds
  `refresh_token`, `expires_at`, `redirect_uri` to `pixelfed_account`; an upstream 401 on the feed or an action
  triggers one refresh with the issuing app's credentials (looked up by redirect_uri), under a tokio mutex that
  re-reads the stored token first, because Passport rotates the refresh token and a second concurrent refresh would
  fail and sign the user out. Refresh rejected / impossible -> the old 401 behaviour. Pixelfed tokens last a year,
  so this mostly covers expiry, not revocation. The fake mirrors all of this (hides boosts without the flag,
  Passport-shaped token response, rotating `refresh_token` grant, `expire_access_tokens()` test hook).

## D18: Pixelfed widgets: Coil, carousel aspect rule, simulator images

- **Coil 3.4.0** (`coil-compose` in `:shared` commonMain, `coil-network-okhttp` in jvmCommonMain, auto-registered
  via ServiceLoader on desktop and Android). 3.4.x is the newest line built against Kotlin 2.3 / Compose 1.9; 3.5+
  requires Kotlin 2.4 and Compose 1.11.1+, newer than this project's 1.11.0-beta01. OkHttp rather than Ktor: one
  engine for both JVM targets, no extra client setup. `CanvasImage` wraps `AsyncImage` over a placeholder colour
  (the BlurHash's average colour when known), crossfade on, `ContentScale.Crop`, an error state with the alt text.
- **Icons:** the handful needed (heart, bookmark, comment, boost, broken image, warning) are inlined as Material
  path data (`CanvasIcons`); Compose MP 1.11 has no current material-icons artifact and the extended set is large.
- **Carousel aspect:** the first media entity's `aspect_ratio` clamped to [0.8, 1.91] (Instagram's 4:5 .. 1.91:1),
  else the node's `aspect_ratio` (unclamped, must be positive), else 1. Every page uses that one ratio (cropped), so
  swiping never changes the card height. No media renders nothing. Page dots sit below the image (overlay dots
  vanish on light images), with an `n/N` badge on the image.
- **Simulator images:** `SimImages` installs Coil's singleton loader per run. Fixture mode: an interceptor maps
  `/storage/<file>` and `/v1/pixelfed/media/<file>` URLs to `fixtures/pixelfed/media/` and fails every other URL
  without touching the network; live mode uses OkHttp. Disk cache off. `ImageLoads.inFlight` (commonMain) counts
  on-screen images from creation until success/error/disposal, so an image whose request has not started yet also
  counts; `wait-images` and every `shot` wait for it to reach 0 and then for Coil's crossfade, which runs on the
  wall clock, not the test clock (300 ms after the last success).
- **Fixture:** `fixtures/pixelfed_home_feed_bundle.json` is `fixtures/pixelfed/timeline_home.json` normalized by
  the D17 rules (with `{{BASE}}` = `http://127.0.0.1:8081`, image URLs left un-proxied so the sim can map them)
  plus the contract's `pixelfed_home` layout and `queries`.

## D19: Client sign-in (OAuth2 PKCE in client-core, session store, 401 policy)

- **Flow** (`client_core::auth` + engine, on a `canvas-engine-auth` thread; dispatch never blocks):
  `ActionIntent::AuthLogin{instance_url}` -> `auth_json` pending -> redirect URI (loopback: bind
  `127.0.0.1:0`, `http://127.0.0.1:<port>/callback`; custom: e.g. `canvas://oauth`) -> `POST /v1/auth/pixelfed/app`
  -> 64-char verifier from the unreserved set (getrandom, rejection-sampled), S256 challenge, 32-char state ->
  authorize URL (RFC 3986 percent-encoding, `%20` not `+`) -> `UrlOpener.open` -> redirect (loopback listener, or
  `handle_auth_redirect(url)`) -> state check -> `POST /v1/auth/pixelfed/token` -> session stored -> signed_in ->
  outbox woken -> refetch. 5 min redirect timeout. A newer AuthLogin or `logout()` cancels a pending flow.
- **Redirect handling:** a redirect whose `state` does not match is ignored (loopback: 400 page; custom:
  `handle_auth_redirect` returns false) and the flow keeps waiting, so a stray or forged redirect cannot kill the
  real sign-in. Other loopback paths (favicon) get 404. `error=` with the right state ends the flow as `error`.
  **Loopback port:** the pod registers one upstream app per redirect_uri, so a random port per sign-in would
  register a new app on the instance every time. The listener first tries `127.0.0.1:47823`
  (`EngineOptions.loopback_port`, default `DEFAULT_LOOPBACK_PORT`; 0 = always random), retrying 3x100 ms (a
  cancelled flow's listener may still be closing), then falls back to a random port (logged; that sign-in registers
  another app). The FFI uses the default.
- **No browser:** no opener, `open` returning false or throwing -> `auth_json` error "no browser available" and the
  authorize URL is logged; the flow keeps waiting so a human can open it (success then moves to signed_in).
- **auth_json:** `{"state":"signed_out"}`, `{"state":"pending"}`, `{"state":"signed_in","username",
  "display_name","avatar_url"}` (from the token response), `{"state":"error","error":"<message>"}`.
- **Session store:** one key `pod_session:<base_url>` holding JSON `{token, username?, display_name?, avatar_url?}`
  (a bare non-JSON value is read as a token). Restored at construction (snapshot starts signed_in). Default store
  when the platform passes none: macOS login Keychain (`keyring` 3, apple-native only, service
  `dev.canvas.client`; a `cfg(target_os = "macos")` dependency so Android never compiles it), in-memory
  elsewhere. `init`/`init_with_workspace` use the default; `init_with_config` (tests) uses in-memory. Platform
  callbacks (store, opener) run inside `catch_unwind`, so a foreign exception surfaced as a panic is contained.
- **Bearer:** `Authorization: Bearer <session>` on workspace fetches and outbox POSTs; not on `/healthz` probes.
  An outbox POST gets it only if `base_url + endpoint` keeps the pod's origin (an endpoint like `@host/x` does not).
- **401 policy:** fetch 401 with a session -> session cleared (memory + store), signed_out, one retry without the
  bearer. Outbox POST 401 -> the row stays PENDING with its optimistic value (no rollback, retry count unchanged),
  the session is cleared, and flushing pauses (`auth_blocked`, in memory) until the next sign-in wakes the worker;
  after a restart the row is tried once more and pauses again on another 401. A 401 for a session that was
  replaced while the POST was in flight (a sign-in or logout finished) neither pauses nor clears anything; the
  row is retried with the current session. A user's like is never silently
  undone just because the session expired. The pod answers a stale bearer on `pixelfed_home` with 401 (D17), so
  a stale session is cleared on the first workspace fetch (e.g. at startup) and the signed-out bundle follows.
- **Logout:** non-blocking: cancel flow, clear session + store, notify signed_out; then on a thread `POST
  /v1/auth/logout` (best effort) and refetch.

## D20: Local real Pixelfed for end-to-end tests (`dev/pixelfed`, `scripts/pixelfed_dev.sh`)

- **Runtime:** Colima (vz) + Lima + docker CLI + compose plugin as checksummed release binaries in `~/.local`
  (no Homebrew/sudo available). VM 2 CPU / 4 GB / 15 GB.
- **Stack:** official `ghcr.io/pixelfed/pixelfed:v0.15.2` pinned by digest, MariaDB 11.4, Redis 7.4, named
  volumes, only `127.0.0.1:8090` published. Federation, open registration, mail (log driver) off; HTTP without
  https forcing. Committed `pixelfed.env` holds dev-only dummy secrets.
- **Horizon replaced** by one `php artisan queue:work redis --queue=<all Pixelfed queues>` (Horizon's auto balance
  ran ~16 workers, >1 GB RSS; the single worker uses ~75 MB). The scheduler is omitted.
- **`APP_DOMAIN=localhost`:** Pixelfed's web routes (including `/oauth/*`) are domain-bound, so the pod must use
  `http://localhost:8090`, and the emulator `adb reverse` + `localhost` rather than `10.0.2.2`.
- **Upstream workarounds:** OAuth client ids start at 1000 (Passport 13 rejects a token whose user id equals a
  first-party confidential client's id), default avatar/header installed into the storage volume (the image's
  `.dockerignore` drops `storage/app/public`), media upload retried on `200 []` (first write of a fresh user).
- **Tokens for seeding/tests** are personal access tokens minted via tinker (scopes read write follow push); the
  real PKCE flow was verified separately with curl (see `fixtures/pixelfed/recorded/` and the M6 report).
- **Pod against it:** `PIXELFED_INSTANCE_URL=http://localhost:8090` (not 127.0.0.1: domain routing), and
  `PIXELFED_PUBLIC_URL` left at its default (= instance URL), so the authorize link is `localhost` too. Verified end
  to end with the desktop simulator's scripted login (D21): sign-in, feed with the boost, real JPEGs through the
  media proxy, carousel swipe, like reaching the instance (`favourited: true`).

## D21: Platform sign-in glue (desktop, Android, simulator)

- **Bridge:** `UniffiEngineBridge.create(storageDir, baseUrl, workspace, secureStore?, urlOpener?, redirectUri?)`
  wraps `EngineHandle.newWithOptions`. `CanvasEngineBridge` gained `handleAuthRedirect(url): Boolean` and `logout()`
  with no-op defaults so fakes compile unchanged; `CanvasController.logout()` backs the status line's "Sign out"
  (a small clickable label, not a Material `TextButton`, whose 40 dp minimum height would bloat the status line).
- **Desktop:** no store (Keychain), loopback, `java.awt.Desktop.browse` if `BROWSE` is supported, else
  `ProcessBuilder("open", url)`; false if both fail.
- **Android:** `launchMode="singleTask"`, not singleTop: the browser is started with `FLAG_ACTIVITY_NEW_TASK` and runs
  in its own task, so with singleTop the `canvas://oauth` VIEW intent would create a second `MainActivity` (and a
  second engine without the pending flow) in the browser's task. `onNewIntent` (and `onCreate` on a fresh start)
  forwards `canvas://oauth` VIEW intents to the ViewModel's bridge. `KeystoreSecureStore`: AES-256/GCM/NoPadding key
  `canvas_session_key` in AndroidKeyStore (generated on first use), value stored as `base64(iv):base64(ciphertext)`
  in private SharedPreferences `canvas_secure` (`commit()`, synchronized); undecryptable values read as absent.
  `AndroidUrlOpener` posts `startActivity(ACTION_VIEW, NEW_TASK)` to the main thread and waits up to 5 s (calls it directly when already on the main thread); false on
  `ActivityNotFoundException`. Custom Tabs are not used (no extra dependency).
- **Simulator:** `InMemorySecureStore`; `AutoApproveBrowser` GETs `<authorize>&auto=1` without following redirects;
  only a 3xx whose `Location` starts with the URL's `redirect_uri` is followed (on a daemon thread, with
  `Redirect.NORMAL`, because the engine serves the loopback listener only after `open` returns). Anything else (a
  real instance's login page) prints the URL and returns false. **Real instances:** opt-in
  `-PpixelfedLogin=<login>:<password>` selects `PixelfedLoginBrowser`, which scripts Pixelfed's web login (by
  e-mail; a bare login gets `@example.test`, the seed domain) and the Passport consent form (the form without
  `_method=DELETE`) with its own cookie jar and no auto-redirects, then follows the final redirect to the loopback
  listener on a daemon thread. Credentials are a dev convenience for the local seeded instance only.
