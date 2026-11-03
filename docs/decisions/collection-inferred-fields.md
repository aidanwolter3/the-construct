# Collections infer a choice field from another field's text, and order rows by several fields

Date: 2026-11-03. Related: [collections](collections.md), [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
issue #62.

The custom Groceries app (collection `groceries`: `name`, `quantity`, `done`) should list items the way a person walks
the store: by section (Produce, Bakery, …), unbought first inside each, with the section worked out from the item's
name. Rows added from a recipe screen (`/v1/collections/groceries/create`) must land in the same place. Collections
could not express it: a value came only from the write or a static default, `order_by` was one field, and a choice
sorted by its option order only by accident of the code, not by contract. Considered: a grocery-specific data source
(rejected: app logic in an adapter for one custom app; the design chat chose generic collection features) and computed
fields as stored code (rejected in decision `collections`: no logic from a prompt).

Decision: two generic collection features.

## Inferred fields

- A field may declare `infer: {from: <field>, hint}`. Only a **choice** field, so the answer is always one of its
  options; it must be optional or have a default (what a row reads until it is inferred); `from` must be another live
  **text** field (stored by field id, so a rename keeps it; it can't be removed while something infers from it).
  `hint` says what the options are ("grocery store section"; default: the field's name).
- **When:** a create that leaves the field empty, an update that changes the `from` text without setting the field
  (its old answer is dropped), and every existing empty row when a change adds or changes `infer`. `edit_rows` follows
  the same rules.
- **How:** answers are cached in `collections_inferred` by collection, field id and the normalized `from` text
  (lowercase, inner spaces collapsed). A cached answer applies inside the write itself. Otherwise the row is stored
  with the field empty (it reads the default) and a background task asks the classifier, stores the answer and updates
  the row, unless the row changed meanwhile (set by hand, renamed, deleted); devices get the usual data event. Each
  text is classified once per field.
- **Learning:** a write that sets the field by hand stores that value for the row's `from` text, marked as a
  person's answer; a classifier's answer never replaces one.
- **The classifier** is a small trait (`app_collections::infer::Classifier`); tests use a fake. The server's asks a
  model over the Messages API when `CONSTRUCT_INFER_API_KEY` and `CONSTRUCT_INFER_MODEL` are set
  (`CONSTRUCT_INFER_URL` overrides the endpoint). Without them inferred fields keep their default (cached answers,
  including people's, still apply). An answer that is not one of the options (after the usual case-insensitive match)
  is dropped.
- MCP: `define_collection` fields and `apply_change` `add_field` / `update_field` take `infer` (`null` stops it);
  `describe_collection` shows it per field and the number of cached answers; `apply_change` answers `inferring_rows`.

## Multi-key order

- `order_by` is a field or a list of fields (at most 4), first key first; `descending` applies to every key. Empty
  values sort last per key, then rows stay oldest first. A **choice** sorts by the order of its options (a value no
  longer an option after them).
- Stored as before when there is one key (a string), as a list otherwise, so definitions written before this read
  unchanged and older servers still read single-key ones. Removing a field drops it from the order and keeps the
  rest. `set_order` takes `field` as a name or a list.

## Consequences

- Schema: one new table, added by migration 2 of the `collections` adapter (works on staging's existing databases).
  No protocol change and no client build: grouping by `section` is the existing List `group_by`.
- Inferred values are the pod's only outbound call for Collections. It sends the `from` text and the options to the
  configured model; no other row data.

Not done yet: a way to clear a learned answer other than setting the field again; inferring from more than one field
or into non-choice types; per-key directions in an order; showing which recipe ingredients are already on the list (a
lookup binding, separate work).
