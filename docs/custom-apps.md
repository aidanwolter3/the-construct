# Custom apps, Collections and Events

A pod's apps are the user's own: any layout the [vocabulary](layouts.md) can draw, over the feeds, screen data and
actions the pod's data sources declare. They are usually built by Claude through the pod's MCP endpoint, often from a
**blueprint** (a built-in layout the server ships and updates, such as `pixelfed`, `recipes` or `events`). A new pod
has one app, `home`; nothing else until the user asks for it. Data an app keeps itself (a todo list, a maintenance
log) lives in **Collections**.

Decisions: [`custom-apps-and-data-sources`](decisions/custom-apps-and-data-sources.md),
[`mcp-connector`](decisions/mcp-connector.md), [`layout-parts`](decisions/layout-parts.md),
[`collections`](decisions/collections.md), [`blueprints-and-home`](decisions/blueprints-and-home.md) and the
`collection-*` decisions.

## Blueprints and the home app

Built-in apps are blueprints: served by id, but listed only with `GET /v1/apps?blueprints=1` (MCP `list_apps` answers
`{apps, blueprints}`). An app can be a whole blueprint by reference, kept current by the pod:

```json
{"title": "Recipes", "icon": "recipe", "feeds": [["recipes", "all"]],
 "layout": {"type": "core://Ref", "app": "recipes", "part": "layout"}}
```

Give it the blueprint's feeds (`list_apps` shows them). To change a blueprint, copy its layout (`get_layout`) and
pass `copied_from`. Every pod's **`home`** app is the screen clients open, and the only one: it is the user's whole
interface. It starts as a reference to the `shell` blueprint, a blank page, and the agent builds into it only what
the user asks for: one app is a `core://AppFrame` as home's one slot; more apps add a `core://Drawer` or
`core://Dock` over `local:apps`; notifications, sign-in or settings come from the `describe_vocabulary` snippets.
It can't be deleted. A pod made before blueprints got an app
referencing each built-in it showed, so nothing disappeared.

## Custom apps

A custom app is a title, an icon, a list of **feeds** (`["recipes", "all"]`, `["pixelfed", "home"]`,
`["collections", "todos"]`) and a layout history. It is served exactly like a built-in app (workspace, events
stream, versions, feature negotiation) and marked `"kind": "custom"` in `GET /v1/apps`. Ids are `c-` and 10
characters, except the pod's own `home`.

```bash
pod=http://127.0.0.1:8080; token=cpod_…
curl -fsS -H "Authorization: Bearer $token" -H 'content-type: application/json' $pod/v1/custom-apps -d '{
  "title": "Kitchen", "icon": "recipe", "feeds": [["recipes", "all"], ["pixelfed", "home"]],
  "note": "Recipes next to my feed", "layout": {"type": "core://Workspace", "title": "Kitchen", "slots": []}}'
```

- **Validation.** Every write is checked against the sources the app's feeds name: a layout may only bind query keys
  those feeds fill and call the data paths and endpoints their manifests declare. Errors name a JSON pointer and
  what is allowed. The routes are in [api.md](api.md#custom-apps).
- **Source status.** The root gets a `core://sources/<source>` entity per source, `{title, signed_in, error?}`, so a
  layout can say when a source is signed out or failing (`${core://sources/pixelfed#signed_in}`). Sign-ins start
  from Settings; custom layouts can't hold sign-in buttons.
- **Built-in parts.** `{"type": "core://Ref", "app": "pixelfed", "part": "screen:post"}` reuses a built-in's
  screen, slot or declared part, expanded and kept current by the pod. A copy instead says `"copied_from": {app,
  version}`, and `GET /v1/apps` reports `updates_from` when that built-in moves on.
- **Device data.** `${local://connection#label}`, `${local://inbox#unread}`, the `local:apps` query and the other
  [device data](layouts.md#device-data) need no feed.
- **Hiding and deleting.** Any app can be hidden from the app list on every device; custom apps can be deleted
  (purged after 30 days), except `home`.

## Building apps with Claude

Each pod has an MCP endpoint, `POST /v1/mcp`, so Claude can read the pod's sources and build and edit its custom apps.
Writes go through the same validation as the HTTP API and appear in the version history as written by Claude.

- **Claude apps (web, desktop, mobile):** add a custom connector with the URL `https://<server>/v1/mcp`. Claude
  discovers the server's OAuth endpoints, registers itself and opens a page where you type your pod code. The
  credential it gets reaches only that pod's `/v1/mcp`: no app routes, sign-ins or pod settings.
- **Apps that ask for a client ID and secret** (e.g. Gemini): set `CONSTRUCT_OAUTH_CLIENT_ID`,
  `CONSTRUCT_OAUTH_CLIENT_SECRET` and the app's callback in `CONSTRUCT_OAUTH_CLIENT_REDIRECT_URIS`
  ([server.md](server.md)), and enter the same ID and secret in the app. You still type your pod code to connect.
- **Claude Code:** `server pods mcp <name>` prints a credential, then:

  ```bash
  claude mcp add --transport http construct https://<server>/v1/mcp --header "Authorization: Bearer cmcp_…"
  ```

- **Disconnect:** `server pods revoke-mcp <name>`. Rotating the pod code keeps Claude connected.

Tools: `list_apps`, `list_data_sources`, `describe_vocabulary`, `get_layout`, `sample_data`, `validate_layout`,
`create_app`, `update_layout` (a whole layout or a JSON Patch, with `base_version`), `restore_version`, `rename_app`,
`delete_app`, `set_hidden`, `list_parts`, `rebase_context`, `dismiss_update`, `set_time_zone`, plus the Collections
tools below. The server's MCP instructions tell Claude how to use them.

## Collections

Collections is a built-in data source (`crates/app-collections`) that stores any collection Claude declares. Claude
never writes code: it declares typed fields (`text`, `number`, `bool`, `date`, `datetime`, `choice`; optional, with a
default), and the pod stores, validates and serves the rows.

| A layout uses (for collection `<c>`) | |
|---|---|
| feed `["collections", "<c>"]` | Rows `collections://<c>/<id>` under the query `collection:<c>`, with every field plus `id`, `deleted`, `created_at`, `updated_at`. |
| `GET /v1/collections/<c>/rows` | The same, as a screen's data path. |
| `GET /v1/collections/<c>/row?id=<id>` | One row (`collection:<c>:row:<id>`). |
| `POST /v1/collections/<c>/create {<field>: …, client_id?}` | For `action.mutation.create`; a repeated `client_id` answers the same row. |
| `POST /v1/collections/<c>/update {id, <field>: …}` | For `action.mutation.optimistic`; an id `pending-<nonce>` finds a row whose create was just sent. |
| `POST /v1/collections/<c>/delete {id}` | Sets `deleted`; hide such rows at once with `where: {field: "deleted", equals: "false"}`. |
| `GET /v1/collections/<c>/export` | Everything, deleted rows and kept-aside values included. |

`describe_collection` gives ready-made layout snippets for all of these.

- **Change sets.** `apply_change` sends a collection change together with the app layouts that go with it. Every
  custom app using Collections is checked against the new definition; if one would break, nothing applies and the
  error names the app and binding.
- **Safe schema changes.** Fields have permanent ids. A rename keeps the old name as an alias; a removal hides the
  field and keeps its data 30 days (`restore_field`); a type change converts row by row and keeps what doesn't
  convert aside. A write made under an older definition (an offline device) still lands.
- **Computed fields.** A field's `compute` is one kind or a list tried in order: `expr` (a small expression over
  the row), `lookup` (the value of the best-matching row of another collection, kept as editable rules) and `model`
  (a model's answer, in the background). With `[lookup (learn: true), model]` the model is asked only when no rule
  matches, and its answers and hand edits become rules. Decisions
  [`collection-computed-fields`](decisions/collection-computed-fields.md) and
  [`collection-lookups`](decisions/collection-lookups.md).
- **Reminders.** `apply_change {op: add_reminder, field: "due", title: "Due today: {title}", unless: {field:
  "done", equals: true}, at?: "09:00"}` notifies once per row when its date arrives, in the pod's time zone
  (`set_time_zone`). Decision [`collection-reminders`](decisions/collection-reminders.md).
- **Row edits by Claude.** `edit_rows` (imports, backfills) first answers a preview and a confirm token, which Claude
  shows the user before applying; `undo_edit` reverts it. `add_row` writes one row the user just asked for at once,
  undoable the same way (decision [`collections-add-row`](decisions/collections-add-row.md)).
- **Limits per pod:** 20 collections, 40 fields each, 5,000 rows and 4 MiB of values in all, 500 rows per edit,
  4,000 characters per text value.

MCP tools: `list_collections`, `define_collection`, `describe_collection`, `apply_change`,
`restore_collection_version`, `edit_rows`, `add_row`, `undo_edit`, `export_collection`.

### Fillers and alerts

A collection can keep itself current from the web (decision
[`collection-fillers`](decisions/collection-fillers.md)). Each **filler** writes its own rows: on a schedule
(`every_minutes`), when an app showing it opens with stale data (`on_open_minutes`), from Claude
(`collections_refresh`), or from a layout's refresh action. A filler is data in the definition, not code.

- **`fetch`** (no model call per fill): a URL (or each row's own URL), an extractor and a map from fields to paths in
  each item. Extractors: `events` (an iCalendar feed or a page's schema.org events), `json`, `embedded-json`,
  `jsonld`, `rss`, `ics`, `selectors` (CSS), and `llm` (a model reads the page's text, only when it changed).
  Conditional requests and a body hash skip unchanged pages; errors back off; values are coerced to the fields'
  types. Headers can carry a sealed secret (`collections_set_secret`). Extraction lives in `crates/web-extract`.
- **`agent`** (costs model calls): a prompt; a model with web search answers rows that fit the fields, within a
  daily budget.
- **Modes:** `replace` (the filler's rows are its latest answer, kept stable by `key`) or `upsert` by `key`. Filled
  fields can't be edited by hand.
- **Alerts** notify once when a row matches: `any` / `all` / `none` phrases, a filler, days of the week, a window on a
  date field. Rows are served with `alert_matched` and `alert_names`.

| A layout uses | |
|---|---|
| feed `["collections", "<c>"]` | Also `collection:<c>:fillers` (each filler's title, URL, state, health and alerts) and `collection:<c>:alerted`. |
| `GET /v1/collections/<c>/fillers/<f>` | A filler, its rows and its alerts. |
| `POST /v1/collections/<c>/refresh {filler?}` | Fill now. |

MCP tools: `collections_preview` (try a recipe without storing anything), `collections_status`,
`collections_refresh`, `collections_set_secret`; `apply_change` ops `set_filler`, `remove_filler`, `add_alert`,
`update_alert`, `remove_alert`. Fillers are off on temporary pods.

## The Events app

The **Events** app (app id `events`, `crates/app-events`) shows every upcoming event of the websites the user
watches and notifies when one matches ("a show with Brass Lanterns at Improv Millbrook"). Its data is the system collection
`events` (`source_id`, `title`, `start`, `end`, `venue`, `description`, `url`, `tickets_url`, `seen`): each website is a
fetch filler, and each rule an alert on that filler. Claude adds and changes them with the Collections tools; the
MCP instructions include the recipe. The app shows matches, sites with their health and alerts, a site's events, and
an event with ticket and page links. The fields of `events` can't be renamed or removed.

The app replaced an earlier Watcher with its own tables (decisions [`watcher`](decisions/watcher.md),
[`watcher-rules-per-site`](decisions/watcher-rules-per-site.md)); its app id was `watcher` until decision
[`rename-apps-and-folders`](decisions/rename-apps-and-folders.md).

Try it locally: `CONSTRUCT_FILL_PRIVATE_HOSTS=1 cargo run -p server`, serve an `.ics` file, and ask Claude to watch it.
