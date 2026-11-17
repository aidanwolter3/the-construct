# Custom apps, Collections and Events

A pod's apps are the user's own: any layout the [vocabulary](layouts.md) can draw, over the feeds, screen data and
actions the pod's data sources declare. They are usually built by Claude through the pod's MCP endpoint, often from a
**blueprint** (a built-in layout the server ships and updates, such as `pixelfed` or `events`). A new pod
has one app, `home`; nothing else until the user asks for it. Data an app keeps itself (a todo list, a maintenance
log) lives in **Collections**.

Decisions: [`custom-apps-and-data-sources`](decisions/custom-apps-and-data-sources.md),
[`mcp-connector`](decisions/mcp-connector.md), [`layout-parts`](decisions/layout-parts.md),
[`collections`](decisions/collections.md), [`blueprints-and-home`](decisions/blueprints-and-home.md) and the
`collection-*` decisions.

## Blueprints and the home layout

There are no apps at runtime (decision `layouts-not-apps`): the pod's units are **layouts**, and every layout is a
name in the pod's content-addressed construct (decisions `content-addressed-construct`, `one-layout-system`).
Built-in layouts are blueprints, named `@core/<blueprint>` and moved to each deploy's files; MCP `list_layouts`
answers `{layouts, blueprints}`. A layout can be a whole blueprint by reference, kept current by the pod:

```json
{"title": "Events", "icon": "dashboard", "feeds": [["collections", "events"]],
 "layout": {"type": "core://Use", "ref": "@core/events"}}
```

Name the blueprint's feeds (`list_layouts` shows them): they join home's. To change a blueprint, read it
(`get_layout`) and write your own. Every pod's **`home`** layout is the screen clients open, and the only one: it
is the user's whole interface, and it embeds or presents every other layout. It starts as the `shell` blueprint's
layout, a blank page with Notifications and Settings buttons, and the agent builds into it only what the user asks
for: one layout is a `core://Layout` as home's one slot; with more, home lists them and each row pushes its layout
(`action.navigation.open {"ref": "<layout id>", "presentation": "Push"}`); a `core://Layout` can also put a layout in
a card-sized region, or show it as a sheet or side panel while a condition holds. `@core/settings` and `@core/inbox`
are Settings and the inbox. Snippets for each are in `describe_vocabulary`. Home can't be deleted.

**What the user sees is what home reaches** (decision [`trace-from-home`](decisions/trace-from-home.md)).
`list_layouts` and `get_layout` give each layout `reached`: the shortest path from home, each step with the field
that takes it, its presentation and its label (`home → (on_swipe_down, Overlay from top) → c-…`), or "not reached
from home". A pod layout titled like a blueprint it doesn't reference gets `copy_of: "@core/<blueprint>"`: it is
the blueprint as it was when copied, and doesn't follow it; a blueprint row lists its `copies`, the layouts that
`use` it (`used_by`) and its own `reached`. The pod's MCP instructions tell agents to change the element home
actually reaches, not the blueprint it may have started from.

## The pod's layouts

A layout of the pod is a name (`c-` and 10 characters, except `home`) pointing to an element with a title, an icon
and a node; the name keeps its history, and `restore_version` moves it back. Devices show a layout's `title` and
`icon` from its element.

**Home owns the feeds** (`["pixelfed", "home"]`, `["collections", "todos"]`, `["collections", "recipes"]`, at most 32): every
layout may bind any of them, and a layout's request loads only the feeds whose query keys it binds. Feeds named when
creating a layout (or renaming one) are added to home's. Changing home's own feed list is refused (`feed_in_use`,
naming the layout) while another layout still binds a feed it would drop. Home's event stream carries every change of
the construct (a new root) and every data change of its feeds, so one engine follows them all.
When a pod opens, feeds naming a source the server no longer has (such as the removed `recipes` source) are dropped
from home's feeds (a new home element without them), so home's scope holds no feed nothing can fill (decision
[`remove-recipes-source`](decisions/remove-recipes-source.md)).

- **Validation.** Every write is checked against the sources of home's feeds: a layout may only bind query keys
  those feeds fill and call the data paths and endpoints their manifests declare. Errors name a JSON pointer and
  what is allowed. Limits: 50 layouts, 256 KiB per layout.
- **Source status.** The root gets a `core://sources/<source>` entity per source, `{title, signed_in, error?}`, so a
  layout can say when a source is signed out or failing (`${core://sources/pixelfed#signed_in}`). A layout signs in
  with `action.device` `sign_in` (`id`: a provider from `local:providers`; `value`: another server, e.g.
  `$form?.server`), e.g. a button shown while `signed_in` is false. The raw `action.auth.*` intents stay refused.
- **Components.** `{"type": "core://Use", "ref": "@core/card", "args": {...}, "children": [...]}` uses a component:
  one the server ships (`@core/card`, `@core/section_header`, `@core/workspace`, …), a blueprint or its part
  (`@core/pixelfed/screen:post`), or the pod's own (`@me/<name>`), expanded by the app and kept current by its name
  ([layouts.md](layouts.md#porcelain-components)). `"component": "core:<name>"` is still accepted when written.
  `@core/` components are read-only: `copy_component` makes one of the user's own to change.
- **Plumbing.** A layout may be a `core://Page` (any node as its root, its own header and Back) instead of a
  `core://Workspace`; the app then draws nothing around it ([layouts.md](layouts.md#structure-of-a-page)).
- **Theme.** `${local://theme#card_border}` and the other tokens are the pod's colours for the app's mode; MCP
  `set_theme` changes them and can add up to 40 named colours of the pod's own (decision `custom-theme-tokens`).
  Use a token wherever a colour goes (`page`, `card`, `muted_text`, …): a fixed `#RRGGBB`
  stays the same in dark mode, and layout writes answer `warnings` for each (decision `dark-palette`).
- **Device data.** `${local://connection#label}`, `${local://inbox#unread}` and the other
  [device data](layouts.md#device-data) need no feed.
- **Deleting.** The pod's layouts can be deleted with MCP `delete_layout`, except `home`.

## Building apps with Claude

Each pod has an MCP endpoint, `POST /v1/mcp`, so Claude can read the pod's sources and build and edit its layouts.
Writes are validated and appear in each name's history as written by Claude.

- **Claude apps (web, desktop, mobile):** add a custom connector with the URL `https://<server>/v1/mcp`. Claude
  discovers the server's OAuth endpoints, registers itself and opens a page where you type your pod code. The
  credential it gets reaches only that pod's `/v1/mcp`: no other routes, sign-ins or pod settings.
- **Apps that ask for a client ID and secret** (e.g. Gemini): set `CONSTRUCT_OAUTH_CLIENT_ID`,
  `CONSTRUCT_OAUTH_CLIENT_SECRET` and the app's callback in `CONSTRUCT_OAUTH_CLIENT_REDIRECT_URIS`
  ([server.md](server.md)), and enter the same ID and secret in the app. You still type your pod code to connect.
- **Claude Code:** `server pods mcp <name>` prints a credential, then:

  ```bash
  claude mcp add --transport http construct https://<server>/v1/mcp --header "Authorization: Bearer cmcp_…"
  ```

- **Disconnect:** `server pods revoke-mcp <name>`. Rotating the pod code keeps Claude connected.

Tools: `list_layouts`, `list_data_sources`, `describe_vocabulary`, `get_layout`, `sample_data`, `validate_layout`,
`create_layout`, `update_layout` (a whole layout or a JSON Patch, with `base_version`), `restore_version`,
`rename_layout`, `delete_layout`, `list_parts`, `set_time_zone`,
the component tools `list_components`, `get_component`, `expand` (what a layout or component becomes, as plumbing),
`define_component`, `update_component`, `copy_component`, `restore_component`, `delete_component`, and `set_theme`,
plus the Collections tools below. `render_layout_preview` and `dispatch_virtual_interaction` draw a layout with the
user's data (feeds, device data such as `local:notifications`, screens' data paths) and answer an image with every
node's bounds, then tap, scroll or type into it (nothing is written; `target_uri` opens a screen directly, `item`
and `entities` try a row with made-up data, `full_height` draws a long screen whole:
[rendering.md](rendering.md#agent-previews-mcp)). `get_layout` takes `expanded: true`. Layouts are named `id` (`app` is still accepted).
The server's MCP instructions tell Claude how to use them. After a change to the tools, the Claude connector needs
"Refresh tools list" in its settings.

### Sharing with other users on the server

Claude can share a layout, a component or the whole construct (`home`) with other users of the same server, and adopt what
they share (decision [`sharing`](decisions/sharing.md)). Share links work through Claude only: the apps have no
share screen.

- `set_handle` chooses the pod's handle (`@ann`, unique on the server); publications are named `@<handle>/<slug>`.
- `publish {name}` answers a review and shares nothing: every element of the closure, the interface (each
  collection the layout uses becomes an input, with the fields it reads and writes and a suggested schema) and
  each literal text or URL that would go out. A reference to one row is refused. `publish {name, confirm:
  <review_id>}` shares exactly what was reviewed and answers the share link (`share-…`); `leave_out` leaves
  elements out, `keep_shared: true` sends later versions without asking while they bring no new text or URLs.
  Only `visibility: link` (unlisted) for now.
- `describe_publication {link}` shows what a link holds; `adopt {link, bindings}` copies it into the pod bound to
  the user's own collections (`{"chores": {"collection": "tasks", "fields": {"title": "name"}}}`, or `{"new":
  "chores"}` for an empty one from the suggested schema) and answers the reference to put in home. Without
  bindings it proposes them.
- `list_adoptions` and `update_adoption` follow new versions while they fit the bindings; `list_publications` and
  `revoke_publication` manage the pod's own. A revoked publication keeps drawing where it was adopted.

### Public links for people without a pod

Where the server serves them (`CONSTRUCT_PUBLIC_WEB=1`, decision [`public-web-links`](decisions/public-web-links.md)),
Claude can make a page anyone can open in a browser: a copy of a layout and the Collections rows it shows,
read-only. The tools are listed only on such a server.

- `share_publicly {layout}` answers a review and shares nothing: every text and value a visitor would read, every
  URL, the rows and fields that go (only the fields the layout reads), and what was taken out (controls that write
  or open other layouts, embedded layouts, screens). `rows: [<entity URI>]` shares only some rows (one recipe);
  `item` draws the layout with one row as `$item`. `share_publicly {layout, confirm: <review_id>}` shares it and
  answers its `url` (`/s/pub-…`). Sharing the same layout and rows again updates the same link.
- The copy is made again when the pod's Collections data changes, so new rows the layout shows appear on the
  page. A layout that draws anything but Collections data (Pixelfed, Events) is refused.
- `list_public_links` lists them; `unshare_publicly {link}` stops one at once and its copy leaves the server.

## Collections

Collections is a built-in data source (`crates/source-collections`) that stores any collection Claude declares. Claude
never writes code: it declares typed fields (`text`, `number`, `bool`, `date`, `datetime`, `choice`, `ref`,
`quantity`; optional, with a default, `unique`), and the pod stores, validates and serves the rows.

| A layout uses (for collection `<c>`) | |
|---|---|
| feed `["collections", "<c>"]` | Rows `collections://<c>/<id>` under the query `collection:<c>`, with every field plus `id`, `deleted`, `created_at`, `updated_at`. A `datetime` field `<f>` also comes as `<f>_date` (`2026-11-06`), `<f>_time` (`7:30 PM`, empty for a date alone), `<f>_day` (`Fri Nov 6`) and `<f>_text` (`Fri Nov 6 · 7:30 PM`), on the value's own wall clock. |
| feed `["collections", "<c>"]`, a view | Each of the collection's views as its own query, `collection:<c>:view:<v>`: the rows it keeps, in its order (see **Views** below). |
| `GET /v1/collections/<c>/rows` | The same, as a screen's data path. |
| `GET /v1/collections/<c>/row?id=<id>` | One row (`collection:<c>:row:<id>`). |
| `POST /v1/collections/<c>/create {<field>: …, client_id?}` | For `action.mutation.create`; a repeated `client_id` answers the same row. |
| `POST /v1/collections/<c>/update {id, <field>: …}` | For `action.mutation.optimistic`; an id `pending-<nonce>` finds a row whose create was just sent. |
| `POST /v1/collections/<c>/delete {id}` | Sets `deleted`; hide such rows at once with `where: {field: "deleted", equals: "false"}`, or, with another filter, `where: [{field: "deleted", equals: "false"}, {field: "category", equals: "$form.tab"}]` (every condition must match). |
| `GET /v1/collections/<c>/export` | Everything, deleted rows and kept-aside values included. |
| `POST /v1/collections/<c>/actions/<a> {<param>: …, client_id?}` | Runs an insert-from-query, update or delete action, sent with `action.mutation.optimistic` (its `field` a view flag, e.g. `adding` on `$item.uri`); the new (or deleted) rows arrive through the feed; a repeated `client_id` is applied once. |

`describe_collection` gives ready-made layout snippets for all of these.

- **Change sets.** `apply_change` sends a collection change together with the layouts that go with it (`apps:
  [{app, layout | json_patch, base_version?}]`): the edits are written as `update_layout` writes them
  (`base_version` is the version `get_layout` shows) in the same step, and every layout is checked as devices draw
  it, bound with what home passes it. If one would break, nothing applies and `breaks_layout` names the layout and
  binding.
- **Safe schema changes.** Fields have permanent ids. A rename keeps the old name as an alias; a removal hides the
  field and keeps its data 30 days (`restore_field`); a type change converts row by row and keeps what doesn't
  convert aside. A write made under an older definition (an offline device) still lands.
- **Computed fields.** A field's `compute` is one kind or a list tried in order: `expr` (a small expression over
  the row; `entity.id` reads the row's own id), `lookup` (the value of the best-matching row of another
  collection, kept as editable rules: the longest key wins, then the oldest row; deleted rows never match), `count`
  `subject` (`{kind: "subject", from, value}`: what the row an entity URI in text field `from` names serves as
  `value`, a field or a derived name such as `alert_matched`, whatever its collection; live like a lookup, and when
  that collection's definition changes; decision `notifications-follow-their-alerts`)
  and `model` (a model's answer, in the background: the inputs waiting for a field go up to 50 to a call, and one
  whose answer fails is asked again hourly with backoff, at most 5 times; `describe_collection` counts what is
  retrying or gave up, and `recompute_field` starts again). With `[lookup (learn: true), model]` the model is asked
  only when no rule matches, and its answers and hand edits become rules; a learning lookup naming a collection that
  doesn't exist makes it (its `key` text, its `value` of the field's type). `{kind: "count", from, collection, key,
  where?: [{field, equals}]}` (a number field's only kind) is the number of live rows of `collection` whose `key`
  equals this row's `from` (ignoring case and spacing; a `datetime` key by the day it falls on, its stored wall
  clock, so a date counts the events that start on it) and that match every condition (the field's value as text,
  as a layout `where` compares: `"true"`, `"false"`, a number or text). Lookups and counts are **live**: when rows
  of the collection they read are created, changed, deleted or restored, the rows whose answer could change are
  computed again in the same write and reach devices through the normal change feed, cascading (a count's `where`
  may name a lookup field, e.g. meals → `meal_ingredients.favorite` → `ingredients.favorite_count`) for at most 8
  waves, so a cycle ends. A lookup's `value` may be `id`, the one system name it can return: the matched row's
  id, as text (into a text field, without `learn`), so a button can act on that row, e.g. `ingredients.grocery_id`
  → `/v1/collections/groceries/delete {id}`. It follows like any lookup: when that row is deleted the field
  empties (or takes the next match's id) and comes back on restore. Decisions [`collection-computed-fields`](decisions/collection-computed-fields.md),
  [`collection-lookups`](decisions/collection-lookups.md) and
  [`live-computed-fields`](decisions/live-computed-fields.md).
- **Views.** A collection's `views` (`define_collection`, or `apply_change` `set_view` / `remove_view`) are small
  derived lists served beside the whole list, each as `collection:<c>:view:<name>` (bind a `core://List`'s
  `query_key` to it; `$item.<field>` is checked as for the whole list, and a layout naming a view that doesn't
  exist is refused with the ones that do). A view is `{name, where?, date?, order_by?, descending?, limit?}`:
  every `where` `{field, equals}` must match (as text, as a layout `where` compares), `date: {field, from_days?,
  to_days?}` keeps rows whose date or datetime falls that many days from today in the pod's time zone (0 today,
  1..7 next week, negative the past; a datetime by its local date; empty dates never match), `order_by` (else the
  collection's order) sorts, and `limit` (1–200) keeps the first rows. E.g. the next three: `{name: "upcoming",
  date: {field: "start", from_days: 0}, order_by: "start", limit: 3}`. At most 10 per collection; fields are kept
  by id (a rename keeps the view), and removing a field a view reads is refused until the view changes. Views are
  worked out when the list is served; devices are told to reload dated ones just after the pod's local midnight
  (on the Collections poll, which runs while a device is signed in). Decision
  [`collection-views`](decisions/collection-views.md).
- **References.** A field `{type: "ref", collection}` holds the id of a row of that collection. Writes give the id
  or `{"<unique field>": value}` (`{"meal": {"name": "Harvest Soup"}}`), resolved to the id; anything that names
  no live row is refused. Rows serve the id; a layout reads through it with
  `${collections://<target>/$item.<ref>#<field>}` (the target must be one of its feeds). When the target row is
  deleted the ref keeps the id (reading through it shows nothing) and a restore heals it. `unique: true` (text or
  ref) keeps a value to one live row, text ignoring case and spacing, on create, update and restore; making a field
  unique fails while rows share a value. `update_field {type: "ref", collection}` turns a text field into a ref by
  the target's ids and unique text fields, keeping what matches nothing aside. `{kind: "lookup", via: <ref>,
  value}` reads a field of the row a ref points at, live.
- **Rollups.** `{kind: "rollup", collection, via, op: count|sum|min|max|any|list, field?, where?}` aggregates the
  rows of another collection whose ref `via` points at this row (and that match `where`): a count, a sum (numbers,
  or quantities), the smallest or largest number, whether there is any, or the distinct values as a ", " list. The
  field's only kind, live like a count; prefer it to `count` for new fields.
- **Quantities.** A `quantity` field parses text such as "1 1/2 cups", "2 Tbsp", "1 (14 oz) can", "1/4 cup plus 2
  tbsp" or "a pinch" (no model), stores `{amount, unit}`, and serves `<name>_text` ("1 1/2 cups", "2 Tbsp": one spelling per unit, so bind it rather than the text as
  written). Text that
  doesn't parse ("to taste") is kept and shown as written, left out of sums, and counted by `describe_collection`.
  A rollup `sum` converts within volume and weight, keeping the units written: one unit stays (4 tbsp + 2 tbsp =
  "6 tbsp"), several make one amount of the largest when it is a plain fraction (1/2 cup + 2 tbsp = "5/8 cup"),
  else stay apart ("1 cup + 3 tbsp"). Other units stay apart too ("2 cups + 3 cloves"); metric picks ml/l or g/kg.
- **Actions.** A collection may hold named insert-from-query actions (`apply_change {op: set_action}`): for each
  row of a source collection matching `where`, find the row of this collection by `match` (else create it), apply
  `update` (first deleting its children when it matches `reset_when`), and add a child row to another collection,
  values scaled by `times / per` where marked. A grocery list built this way: one row per ingredient with the total
  across recipes (a rollup sum over its entries) and the recipes it's for (a rollup list), manual items still typed
  by hand, a checked-off item starting again when a recipe adds to it. A delete action (`delete: true`, its source
  its own collection, no `match`, `create`, `update`, `reset_when`, `scale` or `child`) instead deletes every live row
  matching `where`, as a row delete does: a one-tap "Clear bag", `{"name": "clear_bag", "title": "Clear the bag",
  "source": {"collection": "groceries", "where": [{"field": "done", "equals": true}]}, "delete": true}`; the answer
  says how many it `deleted`. An update action (its source its own collection, only `update`) writes `update` to
  every live row matching `where`, as a row update does: `{"name": "mark_all_read", "source": {"collection":
  "notifications", "where": [{"field": "read", "equals": false}]}, "update": {"read": true}}`; the answer says how
  many it `updated`. A call is one edit across both collections, undone with `undo_edit`; `run_action`
  runs one over MCP. Decision
  [`collection-primitives`](decisions/collection-primitives.md).
- **Reminders.** `apply_change {op: add_reminder, field: "due", title: "Due today: {title}", unless: {field:
  "done", equals: true}, at?: "09:00"}` notifies once per row when its date arrives, in the pod's time zone
  (`set_time_zone`). Decision [`collection-reminders`](decisions/collection-reminders.md).
- **Row edits by Claude.** `edit_rows` (imports, backfills) first answers a preview and a confirm token, which Claude
  shows the user before applying with `edit_rows {collection, confirm}` (the pod keeps the previewed edits for an
  hour, so they aren't sent twice); `undo_edit` reverts it. The edits are inline, or a file: `url` (fetched with the
  `web` jack) or a body posted to `POST /v1/collections/<c>/import` with the pod token, as a JSON array of edits or
  rows, JSON lines, or CSV (a header of field names, optional `op` and `id` columns, `recipe.name` columns naming a
  ref's row by a unique field). A preview checks the whole set and lists every invalid edit by `/edits/<i>` or the
  file's line (decision [`bulk-row-edits`](decisions/bulk-row-edits.md)). `add_row` writes one row the user just
  asked for at once, undoable the same way (decision [`collections-add-row`](decisions/collections-add-row.md)).
- **Limits per pod:** 50 collections (decision `more-collections-per-pod`), 40 fields each, 5,000 rows and 4 MiB of values in all, 5,000 rows per edit or action call (8 MiB per imported file, refused as soon as it holds more rows or a CSV line over 1,000 cells),
  4,000 characters per text value.

- **Retention.** A collection may keep only its newest rows (`keep: {rows, days}` in its definition): when a write
  settles, rows beyond the newest `rows` live ones, or created more than `days` ago, go for good. The pod's
  `notifications` keeps 500 of 30 days.
- **Notifications** are the system collections `notifications` and `notification_channels` (decision
  [`notifications-in-a-collection`](decisions/notifications-in-a-collection.md), [notifications.md](notifications.md)):
  every pod has them, layouts bind them, and they don't count against the limits above.
- **Deleting.** `delete_collection` removes a collection once no layout binds it and no other collection's ref,
  lookup, count, rollup or action names it (the refusal names them); home's feed for it is dropped too. It is restorable with `restore_collection`
  for 30 days, then purged (decision [`collection-delete`](decisions/collection-delete.md)); sooner when what the
  pod keeps for undoing passes twice its bytes cap (the oldest goes first). A stale feed alone is
  dropped with `rename_layout` on home, passing home's whole feed list without it.

MCP tools: `list_collections`, `define_collection`, `describe_collection`, `apply_change`,
`restore_collection_version`, `delete_collection`, `restore_collection`, `edit_rows`, `add_row`, `run_action`,
`undo_edit`, `export_collection`.

### Fillers and alerts

A collection can keep itself current from the web (decision
[`collection-fillers`](decisions/collection-fillers.md)). Each **filler** writes its own rows: on a schedule
(`every_minutes`), when an app showing it opens with stale data (`on_open_minutes`), from Claude
(`collections_refresh`), or from a layout's refresh action. A filler is data in the definition, not code.

- **`fetch`** (no model call per fill): a URL (or each row's own URL), an extractor and a map from fields to paths in
  each item. Extractors: `events` (an iCalendar feed or a page's schema.org events), `json`, `embedded-json`,
  `jsonld`, `rss`, `ics`, `selectors` (CSS), and `llm` (a model reads the page's text, only when it changed).
  A map path may end in filters: `description | link` (the first link in it, Google redirects unwrapped) and
  `description | text` (its text without HTML), e.g. to give `details` a link to follow (decision
  `map-path-filters`). `details` reads each new row's own page once; with `extract: "llm"` and `fields` (and a
  `hint`) a model reads that page for those fields, for pages with no structured data (decision `details-llm`). Conditional requests and a body hash skip unchanged pages; errors back off; values are coerced to the fields'
  types. Headers can carry a sealed secret (`collections_set_secret`, with the host it may be sent to; a
  request to any other host, a redirect off it, or a plain `http://` request doesn't get it). Extraction lives in
  `crates/web-extract`.
- **Per-row fetches:** `url_field` fills each row from its own page; `query_field` (with `extract: "llm"` and
  `fields`) searches each row's text with the pod's `web_search` jack and has a model pick the row's fields from
  the results (a shopping list's store, price and link; people add the rows). A blocked, failed or empty page or
  search never erases a row's values (`on_empty: "keep"`, the default; `"clear"` empties them). `refresh: "once"`
  fills a row when its URL or query is new or changed (the default for `query_field`); `"every"` (the default
  for `url_field`) fills on each run but skips the model when the page's text didn't change (decision
  [`web-and-agent-modules`](decisions/web-and-agent-modules.md)). A fill takes at most 20 rows; rows never filled from
  their current URL or query go first, then the ones filled longest ago, so repeated fills (and refreshes) reach
  every row.
- **Image search:** a `query_field` filler with `extract: "images"` searches each row's text for pictures and fills
  `map` from the first image result: `{"image_url": "image"}`, where a field takes `image` (the picture's URL),
  `thumbnail`, `page` (the page it is on), `title` or `source` (the site); of the first five results, the first
  whose picture loads (a `HEAD` through the host policy), since a search index keeps pictures a shop has since
  removed. No model, so no model budget; Brave's
  image search or SearXNG's `images` category (DuckDuckGo has none, and the fill says so). It suits product photos
  from shops that block the pod's fetches. `refresh` and `on_empty` work as for other `query_field` fillers, and
  it can't be previewed (decision [`image-search`](decisions/image-search.md)). To let people pick a row's picture,
  `GET /v1/collections/<c>/images?id=<row>` (a screen's `data`) lists up to 10 of the row's image results (query
  `collection:<c>:images:<row>`, entities `{image, thumbnail, page, title, source, n, row_id}`, best first; add
  `&filler=<id>` with more than one images filler); a tap saves one into a plain field with the ordinary `update`,
  and an `expr` field shows the picked picture before the found one (decision
  [`pick-a-photo`](decisions/pick-a-photo.md)).
- **Rendered pages:** `render: true` (or `{wait_for: <CSS selector>, timeout_seconds: 5..=45}`) on a `url` or
  `url_field` filler, or on `collections_preview`, loads the page (and its `details` pages) in the server's headless
  browser first, for pages a script builds or that answer a plain fetch with a bot check; the extractors read the
  rendered HTML. One browser at a time per server, 100 renders a pod a day, `every_minutes` and `on_open_minutes` at
  least 360, no headers. A site that still refuses is a block in `collections_status` (decision
  [`rendered-fetches`](decisions/rendered-fetches.md)).
- **`mcp`** (no model call): a tool of an MCP server the pod is connected to (decision
  [`mcp-jack`](decisions/mcp-jack.md)): `{kind: "mcp", server, tool, arguments, items, zone, map}`. The tool's answer
  (its structured content, else its text read as JSON) is read like a JSON page: `items` is the path to the list,
  `map` the fields. Text arguments may hold `{today}`, `{today+N}` or `{today-N}` (days), the date in the pod's zone
  when the fill runs (e.g. `{"from": "{today}", "to": "{today+180}"}`), or `{midnight}` / `{midnight±N}`, the moment
  that day starts in the pod's zone with the offset in effect on that date (`2026-11-15T00:00:00-07:00`). Prefer
  `{midnight±N}` when the tool reads a bare date as UTC: `{"from": "{midnight}", "to": "{midnight+15}"}` is today
  and the next 14 days, whole days. The default title is `<server> · <tool>`.
  An answer like the last one writes nothing. `collections_preview {collection, filler}` calls the tool and shows
  the rows; `mcp_call_tool` shows a tool's raw answer to write `items` and `map` from.
- **`range`** (no web, no model): one row per step of a window of dates that moves with today in the pod's zone
  (decision [`range-filler`](decisions/range-filler.md)): `{kind: "range", from, to, step, map, key}`. `from` and
  `to` are `{today}`, `{today+N}` or `{today-N}`; `step` is `day` (the default), `week` (Monday to Sunday, from the
  week `from` falls in) or `month` (likewise). `map` gives each field a part of the step: `date` (its first day,
  `YYYY-MM-DD`), `end` (its last day), `weekday` (`Mon`), `weekday_long` (`Monday`), `day` (`16`), `month` (`Nov`),
  `month_long` (`November`), `year`, `week` (the ISO week number) or `index` (0 for the window's first step). At
  most 200 rows. Give `key` the date field so a day keeps its row (and its hand-set unfilled fields) as the window
  moves; a day that leaves the window is deleted. It fills hourly unless `every_minutes` says otherwise, and a fill
  on the same day writes nothing. Its use is as a spine: `count` or `lookup` fields over other collections keyed by
  the date (`{kind: "count", from: "date", collection: "workouts", key: "date"}`) make a calendar's days, a habit
  tracker's rows or a monthly budget's months, and a layout lists each day with the other collections' rows on it
  (a `core://List` inside the day's template, `where` its date). Fields it doesn't fill (a day's note, a tick) are
  people's to edit.
- **`agent`** (costs model calls): a prompt; a model with web search answers rows that fit the fields, within a
  daily budget. With the `web_search` jack plugged it searches and reads pages with the pod's own tools; it reads
  only pages it has seen (a URL in the prompt or the rows, a search result, or a link on a page it read), so a page
  can't have it send the rows elsewhere in a URL (decision [`web-fetch-provenance`](decisions/web-fetch-provenance.md)).
- **The web module:** every request goes through the pod's host policy (`web-extract`'s `hosts`: 1 s spacing, a
  daily cap per site, and a shared backoff after a site blocks the pod as an automated visitor). Model work runs
  through the agent module (`crates/agent`): the `model` jack, daily budgets per feature, retries.
- **Modes:** `replace` (the filler's rows are its latest answer, kept stable by `key`, or without one by pairing
  each item with the row it most resembles on the same `upcoming` day) or `upsert` by `key`. Filled fields can't be
  edited by hand.
- **Who wrote a field:** a row a filler made carries its id (`collections_rows.filler`), and every field a filler
  or a model's computed answer wrote last is recorded in `collections_field_writers` (`filler:<id>` or `model`); a
  person's (or Claude's) write of the field clears it. A value from other rows carries their writer too: a live
  lookup, count or rollup's recompute records `derived`, an action's `$row.…` / `$parent.…` copy records the writer
  of the field (or ref) it read, and a guard reading a field counts a lookup, count or rollup as `derived` and an
  expression as the first untrusted writer of the fields it reads (issue #289). The Agents source starts runs only
  from rows a person wrote (decision [`agents-cap-v3`](decisions/agents-cap-v3.md), issue #243).
- **Alerts** notify once when a row matches: `any` / `all` / `none` phrases, a filler, days of the week, a window on a
  date field. Rows are served with `alert_matched` and `alert_names`.

| A layout uses | |
|---|---|
| feed `["collections", "<c>"]` | Also `collection:<c>:fillers` (each filler's title, URL, state, health and alerts) and `collection:<c>:alerted`. |
| `GET /v1/collections/<c>/fillers/<f>` | A filler, its rows and its alerts. |
| `POST /v1/collections/<c>/refresh {filler?}` | Fill now. |

MCP tools: `collections_preview` (try a recipe without storing anything), `collections_status`,
`collections_refresh`, `collections_set_secret`; `apply_change` ops `set_filler`, `remove_filler`, `add_alert`,
`update_alert`, `remove_alert`. Fillers need the pod's `web` jack (`mcp` fillers its `mcp` jack instead) and model
work its `model` jack (decision [`jacks`](decisions/jacks.md)); all are locked off on temporary pods.

### Jacks

A pod's **jacks** are the abilities it is plugged into (decision [`jacks`](decisions/jacks.md)): `model` and `web`
(switches, on by default), `web_search` (config: `provider` `brave` with the pod's own `api_key`, `searxng` with a
`base_url` (`https://` on a public host name, never an IP address or an internal name), or `duckduckgo`), and
`mcp` (a switch, on by default: calling the MCP servers the pod is connected to), and
accounts sources declare: `pixelfed` and `agent_runner` (plugged by their own sign-in). Claude uses `list_jacks`, `plug_jack` and `unplug_jack`; `GET /v1/jacks` lists them. People plug them in the app
too: Settings → Jacks lists each with its state, and a config jack's screen takes its provider and its key in a hidden
field, sent straight to the pod (`GET /v1/jacks/screen`, `POST /v1/jacks/plug` and `/unplug`; decision
[`jacks-in-settings`](decisions/jacks-in-settings.md)). Secrets are sealed and
never shown back. The host can lock a jack off for a pod (`jack.<id> = locked` in `pod_settings`).

### MCP servers

The pod can call the tools of remote MCP servers (decision [`mcp-jack`](decisions/mcp-jack.md), `pod_core::mcp_servers`),
for `mcp` fillers. Claude connects one with `mcp_connect {name, url}` (an `https://` endpoint on a public host
name, default port): a server that needs nothing connects at once, one that takes an API key with `token` (sealed, never shown
back), and one that uses OAuth answers a `sign_in_url` for the user to open in a browser. The pod finds the
server's authorization server (protected resource metadata, then OAuth metadata), registers itself as a public
client and signs in with PKCE; the browser comes back to the pod's public callback
(`/v1/p/<pod>/v1/mcp-servers/callback` on a host), which trades the code for tokens. Access tokens refresh on
their own; when a refresh is refused the server reads `needs_sign_in` and `mcp_connect {name}` gives a new link.
The server must accept the pod's redirect URI (proton-mcp: add the pod's host to `ALLOWED_REDIRECT_HOSTS`).
`mcp_servers` lists the servers with their state and tools (with `name`: each tool's input schema),
`mcp_call_tool` calls one now, `mcp_disconnect` forgets one. At most 20 servers a pod; each call opens its own
session over Streamable HTTP, with no redirects, a minute per request and 8 MB per answer. What a server lists is kept
capped: tool names of at most 128 characters (longer ones are skipped), descriptions of 600, an input schema of
16 KB (a larger one reads as a bare object schema), 200 tools and 1 MB in all.

## The Events app

The **Events** app (app id `events`, `crates/source-events`) shows every upcoming event of the websites the user
watches and notifies when one matches ("a show with Brass Lanterns at Improv Millbrook"). Its data is the system collection
`events` (`source_id`, `title`, `start`, `end`, `venue`, `description`, `url`, `tickets_url`, `seen`): each website is a
fetch filler, and each rule an alert on that filler. Claude adds and changes them with the Collections tools; the
MCP instructions include the recipe. The app shows matches, sites with their health and alerts, a site's events, and
an event with ticket and page links. The fields of `events` can't be renamed or removed.

The app replaced an earlier Watcher with its own tables (decisions [`watcher`](decisions/watcher.md),
[`watcher-rules-per-site`](decisions/watcher-rules-per-site.md)); its app id was `watcher` until decision
[`rename-apps-and-folders`](decisions/rename-apps-and-folders.md).

Try it locally: `CONSTRUCT_FILL_PRIVATE_HOSTS=1 cargo run -p server`, serve an `.ics` file, and ask Claude to watch it.

## Agents

The **Agents** source and blueprint (`agents`, `crates/source-agents`) show Claude Code sessions an external agent runner
runs, with stop, wake, start and schedule actions, notifications, and an opt-in Tasks collection (MCP tool
`agents_link_tasks`): see [agents.md](agents.md).
