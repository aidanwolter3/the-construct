# A carousel's page follows its list item, not its place in the list

Date: 2026-11-24. Related: [scatter-and-tap-paging](scatter-and-tap-paging.md),
[swipe-cards](swipe-cards.md), [images-never-blink](images-never-blink.md).

Swiping the taste deck, the owner saw the card underneath show one photo while it waited, then jump to another
once it came to the top. A `core://Carousel` keeps the page it shows (and its track's offset) under its id, and a
carousel without a `tag` took its path for an id. A list cell's path holds its index (`…/i0`), so the deck's
next card, at `…/i1` while it waited, became `…/i0` on top and took the page the card that left had been tapped
to: its third photo instead of its first. A card never tapped kept page 0 either way, which is why it showed only
after tapping through photos.

Decision:

- **Item-keyed paths.** The builder knows the list cell it is in: the cell's path, and the same path with the
  index replaced by the cell's item (`…/[<item uri>]`).
- **A carousel without a `tag`** takes the item-keyed form for its id, so its page moves with its item when the
  list's order changes (a deck's top card leaving, a list re-sorted) and a card that comes to the top shows the
  page it had, its first unless it was paged. A `tag` still names it outright.
- Other view state keyed by path (scroll offsets, panels) keeps its keys; nothing else had the problem.

Consequences: a client build (renderer only; no vocabulary, layout or pod change). `the_next_card_keeps_its_photo_
when_it_comes_to_the_top` reproduces it (it failed with the next card on `b2.jpg` instead of `b0.jpg`).
