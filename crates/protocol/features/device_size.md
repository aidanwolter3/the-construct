`local://device` `width_dp`, `height_dp` and `size_class`: the window's size in points, and its class by width:
`compact` (under 600), `medium` (600 to 839) or `expanded` (840 and up). They change when the window is resized.
Show different things by size with `core://Conditional` (`"value": "${local://device#size_class}", "equals":
"expanded"`) (decision `screen-sizes`).
