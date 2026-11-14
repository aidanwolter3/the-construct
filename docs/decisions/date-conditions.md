# A Conditional can compare a date with the device's clock, and the client redraws when the answer changes

Date: 2026-11-14. Related: [text-dates-and-counts](text-dates-and-counts.md),
[collection-computed-fields](collection-computed-fields.md), [collection-reminders](collection-reminders.md),
[layout-features](layout-features.md).

A workouts page lets its owner check a session off only once it is over (a class at 8–9 AM, a swim until 3:45 PM).
Layouts couldn't tell whether a row's date had come, so a scheduled agent run flipped an `unlocked` flag on each row
twice a day: a model call for a comparison, late by up to half a day, and one more field and one more job to keep
in step with the schedule.

Considered:

- **Computed fields that read `today` or `now`** (`compute: {kind: expr}` with comparisons), recomputed by the pod
  when the day changes. Rejected: "now" belongs to the viewer, not to the pod (decision `text-dates-and-counts`
  rejected computed dates for the same reason), the pod would have to recompute every such row on a timer (each
  minute, for times) and push the change to every device, and a screen cached offline would stay wrong.
- **A scheduled job per pod** (what the page had). Rejected: it is the workaround this replaces.
- **Comparisons in bindings** (`$item.date < today`). Rejected: layouts don't compute (D37), and an unknown operator
  would draw as raw text on older clients instead of gating the version.

Decision: one additive layout feature on `core://Conditional`, evaluated on the device.

- **`conditional_date`:** `date: Past|Today|Future`, with an optional `time` binding. `value` is a `YYYY-MM-DD`
  date (with `time`, that day at that wall-clock time: `15:45`, `9:00 AM`; a `time` that isn't one leaves the bare
  date), a local `YYYY-MM-DDTHH:MM[:SS]`, or an instant with an offset (a Collections `datetime`). Everything reads
  on the device's own clock and in its time zone, to the minute. A bare date is `Past` once its day is over and
  `Future` until it begins (as `format`'s `past` is "before today"); with a time, `Past` from that minute and
  `Future` until it. `Today` ignores the time. A value that isn't a date shows nothing (neither branch), so an
  unset date never unlocks anything. `equals` may go with it (both must hold).
- **Redrawing:** while laying out, the renderer notes the next minute at which any date conditional it evaluated
  changes its answer (`Laid::changes_at`; list items taken over from the frame before keep theirs). The session
  wakes the window at that minute (`Session::next_wake`, next to the caret's blink) and lays out the affected
  layouts again from scratch, so a tile unlocks at 9:00 on a screen nobody touches, and a list item is never
  taken over past the minute it changes. A frame without date conditionals schedules nothing.
- **The clock** is `dates::Clock`: this device's by default; a pod's MCP previews read it in the pod's zone
  (`set_time_zone`), and the simulator pins it with `--now` and the script command `clock`.
- **Validation:** `time` needs `date`, and a `date` needs a value. Layouts using it are served only to clients that
  draw `conditional_date`; older ones get the newest version without it.

What doesn't change: the pod still knows no "now" for layouts, `format` dates are as before, and a reminder (decision
`collection-reminders`) is still how the pod acts on a date when no one is looking.
