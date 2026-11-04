# Layout vocabulary is named features only; the schema number is gone

Date: 2026-11-04. Related: D36, D37, D38, D51, [layout-features](layout-features.md),
[composable-actions](composable-actions.md). Supersedes the schema numbers of D36/D38 (and `RETIRED`, D37).

`action.mutation.create`'s `dismiss` (D51) is the same as `action.sequence [create, action.navigation.back]`
(decision `composable-actions`), so it should go. With schema numbers, removing vocabulary meant bumping
`SCHEMA_VERSION` with a `RETIRED` entry, which retires *every* version written for an older schema: each app
would need a copy at the new number, custom apps would need re-stamping, and older clients would stop getting new
layouts although they draw a superset of the new vocabulary. Rewriting old layouts on the fly when serving them
was rejected: what the pod serves should be what was written. Decision `layout-features` had already moved
additive vocabulary to named features because one shared counter made parallel PRs conflict; the base vocabulary
still lived behind the numbers.

Decision: every piece of the vocabulary is a named feature, and there is no schema number.

- **One file per feature**, `crates/protocol/features/<name>.md`, for everything schemas 1–12 added too
  (`row`, `list_filter`, `sticky`, …). `protocol::required_features` maps nodes and intents to them. Adding
  vocabulary is adding a file, so parallel changes never touch the same line.
- **Negotiation:** the engine sends every feature it draws in `X-Construct-Features`. The pod records each
  version's features (built-in files when they sync, custom-app writes with their built-in parts expanded) and
  serves a client the newest version whose features are a subset of its list (`ClientCaps::can_draw`). A request
  without the header is a tool, not a client (curl): it gets the latest version. `X-Construct-Schema`,
  `SCHEMA_VERSION`, `RETIRED`, `required_schema` and the `schema` of versions are removed; the versions API no
  longer reports `schema`.
- **Removing vocabulary = moving its file to `features/removed/`** (`protocol::REMOVED_FEATURES`). Clients built
  after that don't list it; the pod marks only the versions that use it `retired` and keeps serving them to older
  clients that list it. The field stays in the Rust types so old versions still parse. Removed now:
  `create_dismiss` and `legacy_nodes` (node types this build doesn't know, i.e. the `pixelfed://` widgets of D37,
  and stored layouts it can't read; this replaces `RETIRED = [(3, 3)]`).
- **Layout files** are `{"note", "layout", "parts"?}`. Deployed files keep their `"schema"`; it is not part of the
  content hash and is ignored. `pixelfed_home` 0009 is 0008 with Share written as `[create, back]`.
- **Pod migration v17** works out the features of every stored version and drops the `schema` column.
- **Custom-app writes** using a removed feature are refused with a hint (`dismiss` → the sequence);
  `describe_vocabulary` lists the features instead of a schema number.
- The Kotlin client drops `dismiss` and the versions' `schema`.

Breaking, on purpose: clients built before this send `X-Construct-Schema` and, at most, their additive features,
so the pod finds them drawing almost nothing and serves `no_compatible_version`. They need this build. Nothing
else forces an update again: a later build that adds or removes a feature still gets every version it draws, and
older builds keep getting every new version that uses only what they list.

Not done: a custom app's `core://Ref` to a built-in part is recorded with the features of the part it expanded to
when written; a ref that follows the built-in (no `version`) can expand to a newer part with other features.
