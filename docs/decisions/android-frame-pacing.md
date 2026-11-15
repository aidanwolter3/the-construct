# Android asks for the display's top rate and gives the scheduler each frame's deadline

Date: 2026-11-15. Related: [rust-clients](rust-clients.md), [android-native-bridge](android-native-bridge.md).

Frame diagnostics (`perf`) from a phone scrolling the Construct Tasks list showed two things. A second into the
scroll the display dropped from 120 Hz to 60 Hz: Android's adaptive refresh can't tell that a NativeActivity's
surface is animating, so it lowers the rate. And each frame's own work took 3.5–6.5 ms. A benchmark of the same
kind of screen through the same renderer (`tests/integration/bench.rs`) takes 0.25 ms a frame on a desktop core,
every stage (scene, glyph prepare, submit) alike, scene building included, which is plain Rust. A phone's fast
cores aren't 15–25 times slower than that, so the drawing thread was most likely running on a slow core at a low
clock: the load looks light to the scheduler, which picks cores and clocks by load, not by deadlines. Making the
renderer's own code faster wouldn't help: it is already a small part of the frame.

Decision: the Android platform sends the system two hints (`pacing.rs`, through `Platform::frame_drawn` after
each frame is presented):

- **The surface's frame rate**: `ANativeWindow_setFrameRate` (Android 11) with the display's highest refresh rate
  (`Display.getSupportedModes()`), compatibility default, once per window. The system only counts the vote while
  the surface gets new frames, so an idle app doesn't keep the display fast. The default change strategy switches
  only where the switch is seamless (no blank flash).
- **Performance hints**: an `APerformanceHint` session (Android 13) for the drawing thread, with one frame at that
  rate as its target, and each frame's work (scene, render, present) reported, so the scheduler gives the thread
  the speed it needs to finish in time.

Both are looked up with `dlsym` in `libandroid.so`, not linked, so the app still starts on Android versions
without them; logcat says once what was set (`pacing: …`). The `perf` line also splits `render` into `prepare`
(glyphon's glyph vertices) and `submit` (encoding and submitting to the GPU) and counts layers, to tell renderer
work from driver work in the next capture.

Not yet checked on a phone: whether the rate holds at 120 Hz while scrolling, and how far frame work drops. The
desktop viewer gets neither hint (desktops don't lower the rate under an animating window).
