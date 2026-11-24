# Devices per day as a grid, with every build a device ran each day

Date: 2026-11-24. Related: [device-versions](device-versions.md), [chart-colours](chart-colours.md),
[replaced-devices-are-one](replaced-devices-are-one.md).

Server health's "Devices per day" was a stacked bar per day, one block per build, today's bar every device at its
latest build. It said how many devices ran each build but not which device, and it couldn't show when a device
updated: the pod kept one build per device and day (`client_days` keyed on day and device), each ping overwriting
it. The developer asked for days across, devices down, each day's cell coloured by the builds the device ran as
the day went on, and a dashed box for a day the device didn't ping.

Considered: drawing the grid from existing nodes (`core://Box` takes only fixed widths and `core://Row` has no
weights, so a cell can't be split by data across the width it's given; fixed 18 dp cells with widths worked out on
the pod would fit a phone badly and limit a day to a few builds), and a new node (the grid is a bar chart laid
on its side, with the same items, colours, hatching, filter and taps).

Decision:

- **Every build a day**: `client_days` is keyed on day, device and build and has `first_seen` and `last_seen`,
  the first and last ping on that build that day (migration `device_day_builds`; days recorded before have one
  build and no times). The `monitor` source folds a replaced device's rows per build, as before per day.
- **A grid is a `core://Bars` with `row`** (feature `bars_rows`): items with the same `row` value make a row,
  labelled `row_label` on a line above it (beside it, a long name on a phone wrapped and pushed the cells out of
  line), and a cell per bar across the full width; a cell's items lie side by side, each `value` wide out of `scale_min`
  (or the cell's total), a hatched one a dashed outline. A tap with `field` sets `<row>/<bar>`. It is drawn by
  the shared renderer, so every client has it with one build; older apps keep the newest layout they can draw.
- **The `devices` feed's `monitor:device_cells`**: a row per device seen in the 14 days (by name), a cell per day.
  A day's builds share its 24 hours by when the device first ran each: the first from the day's start, the last
  to its end; a build it ran for a moment still shows, an hour wide. A day it wasn't seen is one dashed
  `chart_other` part. Today's cell is the day so far the same way, dashed when the device hasn't pinged today.
  Each part's `detail` names the build and its first and last ping (`Nov 14 · 9be03a1 · 19:00–22:30`).
  `monitor:device_days` stays, one build per device and day (the last one run), for the layouts older apps draw.
- **Releases are dashed lines down the grid**: `marks` on the grid (`query_key`, `at`, `color`) draws each item as
  a dashed vertical line through every row at a moment in its column. The feed's `monitor:releases` are the server
  builds the pod ran (`server_builds`), at the moment the pod first ran each, coloured as the build, so a line
  shows where devices could switch to that colour. That is when the pod first saw the deploy (its first request
  after it), not the deploy itself.
- **Server health 0002** draws the grid, filtered by the kind chips, and a tap shows that device's day.

Limits: days recorded before this show one build all day. A device's first build of a day colours the day from
midnight, though it may have first pinged later. Builds are told apart by colour only from the six newest behind
the server's (decision `chart-colours`). Pods with their own copy of Server health don't get the grid until the
copy is changed.
