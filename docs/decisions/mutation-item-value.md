# A mutation can set a field to the tapped row's value

Date: 2026-11-27. Related: D4, [composable-actions](composable-actions.md),
[optional-rollback-value](optional-rollback-value.md).

A picker changes one entity from a row of another: Dinner tonight's recipe rows set today's `days` row's
`dinner_meal` to the tapped recipe. The optimistic step could only set fields to what D4's expressions give, a
literal or the changed entity's own fields, and the renderer left every `value_expr` unbound. So the tap set
`dinner_skipped` at once, while the meal (in the payload only) and its name (a lookup the pod computes) reached the
device only once the pod's write came back as a data event and the day reloaded. Until then, or when that event
didn't reach the phone, Today's Dinner row kept showing "Pick what you're cooking" after a pick, though "Not
cooking tonight", which needs no row's value, showed at once.

Decision: a mutation `value_expr` (the primary one, or one in `also`, of `action.mutation.optimistic` and
`action.local.set`) that is exactly `$item.<field>` or `$route.<name>` is bound by the renderer when it binds the
intent, to that value as a JSON literal (`"m1"`, a number, `null` when missing). The engine then sets it like any
literal. Anything else in a `value_expr` stays for the engine, as before: `$item.` inside a larger expression is
not bound, so D4's subset is unchanged and nothing that parsed before means something new.

- A named layout feature, `mutation_item_value`, which `required_features` adds for such an expression. Older
  builds left it unbound (the engine refused it and the tap did nothing locally), so they get the layout's
  previous version. Needs a new client build: desktop, Android, and the web client with the deploy.
- Only exact values, not interpolation into an expression: a value with quotes is bound as a JSON string, never
  spliced into expression text.
- The pod's write is unchanged; the pod's answer still replaces the local values on the next load.
