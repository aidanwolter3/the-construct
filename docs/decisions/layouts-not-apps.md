# There are no apps at runtime: one home layout embeds and presents other layouts, on one engine

Date: 2026-11-05. Supersedes the navigation parts of [blueprints-and-home](blueprints-and-home.md) (Dock, Drawer,
AppFrame, shell mode). Related: D34, D52, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[layout-parts](layout-parts.md), [named-vocabulary](named-vocabulary.md), [named-migrations](named-migrations.md).
Issue #106.

The client's navigation was built around apps, and the parts didn't compose. A home layout could open an app only
through a `core://Dock` or `core://Drawer`: `action.device` `open_app` and `show` set a request only those two nodes
read, so a plain list of apps did nothing when tapped. Home's own screens weren't routed while another app was framed.
Picking an app changed a form value instead of pushing a stack entry, so Android's Back had nothing to pop and closed
the activity. And the client swapped its engine for each framed app ("one engine shows one app"). The menu and the
inbox were native Compose panels beside all this, with their own Back handling and test tags.

Considered: keeping apps and fixing each gap (a `open_app` every node understands, Back on the dock), which keeps
five nodes and commands doing one job; reusing `action.navigation.open` alone with no node, which can't put a layout
inside part of the screen; keeping the menu and inbox native as a recovery path, which leaves two UIs to keep in step.
The bundled rescue layout already covers a home that can't be drawn.

Decision:

- **Layouts, not apps.** The pod's units are layouts: `home` (the root, which clients open first) and the layouts the
  user's agent builds. Each has an id, a title, an icon and a version history, as apps had. Blueprints stay
  reusable built-in layouts (`core://Ref`), served by id. The HTTP paths keep their names (`/v1/apps`,
  `/v1/custom-apps`, `/v1/workspaces/<id>`) so released clients keep working; the docs, the MCP tools and the device
  data say "layout".
- **Embedding: `core://Layout`** (feature `layout_embed`) `{layout, show_when?, equals?, presentation?, animation?,
  params?, height_dp?}` draws another layout. `presentation` `Inline` (the default) draws it in place, in the space
  it's given (`height_dp` for a card-sized region; otherwise as tall as its content, or the whole screen as a
  workspace's one slot). `Sheet` (a bottom sheet) and `Panel` (a side panel) present it over the screen while
  `show_when` is truthy (or equals `equals`); dismissing one (Back, the scrim) clears the `$form` field `show_when`
  names. Without `show_when` it always shows.
- **Presenting: `action.navigation.open`** takes `layout` (and `params`) as well as `target_uri` (feature
  `navigate_layout`): it pushes that layout on the back stack, presented as `presentation` says: `Push` (the whole
  screen, with a Back bar), `Sheet` or `Panel`. A `target_uri` with it opens that layout's matching screen on top.
  `params` bind as `$route.<name>` in the presented layout, the same as a screen's route parameters.
- **Animation** (`animation`: `none`, `fade`, `slide`, `sheet_up`) is named, never timed: the platform picks
  durations. By default Push slides in from the end, Sheet slides up, Panel slides in from the start and Inline fades.
- **One back stack.** Presented layouts and screens are entries of one stack. System Back pops its top entry, so
  Back needs no special case anywhere (`PlatformBackHandler` on the stack); with nothing to pop, the platform's
  default runs. A screen opens in the layout on top of the stack, falling back to home's screens.
- **One engine.** The client keeps one engine for the whole session. Home is its root; `open_layout` loads any other
  layout into the same store (entities and queries are shared; each layout keeps its own version and cache row).
  Home's event stream carries every layout's `layout` and `deleted` events and every data change of home's feeds.
- **Versions and pins are per layout.** The version list, `pin_version` and `follow_latest` name a layout
  (`action.device` takes `layout`, with `app` kept as an alias). A pin lasts while the client runs.
- **Home owns the feeds.** Home declares every feed (up to 32); other layouts bind any of them and declare none.
  A layout is validated against home's feeds. Saving home (or its feeds) is refused while another layout still binds
  a feed it would drop; the error names the layout and what it binds. A layout's request loads only the feeds whose
  query keys it binds, so a feed loads when a layout that shows it is first shown. Creating a layout with `feeds`
  adds them to home's.
- **Notifications target layouts.** A notification carries `layout`: the pod's layout built on the blueprint that
  sent it (one whose latest version references or was copied from it), else the blueprint's id. Opening one pushes
  that layout and then the notification's `target_uri` in it.
- **Menu and inbox are layouts.** Two blueprints of `app-shell`: `settings` (connection, sign-ins, notification
  channels, the pod's layouts with their versions, the server, Leave pod) and `inbox` (the notifications, newest first,
  by day). They read only device data. Any layout opens them with `action.navigation.open` (`Panel` and `Sheet`);
  `action.device` `open_settings` and `open_inbox` do the same. The native menu, bell, inbox and Settings panels are
  gone. The bundled sign-in and rescue layouts stay in the client.
- **Retired** (moved to `features/removed/`): `core://Dock` (`dock`), `core://Drawer` (`drawer`), `core://AppFrame`
  (`app_frame`). `action.device` `open_app` and `show` are no longer accepted in new layouts and do nothing.
- **Migration `layouts_not_apps`** (pod-core, named): home's feeds become the union of every custom layout's feeds,
  and a home whose latest version uses a retired node gets a new version: a plain list of the pod's layouts, each
  pushing its layout, with Settings and the inbox at the top. Old versions stay in the history.
- **MCP tools** are renamed: `create_layout`, `list_layouts`, `delete_layout`, `rename_layout`; the vocabulary
  snippets show home as a list that pushes layouts. The connector needs "Refresh tools list".

Consequences: new desktop and Android builds are needed; a released client keeps drawing the newest home version it
can (a migrated home is a plain list it can't push from, but every older version still serves it). Hidden layouts
(`set_hidden`) are still left out of `local:layouts`. Not done yet: layouts can't be deleted from the device (the
agent's `delete_layout` can); a pin is forgotten when the client restarts; `show_when` reads form values and device
data, not entity fields of the pod's data.
