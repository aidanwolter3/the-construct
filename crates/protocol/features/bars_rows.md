`core://Bars` `row` and `row_label`: a grid instead of a chart (decision `device-day-grid`). Items with the same
`row` field value make one row, in the order their first item comes, labelled `row_label` (bound to the row's first
item; the `row` value without it) on a line above it; under that, across the full width, a cell per `bar` value, the bars in the order they first
come in any row and labelled `label` underneath. A cell's items lie side by side from the left, each `value` wide
out of `scale_min` (or the cell's total, if more); a cell no item reaches stays empty. `height_dp` is a row's
height (18 by default). With `field`, a tap on a cell sets `$form.<field>` to `<row>/<bar>` and only that cell is
drawn picked; nothing is picked while the value is unset. With `merge` (feature `bars_merge`), a cell's last item and the next
cell's first in the same row join into one bar when they have the same value of it and neither is hatched.
`marks: {query_key, at, color}`: each item of that query is a dashed vertical line down the whole grid, from the
first row's label to the last row's cells, in the column of its `bar` value, `at` (a field) along the cell out of
`scale_min`, coloured `color` (a binding). E.g. devices down the side, days across, each day's
builds in the order a device ran them.
