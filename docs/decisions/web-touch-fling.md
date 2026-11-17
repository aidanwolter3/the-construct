# A touch scroll in the web client flings as the apps do

Date: 2026-11-17. Related: [web-client](web-client.md), [web-gpu](web-gpu.md), `scroll-physics` (the renderer's
fling and stretch).

On a phone's browser the web client moved its content with the finger and stopped dead when the finger lifted: no
momentum. The desktop and Android apps fling with the renderer's own physics (`construct_render::scroll`: a
least-squares velocity of the finger's last 100 ms, Android's `OverScroller` curve).

Decision: **the web client tracks a touch drag with `scroll::VelocityTracker` and, when the finger lifts, runs a
`scroll::Fling` at its speed, a frame at a time, until it ends; a finger landing on a fling still moving faster than
`scroll::CATCH` stops it and is no tap.**

- Samples are stamped with each event's own time (`Event.timeStamp`), not when it is handled: while a frame draws,
  touches queue, and stamping them on arrival makes a steady flick look like stops and starts (no fling at all).
- The pointer is captured on touch-down, so a drag that leaves the canvas stays one drag; `pointercancel` drops it.
- The mouse wheel is left to the browser and the operating system, which already glide a trackpad's scroll.

Not yet: the stretch at a list's ends (`scroll::Stretch`).
