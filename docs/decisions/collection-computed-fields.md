# Collection fields can be computed from the row, and rows ordered by several fields

Amended by [collection-lookups](collection-lookups.md): a `lookup` kind reads rules in another collection, and
`compute` may be a list of kinds tried in order.

Date: 2026-11-03. Related: [collections](collections.md), [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
issue #62. Amends `collections`, which said "no computed fields".

The custom Groceries app (collection `groceries`: `name`, `quantity`, `done`) should list items the way a person walks
the store: by section (Produce, Bakery, …), unbought first inside each, with the section worked out from the item's
name. Rows added from a recipe screen (`/v1/collections/groceries/create`) must land in the same place. Collections
could not express it: a value came only from the write or a static default, `order_by` was one field, and a choice
sorted by its option order only by accident of the code.

Considered:
- A grocery-specific data source. Rejected: app logic in an adapter for one custom app.
- An `infer: {from, hint}` only for choice fields (the first version of this change). Rejected in review: shaped
  by one app; the language should be generic and composable across apps.
- Computed fields as stored code. Rejected in decision `collections`: no code from a prompt. Both kinds below are
  data: an expression in a closed, small language, and a prompt.

Decision: one generic field mechanism, `compute`, and multi-key order.

## Computed fields

A field of any type may declare `compute`, one of two kinds:

- **`{kind: "expr", expr}`**: an expression over the row, evaluated inside the write. It is the mutation-expression
  subset clients already evaluate for `value_expr` (D4: `entity.<field>`, JSON literals, `!`, `+`/`-`,
  `c ? a : b`), widened just enough for row values:
  - `+` joins text when either side is text (else adds numbers);
  - any value can be a condition (empty text, `0`, `false` and empty values are false);
  - ternaries nest, and parentheses group.

  There are no calls and no access beyond the row; an expression is at most 300 characters and 16 levels deep
  (`crates/app-collections/src/expr.rs`). Example: `entity.quantity ? entity.name + " (" + entity.quantity + ")" :
  entity.name`.
- **`{kind: "model", from: <field> | [<fields>], prompt}`**: a model's answer to `prompt` about the values of the
  `from` fields (at most 4), coerced to the field's type. A choice must answer one of its options (case-insensitive
  match); text is trimmed; other types must parse. Example:
  `{from: "name", prompt: "Which grocery store section is this item in?"}` on a choice `section`.

Rules for both:
- **Sources:** other live fields, stored by field id, so a rename keeps them (describe shows names). A source can't
  be removed while a field computes from it. A computed field can't read another computed field (not yet: no
  ordering or cycles to handle).
- **Defaults:** the field must be optional or have a default, which is what a row reads until it is computed (or
  when an expression has no value, e.g. text in a subtraction).
- **When:** on a create that leaves the field empty, on any write that changes a source without setting the field,
  and on a write that clears it. A change that adds or alters `compute` (or a computed choice's options or the
  field's type) computes existing rows: an expression for every row, a model only for empty ones. `edit_rows`
  follows the same rules.
- **Manual writes:** a computed field still takes writes. A value set by hand stays until a source changes.

For `model`, three things more:
- **Cache:** answers are cached in `collections_computed`, keyed by collection, field id and the normalized source
  values (lowercase, inner spaces collapsed). A cached answer applies inside the write. Otherwise the row is stored
  with the field empty and a background task asks the model, caches the answer and updates the row, unless the row
  changed meanwhile; devices get the usual data event. Each input is asked about once per field.
- **Learning:** a write that sets the field by hand caches that value for the row's inputs, marked as a person's
  answer. A model's answer never replaces a person's.
- **The model** sits behind the `app_collections::compute::FieldModel` trait, and tests use a fake. The server's asks
  a model over the Messages API when `CONSTRUCT_INFER_API_KEY` and `CONSTRUCT_INFER_MODEL` are set
  (`CONSTRUCT_INFER_URL` overrides the endpoint). Without them, model fields keep their default; cached answers,
  people's included, still apply.

MCP:
- `define_collection` fields and `apply_change` `add_field` / `update_field` take `compute` (`null` stops computing
  and keeps the values).
- `describe_collection` shows each field's `compute` with field names, and the number of cached answers
  (`computed_answers`).
- `apply_change` answers `computing_rows`, the rows queued for the model.

## Multi-key order

- `order_by` is a field or a list of fields (at most 4), first key first; `descending` applies to every key. Empty
  values sort last per key, then rows stay oldest first. A **choice** sorts by the order of its options (a value no
  longer an option after them).
- Stored as before when there is one key (a string), as a list otherwise, so definitions written before this read
  unchanged and older servers still read single-key ones. Removing a field drops it from the order and keeps the
  rest. `set_order` takes `field` as a name or a list.

## Consequences

- Schema: one new table, created by migration 2 of the `collections` adapter, which works on staging's existing
  databases. No protocol change and no client build: grouping by `section` is the existing List `group_by`.
- `model` fields are the pod's only outbound call for Collections. It sends the prompt, the source fields' names
  and values and the options to the configured model, and no other row data.
- Groceries, after deploy: add `section` (a choice with a `model` compute from `name`), `set_order ["section",
  "done"]`, and group the app's list by `section`. A `label` like the example above is optional.

Not done yet:
- computed fields that read other computed fields;
- a way to clear a learned answer other than setting the field again;
- per-key directions in an order;
- more expression functions (e.g. `lower`, `size`), which would also have to be mirrored in the clients'
  `value_expr` to keep one language;
- showing which recipe ingredients are already on the list (a lookup binding, separate work).
