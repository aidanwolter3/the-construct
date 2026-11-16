# The recipes source is removed; recipes live in Collections

Date: 2026-11-16. Related: D41 (superseded), [recipes-start-empty](recipes-start-empty.md) (superseded),
[collections](collections.md), [named-migrations](named-migrations.md),
[content-addressed-construct](content-addressed-construct.md), issue #311.

The `recipes` source (`crates/source-recipes`, D41) was the first source with no upstream: typed `recipes_*` tables
in the pod's SQLite, a read-only `recipes` blueprint (`@core/recipes`), the `recipes:all` feed, `recipes://`
entities and `/v1/recipes/...` screen data. Since recipes-start-empty, new pods start with no recipes and the app
has no way to add any. Collections now does everything it did and more: the agent declares a recipes collection
(refs to ingredients, rollups, a grocery list built by an action), the user adds and edits rows, and any layout
shows them. Keeping the source meant a second, frozen way to hold the same kind of data, its layouts, tests and
test seed, for a blueprint nobody can fill.

Decision: the recipes source is removed from the workspace and from the server's adapters (`server::adapters`):
the crate, its layout files, its blueprint, endpoints, feed and entities. Recipes are a collection.

- **Feeds of unknown sources are dropped when a pod opens.** A pod whose home bound `["recipes", "all"]` would
  keep that feed for good: the tools only grow home's feeds (feeds named when creating a layout are added; changing
  home's list is refused while a layout binds a feed it would drop), and every check of home's scope would meet a
  feed no source fills. So on open the pod drops every feed naming a source the build doesn't have, on both layout
  systems: from each row's `custom_apps.feeds_json` (`pod_core::custom::drop_unknown_feeds`), and on a switched pod
  by writing a new home element, authored `pod`, without those feeds (`pod_core::construct::write::drop_unknown_feeds`).
  If that write is refused, the pod only logs a warning and opens anyway. This holds for any source removed later,
  not just recipes.
- **Dangling names and history are left.** A `@core/recipes` name on a switched pod and the old `recipes` rows in
  the layout version tables stay as they are: the layout sync only warns about a blueprint with no files, and
  blueprints are listed from the catalog, so nothing lists them. A user's layout built on the blueprint is
  rebuilt over a collection by the agent.
- **The `recipes_*` tables are kept.** Migrations, not resets (CONTRIBUTING.md): the tables and their recorded
  migrations belonged to the removed adapter, so there is no owner left to drop them in a named step without
  inventing pod-level cleanup for removed sources. Nothing reads them, and keeping a pod's data costs nothing: the
  recipes a pod held are still in its database if someone wants to move them into a collection.

Consequences: D41 and recipes-start-empty are superseded. No client build: no vocabulary changes. Not done: an
import of a pod's old `recipes_*` rows into a collection, and dropping those tables.
