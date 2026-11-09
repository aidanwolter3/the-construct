# Collections get references, rollups, quantities and insert-from-query actions

Date: 2026-11-08. Related: [live-computed-fields](live-computed-fields.md),
[collection-lookups](collection-lookups.md), [collection-computed-fields](collection-computed-fields.md),
[composable-actions](composable-actions.md), [collections](collections.md).

A pod's recipes and grocery list had outgrown text keys. `recipe_lines` named its meal and ingredient by text, an
ingredient's "used in favorites" was a `count` matched by name, quantities were text ("1 1/2 cups") that nothing
could add up, and putting a recipe on the grocery list meant one create per line from the device, with no way to
merge "chicken broth" from two recipes into one row with a total.

What the list needed: one row per ingredient showing the total across recipes ("Chicken broth · 6 1/4 cups") and
which recipes it is for; manual items still typed by hand; checking off or removing a row covering every recipe's
part; each recipe's part traceable; a recipe added with one tap, scaled to the servings chosen.

Considered:
- Grocery-shaped options (a "merge by name" flag on creates, a `recipe` adapter). Rejected (decision
  `composable-actions`): shaped by one app, and the next list would need another.
- A query language (joins, group by) over collections. Rejected: too much to keep safe, linear and explainable to an
  agent; four small primitives cover the need and compose.
- Doing the merge on the device (`action.sequence` of creates). Rejected: needs the list's state on the device,
  can't be one undoable step, and a new intent or engine change would need a client build.
- Storing quantities as text and summing at read time. Rejected: every read would parse; a stored value is parsed
  once, and text that doesn't parse can be reported.

Decision: four generic primitives in `app-collections`, server only (no node type, intent, field or engine change;
no client build). The grocery case appears only in tests and docs.

## References (`ref`) and `unique`

- A field `{type: "ref", collection: <name>}` stores the id of a row of that collection (optional or required like
  any field; no default). Writes from layouts, `edit_rows`, `add_row` and actions are resolved and checked
  (`refs::prepare_write`): the value is a live row id (or a pending create's `pending-<client id>`), or
  `{"<unique field>": value}`, resolved to that row's id; anything else is refused with the field named.
- `unique: true` on a text or ref field (not a computed one): no two live rows share a value, text compared as an
  exact lookup compares (case and spacing ignored). Checked on create, update and restore (HTTP, `edit_rows`,
  `add_row`, actions, `undo_edit`); making a field unique fails while live rows share a value, and the error lists
  them (`duplicates: [{field, value, rows}]`).
- `{kind: "lookup", via: <ref field>, value: <field of the target or "id">}` reads the row a ref points at. It is
  stored as a lookup whose `key` is the system `id` (field ids are `f<n>`, so `id` names no field) and `from` the
  ref field, so it reuses the rules machinery and is live under `follow.rs`: the read row created, deleted,
  restored or its value changed recomputes the rows whose ref holds its id; the ref changing recomputes its row.
  Key-matching lookups stay as they were.
- **Target deleted:** the ref keeps the id. Reading through it gives nothing (the entity isn't served), a lookup
  through it answers nothing (the field reads its default), and a restore of the target heals both.
- **Layouts** get the id. They read through it with `${collections://<target>/$item.<ref>#<field>}`: the renderer
  binds `$item.` tokens before resolving `${uri#field}` (a renderer test covers it), so it works in today's
  clients, as long as the target collection is one of the layout's feeds. No `<ref>_<field>` convenience keys are
  served. The pod's binding check checks the target and the field.
- `update_field {type: "ref", collection}` converts a text field: each value becomes the id of the target row whose
  id or unique text field equals it; values that match none go to the kept-aside table, as any conversion's do,
  and are reported (`set_aside`, `samples`).
- A collection can't be deleted while another's ref points at it. A filler can't fill a ref field (a page holds no
  row ids).

## Rollups

`{kind: "rollup", collection, via, op: count|sum|min|max|any|list, field?, where?: [{field, equals}]}` aggregates the
live rows of `collection` whose ref field `via` points at this row and that match every condition (compared as a
`count`'s are; conditions may name computed fields, such as a lookup through a ref).

- `count` (number), `any` (bool), `sum` of a number (number) or of a quantity (quantity), `min` / `max` of a
  number, and `list`: the distinct values of `field` as text, in row order, joined by ", " (text). With no rows:
  count 0, any false, a number sum 0, the others empty.
- The field's only kind, as `count` is; it reads another collection (not its own); `via` must be a ref to this
  collection; the fields stay live (checked on every change to either collection).
- Live under `follow.rs`: a row of `collection` created, deleted, restored, or changing its `via`, `field` or a
  `where` field recomputes the rows its `via` pointed at before and after. It cascades with lookups as before
  (meal favorite → line favorite → ingredient rollup).
- `count` stays and keeps working; new fields should prefer `rollup`.

## Quantities

A field `{type: "quantity"}` holds an amount and a unit (`quantity.rs`, deterministic, no model).

- **Parsing** text: terms joined by `plus`, `+` or `and`, each an amount (`3`, `1.5`, `1/2`, `1 1/2`, `1-1/2`,
  `1 and 1/2`, unicode fractions, `a`/`an`/`one`, a range `2-3` / `2 to 3` / `2 or 3` as its upper end) and an
  optional unit: a size in parentheses and/or a unit word, two-word units first (`fl oz`, `fluid ounces`).
  Built-in units and their synonyms are normalized (`T` tablespoon, `t` teaspoon, `Tbsp.`, `tablespoons`, `lbs`,
  ...); any other single word is a count-like unit, singular (`sprigs` → `sprig`); a size makes its own unit
  (`1 (14 oz) can` is 1 of `(14 oz) can`). Text with anything else left over (`to taste`, `2 cups chopped`) doesn't
  parse.
- **Stored** as `{amount, unit}` (a list of them when parts don't combine), the unit canonical; `""` for a plain
  count. Text that doesn't parse is stored as the text, shown as is, left out of sums, and counted per field by
  `describe_collection` (`quantities`). Writes also take a number (a count) or `{amount, unit}`.
- **Served** with `<name>_text`: ASCII fractions to sixteenths ("1 1/2 cups", "1 3/16 cups"), decimals when an
  amount isn't one ("0.35 cup") and for metric units ("1.25 kg"), plurals ("2 cloves", "2 (14 oz) cans"; tsp,
  tbsp, oz, lb, g, ml and the sizes don't change), parts joined by " + ".
- **Sums** (rollup `sum`): volume (tsp, tbsp, fl oz, cup, pint, quart, ml, l) and weight (g, kg, oz, lb) convert
  within their dimension; count-like units add only to the same unit; the result lists volume, then weight, then
  each count-like unit in the order first seen ("2 cups + 3 cloves").
- **Keeping the units written** in a sum of US units (2026-11): people measure in the units the recipe used, so
  one unit stays (4 tbsp + 2 tbsp = 6 tbsp, not 3/8 cup); several units make one amount of the largest when it is
  a plain fraction to eighths (1/2 cup + 2 tbsp = 5/8 cup, 1 lb + 8 oz = 1 1/2 lb); otherwise they stay apart,
  largest first, a smaller amount's whole multiples of the next larger unit carried up (1 cup + 3 tbsp, not
  1 3/16 cups; 1 cup + 19 tbsp = 2 cups + 3 tbsp).
- **Choosing a unit** after a scaling, or a sum with metric parts: US volume as cups from 1/4 cup, else tablespoons from 1 tbsp, else
  teaspoons, preferring the largest whose amount is a plain fraction (48 tsp → 1 cup, 5 tsp → 1 2/3 tbsp); US
  weight as lb from 1 lb when a plain fraction, else oz; metric (only when every part is) as ml/l and g/kg. A
  value written by hand keeps its unit.
- **Scaling** multiplies every part (used by actions); by exactly 1 it is unchanged.

## Insert-from-query actions

A named, declarative "upsert a parent, add a child", stored on the parent collection's definition (`actions`) and
changed with `apply_change` `set_action` / `remove_action` (or `define_collection` `actions`):

```json
{"name": "add_recipe", "title": "Add a recipe to groceries",
 "params": [{"name": "meal", "type": "ref", "collection": "meals"},
            {"name": "servings", "type": "number", "optional": true}],
 "source": {"collection": "recipe_lines", "where": [{"field": "meal", "equals": "$param.meal"},
                                                    {"field": "ingredient", "empty": false}]},
 "scale": {"times": "$param.servings", "per": "$row.meal.servings"},
 "match": {"ingredient": "$row.ingredient"},
 "create": {"name": "$row.ingredient.name", "done": false},
 "update": {"done": false},
 "reset_when": [{"field": "done", "equals": true}],
 "child": {"collection": "grocery_entries",
           "values": {"grocery": "$parent.id", "quantity": {"from": "$row.amount", "scaled": true},
                      "recipe": "$row.meal"}}}
```

- Values are literals or bindings: `$param.<name>`, `$row.<field>` (through refs: `$row.<ref>.<field>`, up to 4
  names, `.id` last), and in child values `$parent.id` / `$parent.<field>`. Stored by field id, so renames keep
  them; checked against every change to the collections they name (a field an action names can't be removed, a
  collection it reads or adds to can't be deleted).
- A call reads the live source rows matching `where` in the source's order. For each, the `match` values find the
  oldest live parent whose match fields equal them (rows made earlier in the call included), else a parent is made
  from `match` and `create`. A found parent that matches `reset_when` first has its children deleted (once per
  call), then gets `update` (once per call). Then a child row gets `child.values`; exactly one of them is a ref to
  the parent set to `$parent.id`. A row whose match value is empty is skipped and reported.
- `scaled: true` (number or quantity child fields) multiplies by `times / per` when both read as numbers above 0
  (a number, a quantity's amount, or the first number in text: "Serves 6" → 6), else by 1. Text going into a
  quantity field is parsed first (so a text `amount` scales); text that doesn't parse goes in unscaled and is
  reported (`unparsed`).
- Every write goes through the normal checks (types, refs, unique, caps), computes and `follow.rs`. One call is one
  transaction and one edit spanning both collections: the edit table records each row's collection (migration
  `action_edits`), and the after states are taken after computes settle, so `undo_edit` reverts the call (rows it
  made are deleted, rows it changed or deleted come back, children before their parent). `undo_edit` now ignores
  fields that follow other rows (lookups, counts, rollups) when deciding a row "changed since", since those move on
  their own. A repeated `client_id` (stored on the edit, unique) answers the first call's edit without running
  again, so an outbox retry is applied once.
- **From a layout**, with the existing intent `action.mutation.optimistic` (no new vocabulary):
  `{intent: "action.mutation.optimistic", entity_uri: "$item.uri", field: "adding", value_expr: "true",
  endpoint: "/v1/collections/groceries/actions/add_recipe", payload: {meal: "$item.id", servings:
  "$form?.servings", client_id: "$nonce"}}`. The field is a view flag on the device only; the pod's check skips it
  for action endpoints and checks the action exists and that payload keys are its params or `client_id`. The new
  rows reach devices through the change feed (no optimistic rows). The manifest lists the action route, so the
  capability check accepts it.
- **Delete actions** (added later): `{"name": "clear_bag", "title": "Clear the bag", "source": {"collection":
  "groceries", "where": [{"field": "done", "equals": true}]}, "delete": true}` deletes every live row of its own
  collection matching `where` (params work as in any action). Why: a grocery list wants a one-tap "Clear bag" for
  the checked-off items, and deleting them one row at a time is a tap per item and an edit per row. It stays
  declarative, the same shape as the other actions: a delete action is the only one whose source is its own
  collection, and it has no `match`, `create`, `update`, `reset_when`, `scale` or `child` (refused when given). Each
  row is soft-deleted as a row delete does (restorable; `follow.rs` moves the lookups, counts and rollups that
  read it; clients hear of the rows in one event), a row a filler owns is refused as a row delete is, the call is
  one edit (`undo_edit` brings them back) within the row cap (`too_many_rows`), a repeated `client_id` is applied
  once, and the answer counts them (`deleted`).
- **From MCP**: `run_action {collection, action, params}` (a new tool), answering the edit id and what it did.
- Not grocery code: nothing in the code names meals or groceries.

## Consequences

- One migration (`action_edits`: `collections_edit_rows.collection`, `collections_edits.client_id` with a unique
  index). Definitions gain `actions`, fields `collection` and `unique` (serde defaults: old definitions read as
  before).
- No protocol change, no new intent or node, no engine change: no desktop or Android build.
- A write to a collection with unique fields scans its live rows once per unique field; a rollup reads its
  collection once per write or wave, as a count does.
- The connector's MCP tool list changes (`run_action`): "Refresh tools list" after deploy.

Not done yet:
- fillers don't check `unique` (they key their own rows), and can't fill ref fields;
- a rollup over the collection's own rows (a tree), and `min`/`max` of dates;
- cascading deletes (deleting a parent leaves its children pointing at a deleted row; they are out of every live
  list through it, and come back with it);
- a layout can't undo an action call (undo is `undo_edit` over MCP).
