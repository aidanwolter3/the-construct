# Charts colour series from the theme, and merge equal neighbours

Date: 2026-11-20. Related: [device-versions](device-versions.md), [custom-theme-tokens](custom-theme-tokens.md),
[dark-palette](dark-palette.md).

The device chart's builds were six shades of one purple, chosen by the `monitor` source: too close to tell apart,
the same in light and dark mode, and changeable only by a server change. Each device was also its own segment, so
a bar of five devices on one build was five blocks.

Considered: a palette or colour map on `core://Bars` (a client build, and one chart's palette, not the theme's);
merging in the feed (it would group items across the `where` filter, so the Phone/Mac/Web chips would no longer
filter the chart).

Decision:

- **Chart colours are theme tokens**: `chart_1` to `chart_8` and `chart_other`, each with a light and a dark value
  (blue, orange, aqua, yellow, magenta, green, violet, red, grey; an order whose neighbours stay apart for
  colour-blind eyes, checked against the light and dark card). A pod changes them with `set_theme` like any token.
  A feed gives each item a `series` (`1` to `8` or `other`) and a layout colours it
  `${local://theme#chart_$item.series}`: `$item` fields are filled in before the theme lookup, so every client that
  draws `theme` binds it, with no new build. Any chart (or a list's colour dots) can use them.
- **`core://Bars` `merge`** (feature `bars_merge`): a field; neighbouring segments of a bar whose items have the same
  value of it and the same hatching draw as one. Items are still filtered one by one, so `where` keeps working.
  A layout version that uses it is served only to clients that draw it; older clients get the version before.

The `monitor` source's `devices` feed gives `series`, `color` (the series' light colour, for layouts that still bind
it) and `build`, and orders each day's items by build, the devices seen that day first.
