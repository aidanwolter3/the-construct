`core://Scatter {points, colors?, none_color?, highlight_color?, muted_color?, select?, on_tap?, on_select?,
aspect_ratio?, fill?, tag?, content_description?}` (decision `scatter-and-tap-paging`): a scatter plot of
thousands of points, zoomed and panned in place.

Fields: points, colors?, none_color?, highlight_color?, muted_color?, select?, on_tap?, on_select?,
aspect_ratio?, fill?, tag?, content_description?.

`points` (a binding) is one string: the base64 of 6 bytes per point, x and y as little-endian u16 (0..65535
across the 0..1 square, y down), a value byte (0..250 along the colour ramp, 255 none) and a mark byte (0 plain,
1 highlighted, 2 muted) (`protocol::scatter`). `colors` is the ramp (low, middle, high, `#RRGGBB`), `none_color`
a point without a value; highlighted points are drawn larger and ringed in `highlight_color`, muted ones small in
`muted_color`. Pinch or the wheel zooms, a drag pans. A tap near a point runs `on_tap` with `$point.index`,
`$point.x` and `$point.y` bound. While `select` (a binding) is truthy, or after a long press, a drag draws a box
and its release runs `on_select` with `$select.x0`, `$select.y0`, `$select.x1`, `$select.y1` (0..1) and
`$select.box` (`x0-y0-x1-y1` in millionths) bound. As wide as its parent and `aspect_ratio` (default 1) tall,
or the space given with `fill`.
