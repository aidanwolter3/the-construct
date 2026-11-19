`core://Bars {query_key, bar, label?, color, value?, hatched?, where?, height_dp?, scale_min?, field?, on_click?}`:
a bar chart of a query's items. Items with the same `bar` field value make one bar (in the order their first item
comes); each item is a segment stacked from the bottom, `value` (a field, default 1) tall and coloured `color` (a
binding such as `"$item.color"` to `#RRGGBB`); a true `hatched` field draws it as a dashed outline. `label` (bound to
the bar's first item) goes under the bar; the axis reaches the tallest bar, at least `scale_min`. `where` filters
items as a `core://List`'s does. With `field`, a tap on a bar sets `$form.<field>` to its `bar` value (as
`core://ChoiceChips` do) and that bar is drawn picked (the last one while the value is unset); otherwise a tap runs
`on_click` with `$item` its first item. E.g. devices per day, one item per device and day, coloured by build.

Fields: query_key, bar, label?, color, value?, hatched?, where?, height_dp?, scale_min?, field?, on_click?.

Stacked bars of a query's items, one bar per value of a field
