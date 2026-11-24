# Photos page on taps, and a scatter plot draws thousands of points

Superseded in part by [scatter-draws-as-one-run](scatter-draws-as-one-run.md): how the scatter draws.

Date: 2026-11-24. Related: [swipe-cards](swipe-cards.md), [taste-deck-and-map](taste-deck-and-map.md),
[taste-data-source](taste-data-source.md), [rust-clients](rust-clients.md), [web-client](web-client.md).

The taste deck ([taste-deck-and-map](taste-deck-and-map.md)) had to fit two things the vocabulary couldn't draw. A
card's photos were browsed by a tap that only went forward (one `core://Conditional` per photo, the index set on
the device), because a `core://Carousel` pages on sideways swipes, which belong to the `core://SwipeCard` around it.
Each photo was only asked for when it showed, so paging blinked while it loaded. And the taste map was a grid of
tiles: the user asked to see the whole catalog as a 2D map, zoom into it, open any piece, and select any area of
it. A tile grid answers "this tile" but not "this piece" or "this area". Considered: a chart node with axes (more
than a map of a learned space needs), and points as one entity each (tens of thousands of entities per screen
load).

Decision:

- **`core://Carousel` `tap_paging`** (feature `carousel_tap_paging`): a tap on the left third shows the previous
  page and on the right third the next, wrapping round; `on_tap` runs on the middle third. Bars over the top of the
  page show where it is. It doesn't page on swipes, so a sideways drag goes to what is around it. A client without
  the feature gets the core layout's version before it (the deck with forward taps).
- **No blank page:** every image of every carousel a layout lays out (the pages not showing, and the card behind a
  deck's top one) is asked for as soon as it is laid out, before the images drawn; a turn to a page whose images
  haven't loaded and been cut to size waits, the current page showing, until they have or
  `PAGE_WAIT` (1.5 s) has passed. The pod's image proxy keeps what it fetched for an hour (64 MB at most, every pod
  of a server), so a photo seen again isn't fetched from its site again, and the source asks for every photo at
  one width (1080 px), on a card and on a piece's screen alike, so one download serves both.
- **`core://Scatter`** (feature `scatter`): `points` is one string, the base64 of 6 bytes a point (x and y as
  u16 across the unit square, a value byte along a three-colour ramp or none, a mark byte: plain, highlighted,
  muted), `protocol::scatter`: 32,000 points are 190 KB before base64, one entity field. It draws them in place,
  zoomed by pinch or wheel and panned by a drag (kept among the screen's offsets, so moving it lays nothing out).
  A tap near a point runs `on_tap` with `$point.index`, `$point.x`, `$point.y`; while `select` holds, or after a
  long press, a drag draws a box whose release runs `on_select` with `$select.x0` … `$select.y1` and `$select.box`
  (`x0-y0-x1-y1` in millionths, a route segment). It knows nothing of taste: any source with points in a square
  can use it.
- **The taste source feeds it from the service's whole map** (`/api/map`: ids, places, likes, skips and the model's
  chances), kept by the pod for 30 minutes and asked for again 15 s after a reaction; a reaction marks its point
  at once. `/v1/taste/screens/scatter` answers `taste://maps/catalog`; `taste://points/<n>` opens a point's piece
  (`/v1/taste/screens/point?n=`), its lists keyed by the number; a box opens `taste://area/<box>`.
- **A reaction to a piece the service no longer has** (404 or 410: its catalog was rebuilt under the kept deck)
  takes the piece out of the deck and answers success with `gone: true`, so the device doesn't put the card back.

Consequences: a client build is needed for the new layout version (`taste/0003`); older clients keep drawing
0002. The Android bridge is unchanged (touches reach the renderer as before). The scatter is drawn as quads by both
renderers (the CPU one in the simulator); tens of thousands of points draw in a few milliseconds.
