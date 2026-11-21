# A layout written for a phone draws at phone width on a wider window

Date: 2026-11-21. Related: [screen-sizes](screen-sizes.md), [wide-screen-panels](wide-screen-panels.md),
[web-client](web-client.md), [plumbing-and-porcelain](plumbing-and-porcelain.md).

In a browser on a laptop, layouts written for a phone stretched to the window or to the pane they were given:
Pixelfed's photos ran 1280 points wide and a post opened taller than the screen, and a meals list embedded beside
a home's sidebar ran 1000 points wide. Decision `screen-sizes` left it to each layout to use the size vocabulary,
but almost none do, and a user's copies of core layouts never would until edited.

Considered:

- **Add `max_width_dp` to every core layout and the user's copies.** Rejected: copies don't follow core layouts,
  every new layout would have to remember it, and an agent building a screen would have to as well.
- **An explicit opt-in field ("this layout is wide").** Rejected for now: a layout that tests the window's size
  or platform already says it was made for wide windows; a second switch could disagree with it.
- **The renderer caps a layout that doesn't adapt.** Taken. This reverses the part of `screen-sizes` that said the
  app never rearranges a layout on its own, for this one rule.

Decision: **a layout that doesn't adapt to the window draws at most 480 points wide (`PHONE_WIDTH_DP`): centred
on a whole screen, and at the start of its room in an embed or a panel, so panes build up from the left.**

- A layout **adapts** (`adapts_to_window`) when its own tree, not the layouts it embeds, has a
  `core://Conditional` on `local://device` (`size_class`, `width_dp`, `height_dp` or `platform`: a fork for the
  web), a `core://Column` with `max_width_dp`, a `core://List` with `min_column_dp`, or a `core://Layout` with
  `panels: true`. Such a layout gets the whole width as before.
- The cap applies wherever a layout is drawn: the root screen, a pushed screen, a sheet or side panel's content,
  an inline `core://Layout` embed and a panel beside one. A screen of its own (the root, a push, a sheet) is
  centred. An embed sits at the start of its room, against what is drawn before it: a home laid out for wide
  windows (it tests `size_class`) keeps its sidebar, and the layout its detail embeds draws at phone width right
  beside it, the rest of the window left empty.
- Panels (`panels: true`, decision `wide-screen-panels`) no longer share the width equally when their layouts are
  phone layouts: each such column is phone wide, next to the one before it with the hairline between, so opening a
  panel adds a column to the right of the last. Columns whose layouts adapt share what is left; short of room, the
  phone-wide columns shrink too.
- A window or pane narrower than 480 points changes nothing, so phones draw as before. A phone in landscape or a
  tablet shows a phone layout as a centred column too.
- The room beside the column is the window's background. A tap there ends typing, as a tap anywhere outside a
  field does.
- No vocabulary changes: it is renderer behaviour, so every client (desktop, Android, the web client, the
  simulator and the pod's previews) does it once rebuilt. A bare node tree drawn for a test
  (`SceneBuilder::build_from_spec`) is not a layout and keeps the whole width.

Not yet: a sheet's or bottom bar's background still spans the window while its content is capped.
