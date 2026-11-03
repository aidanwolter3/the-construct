# Computed fields can look up rules in another collection, and try kinds in order

Date: 2026-11-03. Related: [collection-computed-fields](collection-computed-fields.md) (amended here),
[composable-actions](composable-actions.md), [collections](collections.md), issue #70.

The Groceries app's `section` field asks a model for every new item name. The per-input cache of
`collection-computed-fields` skips only exact repeats: "English cucumber" and "cucumbers" still cost a call each, and
what was learned is hidden: nobody can see or edit it, or add a keyword rule up front.

Considered:
- A larger cache key (stemming, fuzzy matching). Rejected: still hidden and not editable, and shaped by one app.
- A `rules` option on `model`. Rejected (decision `composable-actions`): an option shaped by one need. The next one
  ("compute a label from a lookup", "a default from another table") would want another option.
- A general join language. Rejected: too much for the need, and harder to keep linear-time and safe.

Decision: two small primitives that compose, both generic.

## The `lookup` kind

`{kind: "lookup", from: <field>, collection: <name>, key: <field>, value: <field>, match?, learn?}`: the `value` of the
row of `collection` whose `key` matches this row's `from` field. Rules are ordinary rows, so people and Claude list,
add, edit and delete them like any data (an app can show them).

- **`match`:** `exact` (the default: the whole input, ignoring case and spacing), `contains` (the key's words appear
  in the input as whole words, in order: "milk" matches "Oat milk", not "Buttermilk"; ignoring case) or `regex`
  (case-insensitive, against the input lowercased with spaces collapsed). Regexes use the `regex` crate, which matches
  in linear time; a pattern is at most 200 characters and compiles within a 64 KiB size limit. A pattern that doesn't
  compile is skipped and listed under `lookups[].invalid_rules` in `describe_collection`.
- **Several matches:** the longest key wins ("coconut milk" over "milk"), then the oldest rule.
- **The answer** is coerced to the field's type like a model's answer (a choice must be one of its options, any
  case); when the best rule's value doesn't fit, the lookup gives no answer and the next kind is tried.
- **When:** inside the write, like `expr`, so a create from any app (the recipe screen too) is answered at once.

## Compute lists

`compute` may be a list of kinds, tried in order; the first that answers wins (an `expr` with no value, a lookup with no
match, or an empty input don't answer). A single object stays valid and is stored as one object, so definitions read
unchanged and older servers still read single-kind ones. A `model` answers in the background, so it may only be the
last kind; at most 4 kinds.

## Learning into the rules

On a lookup, `learn: true` (at most one per list):
- when a later kind (the model) answers, a rule is added to the lookup's collection: key = the normalized input (for
  `regex`, that text escaped and anchored, `^oat milk$`, so it matches only that input), value = the answer. Unless the
  rules already give that answer for the input. A model's answer never changes an existing rule.
- a hand edit of the field (a layout write or `edit_rows`) adds the rule for the row's input, or changes the rule with
  that exact key. A broader rule ("milk") is not changed: the new, longer key wins for that input.
- with a learning lookup, the model's per-input cache (`collections_computed`) is neither read nor written for that
  field: the rules are what was learned. Without one, the cache works as before.
- when a field gets a learning lookup in front of its model (the Groceries switch), its cached answers are learned as
  rules in the same change (a person's answer replacing a rule with the same key, a model's not), so nothing learned
  so far is lost or asked again.
- learning respects the pod's row and byte caps; when full, nothing is learned (the row still gets its value).

## Checks

- The lookup's collection must exist and differ from the field's own; its `key` must be a live text field; its
  `value` must be live and of the field's type, or either side text.
- A collection a lookup learns into must take a row of only a key and a value (its other fields optional or with a
  default) and have no computed fields.
- These are checked on every change, to either collection: removing the key, retyping it, or adding a computed field
  to a learned-into collection is refused, naming the field that reads it. Field ids are stored, so renames are fine.

## Dependencies

A field whose lookup reads another collection is **not** recomputed when that collection changes: rows keep the
section they have (a person may have set it), editing rules stays cheap, and nothing moves under the user without a
request. It is recomputed, as before, when its own row's source changes. An explicit `apply_change` op,
`{op: recompute_field, field}`, computes a field again for every row now: what an expression, a rule or the cache
answers replaces the value (hand-set values included: the rules are where corrections live now); a row only the model
could answer keeps its value, and an empty one is queued for the model.

## Consequences

- No schema migration (rules are rows; the definition JSON gains optional keys) and no protocol change or client build.
- MCP: `define_collection` and `apply_change` take the new compute shapes and `recompute_field`; `describe_collection`
  shows `compute` with field names on both sides and a `lookups` report.
- A `lookup` reads only the named collection's key and value fields; it sends nothing anywhere.
- Groceries, after deploy: `define_collection section_rules {pattern: text, section: text}`, then
  `apply_change groceries [update_field section compute: [lookup section_rules (contains, learn), model]]`; the PR
  that adds this gives the exact calls.

Not done yet:
- recomputing dependent fields automatically when rules change (a flag per lookup, if wanted);
- a lookup over several source fields, or one whose key is not text;
- a learned rule that generalizes (e.g. "cucumbers" teaching "cucumber"): rules are the place a person adds those.
