# A list's where takes several conditions, the same shape as a count's

Date: 2026-11-05. Related: D44, [text-dates-and-counts](text-dates-and-counts.md), [collections](collections.md),
[composable-actions](composable-actions.md), [named-vocabulary](named-vocabulary.md).

`core://List` `where` (D44) is one condition, `{field, equals}`. `core://Text` `count` (decision
`text-dates-and-counts`) filters the same entities with a list of the same conditions, all of which must match. So
the vocabulary had two shapes for one idea, and the list's was the narrower one. That mattered for every
Collections app: a deleted row stays loaded with `deleted: true` until the next load, so a collection list spends
its one `where` on `{field: "deleted", equals: "false"}` and has none left. A Log with a category tab, Groceries
with To buy / In cart, or a todo list showing only the open ones could count such rows ("3 left") but not list
them. Considered: having the client hide entities with `deleted: true` everywhere (rejected: it hard-codes one
collection convention into every list), and a second field such as `and_where` (rejected: another narrow option).

Decision: a list's `where` may also be an array of conditions, read exactly as a count's `where`: every condition
must match, and a blank one (`$form.<field>` while unset) matches everything.

```json
"where": [{"field": "deleted", "equals": "false"}, {"field": "category", "equals": "$form.tab"}]
```

- Additive: the single object stays valid and means what it did; layouts are served in the form they were
  written (`protocol::WhereClause` is untagged). The array form is feature `list_where_all`, so clients that
  draw only `list_where` keep getting versions they can draw.
- The client applies a list's conditions and a count's with the same code (`whereConditions`, `matchesAll`).
- The pod checks every condition (a field and a value, at least one condition); Collections checks each
  condition's field against the collection, at `/where/<i>/field`. `describe_collection`'s notes mention the
  array form.

Needs a client build (the renderer reads the array). No layout file changes: the built-in layouts use one
condition each.
