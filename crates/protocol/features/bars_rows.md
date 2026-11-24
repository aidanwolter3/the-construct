`core://Bars` `row` and `row_label`: a grid instead of a chart (decision `device-day-grid`). Items with the same
`row` field value make one row, in the order their first item comes, labelled `row_label` (bound to the row's first
item; the `row` value without it) on a line above it; under that, across the full width, a cell per `bar` value, the bars in the order they first
come in any row and labelled `label` underneath. A cell's items lie side by side from the left, each `value` wide
out of `scale_min` (or the cell's total, if more); a cell no item reaches stays empty. `height_dp` is a row's
height (18 by default). With `field`, a tap on a cell sets `$form.<field>` to `<row>/<bar>` and only that cell is
drawn picked; nothing is picked while the value is unset. E.g. devices down the side, days across, each day's
builds in the order a device ran them.
