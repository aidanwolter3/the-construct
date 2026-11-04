# Additive layout vocabulary is a named feature, not a schema bump

Superseded in part by [named-vocabulary](named-vocabulary.md): the base vocabulary is named too and the
schema number is gone; clients list everything they draw.

Date: 2026-11-03. Related: D34, D36, D37, [decisions README](README.md).

Every new node type or attribute took "the next schema number". Each PR changed
`protocol::SCHEMA_VERSION`, appended to the schema list and the compatibility tests, and wrote that number into
its layout files and Kotlin comments, so parallel PRs always conflicted. Worse, two PRs that both claimed 13 could
merge cleanly, leaving schema-13 clients that claim to draw vocabulary they don't have. Of schemas 2–12, only 3
was a breaking change. The rest only needed "this client can draw X", which doesn't need an order.

Decision: `SCHEMA_VERSION` stays at 12 (the last additive bump, `core://List` `focus`) as the base
vocabulary. It is bumped only by a breaking change, together with a `protocol::RETIRED` entry (D36).
Additive vocabulary is a **layout feature**:

- One file per feature, `crates/protocol/features/<name>.md` (its description). `crates/protocol/build.rs`
  collects them into `protocol::FEATURES` and `protocol::feature::<NAME>`, so parallel PRs add different files.
  A name goes on the wire, so it is never renamed or removed once shipped.
- `protocol::required_features(node)` maps the vocabulary to features (`node_features` / `intent_features` in
  `schema.rs`), the way `required_schema` maps it to a number.
- Clients send `X-Construct-Features: a,b` (this build's `FEATURES`) beside `X-Construct-Schema`. No header
  means no features.
- The pod works out each version's features when it appends it (`app_versions.features`, migration v15).
  Layout files don't declare them, so new files keep `"schema": 12`. A version is served to a client only if
  the schema is compatible and the client has every feature (`ClientCaps::can_draw`). Otherwise it is
  `update_required`, as before, and the client gets the newest version it can draw. The versions API adds
  `features` per version when there are any.
- Adding a feature still needs a client build (the renderer has to draw it). The PR template's "Schema bump"
  line becomes "Schema bump / new feature".

Layout file numbers stay as they are (`layouts/<app>/NNNN.json`, contiguous). A layout file is a whole snapshot
of the app's layout. If two PRs that change the same app named their files so that both merged cleanly (for
example by date), whichever deployed last would silently drop the other's changes. Today both PRs try to add the
same `NNNN.json`, git reports an add/add conflict, and the second PR has to merge the two layouts into the next
number. That conflict is real and is the right place to resolve it. What did go: the hand-maintained
`layout_file!` lists. Each adapter's `build.rs` (shared code in `crates/pod-core/build_support/`) embeds every
`layouts/<app>/*.json`, so adding a version is adding one file. Different apps never conflict.

Not done: folding features into a new base number. If the header ever gets long, one PR can bump
`SCHEMA_VERSION` and treat the old features as part of it. Nothing needs that yet.
