# A range filler makes one row per day, week or month of a window that moves with today

Date: 2026-11-16. Related: [collection-fillers](collection-fillers.md), [mcp-jack](mcp-jack.md),
[collection-lookups](collection-lookups.md), [date-conditions](date-conditions.md),
[collection-primitives](collection-primitives.md).

A calendar that shows a person's plans, workouts and house days together, one box per day, needs something to
hang each day on. A `core://List` lists one query; the three live in three collections, each with its own date
field. And "today, with nothing planned" has no row anywhere, so a placeholder for it can't be drawn. Nothing on
a pod kept rows of dates current: rows were written by people, by Claude, or by fillers that read the web, a
model or an MCP tool. The only thing that knew the date was the `mcp` filler's `{today±N}` arguments.

Considered:

- **A `days` filler**: one row per day for N days. It works, but is shaped by one screen.
- **A merged collection** whose rows come from several others (plans, workouts and house days as one list with a
  shared date and title). Useful elsewhere, but alone it still can't show an empty today: a day with nothing on it
  has no row to merge.
- **Lists that read several queries, and a `{today}` value in `where`.** The most flexible for layouts, but new
  vocabulary: a client build for every device, for what the pod can already serve as rows.
- **A nightly agent run that writes the days.** A model run a night for arithmetic on dates, and it stops when the
  runner is down.

Decision: a fourth filler kind, **`range`**: `{kind: "range", from, to, step, map, key?, every_minutes?}`
(`crates/source-collections/src/filler.rs`, run by `fill.rs`).

- `from` and `to` are `{today}`, `{today+N}` or `{today-N}`, counted in the pod's zone (as the `mcp` filler's
  arguments). `step` is `day` (the default), `week` (Monday to Sunday) or `month`; a week or month window starts
  with the step the window's first day falls in. At most 200 steps (a fill's row cap).
- Each step is an item `{date, end, weekday, weekday_long, day, month, month_long, year, week, index}` read like a
  JSON page: `map` gives each field one of those parts, and the fill coerces, keys and replaces as for every other
  filler. Nothing new on the row side: with `key` on the date field a day keeps its row (and the fields people set
  on it) while the window moves; a day that leaves the window is deleted, as rows a `replace` filler no longer
  lists always are.
- No jack: it reads nothing outside the pod, so the web and mcp jacks don't gate it, and temporary pods run it. It
  fills hourly unless `every_minutes` says otherwise; a fill on the same day answers the same items and writes
  nothing (the content hash), so the hourly check costs a date computation.
- `collections_preview {collection, filler}` shows the rows it would make today.

It is a spine, not a feature: the collections that hold the data stay as they are. `count` and `lookup` fields
keyed by the date say what falls on each day, and a layout lists the days with each collection's rows inside
(a `core://List` in the day's template whose `where` names `$item.date`), highlighting today with the existing
`core://Conditional` `date: Today`. The same spine makes a habit tracker (a row per day to tick), a weekly planner
or a monthly budget.

Consequences: no protocol change and no client build; the definition gains a filler kind (stored in the
collection's definition JSON, so no migration). Not done: a window anchored somewhere other than today (a fixed
season), steps other than day, week and month, and a merged collection, which stays open for when one list must
sort several collections' rows together.
