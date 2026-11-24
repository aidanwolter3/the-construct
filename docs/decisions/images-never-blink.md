# An image that has shown never blinks back to its placeholder

Date: 2026-11-24. Related: [scatter-and-tap-paging](scatter-and-tap-paging.md) (its "no blank page"),
[swipe-cards](swipe-cards.md), [taste-deck-and-map](taste-deck-and-map.md).

Swiping a taste deck's top card away, the owner saw the card that took its place show its photo, blink blank, and
show it again. The card behind a deck's top one is laid out [`STACK_INSET`] narrower each side (decision
`swipe-cards`), so its photo was cut to that size; on top it is wider. An image is cut for each pixel size it is
drawn at, and the GPU path drew an image only once cut at exactly that size (the cut is made on a background thread
so a frame never waits for it), so the new top card drew its placeholder until the wider cut was made. The CPU
renderer cuts on the spot and never showed it.

Decision:

- **A cut at another size stands in.** When a loaded image's cut at the size it is drawn at isn't made yet, the GPU
  draws a cut of it at another size (the nearest, with the same corners if there is one), scaled to the box with
  the nearest pixel, under a glyph id of its own (`ImageStore::stand_in_glyph_id`), so the atlas never keeps the
  stand-in in place of the exact cut, which replaces it the frame it is ready. Only an image never cut at any size
  (drawn for the first time) shows its placeholder.
- **A deck's next card is cut ahead at its size on top.** The prefetch (`Session::prefetch`) also cuts the first
  page's photos of the card behind a deck's top one at the size they'll have on top: `STACK_INSET` wider each side,
  and both as tall as now (a card stretched to the deck's height) and the same shape (one whose height follows its
  width), since which applies depends on the layout. So the swipe usually finds the exact cut made, and the stand-in
  covers the rest.
- **The narrower card behind stays** (decision `swipe-cards`): the fix is in how images are drawn, not the deck's
  look.

Consequences: no blank frames when a drawn image's size changes (a deck's card taking the top, a window resized, a
panel opening), at the cost of a frame or two of a slightly softer photo while the exact cut is made, and two more
small cuts per deck card. It is a renderer change: it needs new desktop and Android builds; the web client already cut
on the spot.
