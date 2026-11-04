# Custom apps keep their own data in Collections, one generic data source that Claude declares data in

Extended by [collection-computed-fields](collection-computed-fields.md) (computed fields, multi-key order), which
amends "no computed fields" below.

Date: 2026-11-03. Related: D37, D41, D52, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[mcp-connector](mcp-connector.md), [layout-parts](layout-parts.md), issue #54.

Custom apps (#45) could only show data an existing source holds (Pixelfed, Recipes). A user who asks Claude for a
todo app needs new data in the pod, and data sources are adapter code the developer ships; D41 noted that "the
generic `entities` table for new data types is not built yet". Considered: letting Claude write data sources (rejected
in #45: code, upstream calls and secrets from a prompt), a schemaless JSON store (rejected: layouts would bind fields
nothing checks, and a rename would orphan data), and moving Recipes into the new store (deferred, see below).

Decision: one built-in, generic data source, **Collections** (`crates/app-collections`, source id `collections`).
Claude never writes a data source: it *declares* data, which this source stores, validates and serves. A collection
is a description of data, with no logic: no computed fields, triggers or upstream calls.

## The source

- **Pattern feeds** (pod-core): a `FeedSpec` named one whole `{name}` placeholder answers to any `[a-z0-9_]+` name,
  at most one per source, and its query keys may use the placeholder. Collections declares feed `{collection}`
  (key `collection:{collection}`), so an app lists `["collections", "todos"]`. Built-in apps can't list a pattern.
- **Routes**, declared in the manifest with the collection as a parameter (so #45's capability check covers them):
  data path `/v1/collections/{collection}/rows`; actions `…/create`, `…/update`, `…/delete`. Also `…/export` (not in
  the manifest: no layout may call it). Any caller that reaches the pod may use them, as with Recipes: the data
  belongs to the pod, not to an account.
- **Rows** are entities `collections://<c>/<id>`: every live field by name (an empty one reads as its default, else
  `null`), every alias too, and `id`, `deleted` (always `false` when served), `created_at`, `updated_at`.
- **Source checks** (pod-core hook `AppAdapter::check_custom_layout`, run on every custom-app write after the core's
  checks): every collection a layout names exists (feeds, query keys, URIs, routes) and every row binding names a
  field it has: `$item.<f>` in a row list and its `filter`/`group_by`/`where` fields, `${collections://…#f}`, a
  mutation's `field`/`also`/`entity.<f>`, a create's `entity` keys, the routes' payload keys (and `id` on update and
  delete). Errors carry the pointer and the allowed names.
- **Data events:** a write publishes `event: data` to every session (`AppState::publish_data_all`; `DataEvent` gains
  an optional session), so a second device reloads at once.

## Storage and writes

- Tables `collections_defs` (every version of every definition; the latest is live), `collections_rows` (values as
  JSON keyed by **field id** `f<n>`, soft `deleted_at`, the create's `client_id`, unique per collection),
  `collections_aside`, `collections_edits`, `collections_edit_rows`.
- **Types:** `text`, `number`, `bool`, `date` (`YYYY-MM-DD`), `choice` (options); `optional`; `default`. Writes are
  read leniently the way forms send them (`"3"` for a number, `"true"` for a bool, a timestamp's date).
- **Lenient across definition versions:** names resolve through aliases, removed fields and system fields in a write
  are ignored, an unknown name or a wrong type is refused (422, so the client rolls back its optimistic change). A
  todo typed offline before a change therefore still arrives.
- **Idempotent creates:** a repeated `client_id` answers the existing row (an outbox retry). An `id` of
  `pending-<client id>` finds the row its create made (a tick sent right after an add).

## Changing a collection

- **One change set** (MCP `apply_change`): change ops plus the app layouts that go with them. The pod stages the app
  edits (pod-core `custom::stage_layout`: the core's checks), then checks **every** custom app with a Collections feed
  against the new definition (the staged layout for apps in the set, the latest otherwise). If one would break,
  nothing applies: `breaks_app` names the app, its title, the pointer and the binding. Otherwise the definition
  version, the row conversions and the app versions are written in one transaction (`custom::commit_staged`), and
  devices get the usual SSE push.
- **Per kind of change:** add (must be optional or have a default); rename (fields have permanent ids; the old name
  stays an alias forever, so pending writes and older pinned app versions keep working); remove (soft: hidden, data
  kept 30 days, `restore_field`; then purged and the field can't return); change a type or options (converted row by
  row; values that don't convert move to `collections_aside`, are reported to Claude — "3 todos had due values it
  couldn't read" — and come back when a later change can read them); reshape (split, merge, backfill) is a row edit.
- **Undo:** `restore_collection_version` makes an older definition current (a new version): removed fields come back
  with their data, fields added since are soft-removed, types convert back. It is a change set too. App layouts keep
  their own history (`restore_version`).

## Row edits by Claude (Collections only)

A decided exception to #45's "the MCP credential can't touch app data", for conversions, backfills and imports. It
touches only Collections' tables; Pixelfed, Recipes and every other source stay out of reach.

- `edit_rows` (create, update, delete, restore; at most 500 rows) first answers a **preview**: counts, a sample of
  five rows before and after, and a confirm token (a hash of the edits, the definition version and the affected rows'
  current state). Nothing changes until it is called again with that token; if anything changed meanwhile, it answers
  `stale_preview` with a new preview. The instructions tell Claude to show the preview to the user first.
- Each applied edit records every row's state before and after; `undo_edit` restores them as one step (rows changed
  since are left alone and reported). Undo records are kept 30 days.

## Limits

Per pod, temporary pods too (`app_collections::Caps`): 20 collections, 40 live fields each, 5,000 live rows and 4 MiB
of values across collections, 500 rows per edit, 4,000 characters per text value. Definition history: 500 versions
per collection. Change sets that edit apps count against the custom-app write rate. Purge window: 30 days (deleted
rows, removed fields' data, kept-aside values, undo records), swept hourly.

## The four open questions of #54

1. **Deleting an item:** a `deleted` flag, no new intent. A delete is an `action.mutation.optimistic` of field
   `deleted` to `true` against `…/delete`; the List hides it at once with `where: {field: "deleted", equals:
   "false"}` (a create's entity says `"deleted": false`), and the pod stops serving it. No schema bump or client build.
   The row stays restorable for 30 days (`edit_rows` `restore`).
2. **Display formatting** ("Due tomorrow", counts): not in the data source. Relative dates depend on the viewer's
   time zone and "now", which the pod doesn't know and a cached screen would get wrong, so formatting belongs in the
   client as new generic layout vocabulary (a named layout feature), not computed fields. Until then dates show as
   `YYYY-MM-DD` (issue #55).
3. **Reminders** ("notify when `due` passes"): out of scope. A later step may add a few declarative rules on a
   collection that feed the D52 notifications pipeline (the host's poller would evaluate them); no stored code (issue #57).
4. **Links between collections and nested lists:** not now; todos need neither. Measured against Recipes (D41),
   Collections can already express a list with categories (`choice` + `group_by`), search (`filter`), a per-category
   filter (`where`), typed numbers and dates. It can't express linked tables (ingredients and steps of a recipe: needs
   a `link` field type and a per-row data path), fraction and unit formatting (point 2), or a frozen import (an
   `edit_rows` import is the replacement). Moving Recipes into Collections stays deferred.

Not done yet: user confirmation of a row edit happens in the chat with Claude, not in the app (the pod has no way to
ask the user; MCP elicitation needs a server stream this endpoint doesn't have); deleting or renaming a whole
collection; formatting (point 2) and reminders (point 3). (The todo icons are done: feature `todo_icons` adds
`check`, `check_box`, `check_box_outline_blank` and `delete`, and the snippets use them; issue #56.)
