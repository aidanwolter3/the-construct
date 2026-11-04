# Database migrations are named steps, not numbered ones

Date: 2026-11-04. Related: D12, D13, D32, D40, [decisions README](README.md) (the same fix for decision numbers).

Every database counted its migrations: the pod's `server.db` and the server's `host.db` in `PRAGMA user_version`,
each adapter in its `adapter_migrations` row, and the engine's `cache.db` in `user_version` against
`SCHEMA_VERSION`. Step `i` upgraded version `i` to `i + 1`. Two PRs that each added a step both took "the next
number", so whichever merged second renumbered its step, its comments, the version its tests built databases at,
and `SCHEMA_VERSION`. With several sessions working in parallel this happened most days.

Decision: a step has a name, and a database records the names that ran.

- A list is `[Step::sql("name", ...), Step::rust("name", f)]` (`pod_core::migrations`); the engine's is
  `[("name", sql)]`. Names are lowercase words joined by `_` and unique in their list (checked when the list runs,
  and by a test). A name is the step's identity in every database that ran it: never rename, edit or remove a
  step that shipped. Add new steps at the end.
- `schema_migrations(scope, name)` records what ran: scope `core` for the pod's own list, the adapter's name for an
  adapter's, `host` for `host.db`. The engine's `cache.db` has one list, so its table is `schema_migrations(name)`.
- Steps run in list order, each once, in an IMMEDIATE transaction that checks its name again first, so two
  processes opening one database never both run a step.
- **Existing databases upgrade in place.** The first time a scope has no recorded names, the old counter
  (`user_version` or the adapter's `adapter_migrations.version`) says how many leading steps ran, and those names
  are recorded. Nothing else changes in the data.
- The old counter stays up to date: after each step it is set to the number of leading steps that ran. A build
  from before this decision (a rollback on staging, an older app on a phone) still reads a correct count.
- A `cache.db` holding a name this build doesn't know was migrated by a newer app build: it is used as is and
  nothing runs, as before with a version above `SCHEMA_VERSION`. The pod ignores names it doesn't know.
- `SCHEMA_VERSION` is now the length of the engine's list, not a number to bump.

Two PRs that each add a step now only touch the last lines of the same list: git may report a conflict there, and
the fix is to keep both lines. Their order doesn't matter to databases that already ran one of them, since each
name runs once wherever it sits.

Layout files keep their numbers (`crates/<adapter>/layouts/<app>/NNNN.json`): each file is a whole snapshot of the
app, so two PRs that change the same app must merge their layouts into one file, and the add/add conflict on the
number is what makes that happen.
