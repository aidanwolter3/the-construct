# A create's `dismiss` is legacy: closing the screen is `action.sequence [create, back]`

Date: 2026-11-04. Related: D51, [composable-actions](composable-actions.md), [layout-features](layout-features.md).

D51 gave `action.mutation.create` a `dismiss` option so the New post screen's Share closes the screen. Decision
`composable-actions` then added `action.sequence`, which says the same with primitives every app already has: the
create, then `action.navigation.back`. Two ways to write one thing is vocabulary an author (Claude, over MCP) has to
learn and choose between, and `dismiss` only ever covered "close after a create": "close after a mutation" or
"close after a create and a local set" were already sequences. The client also carried a separate close path for it
(`Navigation.dismiss()`), next to `back()`.

Considered: removing `dismiss` from the protocol (breaking: schema-9 layouts `pixelfed_home` 0005–0008 use it, and
they are still served to clients without the `action_sequence` feature; it would need a `SCHEMA_VERSION` bump and a
`RETIRED` entry for nothing a user sees), and rejecting it in custom-app writes (an existing custom app that has it
could not be edited until rewritten; not worth it while the vocabulary no longer offers it).

Decision: `dismiss` stays on the wire, legacy only.

- New layouts close a screen after a create with `action.sequence {steps: [<the create>, {"intent":
  "action.navigation.back"}]}`. The whole sequence is bound at once, so a blank required field still sends nothing
  and keeps the screen open, exactly as before. `pixelfed_home` 0009 is 0008 with Share written that way; it needs
  feature `action_sequence`, so clients without it keep getting 0008, which looks and behaves the same.
- The MCP vocabulary (`describe_vocabulary`) no longer lists `dismiss` and tells authors to use the sequence.
- Clients run a legacy `dismiss` as that sequence (the create, then back), so there is one close path. The form of
  the closed screen is dropped with it, as on Back.
- `protocol::required_schema` still counts `dismiss` as schema 9, and old layout files are not edited (CLAUDE.md).

Not done: retiring `dismiss` from the protocol. If a later breaking change bumps `SCHEMA_VERSION` and retires the
schema-9 versions anyway, `dismiss` can go with them.
