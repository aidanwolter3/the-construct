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
  negation); `entity.<field> + N` and `entity.<field> - N` (integer arithmetic). Anything else
  logs a warning and changes nothing.
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
  lets fixture runs exercise real interactions without Rust or a server. Live mode uses the real JNA engine
  with temp storage.
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
  them with `adb logcat -s CanvasEngine:V`. Desktop builds don't compile this module.

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
  Extras are read only from the intent that creates the engine (see lifetime).
- **Cleartext:** only debug builds allow HTTP, and only to `10.0.2.2`, `localhost` and `127.0.0.1`
  (`src/debug/AndroidManifest.xml` sets `android:networkSecurityConfig`, `src/debug/res/xml/network_security_config.xml`).
  `localhost`/`127.0.0.1` cover a physical phone with `adb reverse tcp:8080 tcp:8080`. Release builds keep the
  platform default (cleartext blocked), so they need an HTTPS server or a config change.
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
