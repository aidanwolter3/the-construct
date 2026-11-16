# Nothing on screen changes during a transition; incoming changes apply on the first frame after it

Date: 2026-11-16. Related: [android-frame-pacing](android-frame-pacing.md),
[rust-clients-on-the-engine](rust-clients-on-the-engine.md), [scoped-data-events](scoped-data-events.md).

Frame diagnostics (`perf`) from a phone on 2026-11-15: opening Calendar from home five times, every open had one to
three slow frames, and most of those bursts showed `data 1` and two to four layouts (14–34 ms). Opening a layout
fetches it and its feeds again behind the cached copy (the engine's `open_layout`), and the answer lands while the
screen is still sliding in. `Session::apply_changes` then lays every screen out again on the next frame, which takes
a phone 10–20 ms, longer than a 120 Hz frame (8.3 ms). The first open after a start was worst (`data 4`, 8 layouts,
64 ms, 6 slow frames). Going back to home after a visit in which its data changed laid home out again during the
slide (12.8 and 20.4 ms). Scrolling was unaffected.

Considered:

- **A. Leave it.** The slow frames stay.
- **B. Hold everything until the transition ends.** Taken.
- **C. Apply the opening screen's own data at once and hold the rest.** It needs to know, per stack entry, what a
  screen binds, and a laid-out screen still costs a layout mid-slide.
- **D. Stop refetching a layout on every open while its cached copy is recent.** Fewer answers land mid-slide, but
  a change from elsewhere (another device, an agent) still can, and it changes the engine's caching for every
  client.

Decision: **nothing on screen changes during a transition; incoming changes apply on the first frame after it.**
A transition is a layout or screen sliding in or out (280 ms). While one runs, `Session::poll` keeps what comes in
aside: the loader's changes (merged, newer over older, `Changes::absorb`) and the pod's answers (write answers,
refusals' undo, a screen's loaded `data`). On the first frame after the slide ends it applies them all, and that
frame lays them out; `Session::animating` stays true while anything is held, so the window draws that frame.
What the user does still changes the session at once: taps, `action.local.set`, optimistic mutations and creates,
typing, and a photo picked in the platform's picker (none of these wait for `poll`).

The rule is general: other kinds of incoming change follow it as they are added. Swapping a whole new layout
root (issue #295's content-addressed construct) is the next: it waits for the end of a transition too, as well as
for a touch or the keyboard to finish.

Limits: a screen whose data loads in the background (`local://screen#loading`) keeps its spinner until its slide
ends, at most 280 ms longer. An engine snapshot taken before a user's write during the slide could briefly put the
old value back after it; the engine's own copy has the write (`Loader::local_set`, its outbox), so its next
snapshot brings the new value. Carousel pages, swipe cards and a sheet settling after a drag are not transitions
here: only the stack's slides are.
