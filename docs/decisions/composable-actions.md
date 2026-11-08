# Actions compose: action.sequence and action.local.set instead of per-feature options

Partly superseded by [held-local-sets-and-list-item-reuse](held-local-sets-and-list-item-reuse.md): a load that can't know of a local set yet keeps it.

Date: 2026-11-03. Related: D34, [layout-features](layout-features.md), [collections](collections.md).

A custom grocery app needed a recipe ingredient's + to add a grocery row and show the ingredient as added. The
first attempt gave `action.mutation.create` a `marks` option (fields of other entities set and rolled back with
the create). It worked, but it was an option shaped by one screen: the next similar need ("add, then close the
screen", "add, then clear a flag") would have needed another option, another client build. Each such option makes
the vocabulary larger without making it more expressive.

Decision: grow the action vocabulary with small primitives that compose, not with options on existing intents.

- `action.sequence {steps}` (feature `action_sequence`) runs intents in order as one tap. The whole sequence is
  bound once, so one `$nonce` names the same new entity in every step. Navigation steps are resolved by the
  client, the rest by the engine.
- `action.local.set {entity_uri, field, value_expr, also?}` (feature `local_set`) sets fields of a loaded entity on
  the device only: no request, no outbox row, no rollback. The next load of the entity from the pod replaces it.
  It is for view state (added, expanded, selected) that a `core://Conditional` reads.
- `UiNode::own_intents` returns the steps inside a sequence too, so feature detection, schema checks and the
  pod's validation (custom-app endpoints, entity schemes) see every nested intent; a sequence cannot hide an
  action a layout may not use.

The ingredient's + becomes `action.sequence [create grocery row, local.set added on $item.uri]`.

Limits: a local set is not tied to the create in the same sequence, so if the pod rejects the create the "added"
state stays until the entity is next loaded. State that must survive reloads or match the pod's data (is this
ingredient already on the list?) needs a lookup binding over the collection, not local state; not built yet.

When a new layout need comes up, first try to express it with existing primitives; add vocabulary only when it is
a primitive several apps would combine.
