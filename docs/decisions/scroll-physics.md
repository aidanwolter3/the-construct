# Scroll physics live in one tested module, timed per burst of input

Date: 2026-11-11. Related: [rust-clients](rust-clients.md), [edge-overlays-and-gestures](edge-overlays-and-gestures.md).

On Android a short, slow drag often flung the list all the way to its end. The fling curve was right (Android's
`OverScroller`); its input was not. winit's Android backend drops each touch event's own time and hands the app
every move queued while a frame drew in one loop, so the window stamped them with `Instant::now()` microseconds
apart. The release velocity (Android's impulse estimator, fed one sample at a time) divided real movement by those
gaps and read thousands of points per second; the fling went at the 8000 cap. None of it could be tested: the
velocity, the fling and the gesture lived inside the winit event handler.

Decision: the physics are a module, `construct_render::scroll`, that reads no clock (every call takes the time in
seconds), so whole gestures replay in unit tests. The window only feeds it events and applies what it returns each
frame. What it does, following Android's and Flutter's sources:

- **Release velocity**: a least-squares quadratic over the last 100 ms (`LSQ2`, both platforms' default for touch),
  at least 3 samples or no fling (Flutter), 20 kept. A gap of over 40 ms, inside the drag or before the lift, means
  the finger stopped: what came before it is dropped, and a lift after it does not fling. **Bursts**: samples less
  than 4 ms apart (real touch is at most 240 Hz) are one sample, the last position at the last time, so a burst's
  movement is measured over the time between bursts, which is when it happened.
- **Fling**: Android's curve in Flutter's closed form (`ClampingScrollSimulation`): Android's distance, starting at
  exactly the release speed so the content doesn't change speed as the finger leaves. 50 to 8000 points per second.
- **Stretch at the ends** (Android 12's `EdgeEffect`): pulling past an end stretches the content instead of moving
  it, by the pull as a fraction of the view's height (at most 1), with Android's intensity curve (linear plus one
  that saturates by a third of the height). Pushing back undoes the stretch before scrolling. Letting go springs it
  back (natural frequency 24.657, damping 0.98, the last 8 points at 200 per second); a finger landing catches it. A
  fling that meets an end stops there and throws 13× its speed into the spring: a short bump. A view whose content
  fits doesn't stretch, so a pull on it stays a swipe. Android stretches with a GPU shader, strongest at the pulled
  edge; this renderer has no transforms, so the view's content (`stretch:<id>` among the scroll offsets) is drawn
  scaled along the axis from the pulled end, boxes and images taller, text and icons moved but their size kept, up
  to about 10% at a full pull (`STRETCH_GAIN`, matched by eye, not from source).
- **Catching**: a finger landing on a fling still faster than 50 points per second stops it and is not a tap or a
  long press (Android's `ScrollView`, Flutter's `IgnorePointer` while flinging, Firefox's 0.05 px/ms threshold);
  on the last slow crawl a tap goes through.
- **Wheel**: a notch is 64 points (Android's scroll factor) and glides there, easing out over about 120 ms; notches
  add to the glide, reversing drops the rest. A trackpad's pixel deltas apply as they come, and the window never
  flings them: macOS sends its own momentum after the fingers lift.

Mobile and desktop differ by input device, not by platform: touch flings with Android's curve wherever it comes from
(Android is where touch is), the wheel glides, the trackpad follows the OS. The other choices considered:

- **iOS-style physics** (exponential decay at 0.998 per ms, rubber-band overscroll that moves the content): longer,
  floatier flings. On an Android phone they'd feel foreign.
- **The events' own timestamps**: the right fix, but winit doesn't expose them, and the Java bridge doesn't see
  touch (NativeActivity). Burst coalescing gets the same velocity without them.
- **Fling stacking** (successive flicks speed up, iOS): Android's stock views don't (a touch aborts the fling).

Not yet: smoothing a burst's moves across the frame (a drag's scrolling is applied once per frame), and the stretch
on horizontal carousels and for the mouse wheel or trackpad.
