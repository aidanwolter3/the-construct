# Built-in apps are blueprints; every pod opens on its own `home` app, a dock drawn from a layout

Superseded in part by [layouts-not-apps](layouts-not-apps.md): the Dock, Drawer, AppFrame, shell mode and the native
menu and inbox are gone; blueprints, home and device data stay.

Date: 2026-11-05. Related: D34, D46, D52, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[layout-parts](layout-parts.md), [named-vocabulary](named-vocabulary.md),
[settings-page-and-sign-ins](settings-page-and-sign-ins.md).

Most of the client outside an app was native Compose: the menu (apps, versions, Settings), the inbox, the menu
button and bell, and "Open your pod" (served by the pod host). Changing any of it needed a client release. Every pod
also showed every built-in app (Pixelfed, Recipes, Events), whether the user wanted them or not, while the user's
agent could already build custom apps that reference or copy the built-ins.

Considered: moving each native screen into layouts one at a time (no change of model, and the menu would still
decide navigation); composing the main layout on the pod, inlining every app into one workspace (no client
change, but every app's feeds would load on every request and a broken app would break the whole page); two engines
at once, one for the main layout and one for the framed app (the engine's outbox has no claiming, so two engines
on one cache could send a queued change twice).

Decision:

- **Blueprints.** Built-in apps are blueprints: shipped and updated as layout files as before, served by id
  (notifications and links still name them), but not listed. `GET /v1/apps` lists the pod's own apps; `?blueprints=1`
  adds the blueprints (`kind: builtin`). MCP `list_apps` answers `{apps, blueprints}`, and the server instructions
  tell the agent to reference a blueprint, or copy it to change it, per request.
- **Whole-blueprint references.** `{"type": "core://Ref", "app": <blueprint>, "part": "layout"}` as a layout's root
  is the whole blueprint (pod-only, like other parts); the expansion keeps the app's own `id` (and `title`, when the
  ref has one). Anywhere else it is refused. A blueprint deploy appends `ref_update` versions as before.
- **The home app.** Every pod has a custom app with the fixed id `home` (`pod_core::HOME_APP`), created on open as a
  whole reference to the `shell` blueprint (`pod_core::HOME_BLUEPRINT`, crate `app-shell`). It is listed first,
  can't be deleted (409 `home_app`), and is otherwise the user's to change. A new pod has nothing else.
- **Older pods keep their apps.** Migration `blueprints` marks a pod that already had layout history; on its next
  open the pod creates one app referencing each built-in it showed (not hidden, not dev), with the built-in's feeds,
  then clears the mark.
- **New vocabulary** (one client build): `core://Dock` (feature `dock`): tabs along the bottom (a rail on wide
  windows) from fixed tabs and a query, the selected tab's content filling the screen, the selection a remembered
  form value; `core://AppFrame` (feature `app_frame`): another app of the pod drawn whole; `action.device` (feature
  `device_action`): `open_settings`, `open_inbox`, `refresh`, `leave_pod`, `open_app`. Any layout may use any command.
- **Device data** (feature `local_data`): `local://` entities and `local:` queries the client provides with no pod
  request: `local://connection`, `local://inbox`, `local://apps` and the `local:apps` query, `local://device`,
  `local://session`. Custom layouts may bind them without a feed. A `core://Conditional` value may now be a
  `${uri#field}` binding.
- **The `shell` blueprint is a blank page**: one off-white page (`core://Box` `fill`, feature `box_fill`) saying "The
  Construct" and "Ask your agent to build anything". The pod owner has the agent build everything into home, only
  what they ask for. While prototyping, the blueprint was a dock (Apps, Notifications, Settings tabs) and then a left
  drawer with those pages; agents copied those whole pages into the first app they were asked for, so the versions
  were removed before merging and only the pieces stay (below), with small single-purpose snippets in
  `describe_vocabulary` (an app filling home, a drawer of apps, a sign-in row, a notification list, a channel
  switch, a version list, a connection dot). A `core://AppFrame` as a workspace's one slot fills the screen.
- **Pieces for navigation and settings:** `core://Drawer` (feature `drawer`: pages, a menu button with a `badge`, a
  panel of any layout with a pinned `footer`; any action in the panel closes it, and the panel's actions read its
  own form), `core://Dock` (feature `dock`), `core://Switch` (feature `switch`), icons `settings` and
  `notifications` (feature `icons_shell`), a `core://Conditional` `equals` that may bind the item, and device data
  and commands: `local:notifications`, `local:channels`, `local:sign_ins`, `local:providers` (`custom_server`,
  `server_placeholder`), `local:servers`, `local:versions`, `local://connection#color`; `open_notification`,
  `mark_all_read`, `toggle_channel`, `sign_in` (`value`: another server, e.g. `$form?.server`), `sign_out`,
  `set_server`, `show` (a Drawer or Dock page), `show_versions`, `pin_version` (the shell keeps a pin per app across
  app switches), `follow_latest`. `action.device` carries `app`, `id` and `value`. An app asked for that has no
  page (a notification's blueprint app) is shown in the drawer's or dock's frame.
- **Shell mode** (clients open `home` by default): the controller keeps the last home layout it drew and draws it
  over the device data, while its one engine follows the app of the visible frame (switching as today). The native
  menu button and bell are left out; Settings and the inbox open from `action.device`. Before a pod is open the
  client draws a bundled sign-in layout (the same intents as the pod host's screen); with a pod but no drawable home,
  a bundled rescue layout (Try again, Leave pod). Repairing a broken home is the agent's job (`restore_version`).
- Collections: the filler and alert keys (`filler_title`, `alert_names`, ...) may be bound before the collection has a
  filler or an alert, so the Events blueprint can be referenced on a new pod.

Not done yet: hiding and deleting apps are only in the native menu (no layout opens it by default); Android's
notification permission row isn't offered to layouts; notifications load the newest page only; the home layout
updates only when it is drawn on its own engine or the apps list shows a newer version (no live event while a frame
has the engine); notifications and links open the blueprint by id rather than the user's app built on it; a frame
inside a framed app shows a message; old clients can't draw `home` and need an update.
