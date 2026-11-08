# Lookups follow the collection they read, and a `count` kind counts rows of another collection

Date: 2026-11-08. Related: [collection-lookups](collection-lookups.md) (its "Dependencies" section is superseded
here), [collection-computed-fields](collection-computed-fields.md), [collections](collections.md).

A pod has `meals` (name, favorite), `meal_ingredients` (meal, item; `favorite` a lookup of the meal's favorite) and
`ingredients` (name), which should show how many favorite meals use each ingredient. Two gaps:

1. A lookup is evaluated only when its own row is written. Toggling `meals.favorite` left
   `meal_ingredients.favorite` stale until someone ran `apply_change recompute_field`.
2. No compute could count rows of another collection.

Considered:
- Recompute every lookup of every collection on every write. Rejected: a write to a few-thousand-row pod would read
  every collection.
- Database triggers over `values_json`. Rejected: the match modes (`contains`, `regex`) and coercion live in Rust,
  and triggers would hide the logic from the code that owns computes.
- An `aggregate` language (sum, min, filters). Rejected for now (decision `composable-actions`): `count` with
  equality conditions covers the need; another kind can come when a need does.
- Refusing cycles when a definition is saved. Rejected: a cycle across collections is legitimate as data shapes go
  (two lookups reading each other usually settle); a depth cap ends the ones that don't.

Decision: lookups and counts are live, and `count` is a new compute kind. Both generic, server only.

## Change tracking

Every row write in the Collections store (`insert_row`, `save_row`, `save_row_values`, a filler's hard delete) notes
the row and its state before its first change in a temporary table, `temp.collections_changed`. It belongs to the
connection and rolls back with the transaction that wrote it, so a refused write leaves nothing behind. No migration.

Before a transaction commits, `follow::settle` reads the noted rows (the shared `refusable` helper, the fillers'
own transactions and the model's background answers all call it) and, for every computed field of any collection
whose lookup or count reads a changed row's collection:

- **lookup**: when the row was created, deleted or restored, or its `key` or `value` changed, the rows whose `from`
  matches the row's key before or after (under the lookup's `exact` / `contains` / `regex` mode) are computed again;
- **count**: when the row was created, deleted or restored, or its `key` or a `where` field changed, the rows whose
  `from` equals the key before or after are computed again;
- a **restored** row's own lookups and counts are computed again (they didn't follow while it was deleted).

Only collections that read a changed one are read, once per wave. A recomputed row is saved like any other write
(`updated_at` moves; clients are told through the change feed) and noted in turn, so a change **cascades** in waves:
`meals.favorite` → `meal_ingredients.favorite` → `ingredients.favorite_count`. After **8 waves** the rest is dropped
with a warning, so a cycle (a count over a lookup of that count) ends.

What doesn't change: `learn` and the model fallback. A recomputed field runs its whole compute list, so when a rule
stops matching, the next kind answers (a model job in the background, as before). A hand-set value stays until its
inputs change, and a change to the rows a lookup or count reads is such a change. Because a hand edit of a learning
lookup changes its rule, other rows with that same input now take the edited value at once.

## The `count` kind

`{kind: "count", from: <field>, collection: <name>, key: <field>, where?: [{field, equals}]}` (stored by field ids;
`where` also takes one object): the number of live rows of `collection` whose `key` equals this row's `from`, by the
`exact` lookup normalisation (case and spacing ignored), and which match every condition. A condition compares the
field's value as text, as a layout `where` does: a bool reads `"true"` or `"false"`, a number its digits, an empty
field its default; `equals` may be sent as text, a bool or a number and is stored as text. A blank `equals` is
refused (it would match everything). Conditions may name computed fields of the counted collection.

- It fits only number fields, and is the field's only kind (it always answers; an empty `from` counts 0).
- It reads another collection, not its own; the collection, `key` and `where` fields must stay live (checked on
  every change to either collection, as lookups are), and `delete_collection` refuses while a count reads it.
- `apply_change` `add_field` / `update_field` with a count compute every live row at once, like `expr`;
  `recompute_field` works.

## Consequences

- No schema migration, no protocol change and no client build: values change through the normal write path.
- The MCP tool descriptions of `define_collection` and `apply_change` describe `count` and live recompute;
  `describe_collection` shows a count with field names on both sides.
- A write costs one more indexed read per row (the state before) and a temporary-table insert; a collection that
  nothing reads costs nothing more at settle.

Not done yet:
- other aggregates (sum, min, max) or conditions other than equality;
- counting rows of the field's own collection;
- reporting in `describe_collection` when a cycle hit the wave cap (it is only logged).
