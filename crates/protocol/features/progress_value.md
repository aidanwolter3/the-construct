`core://Progress` with `value` (and `max`, `color`) is a determinate bar: as wide as it is given, `size_dp` tall
(default 6), filled `value / max` (max default 1, clamped to 0..1) in `color` (default the theme's primary) over a
quiet track. `value` and `max` are numbers or bindings (`"$item.reading_card"`, `"$item.card_count"`): a lesson's
place, a track's lessons done, a week's goal.

Fields: value, max?, color?.

A determinate progress bar: core://Progress with value / max
