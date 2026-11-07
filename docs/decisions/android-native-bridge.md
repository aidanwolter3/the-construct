# The Rust Android app reaches Android through a thin Java bridge

Date: 2026-11-07. Related: [rust-render-engine](rust-render-engine.md),
[android-app-link-sign-in-redirect](android-app-link-sign-in-redirect.md).

The Rust Android app is a NativeActivity: Rust draws everything and calls Android through JNI. Some of Android only
calls in through Java: a link that opens the app (`onNewIntent`, the sign-in redirect), an activity's result (the
photo picker), a broadcast or service started while the app is closed (push), and an input connection for the
keyboard. A NativeActivity has no code that receives them. Considered: no Java at all (GameActivity's prebuilt
classes for the keyboard, the app's own photo grid, polling the intent on resume, and no push while closed), and a
small bridge of our own. A prototype of the bridge (a link, the photo picker, a message while closed posting a
notification) worked on a real phone.

Decision: a bridge in `crates/construct-render/android/java`, compiled by `scripts/android_native.sh` (`javac`,
`d8`) into the APK. Its classes only forward: each Android callback becomes a native call into Rust
(`Native`, implemented in `src/bridge.rs`), and each thing Rust asks of Android that must run on the main thread
or needs an activity method is a small public method (`pickImages`, `askPermission`). No app logic, state or UI lives
in Java; a new need adds a forwarding method or class, never behaviour. Events queue in `bridge.rs`, wake the
window and reach it through `Platform::events`.

The platform interface (`platform.rs`: `Platform`, `PlatformEvent`, `Reopen`) is what each platform implements:
Android (`android.rs` with the bridge), the desktop viewer, and later a web build, so the engine and the window
don't change per platform.
