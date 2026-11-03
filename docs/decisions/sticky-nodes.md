# `core://Sticky` pins content to the top while its parent scrolls (schema 11)

Date: 2026-11-03. Related: D34, D36, D43, D44.

The recipes list has a search box and category chips above about 80 recipes. Scrolling down the list
scrolled them away, so changing the search or the category meant scrolling back to the top. Apps need
a way to keep some content in view while the rest scrolls, without the client knowing any app.

Considered:

- A `sticky: true` flag on `core://Column` / `core://Row`. Every container would grow a field that
  only matters at one level of the tree.
- A fixed `header` on the workspace or a screen, next to the existing screen `footer`. Simple, but the
  header could never scroll in first (the recipes title should scroll away, then the search box pin),
  and a slot could not pin something in the middle of the page.
- A wrapper node, like `core://Tappable` (D42). Any content can be pinned, and the rule is the
  familiar CSS `position: sticky` one.

Decision: a new wrapper node, `core://Sticky {content}` (schema 11, additive). Its content scrolls
with the page until it reaches the top of the scrolling view, then stays pinned there while the rest
of its parent scrolls under it; when the end of its parent reaches it, it scrolls away with it. While
pinned it is drawn on the page background with a hairline under it, so what scrolls under it is
hidden. In the workspace it pins 56 dp below the top, clear of the menu and bell buttons drawn over
the corners (the background fills that band too); in a screen it pins right under the screen's bar.
The pod rejects a `core://Sticky` inside a `core://List` or `core://Carousel` template or inside
another `core://Sticky`, where pinning would mean nothing. `layouts/recipes/0005.json` pins the
search box and the chips together.

The client moves the content during placement (`Modifier.offset { }` reading the scroll state), not in
a graphics layer: it stays in step with the scroll in the same frame and taps land on the pinned
content. Old clients never see the node: the pod serves them `recipes/0004`.

Not done: several sticky nodes in one parent stack on top of each other rather than pushing the
previous one away (iOS section-header style); none of today's layouts need that.
