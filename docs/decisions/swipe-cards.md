# Swipe cards follow the finger, and a list can be a deck

Date: 2026-11-14. Related: [edge-overlays-and-gestures](edge-overlays-and-gestures.md),
[named-vocabulary](named-vocabulary.md), [plumbing-and-porcelain](plumbing-and-porcelain.md).

A deck to swipe through (like or pass, one card at a time) was built from a `core://Gesture` around each item of a
list: `on_swipe_left` and `on_swipe_right` run only when the finger lifts, and nothing moves while it drags, so the
swipe feels dead, and every card of the list shows at once.

Decision: two named features.

- **`core://SwipeCard`** (feature `swipe_card`): `{content, on_swipe_left?, on_swipe_right?, threshold_dp?,
  content_description?}`, at least one side set. A node of its own rather than a mode of `core://Gesture`: a card
  that moves under the finger is a different thing from a handler that fires on release, and a gesture's other
  handlers (tap, long press, up, down) don't fit a card that is thrown aside. While a sideways drag that started on
  it lasts, the card moves with the finger (the offset is one more scroll offset, `card:<id>`, so the frame is only
  placed again, not laid out). Past its threshold (default 96 dp) it is washed with the colour of the side it would
  go (theme `success` right, `error` left; lighter on the way there). Let go past the threshold, or flung that way
  past half of it, it flies off the screen and only then runs that side's intent, and is put back in its place,
  where the next card of a deck now is; let go before, it springs back with a little overshoot. A swipe that isn't
  a drag (a screen reader's, the simulator's `swipe-left`) runs the side's intent at once, as a gesture's does.
- **`core://List` `stack`** (feature `list_stack`): a deck. Only the first item is drawn, with the second behind it,
  as tall as it and a little narrower, peeking out 10 dp below, so the deck shows one card at a time and the next is
  there as the top one is dragged away. A list rather than a new node, so a deck keeps `where`, `filter`, `empty_text`
  and the item reuse of lists. The cards' intents take the item out of the list (a verdict field the list's `where`
  filters on), so the next card comes up; a deck has no `columns` or `group_by`.

In a deck the card takes a sideways drag before a carousel on it does, so photos in a card are paged by their dots,
not by dragging.

Not yet: the **slight rotation** a dragged card often has: the renderer has no transforms (quads, text and images
are drawn axis-aligned on both the GPU and the CPU), and a rotation would be one for every draw. **Mouse dragging**
on desktop moves no card (as it pages no carousel). The feel (threshold, fling speed, timing) needs a check on a
phone.
