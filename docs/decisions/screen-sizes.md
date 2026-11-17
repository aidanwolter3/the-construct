# One layout fits every window: size as device data, max widths and automatic columns

Date: 2026-11-17. Related: [named-vocabulary](named-vocabulary.md),
[plumbing-and-porcelain](plumbing-and-porcelain.md), [content-addressed-construct](content-addressed-construct.md).

Layouts were written for phones. On a tablet, a desktop window or (soon) a browser they stretched edge to edge: the
Events cards ran 1280 points wide, and nothing in the vocabulary knew the window's size, so a layout couldn't do
better.

Considered:

- **The pod serves a layout per size class.** Rejected: home's hash is the whole construct's, so each class would
  fork the root, and every edit would be made several times.
- **The app rearranges layouts on wide windows** (a pushed layout opens beside home, panels dock). Rejected: it
  changes how every existing layout draws, needs per-client switches while it is new, and puts presentation
  policy in the renderer, which draws plumbing.
- **The layout says how to use the room, in plumbing, with porcelain for the common patterns.** Taken.

Decision: **one layout serves every window and adapts when drawn, only as it asks.**

- **Plumbing** (each a named feature, so clients without it get older versions):
  - `device_size`: `local://device` gains `width_dp`, `height_dp` and `size_class` (`compact` under 600 points,
    `medium` under 840, `expanded` from 840, the common phone, tablet and desktop breakpoints). The session writes
    them from the window on each frame, so a resize changes them. A layout uses them with `core://Conditional`.
  - `max_width`: `core://Column` `max_width_dp`, at most that wide and centred (auto margins; down a column it
    takes the whole width up to the limit).
  - `list_auto_columns`: `core://List` `min_column_dp`, as many equal columns as fit at least that wide, drawn
    as a grid with `repeat(auto-fill, minmax(min, 1fr))` (taffy's grid, now built), so the count follows the width
    the list is actually given (a pane, not just the window). Group headers and the empty text span every column.
    It replaces `columns` where set; a deck can't have it.
- **Porcelain**: `@core/two_pane` puts its first child (a list) beside its second (the detail) on an `expanded`
  window and shows the list alone otherwise, from `core://Box` `width_dp`, a filling box and `core://Conditional`:
  no new node (its narrow branch uses `not`). The rows' own intents choose between selecting (wide) and pushing (narrow), because a link inside a
  component template isn't an edge of the construct (links are made references when a layout is written, before
  devices expand components).
- No blueprint uses the new vocabulary yet; blueprints adopt it in new layout versions when we choose. Until
  then nothing an existing pod draws changes, on any client.

A row's narrow branch is a `core://Conditional` with `not: true` (feature `conditional_not`), so it needs no
separate `compact` and `medium` branches.

Not yet: `two_pane` highlighting the selected row; docked side panels.
