# The taste feed is a deck, and the taste map is tiles

Date: 2026-11-23. Related: [taste-data-source](taste-data-source.md), [swipe-cards](swipe-cards.md),
[device-owned-local-state](device-owned-local-state.md).

The owner wanted the taste feed to work like a dating app: one piece at a time, swiped right to like and left to
skip, its photos browsed by tapping, its description readable without scrolling, and a liked piece gone from the
screen like a skipped one. They also wanted to see their taste on a two-dimensional map and look at any area of it.
The taste service changed with it: it now looks for the edges of their taste (pieces it can't tell they'd like)
rather than serving what they most likely like, orders each item's photos with the ones of someone wearing it
first, and serves a map of its catalog.

Two parts don't fit today's vocabulary: tapping the left or right half of a photo to go back or forward (no node
can lay tap zones over an image or tell where a tap landed), and a scatter of the catalog to drag a selection on
(no plot or canvas node). Each needed a client build; the owner chose not to need one: a tap shows the next photo,
wrapping, and the map is a grid of tiles to zoom into.

Decision:

- **A deck.** `layouts/taste/0002.json` puts `taste:feed` in a `stack` list of `core://SwipeCard`s: right likes,
  left skips (`/v1/taste/react`), and the list's `where` drops a piece once it is liked or skipped, so it leaves the
  screen at once. The pod's `taste:feed` holds only the pieces not reacted to yet.
- **The deck stays stocked.** After a reaction that leaves fewer than six pieces waiting, the pod fetches the next
  page in the background (one fetch at a time per pod, skipped if another already filled it) and sends a data event.
  The pod keeps the newest 60 reacted pieces, for the reasons lists borrow, and drops older ones.
- **Photos by tapping, with no arithmetic in the layout.** The pod serves `image_0`..`image_5` and, for each,
  `next_<k>`, the photo after it wrapping to 0. A card has one `core://Conditional` per photo on the device-only
  field `photo` (`not` for photo 0, `equals: "k"` for the rest); its tap is `action.local.set` of `photo` to
  `entity.next_<k>`. `photo` is never served, so a reload starts at the first photo.
- **On screen without scrolling.** On a phone the photo is 4:5 with everything under it (three lines of
  description), and the card ends well above the bottom of a 390 × 844 screen; on a wide window
  (`local://device#size_class` `expanded`) a second card template puts the photo on the left, 420 dp wide.
- **The map is tiles.** An area of the service's map is named by its box in millionths, `x0-y0-x1-y1`, which a
  route can carry. `/v1/taste/screens/map?area=` answers the area cut into 4 × 4 tiles, each with the service's
  tint, a photo, its counts and `opens`, worked out by the pod: a finer map of the tile, or its pieces
  (`/v1/taste/screens/area?area=`) when it holds six or fewer, or nothing for an empty tile. Zooming in is pushing
  the same screen for a smaller area; Back zooms out.
- **Reasons.** `why` is now `boundary`, `explore` or `likely` (On the edge, Exploring, Probably yours); the old
  values still read.

Consequences: no client build. Pods using `@core/taste` unpinned get the deck as they open; a copy of 0001 keeps the
grid. A card can't go back to the previous photo (only round again), and a map selection is a tile, not a free
shape: both would come with new vocabulary (a carousel paged by taps, a scatter node).
