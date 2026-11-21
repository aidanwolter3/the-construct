# Progress rings, count totals and seven-column lists

Date: 2026-11-21. Related: [determinate-progress](determinate-progress.md), [merged-lists](merged-lists.md),
[text-dates-and-counts](text-dates-and-counts.md), [named-vocabulary](named-vocabulary.md).

The UX audit's mockups asked for three things layouts could not draw: a workout week's goal as a ring ("5/5"), a
shopping bag's total ("$518.84") and a Mon–Sun strip of the week. A determinate `core://Progress` was only a bar;
a `core://Text` `count` could count rows but not add up a field; a `core://List` grid stopped at six columns, so a
week was seven lists side by side.

Decision: **`core://Progress` takes `shape: Ring` and `label`; a Text `count` takes `sum`; a List grid takes up to
seven columns.**

- **Ring** (feature `progress_ring`): with `value` and `shape: Ring`, `core://Progress` is a circle `size_dp` across
  (default 56), a quiet track with `value / max` of it drawn clockwise from the top in `color`, and `label` (bound
  text) in its middle. It reads out as the label and the percentage. The arc is drawn as overlapping round dots,
  as the spinner is, since the renderer's quads don't draw arcs. A ring needs a value; the pod refuses one without.
- **Sum** (feature `count_sum`): `count: {query_key, where?, sum: "<field>"}` adds up that field over the counted
  entities. `$sum_money` is the total as a price, written as the collections' `money()` writes one (`$98`,
  `$1,298.50`); `$sum` is the plain number. Values that aren't numbers count as 0; text such as `"1,200.5"` or
  `"$98"` reads as its number. The Collections check refuses a `sum` that names no field of the collection.
- **Seven columns**: `core://List` `columns` goes up to 7. Renderers never capped the columns, only the pod's
  validation did, so this needs no feature and installed apps draw it.

Rejected: a pod-side total as a computed field (a total over many rows needs a parent row every row points at, a
collection made only to hold it); a `core://Ring` node (a ring is a progress, and `progress_value` already says
what value and max mean); a serif text field (the `Display` style is already the serif face, Lora).

Consequences: a new desktop, Android and web build draws rings and totals; a layout that uses them is served to
older builds as its previous version. Seven-column lists need no build.
