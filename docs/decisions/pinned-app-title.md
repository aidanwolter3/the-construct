# The app title is pinned at the top; `title_scrolls` lets a feed scroll it away

Date: 2026-11-04. Related: D34, [sticky-nodes](sticky-nodes.md),
[named-vocabulary](named-vocabulary.md).

The workspace title was the first thing in the scrolling view. With `core://Sticky` (decision
`sticky-nodes`) the search box and chips of Recipes, Watcher and custom apps (a log with filter
chips) pinned 56 dp below the top while the title above them scrolled away, so the pinned part
floated under the menu and bell buttons with no name. Most apps read better with their name kept
in view, like the bar of a pushed screen. A feed (Pixelfed) is the exception: there the screen
space matters more than the name.

Considered:

- Pin the title whenever the app has sticky content. No new vocabulary, but the layout could not
  say what it wants, and an app without sticky content could never pin its title.
- A `title_pinned: true` opt-in. Custom apps and every built-in except Pixelfed would have to set
  it, and forgetting it brings the floating chips back.

Decision: the client pins the workspace title in a bar at the top, beside the menu and bell
buttons; only the slots scroll, and `core://Sticky` content pins right under the title. A workspace
sets `"title_scrolls": true` (feature `title_scrolls`) to get the old behaviour back: the title is
the first item of the scrolling view and sticky content pins 56 dp below the top.
`layouts/pixelfed/0010.json` is 0009 with `title_scrolls`.

Clients built before this always scroll the title; the pod keeps serving them `pixelfed`
0009, and Recipes, Watcher and custom apps look as before on them until they update. This
replaces the sentence of `sticky-nodes` that the recipes title "should scroll in first".
