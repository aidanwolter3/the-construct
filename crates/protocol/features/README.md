# Layout features

Every piece of the layout vocabulary is a named feature, one file here (decision `named-vocabulary`). The file
name is the feature's name and goes on the wire, so never rename one that has shipped. The file's text is the
feature's description; it becomes the doc comment of `protocol::feature::<NAME>`. There is no schema number:
a client sends every feature it draws in `x-construct-features`, and the pod serves it the newest version whose
features are all in that list.

To add a feature, e.g. a new node `core://Foo`:

1. Add `features/foo.md` with a line or two on what it adds.
2. In `crates/protocol/src/features.rs`, add an arm to `node_features` (or `intent_features`) that
   inserts `feature::FOO` when a node uses it.
3. That's all on the protocol side. Clients built with the file send `foo`; the pod serves layouts that use
   `core://Foo` only to them, and older clients get the newest version they can draw.

To remove one, move its file to `removed/` (keep the arm, so the pod still finds it in old versions) and stop
drawing it. Clients built after that don't list it; the pod marks only the versions that use it `retired` for
them, and keeps serving those versions to older clients that still draw it. Keep the removed field in the Rust
types so old versions still parse. `removed/legacy_nodes.md` stands for node types this build doesn't know (the
`pixelfed://` widgets of the first layouts, D37) and stored layouts it can't read.
