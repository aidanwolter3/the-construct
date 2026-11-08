# The Rust clients are the product; the Compose clients are gone

Date: 2026-11-08. Related: [rust-render-engine](rust-render-engine.md),
[rust-clients-on-the-engine](rust-clients-on-the-engine.md), [rust-simulator](rust-simulator.md),
[android-native-bridge](android-native-bridge.md), [android-app-link-sign-in-redirect](android-app-link-sign-in-redirect.md),
D5, D8, D11. Issue #166.

Two clients drew the same layouts: the Compose UI (`app/`: the shared UI, the Android app, the desktop app and the
Compose simulator, over `client-core` through UniFFI in `client-ffi`) and the Rust rendering engine's clients
(`construct-render`: `construct-viewer` and the NativeActivity APK, on the same engine as a library). Every piece of
vocabulary was written twice, in Kotlin and in Rust, and the Rust clients had closed the gaps that kept Compose the
product (offline cache, outbox and live updates; push; upstream sign-in; media upload; keyboard input; accessibility
on the desktop; the remaining vocabulary and device commands; UI checks in `construct-sim`). Keeping both meant two
renderers drifting apart; considered keeping Compose for Android only, which still leaves two renderers.

Decision: the Rust clients are The Construct's only clients, and the Compose clients are deleted. This supersedes
"The Compose clients stay the product" in `rust-render-engine`.

- **Deleted:** `app/` (Gradle, Compose Multiplatform, the Kotlin Android and desktop apps, the Compose simulator),
  `crates/client-ffi` (nothing else used UniFFI), `scripts/build_ffi.sh` and `scripts/build_android.sh`, and CI's
  JVM and Gradle jobs. New vocabulary lands in `construct-render` (and `construct-sim`, which draws with it).
- **Android:** `scripts/android_native.sh` builds the APK with cargo-ndk, aapt2, javac and d8, no Gradle; Java only
  forwards Android's callbacks to Rust (`android-native-bridge`). The launcher icon and backup rules moved to
  `crates/construct-render/android/res`. A development build is debuggable and signed with the machine's debug
  key; `--release` builds a non-debuggable APK signed with the release key from the environment. The App Link host
  (Gradle's `constructAppLinkHost`) is `CONSTRUCT_APP_LINK_HOST`, put in the manifest by the script and compiled into
  `signin.rs` from the same variable. The package stays `org.theconstruct.android` and the engine's data and the
  keystore session keep the Compose app's formats, so an update over a Compose build signed with the same key keeps
  the sign-in.
- **macOS:** `scripts/macos_app.sh` wraps `construct-viewer` in `The Construct.app` (bundle id
  `org.theconstruct.desktop`, the Compose desktop app's) and a `.dmg`, signed ad hoc.
- **Releases:** `.github/workflows/release.yml` builds both on a `v*` tag and publishes them as a GitHub release;
  the APK is signed when the repository's `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`,
  `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD` secrets are set. CI builds the development APK on every PR.

Nothing on the wire changes. Every user reinstalls once: the release key differs from the debug key the phone's
current build is signed with, and the Mac app is a new bundle.

Not done, which the Compose clients had: TalkBack on Android (`rust-client-accessibility`, #172); the system font
size on Android; desktop notifications, the tray icon and the dock badge; notification channels created from the
pod's list (title, importance, groups) rather than on a notification's arrival; launch options to open another
layout than home (`construct.workspace`, Android's debug intent extras); x86_64 Android builds and Android 8
(minSdk 28); a Developer ID signature and notarization for the Mac app; Windows packaging and a Windows photo
picker; the desktop's old cache and outbox in `~/.construct` (the viewer keeps its own, so writes queued in the
Compose desktop app and not yet sent are not carried over); `local://device#notification_permission` (always `""`);
`publish-version` in the simulator.
