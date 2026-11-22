# Fillers don't store an item twice, and a removed filler takes its rows

Date: 2026-11-22. Related: [collection-fillers](collection-fillers.md), [collections](collections.md).

A filler only touched its own rows, and its `key` only matched within them. Two ways left a collection holding the
same event two to four times:

- **A site set up again.** Changing how a site is read by removing its filler and adding a new one (instead of
  `set_filler` with its id) left the old filler's rows behind, "handed to people", and the new filler made its own.
  On one pod a downtown calendar rebuilt twice held each event three times until the old rows were deleted by hand,
  and a skating rink's and a lake's sites set up again kept their first setups' rows beside the new ones.
- **Two listings of one site.** A site's page one and page two (two fillers) both listing a featured event each
  stored it, under the same key.

Considered: dropping orphaned rows on a schedule (rejected: a row with no filler is a person's, or was handed to
them on purpose); matching rows across fillers by title and date (rejected: keys already say which items are the
same, and titles repeat across weeks); making the second listing a second URL of one filler (a bigger recipe change
that doesn't help two separate feeds that overlap).

Decision:

- **`remove_filler` deletes a `replace` filler's rows**, the same as a fill that lists nothing (rows its `keep`
  conditions keep stay). They go as a person's delete does, restorable for the purge window, not for good. The
  change answers `removed_rows`. `keep_rows: true` keeps the old behaviour: the rows stay, unlocked. An `upsert` or
  per-row filler's rows always stay (they were never only the latest answer).
- **A keyed filler leaves out an item whose key another live filler's row already holds.** The oldest row keeps the
  item; a younger copy a filler already holds (made before this) goes with the rows its answer left out at its next
  fill. When the other filler stops listing the item, its row goes and the next fill of this one adds it. A row with
  no filler (a person's) isn't matched, and a scoped (`for_each`) answer, each its own parent's, doesn't look.

Not done: keyless fillers (a page read by a model, most agents) aren't matched across fillers, since they have no
ids; set such a site up again with `set_filler` and its id. Rows left behind before this stay until deleted.
