# `core://Box` draws dashed borders, and the field doesn't gate the layout

Date: 2026-11-15. Related: D44, [named-vocabulary](named-vocabulary.md), [layout-features](layout-features.md).

The Pinned ingredients screen wants rows already on the grocery list drawn with a dashed outline, so they read
as "already handled". `core://Box` only drew a solid `border_color` outline; the rows used a muted background
instead.

Considered, for how the field is negotiated:

- Gate it like every other field: `required_features` lists `box_border_style` and a client without it is
  served an older version of the layout. A dashed outline is decoration: falling back to last week's layout (or
  "update required") to avoid drawing a solid line instead of a dashed one costs far more than it saves.
- Let the pod rewrite the layout for older clients. A second code path for a change an older client already
  makes on its own.
- Not gate it. Clients parse nodes ignoring fields they don't know, so an older client reads the box without
  `border_style` and draws the outline solid: what the layout looked like before.

Decision: `core://Box` gets `border_style: Solid | Dashed` (`Solid` by default, not written), feature
`box_border_style`. Its feature file exists and this build lists it in `x-construct-features`, but
`required_features` doesn't add it: a layout using it is served to every client, and one without the feature
draws a solid border. The renderer draws dashes of about 4 pt with 3 pt gaps by arc length round the rounded
rectangle (`QuadInstance::border_dash`, the same maths in `cpu.rs` `dash_mask` and `quad.wgsl`), stretched so
a whole number fit the perimeter and there is no short dash where it closes.

This is for fields an older client can safely drop, where its fallback is what the layout drew before. A field
whose absence changes what a layout does or means (an intent, a filter) still gates the layout.
