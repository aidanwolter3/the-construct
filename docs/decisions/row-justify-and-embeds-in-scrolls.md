# Rows and columns place their children along their axis; inline layouts in a scroll size to their content

Date: 2026-11-15. Related: D44, [layouts-not-apps](layouts-not-apps.md), [named-vocabulary](named-vocabulary.md).

A row of fixed-size tiles (four 80 dp icon tiles on a home) couldn't be centred: `align` on `core://Row` is
cross-axis only. Layouts faked it with an unsized `core://Spacer` at each end, which works until the tiles, their
spacing and the padding are wider than a phone, and then the row overflows. Separately, an inline `core://Layout`
without `height_dp` in a scrolling column was given the room the screen had left, not its content's height: a home
whose page ran past the screen showed one card of an embedded layout, so the embed got a fixed `height_dp`, which
leaves a gap on taller screens.

Decision:

- **`justify`** (feature `justify`) on `core://Row` and `core://Column`: `start` (the default), `center`, `end`,
  `space_between`, `space_evenly`, placing the children along the node's own axis. Lowercase, as the newer enums
  (`edge`, `animation`) are. The renderer gives the children automatic margins rather than a flexbox
  `justify-content`: automatic margins take the room left and are nothing when there is none, so children that
  don't fit start at the padding whatever `justify` says (a centred flexbox would hang them off both ends, out of
  reach). A column with `justify` fills the height it is given (as a filling child does), since a column only as
  tall as its children has nothing to place them in. An unsized `core://Spacer` in the row still takes the room
  first.
- **Inside a scroll's content, filling children take at least the room short content leaves and otherwise their
  content's height.** A scroll's content has no height to share: a child that fills height there (an inline layout
  without `height_dp`, a filling box, a column with `justify`) grows into what is left when the content is shorter
  than the view, as before, and is as tall as its own content when it is longer, instead of being squeezed into the
  room left. A `height_dp` (on a box or a layout) gives its content a fixed height again. Outside a scroll nothing
  changes: a layout as a workspace's one slot still fills the screen.

`justify` is new vocabulary, so clients need a new build; a pod serves layouts using it only to clients that list the
feature. The embed sizing is a renderer change only: older builds keep the old sizing, so a layout should keep its
`height_dp` until its devices are updated.
