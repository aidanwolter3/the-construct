# A collection is deleted once nothing shows it, and stays restorable for 30 days

Date: 2026-11-06. Related: [collections](collections.md) ("not done yet": deleting a whole collection),
[layouts-not-apps](layouts-not-apps.md) (home owns the feeds), [collection-lookups](collection-lookups.md). Issue #107.

Once a collection existed, nothing could remove it: `apply_change` removes fields and fillers but not the
collection, so a user who asked to remove "the daily poem and its collection" kept the collection, its rows and
home's feed. Considered: deleting at once (rejected: the pod is the only copy of this data, and removed fields and
deleted rows are already kept 30 days); refusing while home's feeds list it (rejected: under
[layouts-not-apps](layouts-not-apps.md) every layout reports home's feeds, so the feed list says nothing about who
shows a collection, and a stale feed would block the delete forever); a separate "edit feeds" tool (not needed:
`rename_layout` on home already replaces home's feed list, refused while a layout binds a feed it drops).

Decision:

- **MCP `delete_collection {collection, base_version?}`** deletes the definition with its rows, fillers, alerts and
  reminders. It is refused (409 `collection_in_use`) while a layout's latest version binds the collection (its query
  key, a row URI or a route: the same check as `list_collections`' `used_by`), naming each layout and its title, or
  while another collection's `lookup` field reads it, naming the collection and field. The `events` collection
  (a system collection) can't be deleted. A `base_version` other than the latest is refused (`stale_version`).
- **Home's feed goes with it.** If home still lists `["collections", <c>]` (nothing binds it, or the delete would
  have been refused), the delete drops it first, through the same home feed update as `rename_layout`: a layout that
  names a collection that doesn't exist fails validation, so a stale feed would block every later save of home.
- **Soft, 30 days.** A deleted collection is a row in `collections_deleted` (migration `deleted_collections`). Its
  data stays but everything that reads live definitions skips it: routes answer 404, fillers and reminders stop, its
  rows don't count toward the caps. `list_collections` lists it under `deleted` with `purged_after`.
  **`restore_collection {collection}`** brings it back as it was (checked against the collection and row caps) and
  restarts its fillers; its feed isn't added back to home (a layout that shows it again adds it). The hourly purge
  drops it after 30 days, every table of it; its name is then free. Until then `define_collection` refuses the name
  (`collection_deleted`).
- **Held to the caps.** Deleted collections' data counts toward what a pod may keep for undoing (twice the bytes
  cap, with deleted rows, kept-aside values and undo records): over it, the oldest goes before the 30 days are up, so
  deleting and defining collections under new names can't fill the volume (security audit).

Pod-only: no client build. The two new MCP tools need "Refresh tools list" in the connector settings.

Not done yet: renaming a whole collection; a deleted collection's alerts and reminders that already notified are
forgotten when it is purged, so a restored-then-redefined name starts fresh.
