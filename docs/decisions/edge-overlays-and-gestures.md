# Overlays come from any edge, and any node can take gestures

Date: 2026-11-06. Related: [layouts-not-apps](layouts-not-apps.md), [named-vocabulary](named-vocabulary.md),
[screens-inside-frames](screens-inside-frames.md), issue #123.

A layout couldn't open the inbox as a sheet sliding down from the top, and couldn't react to a swipe. Overlays were
two fixed kinds, `Sheet` (from the bottom) and `Panel` (from the start), and the only triggers were taps and a text
field's submit. A one-off `TopSheet` presentation and a `core://Swipe` node would have worked, but each new
direction or gesture would then need its own name, against a small, generic vocabulary.

Decision: two generic primitives, each a named feature.

- **Edge overlays** (feature `overlay_edge`): presentation `Overlay` with `edge: top|bottom|start|end` (bottom when
  unset) and an optional `size_dp` (its height from the top or bottom, its width from the start or end), on
  `core://Layout` and `action.navigation.open`. It slides in from its edge (`animation` fade or none instead), and
  dragging it back toward that edge dismisses it, for every edge: a sheet by its handle (on its inner side) or by
  its content once that can't scroll further, a panel anywhere. `Sheet` is an overlay from the bottom and `Panel`
  one from the start: both stay on the wire unchanged, so existing layouts and older clients are unaffected, and
  they keep their old animations. The client draws all four with one frame (`OverlayFrame`, `Layouts.kt`).
- **Gestures** (feature `gesture`): `core://Gesture {content, on_tap?, on_long_press?, on_swipe_up?,
  on_swipe_down?, on_swipe_left?, on_swipe_right?}`, each an ordinary intent, at least one set (the pod refuses a
  gesture without one). A wrapper node rather than a field on every node: it touches one renderer arm, not every
  node type. `core://Tappable` stays as the tap case. A swipe is a drag of at least 48 dp, mostly one way, and runs
  its intent when the finger lifts. It counts the movement nothing inside used and the scroll the content had no
  room for, so a page inside still scrolls and pulling it down at its top is a swipe down. A drag an outer scroll
  takes is not a swipe, so a gesture belongs around what scrolls, not inside it. Left and right are on screen,
  not start and end.

These compose with what exists: a `core://Layout` with `show_when: $form.inbox` shows while that form field is set,
so a button, a swipe or a notification can all open the same overlay. Home opens the inbox from the top on a swipe
down with a gesture around its content whose `on_swipe_down` is `action.navigation.open {layout: inbox,
presentation: Overlay, edge: top}`.

Not yet: **pull to reveal** (an overlay dragged open under the finger from its edge, instead of a swipe firing on
release) is a separate issue; on Android a pull from the very top edge opens the system notification shade, so a
top pull must start below the status bar. The swipe feel and conflicts with Android's system gestures (Back from
the side edges) need a check on a phone.
