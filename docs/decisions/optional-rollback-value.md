# An optimistic mutation's rollback_value is optional

Date: 2026-11-06. Related: D34, [composable-actions](composable-actions.md), [collections](collections.md).

`action.mutation.optimistic` required `rollback_value`. The engine only reads it when a refused change has to put
back a field that had no value before (`prev_value` null or absent): every other refusal restores the value the
entity had, for `field` and each `also` field alike. So every like, tick and delete in every layout carried
`"rollback_value": false` (Pixelfed's like and boost, the Collections hints' toggle and delete, every agent-built
Groceries or todo row), a field an agent had to learn, fill in and get right, for a case that restores "nothing"
either way: `null` and `false` both read as not set to `core://Conditional`, `IconButton` `active` and
`core://Switch`. A layout that left it out was refused as invalid.

Decision: `rollback_value` is optional and defaults to `null`.

- Additive and server-only: the protocol type reads a missing field as `null` and still **writes it every time**
  (as `null`). The pod parses every stored and served layout through these types, so a layout written without it
  is stored and served with `"rollback_value": null`, and released builds, which require the field, read it as
  before. No new feature, no client build, no layout file changes.
- The app's Kotlin model still requires the field: the pod always fills it in, and the app re-encodes intents for
  the engine with it (a Kotlin default would drop it on encode).
- The vocabulary (`describe_vocabulary`), the Collections layout hints and `docs/layouts.md` list it as optional and
  drop it from their snippets, so new layouts are a field shorter per mutation.

Existing layout and component files keep the field (they are append-only); nothing needs rewriting.
