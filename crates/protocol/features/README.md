# Layout features

One file per piece of additive layout vocabulary added after schema 12
(docs/decisions/layout-features.md). The file name is the feature's name and goes on the wire, so never
rename or delete one that has shipped. The file's text is the feature's description; it becomes the doc
comment of `protocol::feature::<NAME>`.

To add a feature, e.g. a new node `core://Foo`:

1. Add `features/foo.md` with a line or two on what it adds.
2. In `crates/protocol/src/schema.rs`, add an arm to `node_features` (or `intent_features`) that
   inserts `feature::FOO` when a node uses it.
3. That's all on the protocol side: no number to bump. Clients built with the file send `foo` in
   `x-construct-features`; the pod serves layouts that use `core://Foo` only to them, and older clients get
   the newest version they can draw.

The base vocabulary of schemas 1–12 is named in `protocol::BASE_FEATURES` (decision `named-vocabulary`).
To remove vocabulary, record the removal there (the schema that removed it) and stop drawing it: clients of
that schema no longer list it, the pod retires only the versions that use it, and older clients keep them.
