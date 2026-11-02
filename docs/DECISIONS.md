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

- `org.theconstruct.engine.UniffiEngineBridge` (also in `jvmCommonMain`) implements `ConstructEngineBridge` over
  `EngineHandle`. `UniffiEngineBridge.create(storageDir, baseUrl, workspace)` builds the engine. The bridge
  holds the handle and observers, so callers only need to keep the bridge reachable.
- Logging goes through `internal expect fun engineLog`: stdout `[desktop] ...` on desktop (Rust already
  prints `[ConstructEngine] ...` there) and `android.util.Log` tag `ConstructEngine` on Android.
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
`LocalConstructQueries`) skips listed URIs that have no entity and drops duplicates.

## D8: Simulation mode harness

`:desktopApp:simulate` (README, "Simulation mode") renders `ConstructApp` headlessly for screenshot-driven
feedback.

- **Harness:** the Compose UI test harness, `runSkikoComposeUiTest(size, density)` from
  `org.jetbrains.compose.ui:ui-test` (matching the Compose Multiplatform version). It is called from a plain
  `main`, with no JUnit. It renders to a Skia raster surface, so `java.awt.headless=true` works. Clicks go
  through `performMouseInput` (move, click, move off the node). `performClick()` injects touch events that
  leave a Material button with a hover tint in later screenshots. Screenshots come from
  `onRoot().captureToImage()` and are encoded to PNG with Skia. `ImageComposeScene` was not needed.
- **Source set:** the simulator and fake engine live in a separate `sim` source set of `:desktopApp`. It sees
  `main`, and tests see `sim`. The test harness stays off the shipping app's runtime classpath.
- **Fake engine:** `FakeEngineBridge` implements `ConstructEngineBridge` in memory. It emits D6-shaped snapshots
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
  (`CONSTRUCT_ANDROID_ABIS` overrides them), and the NDK defaults to the newest complete one (`ANDROID_NDK_HOME`
  overrides it). The `.so` needs only `liblog`, `libdl` and `libc`, not `libc++_shared.so`. reqwest's rustls
  (ring) cross-compiles cleanly.
- **Android logging:** client-core logs with `println!`/`eprintln!`, which Android discards. client-core is
  unchanged. Instead, `client-ffi/src/android_log.rs` (`cfg(target_os = "android")`, run once from the
  `EngineHandle` constructors) uses `pipe2` and `dup2` to point fds 1 and 2 at pipes. A thread per pipe
  forwards each line to `__android_log_write` with tag `ConstructEngine`: stdout at INFO and stderr at WARN. View
  them with `adb logcat -s ConstructEngine:V`. Desktop builds don't compile this module. The redirect applies
  to the whole process, so the tag also carries any other native stdout/stderr from it, such as GL/EGL driver
  lines, and not only engine output.

## D10: Android target uses AGP 9.1.1's `com.android.kotlin.multiplatform.library` plugin

`:shared` gets its Android target from the KMP-native Android library plugin
(`com.android.kotlin.multiplatform.library`, AGP 9.1.1), configured with
`kotlin { android { namespace = "org.theconstruct.shared"; compileSdk = 36; minSdk = 26 } }`. It does not
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
applied. namespace/applicationId `org.theconstruct.android`, compileSdk/targetSdk 36, minSdk 26, Java/Kotlin
target 21 (same toolchain as `:shared`). Dependencies: `:shared`, `androidx.activity:activity-compose` 1.12.4,
and the same `org.jetbrains.compose.material3` as `:shared`. The JNA aar (`libjnidispatch.so`) arrives through
`:shared`'s `api` dependency; there is no duplicate-class conflict with the desktop jar (D5).

- **Base URL / workspace:** an intent extra (`baseUrl`, `workspace`) wins over `BuildConfig.CONSTRUCT_BASE_URL` /
  `BuildConfig.CONSTRUCT_WORKSPACE`, whose defaults are `http://10.0.2.2:8080` (the emulator's alias for the
  host loopback) and `home`, overridable at build time with `-PconstructBaseUrl=...` / `-PconstructWorkspace=...`.
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
- **Storage:** `Context.filesDir` (app-private, `/data/data/org.theconstruct.android/files`); the engine's
  workspace cache lands there as `workspace_<id>.json` (D6).
- **Engine lifetime:** `ConstructViewModel` (an androidx `ViewModel`) creates the `UniffiEngineBridge` and
  `ConstructController`. It survives configuration changes, so rotation reuses the engine instead of creating a
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
  (default `arm64-v8a,x86_64`) is passed to the script as `CONSTRUCT_ANDROID_ABIS` and also sets
  `ndk.abiFilters`, so the APK contains only ABIs that have `libclient_ffi.so` (the JNA aar ships more).
- **Minify:** off for debug and release. `androidApp/proguard-rules.pro` already keeps `com.sun.jna.**` and
  `uniffi.**` (JNA uses reflection and native lookups) for when release minify is turned on.

## D12: Server persistence (SQLite counters, fault-injection hook)

- **Store:** one SQLite file, `$CONSTRUCT_DATA_DIR/server.db` (default `~/.construct-server`), via `rusqlite`
  (`bundled`, workspace dependency shared with client-core), WAL mode. A single `Connection` behind a `Mutex`
  in `AppState` (an `Arc` passed to `server::app(state)`); handlers run store calls in `spawn_blocking`. Tests
  use `AppState::in_memory()` / `server::app_in_memory()` or `AppState::open_dir(tempdir)`.
- **Migrations:** `PRAGMA user_version`; `store::MIGRATIONS[i]` upgrades v`i` to v`i+1` in one transaction.
  v1 = `counters(id PK, count)`. Later milestones append steps (layout versions, Pixelfed tokens).
- **Counters:** the fixture is still the layout source; `GET /v1/workspaces/home` overlays the stored count on
  `core://counters/home`. Decrement is a conditional `UPDATE ... WHERE count > 0`, so the floor check and write
  are atomic; at 0 it answers 422 `count_below_zero` with the unchanged count (the client rolls back on 4xx).
- **Fault hook:** `CONSTRUCT_TEST_HOOKS=1` registers `POST /v1/test/fault {status,count}`; otherwise the route
  does not exist (404). A route layer answers the next `count` POSTs (except `/v1/test/*`) with `status`
  before they reach a handler, so nothing is applied. In-memory only; it does not survive a restart.

## D13: Client SQLite cache and outbox (schema extensions, write-through)

- **File:** `<storage_path>/cache.db` (desktop `~/.construct/cache.db`, Android `filesDir`); empty `storage_path`
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
  unit). Kotlin receives them as `EngineSnapshot(seq: Long, pendingActions: Int, queriesJson, authJson)`. `ConstructController` drops a snapshot whose seq is <= the last applied one. The check and the write are one
  atomic `MutableStateFlow.update`, because observers fire from several engine threads. The status line shows
  "N pending" when N > 0.

## D14: Outbox sync worker and refetch merge

- **Threads, not Tokio:** DESIGN suggests a Tokio worker; client-core keeps its blocking-reqwest + thread
  pattern. A `construct-engine-sync` thread waits on a `Condvar` (woken after every dispatch and whenever the engine
  goes online: successful fetch or probe) and holds only a `Weak` to the engine between passes; dropping the
  engine signals it to exit. HTTP never runs with the state mutex held. A separate `net` mutex serializes a
  workspace fetch+merge with an outbox POST+apply, so a bundle never races a flush (which could count a row
  twice or drop it). That lock is shared by every engine in the process on the same `cache.db`, and dropping
  a `ConstructEngine` stops its worker before the next row, so a closing engine that is mid-POST and its
  successor (Android activity relaunch) never send the same row twice. The shutdown flag set in
  `ConstructEngine::drop` is checked by the worker right before each POST and by the fetch thread after its HTTP
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
  `ConstructEngine::init_with_config` (Rust only; `init` uses the defaults; FFI unchanged). Tests shorten them.

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

**Amended (M6-X): the fake is the first-class e2e target**, the real Pixelfed (D20) is optional (fixture recording,
contract test). Fast, deterministic, no containers, and it can inject faults the real instance cannot.
- **Single binary:** `build.rs` embeds the two JSON fixtures and `media/` (`include_bytes!` list generated at build
  time); an empty `Config::fixtures_dir` selects them, `FAKE_PIXELFED_FIXTURES` loads a directory instead. Bind
  `FAKE_PIXELFED_ADDR`, else `0.0.0.0:$PORT`, else `127.0.0.1:8081`; SIGTERM stops it. Dockerfile: rust slim builder
  -> distroless `cc` (glibc; no musl toolchain needed), 51 MB image, 2.1 MB stripped binary, ~1-4 MB RSS idle.
- **Admin routes** (`/_admin/reset[?keep_auth=1]`, `/_admin/state`, `POST|DELETE /_admin/faults`) are served by
  default only when bound to loopback (`FAKE_PIXELFED_ADMIN=1|0` overrides): they let anyone wipe state, so not on a
  public bind by accident. The library always has the same controls (`AppState::reset`, `add_faults`, ...).
- **Reset** restores the fixture content and clears faults, request log and grant counters; without `keep_auth` it
  also drops apps/codes/tokens (an instance wipe; the seed token survives).
- **Faults:** ordered rules `{path_prefix?, method?, times?, kind}`; per request every matching `latency` adds up
  and the first matching `status`/`rate_limit` answers; each used rule spends one `times`. `expire_tokens` acts at
  once and is not stored. The 401 is shaped like an expired Passport token so the pod's refresh path runs. The
  `X-Fake-Fault` header covers direct calls (the pod forwards no custom headers). The request log keeps the newest
  500 non-admin requests; a request whose client hung up mid-way (a latency fault past its timeout) is logged as 499.
- **e2e crate** (`crates/e2e`): pod + engine in-process; the engine reaches the pod via a switchable TCP proxy
  (offline without restarting the pod, whose in-memory store must survive). Upstream from `E2E_UPSTREAM`
  (`fake` default, in-process or `E2E_FAKE_URL`; `real`). One sequential suite test, so a real instance sees one
  sign-in per run (Pixelfed throttles `/oauth/*` to 10/min).

## D17: Pixelfed token ownership and the pod adapter (`server::pixelfed`)

- **Token ownership:** the pod holds the upstream access token; private data stays on the pod. The client runs
  PKCE against the instance's authorize page, then hands `code` + `code_verifier` to `POST
  /v1/auth/pixelfed/token`; the pod exchanges them with its `client_secret`, calls verify_credentials, stores the
  token in server SQLite and returns a random 256-bit **pod session token**. The client keeps only that, in a
  secure store, and sends `Authorization: Bearer <session>` on every pod request.
- **Store (migration v2):** `pixelfed_apps(instance, redirect_uri, client_id, client_secret)` (one app per
  redirect_uri, registered lazily with scopes `read write`; deleted when the token endpoint answers 401
  invalid_client, at code exchange or at the `/app` validation below, so a new app is registered), `pixelfed_account(instance PK, access_token, ...)`,
  `sessions(token_hash PK, created_at)` holding SHA-256(token) hex only, and `pixelfed_media(key, url)`.
- **Single tenant, pinned owner (migration v4 `pixelfed_owner(instance, account_id)`):** one account per instance;
  any valid session uses it. The first successful sign-in pins its upstream account id as the owner (an account
  stored before v4 is pinned by the migration); a later sign-in by another account id gets 403
  `account_not_owner` and changes nothing (owner check, account save and session insert are one transaction), so
  another user of the instance cannot take over the pod. The owner re-signing in is fine. Overrides:
  `PIXELFED_OWNER_ACCOUNT_ID` (only that id may sign in; it replaces a pinned owner) or
  `PIXELFED_ALLOW_ACCOUNT_CHANGE=1` (any account replaces the stored one). Logout deletes only the session. Only
  the configured instance is accepted in auth calls (`PIXELFED_INSTANCE_URL` or `PIXELFED_PUBLIC_URL`, trailing
  slash ignored), so the pod never calls a client-chosen host. **Sessions expire** after `CONSTRUCT_SESSION_TTL_DAYS`
  (default 90; an expired session is treated as unknown: 401); each sign-in prunes expired rows.
- **Redirect URIs:** `/v1/auth/pixelfed/app` registers apps only for `http://127.0.0.1:<port>/callback`,
  `http://localhost:<port>/callback`, `construct://oauth` and exact `PIXELFED_EXTRA_REDIRECTS` entries (else 400
  `redirect_uri_not_allowed`), so the pod cannot be used to mint upstream apps for arbitrary redirects.
- **Stale app recovery:** a cached app is checked on every `/app` call with a `client_credentials` grant
  (`scope=read`; Pixelfed/Passport issues an app-only token for apps from `/api/v1/apps`, verified on 0.15.2; the
  token is discarded). 401 / `invalid_client` -> the row is deleted and a new app registered (an instance reset
  otherwise left the authorize page failing with invalid_client before any token call). Unreachable, 5xx, 429 or
  any other answer keeps the cached app. Cost: one extra `/oauth/token` request per sign-in, which counts toward
  Pixelfed's 10 requests/minute/IP throttle on `/oauth/*` (a 429 keeps the cached app).
- **Upstream 401 cleanup is compare-and-delete:** a 401 that survives refresh deletes the stored account only if it
  still holds the access token that failed, so a concurrent sign-in or refresh is not wiped.
- **Status mapping:** `GET /v1/workspaces/pixelfed_home` without a bearer -> 200 signed-out bundle (AuthLogin
  button). With a bearer that gets no feed (unknown session, no stored account, or upstream 401, which also deletes
  the stored account) -> 401 + `WWW-Authenticate: Bearer` + `X-Construct-Session: invalid`, body = the same signed-out
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
  **SSRF guard:** only origins in the allowlist are proxied: the host+port of `PIXELFED_INSTANCE_URL` and
  `PIXELFED_PUBLIC_URL`, plus `PIXELFED_MEDIA_HOSTS` (comma-separated `host` = default port only, `host:port`, or
  `scheme://host[:port]`; for CDNs/S3). A URL outside it is dropped to `null` at normalization (no key stored; the
  client shows its placeholder; warned once per host) and a stored key for one is refused with 403
  `media_host_not_allowed`. Media redirects are followed (max 5) only to allowlisted origins (else the 3xx ->
  502). Bodies are capped at 25 MB: a larger `Content-Length` -> 502 `media_too_large`, and a body that grows past
  the cap while streaming is aborted.
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
  Passport-shaped token response, rotating `refresh_token` grant, `client_credentials` grant issuing an app-only
  token the API does not accept, `expire_access_tokens()` and `forget_apps()` test hooks).
- **Signed-out bundle:** the workspace title is the only "Pixelfed" heading (no extra title Text).

## D18: Pixelfed widgets: Coil, carousel aspect rule, simulator images

- **Coil 3.4.0** (`coil-compose` in `:shared` commonMain, `coil-network-okhttp` in jvmCommonMain, auto-registered
  via ServiceLoader on desktop and Android). 3.4.x is the newest line built against Kotlin 2.3 / Compose 1.9; 3.5+
  requires Kotlin 2.4 and Compose 1.11.1+, newer than this project's 1.11.0-beta01. OkHttp rather than Ktor: one
  engine for both JVM targets, no extra client setup. `ConstructImage` wraps `AsyncImage` over a placeholder colour
  (the BlurHash's average colour when known), crossfade on, `ContentScale.Crop`, an error state with the alt text.
- **Icons:** the handful needed (heart, bookmark, comment, boost, broken image, warning) are inlined as Material
  path data (`ConstructIcons`); Compose MP 1.11 has no current material-icons artifact and the extended set is large.
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

- **Flow** (`client_core::auth` + engine, on a `construct-engine-auth` thread; dispatch never blocks):
  `ActionIntent::AuthLogin{instance_url}` -> `auth_json` pending -> redirect URI (loopback: bind
  `127.0.0.1:0`, `http://127.0.0.1:<port>/callback`; custom: e.g. `construct://oauth`) -> `POST /v1/auth/pixelfed/app`
  -> 64-char verifier from the unreserved set (getrandom, rejection-sampled), S256 challenge, 32-char state ->
  authorize URL (RFC 3986 percent-encoding, `%20` not `+`) -> `UrlOpener.open` -> redirect (loopback listener, or
  `handle_auth_redirect(url)`) -> state check -> `POST /v1/auth/pixelfed/token` -> session stored -> signed_in ->
  outbox woken -> refetch. 5 min redirect timeout. A newer AuthLogin or `logout()` cancels a pending flow while
  it waits for the browser (a closed tab must be retryable). **Post-sign-in refetch:** from the code exchange on
  (`Flow.exchanging`) the flow stays current until the first signed-in fetch is done; `auth_json` is `pending`
  with `"message":"Loading your feed…"` while the session is already in place, and goes `signed_in` only after
  that fetch (fetched or failed; skipped if a logout/401/newer session got there first), so the status line never
  shows `@user` over the signed-out screen. An AuthLogin in that phase is ignored (logged), not a second flow.
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
- **auth_json:** `{"state":"signed_out"}`, `{"state":"pending","message"?}`, `{"state":"signed_in","username",
  "display_name","avatar_url"}` (from the token response), `{"state":"error","error":"<message>"}`.
- **Error messages are short and human** (`auth::SIGN_IN_*`, `auth::sign_in_error_message`), mapped from the
  pod's `error` code: `account_not_owner` -> "This pod belongs to a different Pixelfed account.",
  `oauth_exchange_failed` -> "Pixelfed rejected the sign-in. Please try again.", `instance_not_allowed` -> "This
  pod isn't configured for that Pixelfed instance.", `redirect_uri_not_allowed` -> "This pod doesn't accept
  sign-ins from this app.", `upstream_unreachable`/`upstream_error`/502/504 -> "Can't reach Pixelfed right now.",
  transport failure -> "Can't reach your pod."; `error=access_denied` redirect -> "Sign-in was cancelled.",
  timeout -> "Sign-in timed out. Please try again.", anything else -> "Something went wrong. Please try again."
  The URL, HTTP status and body go to the log only (`sign-in failed: <detail> (shown: <message>)`). The status
  line wraps the error to 2 lines (the auth text takes the row's remaining width) instead of truncating it.
- **Session store:** one key `pod_session:<base_url>` holding JSON `{token, username?, display_name?, avatar_url?}`
  (a bare non-JSON value is read as a token). Restored at construction (snapshot starts signed_in). Default store
  when the platform passes none: macOS login Keychain (`keyring` 3, apple-native only, service
  `org.theconstruct.client`; a `cfg(target_os = "macos")` dependency so Android never compiles it), in-memory
  elsewhere. `init`/`init_with_workspace` use the default; `init_with_config` (tests) uses in-memory. Platform
  callbacks (store, opener) run inside `catch_unwind`, so a foreign exception surfaced as a panic is contained.
  **Store I/O off the UI thread, never under the flow lock:** every session change (sign-in, logout, 401) updates
  memory first and then calls `sync_session_store` on a background thread (auth / logout / fetch / sync worker),
  which writes whatever session is current *when it runs* (set, or delete when signed out), serialized by its own
  lock. So the last write always reflects the last change and a logout racing a sign-in's slow Keychain write
  cannot leave the session in the store. Consequence: right after `logout()` returns the store may still hold the
  session for a moment (memory is already signed out).
- **Restart-safe custom-scheme sign-in:** before opening the browser, a custom-scheme flow persists
  `{state, code_verifier, redirect_uri, instance_url, created_at}` in the secure store under
  `pending_auth:<base_url>` (`auth::PendingAuth`, TTL 10 min = `PENDING_AUTH_TTL`); loopback flows do not (the
  listener dies with the process anyway). If the app process dies while the browser is in front (Android), the
  `construct://oauth` redirect cold-starts a new engine with no flow in memory: `handle_auth_redirect` installs a
  placeholder flow (so AuthLogin/logout cancel it), returns true, and on the auth thread loads the record; if the
  state matches and it has not expired it is deleted and the code exchanged as usual (pending -> signed_in), else
  `auth_json` becomes error "Sign-in was interrupted; please try again" (`SIGN_IN_INTERRUPTED`; not shown if
  already signed in, e.g. a replayed redirect). The record is deleted once the redirect arrives, when a flow ends
  (success, failure, timeout, cancel) unless it belongs to the then-current flow, on logout, and when found
  expired; it is kept when the engine is dropped mid-flow (the next engine may complete it).
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
- **Logout:** non-blocking: cancel flow, clear the in-memory session, purge the private cache (below), notify
  signed_out; then on a thread: sync the store (delete the session, drop a pending sign-in), `POST /v1/auth/logout`
  (best effort) and refetch.
- **Private cache purge on sign-out:** the signed-in feed must not outlive the session in `cache.db`. On logout and
  on a 401 sign-out, in one transaction: every entity of a private domain (`PRIVATE_DOMAINS` = `pixelfed`) that no
  PENDING outbox row targets is deleted, and so is the current workspace's `active_workspace` row if it references
  any `pixelfed://` URI; in memory those entities and every query listing a private URI go (the layout stays until
  the refetch replaces it). Explicit logout first deletes PENDING rows whose endpoint starts with `/v1/pixelfed/`
  (they need a session and can no longer be sent as anyone); a 401 sign-out keeps them (paused-flush policy above)
  together with their entities, and the sync worker then refetches so the signed-out bundle replaces the emptied
  feed. A workspace fetch that was sent with a session which is no longer current when it returns (logout or a
  401 meanwhile) is discarded rather than applied, so an in-flight fetch cannot re-cache the private feed.
  The client DB runs with `PRAGMA secure_delete=ON` (freed cells/pages zeroed; cheap at this size) and the purge
  ends with `PRAGMA wal_checkpoint(TRUNCATE)`, so the purged rows survive neither in free pages of `cache.db` nor
  as old WAL frames (tested on the raw bytes). Fragments of rows overwritten before this change in an existing
  file are not scrubbed (no VACUUM).

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
- **Amended (M6-X): optional, secondary.** The fake (D16) is the primary e2e target. The real stack is for
  re-recording `fixtures/pixelfed/recorded/` and a contract test: `scripts/e2e.sh --upstream real` runs the same
  `crates/e2e` suite (scripted web login as alice, like, offline like, bookmark, all undone at the end) against it,
  intended for a scheduled CI job on a throwaway instance (each run registers one OAuth app). Found while porting
  the login: Pixelfed answers `POST /login` with 500 when the request has no `User-Agent` (it records the device).

## D21: Platform sign-in glue (desktop, Android, simulator)

- **Bridge:** `UniffiEngineBridge.create(storageDir, baseUrl, workspace, secureStore?, urlOpener?, redirectUri?)`
  wraps `EngineHandle.newWithOptions`. `ConstructEngineBridge` gained `handleAuthRedirect(url): Boolean` and `logout()`
  with no-op defaults so fakes compile unchanged; `ConstructController.logout()` backs the status line's "Sign out"
  (a small clickable label, not a Material `TextButton`, whose 40 dp minimum height would bloat the status line).
- **Desktop:** no store (Keychain), loopback, `java.awt.Desktop.browse` if `BROWSE` is supported, else
  `ProcessBuilder("open", url)`; false if both fail.
- **Android:** `launchMode="singleTask"`, not singleTop: the browser is started with `FLAG_ACTIVITY_NEW_TASK` and runs
  in its own task, so with singleTop the `construct://oauth` VIEW intent would create a second `MainActivity` (and a
  second engine without the pending flow) in the browser's task. `onNewIntent` (and `onCreate` on a fresh start)
  forwards `construct://oauth` VIEW intents to the ViewModel's bridge. `KeystoreSecureStore`: AES-256/GCM/NoPadding key
  `construct_session_key` in AndroidKeyStore (generated on first use), value stored as `base64(iv):base64(ciphertext)`
  in private SharedPreferences `construct_secure` (`commit()`, synchronized); undecryptable values read as absent.
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

## D22: Versioned layouts and the SSE push channel (`server::layouts`)

> Superseded by D32 in part: no rollback, versions or agent routes and no `CONSTRUCT_AGENT_OPEN`; versions now come only from layout files (`app_versions`). The SSE push channel below still holds.

- **Storage:** migration v5 `layout_versions(id, workspace_id, version, json, prompt, provider, patch_json,
  created_at, UNIQUE(workspace_id, version))`. Replaced by the v6 catalog (D26); the rules below still hold per layout. Only the workspace root `UiNode` is versioned; entities still come
  from fixtures / the upstream. Versioned workspaces: `home`, `pixelfed_demo`, `pixelfed_home` (signed-in feed
  layout only; the signed-out screen is a different screen and carries no version header). Version 1 is the
  built-in layout, inserted lazily (`INSERT ... WHERE NOT EXISTS`) on first read or commit. Appends compute
  `MAX(version) + 1` inside the INSERT, under the store mutex, so versions are gapless and never reused.
- **Append-only:** a rollback copies version N into a new version (provider `rollback`, prompt `rollback to vN`);
  nothing is ever updated or deleted. Old fixture changes don't reach an existing database's v1 (by design: the
  stored history is what clients saw); delete `server.db` to reseed.
- **Internal API (for the agent endpoint):** `layouts::latest(store, ws) -> Option<(version, UiNode)>`,
  `layouts::commit(store, ws, &UiNode, prompt, provider, patch) -> version` (no validation, no broadcast),
  `layouts::commit_and_publish(&state, ...)` (async; commit + broadcast), `layouts::require_agent_auth`.
- **Auth:** versions/rollback/agent require a valid pod session unless `CONSTRUCT_AGENT_OPEN=1` (dev/demo, so QA can
  curl without signing in). The SSE endpoint follows the workspace GET: anonymous subscriptions are allowed because
  an event carries only `{workspace_id, version}` (no layout, no entities); a client must still fetch the workspace
  with its own credentials. A stale bearer on `pixelfed_home` gets 401 so the client notices.
- **Push:** a `tokio::sync::broadcast` channel (capacity 64) in `AppState`; each SSE stream subscribes before
  reading the current version (no missed commit; a duplicate version is harmless), filters by workspace, and on
  `Lagged` re-sends the current version instead of the skipped ones. Keepalive is axum's `KeepAlive` comment
  (`: keepalive`, 15 s, configurable via `AppState::with_sse_keepalive` for tests). `AppState::shutdown()` ends
  open streams; `main` calls it on Ctrl-C/SIGINT or SIGTERM so graceful shutdown is not held open by subscribers
  and clients see the stream end (a process killed without it cuts the chunked body, which reqwest reports as
  "error decoding response body"; the client logs that as "connection lost mid-stream" and reconnects).

## D23: In-app layout agent UI (shared Compose) and the simulator's fake rules

> Superseded by D33: the AI FAB/panel, `AgentPromptOverlay` and `FakeLayoutRules` are removed. Still valid: slot-keyed recomposition, `PlatformBackHandler`, the simulator's `type` and must-hit `click`.

- **Panel, not ModalBottomSheet:** a FAB (`agent-fab`, "Edit layout with AI") bottom-end opens an in-tree animated
  bottom panel (`agent-panel`: `agent-prompt` field, suggestion chips, `agent-send`, `agent-status`, `agent-close`).
  An in-tree panel renders in the headless harness's `onRoot().captureToImage()` and leaves the feed visible and
  interactive above it, so the layout change is seen as it happens. Insets come from the platform root (Android pads by
  `safeDrawing`, which includes the IME).
- **State outside the server-driven tree:** `AgentPromptOverlay` is drawn by `ConstructApp` over the workspace;
  `rememberSaveable` open flag and prompt text. The text is cleared only when the agent status, after this panel's
  send, changes to `done` and only if the field still holds the sent text (an edit made while running is kept); an
  error keeps it for editing. Send is disabled while blank or `running`, and from a tap until a newer snapshot seq
  arrives (two taps or IME action + button within one frame post once). Android: `adjustResize`, so the IME arrives
  as insets (no window pan) and the root's `safeDrawingPadding` lifts the panel; the status line is a polite live
  region.
- **Status:** `EngineSnapshot.agentJson` (default `{"state":"idle"}`) -> `ConstructState.agent` (`AgentInfo`):
  "Applying...", "Layout updated (v3, rules)", the engine's "No change needed (v3)" when `unchanged`, or the error
  message. A send whose result equals the status before it (the same "No change needed" twice) still completes:
  the pending send is re-based on `running`.
- **Back:** `expect fun PlatformBackHandler(enabled, onBack)` (commonMain); Android actual = activity-compose
  `BackHandler` (dependency added to `:shared`'s androidMain), desktop actual = a no-op unless a `LocalBackPresses`
  is provided (tests). The open panel closes on Back, keeping the typed text; closed, Back is not consumed.
- **Recomposition stability:** workspace slots are keyed by `slot_id` and the PostCard's author/media/action slots by
  node kind (`postCardSlotKeys`), on top of FeedList items keyed by URI. So a carousel keeps its page when captions are
  hidden, when the carousel moves to the top slot, and when a slot is inserted above the feed (tested).
- **Fixture mode:** `FakeEngineBridge` answers `PromptAgent` with `FakeLayoutRules` (hide captions, media at the top /
  photos first, add a weather widget; composable, tree-walking, same shapes as the server's rules provider), bumps
  its layout version (not when the layout is already in the requested state, like the server's `unchanged`) and
  reports provider `fake-rules` ("No change needed (vN)" with `unchanged` when nothing changed); unknown prompts ->
  error "I can't do that yet. Try: ..." (client-core's wording); the weather slot is the server's padded Container; a caption/media rule on a
  layout without a PostCard -> the server's "this layout has no pixelfed://PostCard", offline -> error. Live mode sends the intent to the real engine.
- **Simulator `type <target> <text>`:** clears the field then `performTextInput`; target `tag:<t>`, one word, or
  `"quoted text"`.
- **Simulator `click` must hit:** the click lands at the node's center, so a match whose center is outside its
  ancestors' clip or the screen (the feed is a scrolling Column, every post is composed) is first scrolled into
  view with `performScrollTo`; still off screen -> the step fails with the bounds. Before, `click Like` could pass
  while dispatching nothing.

## D24: Client agent prompt, layout version and push subscriber (client-core)

> Superseded by D32/D33 in part: `PromptAgent`, the agent thread and `agent_json` are removed. The layout version tracking and the SSE push subscriber below still hold.

- **PromptAgent:** dispatch trims the prompt (blank -> `agent_json` error, no request), goes `running` and POSTs
  `{"prompt","workspace_id": <engine's workspace>}` to `/v1/agent/modify_layout` on a `construct-engine-agent` thread
  (bearer when signed in, 180 s timeout: the pod's LLM call may take 120 s plus retries). A per-prompt generation
  counter lets only the latest prompt report status; an older one's layout is still applied if newer.
- **200:** the response's `workspace` is applied at once (no refetch): layout only, `State::apply_layout` keeps
  entities, queries and the outbox and rewrites the cached `active_workspace` row with its existing entity URIs and
  queries (the response carries `entities: {}`, so the fetch merge must not be used). Skipped if an equal or newer
  version is already applied (the SSE refetch may win the race): both paths are idempotent. Not applied (a refetch
  instead) when no versioned layout is on display (`layout_version` is `None`, e.g. the signed-out `pixelfed_home`
  screen with `CONSTRUCT_AGENT_OPEN=1`) or the session changed since the request. `agent_json` =
  `{"state":"done","message":"Layout updated (vN, <provider>)","provider","layout_version"}`; an `"unchanged": true`
  response -> `"message":"No change needed (vN)"` plus `"unchanged":true`.
- **Errors:** non-2xx -> `error` with a sentence per known case: `unsupported_prompt` -> "I can't do that yet. Try:
  <first 3 supported>"; `invalid_patch` -> "That change would break the layout: <detail>"; 409 / `conflict` -> "The
  layout changed meanwhile; please try again."; `provider_not_configured` / `provider_error` -> fixed sentences
  (details stay in the pod log); 401 -> "Sign in to change the layout.". Others: `"<error>: <detail>"` or
  `HTTP <code>: <body start>`; plus `provider` when the pod names one; transport failure -> `"offline: try again
  when online"`. A 401 for the current session signs out like a fetch 401 (D19) and refetches.
- **Stale status:** a fetch whose layout version is newer than the applied one resets a `done`/`error` status to
  idle unless that status names this version (the prompt's own SSE echo); `running` is left alone.
- **Layout version:** from `X-Construct-Layout-Version` on every applied fetch (absent -> `None`, e.g. the signed-out
  `pixelfed_home` screen), or the prompt response's `layout_version`. In memory only (a restart refetches anyway). A
  fetch whose header is older than the applied version (it started before a prompt response was applied) keeps the
  newer layout and takes only the entities.
- **Push subscriber (`client_core::events`):** a `construct-engine-events` thread, connected only while `is_online`,
  `GET /v1/workspaces/<id>/events` with `Accept: text/event-stream` and the bearer when signed in. Async reqwest on a
  private current-thread Tokio runtime (`tokio` with `rt,time,net`), read in 200 ms slices (`chunk()` is
  cancel-safe), so a dropped engine closes the connection within a slice and a session change reconnects with the
  new bearer. Blocking reqwest could not do this: its client timeout is both the per-read and the header timeout.
  The thread holds only a `Weak` while reading; the refetch (blocking `refresh`, under the `net` lock as usual) runs
  outside the runtime. No bytes for 45 s (3 missed 15 s keepalives) -> reconnect. Reconnect backoff 1 s doubling to
  30 s, reset by the first bytes received; repeated identical errors are logged once. Stream 401 with a session ->
  same sign-out as a fetch 401.
- **Parser:** WHATWG event-stream rules: LF/CRLF/CR line ends (also split across chunks), `:` comments ignored,
  one leading space stripped, multi-line `data` joined with `\n`, blank line dispatches only if data was seen,
  unknown fields ignored. A line or one event's data over 64 KB (`MAX_EVENT_BYTES`) is an error: the connection is
  dropped and reopened (with the usual backoff) instead of buffering without bound. `event: layout` with `{"workspace_id","version"}` for this workspace and a version newer
  than the applied one (or none known) -> refetch; equal/older ignored, so the server's on-connect "current
  version" message costs nothing unless the client missed a change. The refetch merge (D14) re-applies PENDING
  outbox rows, so optimistic values survive a pushed layout change.
- **Snapshot/FFI:** `StateSnapshot.agent_json` / `FfiStateSnapshot.agent_json` (default `{"state":"idle"}`), mapped
  to `EngineSnapshot.agentJson` in `UniffiEngineBridge`.

## D25: Layout agent endpoint and providers (`server::agent`)

> Superseded by D32: `POST /v1/agent/modify_layout` is no longer routed (the module stays in the library with its tests, and its structural validator checks layout files); `CONSTRUCT_LLM_*` / `ANTHROPIC_*` have no effect on a running pod.

- **Flow:** `POST /v1/agent/modify_layout` loads the latest version, asks the provider for an RFC 6902 patch (paths
  relative to the `core://Workspace` root), applies it to a copy with `json-patch` one op at a time (<= 200 ops,
  patch <= 256 KB; depth and size are checked after every op, since `copy` can double the document), validates
  (deserializes as `UiNode`; JSON depth <= 32, since clients parse and render recursively; <= 256 KB; same root id;
  1..=50 slots with unique `slot_id`s; `MediaCarousel.aspect_ratio` in 0.2..=5 (a non-finite `f32` would serialize
  as `null` and make the stored layout unreadable); `Container.padding_dp` <= 64; `OptimisticMutate.endpoint` starts
  with `/v1/`; every `OptimisticMutate` / `AuthLogin` / `Navigate` intent must equal one already in the current
  layout, so a patch can move or copy intents but never invent endpoints, targets or sign-in instances;
  `PromptAgent` / `NavigateBack` are free), and only then commits with prompt, provider and patch. Any failure
  commits nothing. An empty patch, or one whose result equals the current layout, answers 200 `"unchanged": true`
  at the current version (no empty versions). The commit is compare-and-append on the version the patch was made
  for (`Store::append_layout_if_latest`, one statement); if a concurrent prompt or rollback committed meanwhile, the
  same patch is re-applied and re-validated on the new latest (up to 3 more times, then 409 `conflict`), so a slow
  provider call never silently reverts a rollback.
- **Provider trait:** `LayoutProvider { name, model, generate -> boxed future of Generated{ops, explanation} }`
  (boxed so the selection is an `Arc<dyn>`). `Selection::from_env` resolves `CONSTRUCT_LLM_PROVIDER`; an explicit
  `anthropic` without a key is `NotConfigured` (server starts; prompts get 503) rather than a silent fallback.
- **Rules provider:** phrase matching on a normalized prompt (lowercase, punctuation -> spaces, word-bounded); the
  matched rules run in prompt order, each on the previous one's result, emitting `replace`/`add`/`remove` ops with
  explicit values only for slots that change (so they compose and are idempotent). Nodes are found by walking the
  JSON for `pixelfed://PostCard` objects and by node kind inside their four slots, not by index. Moves ("media at
  the top", "action bar above the photo") move one slot node to a new position and keep the others' relative order
  (for the built-in card both are the contract's swaps). The weather slot is a `core://Container` (Vertical,
  `padding_dp` 16) around the Caption text, so it is not flush with the screen edge. A recognized rule with nothing to act on (e.g. captions on
  `home`) is `unsupported_prompt` with a `detail`.
- **Anthropic provider:** raw reqwest, non-streaming (`max_tokens` 16000 fits the 120 s timeout), structured outputs
  with the contract's PATCH_SCHEMA; the system prompt is two blocks, instructions + registry first (with
  `cache_control`, since they are identical across prompts) then the pretty-printed layout (serde_json maps are
  sorted, so the text is deterministic). Refusal / `max_tokens` / non-JSON text / unparsable `value_json` ->
  502 `provider_error`; 401/403 -> 503 `provider_not_configured` naming the rejected key; 429/5xx/`overloaded_error`
  retried twice (honoring `retry-after`, capped at 30 s, else 1 s doubling); connect errors retried, timeouts not.
  The whole call (retries and delays included) is bounded by 170 s, under the client's 180 s. `AnthropicConfig`'s
  `Debug` redacts the key.
- **Request hygiene:** workspace ids are 1..=64 chars of `[A-Za-z0-9_-]` (`layouts::is_valid_workspace_id`); else
  400 `invalid_workspace_id` with a truncated echo, on agent, versions, rollback, events and the workspace GET. The
  agent/history 401 carries `"detail":"a valid pod session is required (or CONSTRUCT_AGENT_OPEN=1 for local demos)"`.
- **Registry:** `schemars::JsonSchema` derived on the protocol AST/intent types behind protocol's `schema` feature
  (the server enables it); `agent::registry()` = `schema_for!(UiNode)` (intents under `$defs`). A new variant must
  derive the schema to compile, and a test checks every variant tag is in the registry.

## D26: Layout catalog, per-layout history and the workspace revision (`server::layouts`, migration v6)

> Superseded by D32: the catalog, built-in alternatives, all `/v1/layouts` and legacy versions/rollback endpoints, catalog writes and the `X-Construct-Layout-Id(-Version)` headers are gone; migration v7 turns the revision log into one linear history per app and leaves the v6 tables inert. Content-addressed `layout_blobs` stays.

- **Model:** a workspace has several layouts (`layouts(id, workspace_id, title, description, preview_hint, builtin,
  created_at)`, ids `<workspace>.<name>`, 1..=128 chars of `[A-Za-z0-9_.-]`), each with its own append-only,
  gapless history `layout_versions(layout_id, version, hash, source, prompt, provider, patch_json, created_at)`;
  `source` is `seed` | `agent` | `rollback` | `restore` | `manual`. Exactly one layout per workspace is ACTIVE: the
  latest row of `workspace_revisions(workspace_id, revision, layout_id, layout_version, reason, created_at)`. Every
  visible change appends a revision in the same transaction as its version: an agent commit to the active layout,
  a restore/rollback of the active layout, an activation. Nothing is updated or deleted.
- **Client contract kept (D24):** `X-Construct-Layout-Version` and the SSE `version` are the workspace REVISION, so
  client-core still sees one monotonic number per workspace. Additive: headers `X-Construct-Layout-Id` and
  `X-Construct-Layout-Id-Version` (the layout's own version) on the workspace GET, rollback and agent responses; SSE
  data `{"workspace_id","version","layout_id","layout_version"}`; agent response `layout_id`, `layout_id_version`
  (its `layout_version` stays the revision). In the catalog endpoints `version` / `layout_version` always mean the
  per-layout version and `revision` the workspace revision.
- **Built-ins:** `fixtures/layouts/*.json` (`include_str!`) plus the existing defaults: `pixelfed_home` and
  `pixelfed_demo`: `classic` (the built-in feed, default), `editorial` (media first at 0.8 portrait, then the
  author, then the caption; no action bar), `compact` (wide 1.91 media inset by a 24 dp Container, Caption-style
  caption), `minimal` (no action bar: an empty Container); `home`: `counter` (default), `big_counter` (count first,
  buttons, small title). The PostCard renders its slots in a fixed order, so variants move nodes between slots, as
  the rules provider does. A test validates every seed with the agent validator against its workspace's default
  layout (same root id, slots, bounds, no new restricted intents) and checks it is canonical and distinct.
- **Seeding:** once per `Store` (an atomic flag), lazily on first catalog/workspace use, in one transaction:
  metadata upserted, version 1 (`seed`) only for a layout without versions, revision 1 on the default layout only
  for a workspace without revisions. So new built-ins reach existing databases, and fixture changes never rewrite
  history (as in D22, delete `server.db` to reseed).
- **Migration v6 (Rust step; `MIGRATIONS` now holds SQL or Rust steps):** the v5 table is renamed, each row (W, N)
  becomes version N of W's default layout (`home.counter`, `<ws>.classic`) with its prompt, provider, patch and
  time (source: `rollback` for provider rollback, `seed` for an unannotated v1, else `agent`/`manual`) and revision
  N of W; then the v5 table is dropped. The revision therefore continues at the old max version and a connected
  client's number never goes backwards. Tested on a database built with migrations 1..=5 and by hand on a db
  written by the previous binary.
- **Storage:** specs are content-addressed: `layout_blobs(hash = SHA-256 hex of the canonical JSON (the `UiNode`
  serialization), encoding, raw_len, data)`, zstd level 19 when smaller (else raw); a restore, rollback, activation
  with a version, or re-seed of identical content adds only a version row and maybe a revision row. Patches stay
  TEXT (191 -> 132 B with zstd: not worth it). Measured (`cargo run -p server --release --example layout_storage
  [N]`, VACUUM'd file, 1k and 10k runs agree):

  | | raw JSON | stored | per distinct edit | per restore | 1k versions | 100k versions |
  |---|---:|---:|---:|---:|---:|---:|
  | v5 (raw row per version) | ~1.0-1.5 KB | same | 2077 B | 2077 B | 2.0 MB | 198 MB |
  | dedupe only | | same | | | 1.3 MB | 124 MB |
  | v6 (dedupe + zstd-19) | ~1.0-1.5 KB | 340-565 B | 1155 B | 184 B | 0.65 MB | 64 MB |

  (1k/100k = half distinct edits, half restores; all-distinct edits: 1.1 MB / 110 MB.) zstd-19 costs ~0.17 ms per
  commit. A zstd dictionary built from the built-ins would shrink an edited layout blob from 616 to ~101 B, but it
  would freeze the dictionary into the storage format; deferred. Every version is kept: no
  `CONSTRUCT_LAYOUT_RETENTION` (deleting versions would break gapless numbering, restore targets and history, and the
  numbers above do not call for it).
- **Endpoints:** `GET /v1/layouts[?workspace_id=]` -> `{"layouts":[{id, workspace_id, title, description,
  preview_hint, builtin, current_version, created_at, updated_at, active, active_version?}], "workspaces":
  [{workspace_id, revision, layout_id, layout_version}]}` (+ `workspace_id`, `workspace_revision` when filtered);
  `GET /v1/layouts/:id` (same object + `workspace_revision`); `GET /v1/layouts/:id/versions[?limit=1..1000
  (100)&before=V]` -> `{layout_id, workspace_id, current_version, versions:[{version, created_at, source, prompt?,
  provider?, hash, bytes}], next_before?}` newest first; `GET /v1/layouts/:id/versions/:n` -> metadata + `layout`;
  `POST /v1/layouts/:id/activate` `{}` | `{"version": n}` (restore n first unless equal to the newest content) ->
  `{workspace_id, revision, layout_id, layout_version, unchanged, restored_version?}`; `POST
  /v1/layouts/:id/restore {"version": n}` -> `{layout_id, workspace_id, version, restored_from, unchanged, active,
  revision?}` (content equal to the newest -> `unchanged`, nothing appended; a restore is itself undoable). Changes
  to the active layout broadcast on the SSE channel; an inactive layout's restore does not. Errors: 400
  `invalid_layout_id` / `invalid_request` (also a body that is not an object, e.g. `[1]`), 404 `layout_not_found` /
  `version_not_found` / `workspace_not_found`. A restore's `active` / `revision` are those its own transaction
  appended (not a later read, which could already see a concurrent switch).
- **Legacy endpoints:** `GET /v1/workspaces/:id/versions` lists the ACTIVE layout's versions (same shape as D22);
  `POST /v1/workspaces/:id/rollback` appends to the active layout (always appends, as before) and answers `version`
  = the layout's new version plus `revision` and `layout_id`. On a fresh database revision and version coincide
  until the first switch, so the existing tests kept their numbers.
- **Agent:** commits to the active layout with compare-and-append on `(layout_id, layout_version)`; a concurrent
  edit, restore or SWITCH makes it re-apply the same patch on the new active layout's latest (up to 3 more times,
  then 409), so a switch during a slow provider call is not reverted and the edit lands on what the user now sees.
- **Auth:** reads follow the workspace GET per layout's workspace: `home`/`pixelfed_demo` anonymous, `pixelfed_home`
  needs a valid session (no bearer -> 401 with the agent detail, stale bearer -> 401 like the GET) unless
  `CONSTRUCT_AGENT_OPEN=1`; the unfiltered list omits `pixelfed_home` for anonymous callers (stale bearer -> 401).
  Writes (activate, restore) need a session unless `CONSTRUCT_AGENT_OPEN=1` (D22).
- **f32 in responses:** layouts are turned into JSON values through their text form (`layouts::node_value`), since
  `serde_json::to_value` widens `aspect_ratio` 0.8 to 0.800000011920929.

## D27: Client layout catalog API (client-core, FFI)

> Superseded by D33: the catalog calls are replaced by read-only `list_apps` / `app_versions` / `app_version`, and `layout_json` is now `{"app_id","version"}`. The error type and blocking-call style below still hold.

- **Calls** (`client_core::catalog`, methods on `ConstructEngine`): `list_layouts()` (`GET
  /v1/layouts?workspace_id=<engine's workspace>`), `layout_versions(id, before?, limit?)`, `layout_version(id, n)`,
  `activate_layout(id, version?)` (`{}` | `{"version": n}`), `restore_layout_version(id, n)`. Each returns the pod's
  JSON body as a `String` (D26 shapes; the same pass-through style as the snapshot's `*_json` fields, so the FFI
  needs no record per response). **Blocking** on the caller's thread with its own reqwest client (connect 5 s,
  total 15 s), not on an engine thread: the platform already has a background dispatcher (`Dispatchers.IO`), and a
  result-returning call is simpler than a status field in the snapshot. Not under the `net` lock (reads do not
  race the outbox). Bearer = the current pod session; layout ids are percent-encoded as path segments.
- **Errors:** `LayoutApiError::{Offline, Unauthorized, NotFound, Server{status}}`, each with a short `reason` for the
  UI: transport -> "Can't reach your pod. Check your connection and try again."; 401 -> "Sign in to change
  layouts." (writes) / "Sign in to see these layouts." (reads); 404 by the pod's `error`: `version_not_found` ->
  "That version doesn't exist.", `workspace_not_found` -> "This workspace has no layouts.", else "That layout
  doesn't exist."; 400 `invalid_layout_id` -> "That isn't a valid layout id.", other codes -> "The pod answered
  HTTP n (code).", 5xx without a code -> "The pod had a problem (HTTP n). Please try again." The URL, status and
  body start go to the log. A 401 for the session that is still current signs out exactly like a fetch 401 (D19:
  memory + store cleared, private cache purged, pending rows kept) and refetches; a 401 without a session changes
  nothing. The UniFFI error's field is `reason`, not `message` (UniFFI's Kotlin exceptions override `message`).
- **Applying a write:** after a 2xx activate (not `unchanged`, `workspace_id` = the engine's) or restore (`active`
  and not `unchanged`), the call runs the normal blocking refetch before returning, so the next snapshot already
  shows the new layout without waiting for SSE. The SSE echo then carries an equal revision and is ignored (D24);
  the fetch merge re-applies PENDING outbox rows (D14). The response itself carries no layout, so applying it
  directly (as the agent path does) is not possible.
- **Snapshot:** `StateSnapshot.layout_json` / `FfiStateSnapshot.layout_json` = `{"layout_id","layout_version",
  "revision"}` (the active catalog layout, its per-layout version, the workspace revision) from
  `X-Construct-Layout-Id`, `X-Construct-Layout-Id-Version` and `X-Construct-Layout-Version` of the applied fetch, or the agent
  response's `layout_id` / `layout_id_version` / `layout_version`; absent keys are omitted, `"{}"` when nothing is
  known (unversioned screen, before the first fetch). In memory only, like the revision.
- **Kotlin:** `ConstructEngineBridge` gained `listLayouts()`, `layoutVersions(layoutId, before?, limit?)`,
  `layoutVersion(layoutId, version)`, `activateLayout(layoutId, version?)`, `restoreLayoutVersion(layoutId, version)`:
  plain blocking `String`-returning functions (call on `Dispatchers.IO`) that throw
  `org.theconstruct.LayoutApiException(message, kind, status)`; the interface defaults throw "Layouts aren't
  available here." so other fakes compile. `UniffiEngineBridge` maps `uniffi.client_ffi.LayoutApiException` (Int
  args clamped to `UInt`, negatives -> 0, which the pod rejects readably). `EngineSnapshot.layoutJson` (default `{}`)
  -> `ConstructState.activeLayout` (`ActiveLayout(layoutId?, layoutVersion?, revision?)`, lenient parse).
  `FakeEngineBridge` serves `FakeLayoutCatalog` (same JSON shapes; `pixelfed_demo` for a bundle with
  `pixelfed://` entities, else `home`; the active layout's newest version is the bundle's layout so the screen starts
  unchanged; activate/restore of the active layout swap the layout and bump the revision; an applied prompt appends
  to the active layout; offline -> `Kind.Offline`).

## D28: App icon "the construct"

- **Design:** an original geometric icon of a bright white space with a soft sky gradient and a grey perspective
  grid floor fading into the horizon, with one white wireframe-edged cube standing on it: a light grid space where
  things get built. No film imagery, text or trademarks. The cube is a flat projection matched to the grid's
  viewing angle, because true perspective made it look distorted.
- **One source:** every asset comes from one geometry definition in `scripts/gen_icons.py` (python3 + Pillow +
  numpy, rendered at 4x and scaled down; `iconutil` for the .icns), run by `scripts/gen_icons.sh`. Generated files
  (`assets/icon/*.svg`, `png/construct-{16..1024}.png`, `.icns`, `.ico`, Android layers and mipmaps, desktop
  resources) are committed, reproducible byte for byte, and never edited by hand.
- **Detail levels:** `full` (SVG and PNGs >= 64 px), `launcher` (heavier lines, bigger cube; the Android layers),
  `small` (48 px: fewer, darker lines) and `tiny` (<= 32 px: darker tile frame, heavy cube outline, 2-3 grid
  lines), so the grid still reads at small sizes.
- **Android:** adaptive icon from vector layers (background = sky, grid, haze; foreground = the cube inside the
  66 dp safe zone; monochrome = grid plus cube outline for Android 13+ themed icons) plus PNG legacy/round mipmaps.
- **Desktop:** `.icns` / `.ico` / png in `nativeDistributions`; at runtime the window icon is the full-bleed tile
  and the dock icon (`java.awt.Taskbar`, guarded by `isSupported` and try/catch, so headless runs are unaffected) is
  the macOS-shaped variant with margin and shadow.

## D29: Default base URL = the staging pod

> Superseded by D33 in part: the server picker is in the shared menu in all builds (incl. release) and on desktop; Android cleartext to the loopback hosts is allowed in all builds. Staging as the default and per-server storage still hold.

- Every client defaults to the deployed staging pod `https://the-construct-pod.fly.dev`: desktop (`construct.baseUrl`
  / `CONSTRUCT_BASE_URL`; `:desktopApp:run` forwards `-Dconstruct.baseUrl` / `-Dconstruct.workspace` to the app JVM),
  Android (`-PconstructBaseUrl` -> `BuildConfig`) and the live-mode simulator (`-PbaseUrl`).
- A local server is always chosen explicitly: `http://127.0.0.1:8080` on desktop and the simulator,
  `http://10.0.2.2:8080` on the emulator (`scripts/android_smoke.sh --base-url`, default the emulator URL), or
  `127.0.0.1` with `adb reverse` on a phone.
- Debug Android builds have an in-app server picker chip (Staging / Local emulator / Local USB / Custom URL /
  Build default; custom input must parse as `http(s)://host[:port]`), saved in SharedPreferences; a choice restarts
  the activity with a new engine. The `baseUrl` intent extra wins over it; release builds ignore both.
- Storage per server: any server other than the build default gets its own directory
  (`filesDir/servers/<sanitized url>`), so a switch never mixes caches or flushes one server's outbox to another,
  and a new engine never opens a cache.db the old one is still closing.
- Debug cleartext stays limited to `10.0.2.2`, `localhost` and `127.0.0.1`; staging is HTTPS.

## D30: Layout browser UI (shared Compose) and simulator commands

> Superseded by D33: the layout browser, its FAB and its simulator commands are removed in favour of the hamburger menu and the version picker.

- **Where:** all in `:shared` commonMain (`LayoutBrowser.kt` UI, `LayoutBrowserModel.kt` model), so desktop, Android,
  the simulator and a future web shell share it. The only platform pieces are `layoutIoDispatcher` (`Dispatchers.IO`),
  the wall clock and the local date format (`jvmCommonMain` actuals).
- **Entry point:** a small secondary FAB (`layouts-button`, "Browse layouts", Material "Dashboard" icon) stacked above
  the AI FAB in `AgentPromptOverlay`'s FAB column, hidden with it while the agent panel is open. Shown on every loaded
  workspace, also when the bridge has no catalog: the browser then shows the readable error ("Layouts aren't
  available here.") instead of the button disappearing.
- **In-tree overlay, not navigation or Dialog windows:** like the agent panel (D23), the browser (`layouts-browser`),
  the confirmation (`confirm-dialog`, scrim + card) and the snackbar (`layouts-snackbar`) are composables drawn by
  `ConstructApp` above the workspace, so the headless harness's `onRoot().captureToImage()` sees them (a Material
  `AlertDialog` opens a second root). The browser is a full-screen `Surface`, which also blocks touches to the feed.
- **Navigation model:** `LayoutNav(open, layoutId?, version?, confirming)`, a pure value: list -> details -> preview,
  `back()` closes the innermost level (dialog, preview, details, list; ignored while the dialog's change runs, like its
  disabled Cancel). The whole browser state (`LayoutBrowserState`: nav, list, histories, the running write, the
  snackbar) is owned by `ConstructController`, so on Android it lives in the ViewModel and a rotation keeps the level, an
  open dialog, a write in flight and the snackbar's Undo; `ConstructApp` also saves the nav with `rememberSaveable`
  (`LayoutNavSaver`, a list of primitives) and restores it once into a fresh controller after process death.
  `PlatformBackHandler(enabled = open)` maps system Back to `back()` (registered after the agent panel's).
- **Loading:** `LayoutRepository` (owned by `ConstructController`, so it survives rotation with the Android ViewModel)
  runs the D27 calls with `withContext(layoutIoDispatcher)`, parses leniently (`LayoutCatalogJson`) and caches version
  specs per (id, version) (immutable on the pod). List and history loads run in `LaunchedEffect`s keyed on the open
  level, Retry tokens and `state.activeLayout`, so closing a level cancels its load and an AI edit or a switch made
  elsewhere reloads it (a reload keeps the stale list on screen). History pages of 20 (`versions-more` uses
  `next_before`; a page that arrives after the newest page was reloaded is dropped, and "Load older" never stays
  spinning when the details close mid-load). Writes and Undo are launched from the UI's scope but run under
  `NonCancellable`, so neither closing the browser nor a rotation drops a result.
  `LayoutCalls.inFlight` counts calls in flight (like `ImageLoads`) for the simulator.
- **Live thumbnails:** `LayoutPreview` renders the real `RenderUiNode` of the layout's current version against the
  current entities and queries at 390 dp (a phone) and scales it into a 120x200 dp box with `graphicsLayer`
  (`wrapContentSize(unbounded)` + `requiredSize`, origin top-left, clipped). Never interactive: intents go to a no-op,
  a pointer layer on top is hit by every event (not consumed, so the enclosing card's click opens the details), and
  `clearAndSetSemantics` hides the inner nodes from screen readers and tests. The on-screen layout uses
  `state.workspace` directly; others fetch their spec lazily; a `preview_hint` placeholder shows while loading or on
  failure. The version preview is the same composable at full height (render width = the box's width, at least
  360 dp). Feeds are plain Columns (every item composed, its images loaded), so previews render at most 3 items per
  query: N thumbnails never mean N x the whole timeline's images.
- **Changes offered:** details of an inactive layout -> "Use this layout" (`layout-use`, activate); preview of a
  version of the active layout -> "Restore this version" (`version-restore`, restore; none for the current version);
  preview of an inactive layout -> "Use this version" (`version-use`, activate with that version, or plain activate for
  its newest). Every one asks first (`confirm-ok` / `confirm-cancel`); a failure (401 "Sign in to change layouts.",
  offline) stays in the dialog with "Try again".
- **After success: close to the workspace.** The user asked for a different screen, so the browser closes and the new
  layout is visible at once (the D27 call refetched before returning); the snackbar says "Switched to Editorial
  journal" / "Restored v2 as v5" with Undo (`snackbar-undo`, 8 s).
- **Undo is append-only:** `undoFor(change, before, outcome)` with `before` = `state.activeLayout` when the change
  started: a switch is undone by `activateLayout(previousId, previousVersion)` (no new version when that is still its
  newest content), a restore by `restoreLayoutVersion(id, previousVersion)` (a new copy on top). No undo when the pod
  answered `unchanged` or the previous layout was unknown. The undo's own snackbar has no Undo. Undo is offered only
  while the screen still shows the layout id and version the change produced (`undoStillApplies`): once another
  device, an AI edit or a later change moved on, Undo would silently revert that instead, so the action disappears.
- **State across a switch:** nothing in the browser touches the workspace tree, so the D23 keys apply: slots by
  `slot_id`, posts by URI, PostCard slots by node kind (a carousel keeps its page when Editorial moves it to another
  slot; tested), the agent prompt text and the outbox are untouched (the engine's merge re-applies pending rows).
- **Fake catalog:** `FakeLayoutCatalog` serves the pod's built-ins with the pod's titles, descriptions and hints,
  read from the repo's `fixtures/layouts/*.json` (found by walking up from the working directory like the bundles;
  rule-derived stand-ins if missing): the feed bundle (root id `pixelfed_home`) gets `pixelfed_home.*`, the one-post
  bundle `pixelfed_demo.*`, the counter `home.*`. Classic has v1 seed, v2/v3 AI prompts, v4 a rollback to v1 (counter:
  seed, AI, rollback), timestamps relative to now.
- **Simulator commands:** `open-layouts`, `select-layout <id | last segment>`, `select-version <n>` (presses Load
  older until the row appears), `use-layout`, `restore-version`, `confirm`, `cancel`, `undo`, `close-layouts`, and a
  generic `back` (the simulator now provides `LocalBackPresses`). Each waits for idle and for `LayoutCalls` to drain,
  waits up to 20 s for its screen, and fails with a hint naming the missing control. `shot` also waits for the
  catalog calls (thumbnails). After every click the harness re-sends a pointer exit, so a screen that opens under the
  old pointer position is not captured with a hover tint.
- **Android:** the debug server chip moved up (96 dp from the bottom) so it never covers the snackbar.

## D31: Rename to The Construct

- **What:** the product "Project Canvas" is now "The Construct", identifiers included: Kotlin packages
  `org.genui.canvas.*` -> `org.theconstruct.*` (Android namespace/applicationId `org.theconstruct.android`, desktop
  bundle ID `org.theconstruct.desktop`, `nativeDistributions` package name "The Construct"); types `Canvas*` ->
  `Construct*` (`ConstructEngine`, `ConstructApp`, `ConstructController`, `ConstructState`, `ConstructEngineBridge`,
  `ConstructViewModel`, `ConstructJson`, `ConstructIcons`, `ConstructImage`) in Rust, UniFFI and Kotlin; logcat tag and
  log prefix `ConstructEngine`, threads `construct-engine-*`; env vars `CANVAS_*` -> `CONSTRUCT_*` (also in
  `deploy/fly/pod.toml` and `deploy/pod.Dockerfile`, same commit), system properties `construct.*`, Gradle properties
  `-PconstructBaseUrl` / `-PconstructWorkspace`; headers `X-Construct-Session`, `X-Construct-Layout-Version`,
  `X-Construct-Layout-Id`, `X-Construct-Layout-Id-Version`; OAuth redirect `construct://oauth`; paths
  `~/.construct`, `~/.construct-server`; Keychain service `org.theconstruct.client`, Android key alias
  `construct_session_key` and prefs `construct_secure`; the pod's Pixelfed `client_name` "The Construct" and
  user agent `construct-pod/<version>`; dev fixtures (`constructdemo`, `constructadmin`, `#construct`, images
  `construct_0N.jpg`).
- **No migration:** clients don't read the old paths, preferences or keychain entries; the Android app is a new
  package (uninstall the old one). Old pod sessions are invalid for the new header names anyway; users sign in again.
- **Staging DB keeps working:** no table or column name contained "canvas", so `/data/server.db` opens unchanged. The
  stored Pixelfed apps are keyed by (instance, redirect URI): the first `construct://oauth` sign-in registers a new app
  (the `canvas://oauth` row is simply unused); an existing account's token refresh still finds its own app row.
- **Deliberately unchanged:** Fly app names and URLs (`the-construct-pod`, `the-construct-fake-pixelfed`), crate
  names (none contained "canvas"), the `kmp-app` directory, git history, and earlier D-entries' narrative (their
  identifiers were updated to match the code).

## D32: One app, layouts as code, read-only versions API (`server::layouts`, migration v7)

- **Product rule:** the pod serves ONE app, Pixelfed (workspace `pixelfed_home`), at its latest layout version. New
  versions come only from the developer, as files in the repo plus a deploy. No client can change layout or version
  state on the pod, and no AI generation is reachable from a client. Pixelfed interactions (like/bookmark through
  the outbox, sign-in/out) and the counter mutation routes stay.
- **Apps:** `layouts::APPS` = `pixelfed_home` "Pixelfed" (icon `photo`), plus two dev apps, `pixelfed_demo`
  "Pixelfed demo" (`photo`) and `home` "Counter" (`counter`). `GET /v1/apps` lists the dev apps only with
  `CONSTRUCT_SHOW_DEV_APPS=1`; their other endpoints always work, because tests, CI and the simulator use them.
- **Layout files:** `layouts/<app>/NNNN.json` = `{"note": "short changelog line", "layout": <UiNode>}`. Names have
  4 digits and are contiguous from 0001. No other keys are allowed, the note must be non-empty, and the root is the
  app's `core://Workspace` with id = the app id. Each file is embedded with `include_str!` through one line in
  `layouts::FILES` (`layout_file!("pixelfed_home", "0002")`). A test fails if a file in `layouts/` is not listed,
  or if a listed file is missing. `0001.json` of each app holds the former built-in default (the classic feed / the
  counter). The named alternatives (Editorial, Compact, Minimal, Big counter; `fixtures/layouts/`) are deleted.
  Files are validated with the agent's structural validator (`agent::validate_structure`: depth, size, slot rules,
  aspect ratio, padding, `/v1/` endpoint prefix). Intents may be new, since a developer wrote the file.
- **Startup sync (`layouts::sync`, from `AppState::try_new`, before listening):** this runs per app, in one
  transaction, and is idempotent. A file already recorded (`file_key` `<app>/NNNN`) must have the same layout hash
  and note. A new file is appended as version `MAX(version) + 1` with source `file`, its note and its key. Anything
  else stops the start and names the file:
  - "layouts/pixelfed_home/0002.json changed after it was deployed as version 7; layout files are append-only, add
    0003.json instead" (reformatting with the same layout and note is not a change)
  - "layouts/<app>/NNNN.json was deployed as version N but is missing; layout files are append-only, restore it
    (and add NNNN.json for a change)"
  - "... is new but numbered below ..., which is already deployed; ... number new files after the newest one"
  - "...: layout files are numbered without gaps, expected NNNN.json next", "... is listed twice", "not valid
    JSON", "unknown key", "\"note\" must be a non-empty string", "invalid layout: ..."

  On a fresh DB `pixelfed_home/0001` becomes version 1. On a migrated DB the files continue after the existing
  maximum, so every 0001 appears once more as the newest version, with the same content as the old built-in.
- **Migration v7 (Rust step):** this adds `app_versions(app_id, version, hash -> layout_blobs, source, note,
  file_key UNIQUE NULL, created_at, PRIMARY KEY(app_id, version))`. Revision R of each workspace
  (`workspace_revisions`) becomes app version R. Its content is the layout shown at that revision (`layout_id` at
  `layout_version`, same blob hash) and it keeps its `created_at`, so the numbers stay the ones clients saw
  (monotonic) and the history shows what was on screen. Source and note:
  - a switch (reason `activate`) -> `switch`, "Switched to <title>"
  - otherwise the shown layout version's source:
    - `seed` -> "Built-in layout: <title>"
    - `restore` -> "Restored <title> vN"
    - `rollback` -> "Rolled back to vN"
    - `agent` -> the prompt, or "AI edit"
    - `manual` -> the prompt, or "Manual edit"

  Legacy rows keep `seed | agent | rollback | restore | manual | switch`; new rows are `file`. If a revision points
  at a missing layout version or blob, the migration refuses (it rolls back and the server does not start) rather
  than invent history. The v6 tables `layouts`, `layout_versions` and `workspace_revisions` are kept but inert:
  nothing reads or writes them after v7. `layout_blobs` stays live (shared by hash). Tested on a DB built with
  migrations 1..=6 (including a switch, a restore and an agent edit), and on an empty v6 DB.
- **HTTP API (all public, no session):**
  - `GET /v1/apps` -> `{"apps":[{id, title, icon, latest_version, updated_at}]}` (RFC 3339 UTC).
  - `GET /v1/apps/{id}/versions[?limit=1..=100 (20)&before=N]` -> `{app_id, latest_version, versions:[{version,
    created_at, source, note?}], next_before?}`, newest first; `next_before` is present iff older versions exist.
  - `GET /v1/apps/{id}/versions/{n}` -> `{app_id, version, created_at, source, note?, layout}`.

  Errors: 400 `invalid_workspace_id`, 404 `app_not_found` / `version_not_found`, 400 `invalid_request` (bad query
  or version). The version API is public because the pod serves the same layouts to everyone (no entities).
  `GET /v1/workspaces/{id}` keeps its auth and sets only `X-Construct-Layout-Version: <latest app version>`
  (`X-Construct-Layout-Id(-Version)` dropped). SSE `event: layout` data is `{"workspace_id","version"}`; clients
  must accept extra keys.
- **Removed (404 now):** `POST /v1/agent/modify_layout`, `POST /v1/workspaces/{id}/rollback`,
  `GET /v1/workspaces/{id}/versions`, and all `/v1/layouts` routes (list, get, versions, activate, restore).
  `CONSTRUCT_AGENT_OPEN` does nothing. `deploy/fly/pod.toml` still sets `CONSTRUCT_LLM_PROVIDER`, which is now
  harmless: nothing reads it at startup.
- **Agent code kept, unrouted:** `server::agent` (the rules and Anthropic providers, patch application and
  validation) stays in the library with its unit tests and `tests/agent.rs`, so the agent can come back later
  (e.g. as developer tooling that writes layout files). `layouts::append_version` (source `manual`, no file)
  exists for tests and tooling only.
- **`ActionIntent::PromptAgent` removed, `ActionIntent::Unknown` added (protocol):** with no agent route, a prompt
  intent could only fail. `Unknown` is `#[serde(rename = "action.unknown", other)]`: any unknown intent tag
  deserializes to it (its fields are dropped) instead of failing the whole layout. This matters because AI-edited
  legacy versions, migrated into the history, still contain `action.agent.prompt` buttons. Without it, those
  versions could not be served by the pod or rendered by clients. Such an action does nothing. Kotlin already
  degraded unknown intents to `Unknown`. Layout files should never use it (rejecting it in files is a follow-up).

## D33: Clients: read-only apps API, hamburger menu and version picker, server picker in all builds

- **client-core / FFI:** `ConstructEngine::{list_apps(), app_versions(app_id, before?, limit?), app_version(app_id,
  n)}` call `GET /v1/apps...`. They are blocking, public (no bearer) and pass the pod's JSON through, in the D27
  style: own reqwest client, caller's thread, `LayoutApiError` with readable `reason`s such as "That app doesn't
  exist.", "That version doesn't exist.", "That isn't a valid app id." and "Can't reach your pod. Check your
  connection and try again.".
  - Removed: `PromptAgent` dispatch, the agent thread, `agent_json`, the catalog writes (activate/restore) and the
    old catalog reads, with their FFI exports and Kotlin wrappers.
  - Snapshot `layout_json` = `{"app_id": "<workspace>", "version": n}` from the applied
    `X-Construct-Layout-Version`, or `"{}"` when unversioned (the signed-out `pixelfed_home` screen).
    `ActiveLayout` also reads the old D27 shape.
  - `UniffiEngineBridge.close()` frees the Rust engine; later calls do nothing.
- **Kotlin bridge:** `ConstructEngineBridge.listApps()`, `appVersions(appId, before: Long?, limit: Int?)` and
  `appVersion(appId, version: Long)` are blocking (call them on `Dispatchers.IO`) and throw `LayoutApiException`.
  The interface defaults throw "Apps aren't available here.". There is also `close()`.
  - `FakeEngineBridge` serves `FakeAppCatalog`, a catalog shaped like `CONSTRUCT_SHOW_DEV_APPS=1` with a short
    history per app: Pixelfed v1..v3, Pixelfed demo v1..v2, Counter v1..v2.
- **Default view:** only the rendered layout plus a small hamburger button at the top left. It is an inset-aware
  overlay (`menu-button`, content description "Menu"). There is no status line, account text, FAB or debug chip; the
  sign-in prompts inside the layout stay.
  - Badge (`menu-badge`, a dot) when offline, when actions are pending, or when a pinned version is behind a newer
    one (`showMenuBadge`).
- **Drawer:** a Material3 `ModalNavigationDrawer` in the same root (`menu-drawer`), so headless captures see it.
  The menu state lives in `ConstructController`, so an open drawer, the picker and a half-typed URL survive
  rotation. Back closes the innermost level (custom URL field, picker, drawer). Sections:
  - Apps: rows `app-<id>` open the app, and `app-history-<id>` opens its version picker.
  - Connection (`menu-connection`): "Online" / "Offline", "N pending".
  - Account (`menu-account`): "Signed in as @x" with `menu-sign-out`, or `menu-sign-in`, which starts the sign-in
    intent the screen offers.
  - Server (`menu-server`).

  "Newer version available" (`menu-newer`, with "Show latest") sits at the top while it applies.
- **Opening another app** goes through an optional engine factory on `ConstructController` (same server and
  storage; the previous engine is closed). Android, desktop and both simulator modes provide one. Without a
  factory, the row says "Opening other apps isn't available here yet.".
- **Version picker (`version-picker`):**
  - Rows: "Latest" (`version-latest`), then versions newest first (`version-<n>`: "vN", relative time, the note or
    the source). Pages of 20 with "Load older" (`versions-more`). The version on screen is checked.
  - Selecting is **client-local**: no mutating call exists. It fetches that version's spec (cached), opens the app
    if needed, renders the spec against the current entities, and closes the drawer.
  - The pin is held by the controller (it survives rotation) but is not persisted: every launch shows the latest.
  - A pin applies only while the engine shows a versioned layout of that app. The signed-out screen is shown as
    is, and opening another app drops the pin.
  - When the pod publishes a newer version, the pin stays. The drawer shows "Newer version available" and the
    hamburger is badged until "Latest" / "Show latest" resumes following.
  - Why client-local: one person's choice of an older version must not change what other clients see, and the
    product rule allows no client writes.
- **Default workspace** `pixelfed_home` on every surface: desktop `DEFAULT_WORKSPACE`, Android `-PconstructWorkspace`,
  the simulator's `-Pworkspace`, and `android_smoke.sh`. The overrides keep working.
- **Server picker in all builds:** the drawer's Server section replaces the debug-only chip (D29).
  - Android presets: Staging, Local emulator (`http://10.0.2.2:8080`), Local USB (`http://127.0.0.1:8080`), plus
    "Build default" when that is not a preset, and a Custom URL validated as `http(s)://host[:port]`
    (`ServerUrls.parse`).
  - Android saves the choice in SharedPreferences `server` / `baseUrl` (choosing the build default clears it; the
    old `debug_server` choice is still read as a fallback) and restarts the activity with a new engine on that server's own
    storage. Precedence: the `baseUrl` intent extra (debug builds only) > saved choice > `BuildConfig`.
  - Cleartext: because the picker ships in release, the main `network_security_config.xml` now allows cleartext to
    `10.0.2.2`, `localhost` and `127.0.0.1` in **all** builds (loopback only, so a release build can reach a local
    pod over `adb reverse` or the emulator). Everything else keeps the platform default. As before (D11), this
    governs only Java/Kotlin traffic (images); engine traffic goes through Rust, and HTTPS enforcement there is a
    follow-up.
  - Desktop presets: Staging, Local (`http://127.0.0.1:8080`). The choice is saved in `java.util.prefs`, node
    `org/theconstruct/desktop`, key `baseUrl` (choosing staging clears it), and the switch happens live: a new
    session on that server, then the old engine is closed.
  - Desktop precedence: `-Dconstruct.baseUrl` > `CONSTRUCT_BASE_URL` > saved choice > staging. An override only
    decides the launch server; a menu pick still switches the current run.
  - Storage per server as in D29 (`~/.construct/servers/<url>` for anything but staging). The engine is closed on
    window close.
- **Simulator commands:**
  - `open-menu`, `close-menu`
  - `open-app <id|title>`
  - `open-history <id|title>`
  - `select-version <n|vN|latest>` (presses "Load older" as needed)
  - `set-server <staging|emulator|usb|http(s)://host[:port]>`: the choice is recorded and shown in the menu, but
    the simulator keeps its engine; recreating the engine in live mode is a follow-up.
  - `publish-version <note>` (fixture mode only): the fake pod publishes a new newest version of the app on screen,
    with a "What's new" banner.

  Each command waits for idle and for the apps calls, and fails with a hint when its control is missing. The D30
  browser commands and the agent controls are gone. `-Pworkspace` defaults to `pixelfed_home`.
