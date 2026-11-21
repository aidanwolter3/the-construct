# A determinate progress bar: `core://Progress` with `value`

Date: 2026-11-20. Related: [element-roles-and-reuse](element-roles-and-reuse.md),
[named-vocabulary](named-vocabulary.md).

A UX audit of the developer's pod asked for progress drawn as progress: a lesson's place ("card 7 of 12"), a
track's lessons done, a week's workout goal. Layouts could only write the numbers, or fake a bar with a list of
segments that can't compare a position to a count (`core://Conditional` tests equality only) and with a list's
columns capped at six.

Decision: **`core://Progress` takes `value`, `max` and `color`; with a value it is a determinate bar** (feature
`progress_value`), as wide as it is given, `size_dp` tall (default 6), filled `value / max` (max default 1, clamped
to 0..1) in `color` (default the theme's primary, so it follows the pod's accent) over the theme's quiet surface,
and read out to screen readers as a percentage. `value` and `max` are numbers or bindings.

Why on `Progress` rather than a new node: it is the same idea (how far along), the spinner is what it shows while
the value is unknown, and a layout moves from one to the other by adding a field. A client without the feature
is sent the newest version it can draw, as for any feature.

Not done: a ring. A bar fits every place the audit found (rows, cards, a reader's header); a ring needs an arc in
both renderers' shaders, and waits for a place that needs it.

Consequences: a new desktop, Android and web build draws it; layouts that use it are served to older builds as
their previous version.
