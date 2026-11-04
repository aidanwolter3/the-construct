# Every piece of layout vocabulary is a named feature; clients list what they draw

Date: 2026-11-04. Related: D36, D37, D51, [layout-features](layout-features.md),
[composable-actions](composable-actions.md).

`action.mutation.create`'s `dismiss` (D51) is the same as `action.sequence [create, action.navigation.back]`
(decision `composable-actions`), so it should go. Removing vocabulary used to mean bumping `SCHEMA_VERSION`
and adding a `RETIRED` entry, which retires *every* version written for an older schema: each built-in app
would need a copy at the new number, every custom app would need re-stamping, and older clients would stop
getting any new layout even though they draw a superset of the new vocabulary. Rewriting old layouts on the
fly when serving them was considered and rejected: what the pod serves should be what was written.

Decision: the vocabulary is a set of named features, and a layout is drawn by a client when the features it
uses are a subset of the ones the client lists.

- **Base vocabulary is named.** `protocol::BASE_FEATURES` names everything schemas 1–12 added (`row`,
  `list_filter`, `sticky`, …), each with the schema that added it and, when removed, the schema that removed
  it. Additive features stay one file each in `crates/protocol/features/` (decision `layout-features`).
  `protocol::required_features` returns everything a layout uses; `required_schema` is derived from it.
- **Schema 13 (`NAMED_SCHEMA`) changes the negotiation, not the vocabulary.** A client of schema 13 lists
  every feature it draws in `X-Construct-Features` (`protocol::drawn_features`). A client of an older schema
  lists only additive features; its schema's base vocabulary is implied (`implied_features`), so every
  existing client keeps working with no build.
- **Removing vocabulary = no longer listing it.** This build drops `create_dismiss` (removed in 13). The pod
  marks the versions that use it `retired` for clients that don't list it (`ClientCaps::can_draw`), and
  serves them unchanged to older clients that still draw them, so the field stays in the Rust types. Nothing
  else is retired.
- **Layout files keep declaring schema 12** (`protocol::LAYOUT_SCHEMA`; the pod refuses higher). Numbered
  vocabulary ends there, so a new file is served to every client, older or newer, that draws its features.
- **Stored versions:** pod migration v17 works out the full feature list of every stored version (built-in
  and custom), so versions using `dismiss` are found on existing pods too.
- **Custom-app writes** that use vocabulary this build no longer draws are refused, with a hint
  (`dismiss` → the sequence). `describe_vocabulary` no longer lists it.
- `pixelfed_home` 0009 is 0008 with Share written as `[create, back]`. The Kotlin client drops `dismiss`.

The coarse `RETIRED` table stays for what it did (schema 3 dropped the `pixelfed://` widgets, D37) and is
still checked as a floor; a future removal needs only a `BASE_FEATURES` entry (or, for an additive feature, a
removal record) and a client that stops listing the name.

Not done: a custom app's `core://Ref` to a built-in part is counted by its stored form, so the features of
the part it expands to are not checked per client (as before).
