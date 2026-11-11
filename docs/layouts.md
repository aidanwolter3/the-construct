# Writing layouts

A layout is JSON: a tree of generic `core://` nodes with intents attached. The pod serves it; every client draws it
with the same renderer. The vocabulary has two layers (decision
[`plumbing-and-porcelain`](decisions/plumbing-and-porcelain.md)): **plumbing**, the nodes, intents and device data the
app draws ([Nodes](#nodes), [Intents](#intents), [Device data](#device-data)), and **porcelain**, components defined
as data in terms of plumbing that the pod expands before serving ([Porcelain: components](#porcelain-components)). This page covers the file format, how to publish a new version, and the vocabulary. The
types themselves are in `crates/protocol/src/ast.rs` (nodes) and `crates/protocol/src/intent.rs` (intents), and each
feature has a one-paragraph description in `crates/protocol/features/`.

## Publishing a new layout version

Built-in apps' layouts (the blueprints) live in their adapter crate, `crates/<adapter>/layouts/<app>/NNNN.json` (Pixelfed's in
`crates/app-pixelfed/layouts/pixelfed/`, the Events app's in `crates/app-events/layouts/events/`). Adding a file is the only way to change how a built-in app
looks.

1. Copy the newest file of the app to the next number (four digits, no gaps), e.g.
   `crates/app-recipes/layouts/recipes/0005.json` to `.../recipes/0006.json`, and edit it.
2. The file is `{"note": "<one-line changelog>", "layout": <core://Workspace or core://Page>}`. It may use server
   components (`core://Use` of `core:<name>`); the pod records the layout they expand to. The note is what the app menu's
   version history shows. The root's `id` must be the app id. Don't declare features: the pod works them out. Older
   files carry a `"schema"` number; it is ignored.
3. Run `cargo test` (`-p app-pixelfed`, `-p app-recipes` or `-p app-events` is enough). It validates every file:
   structure, size, slots, routes, endpoints the adapter declares, and nesting rules.
4. Check it in the [simulator](simulator.md) and put screenshots in the PR.
5. Merge to `main`. On start, the pod appends the file as the app's next version. Connected clients get a `layout`
   event on their stream and refetch; a client that pinned an older version keeps it and is told a newer one exists.

There is no list of files to edit: each adapter's `build.rs` embeds its crate's `layouts/<app>/`
directories.

**Files are append-only.** Never edit or rename a file that has been deployed: the pod refuses to start and names
the file. To undo a change, add a new file with the old content. Reformatting a file without changing its
layout or note is fine. If another branch took your number first (git reports an add/add conflict), merge both
layouts into the next number: each file is a whole snapshot.

**Old files can be deleted** once no client needs them, oldest first (decision `retired-layout-files`); each app
keeps at least its newest file. An app's files then start above `0001`. A pod that recorded the deleted versions
keeps them in its history and serves them as before; a new pod starts from the oldest file left, as its version 1,
so a client too old to draw that file gets `409 no_compatible_version` there.

After changing a `pixelfed` layout, regenerate the simulator fixtures:
`UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::`.

## Structure of a page

Plumbing (feature `page`): the root is any node, and the app draws nothing around it. A page draws its own header
and Back, and wraps what scrolls in `core://Scroll`:

```json
{
  "type": "core://Page", "id": "recipes", "title": "Recipes",
  "root": {"type": "core://Column", "children": [
    {"type": "core://Row", "padding_h_dp": 12, "children": [
      {"type": "core://Use", "component": "core:back_button"},
      {"type": "core://Text", "style": "Title", "value_expr": "Recipes"}, {"type": "core://Spacer"},
      {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
       "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}]},
    {"type": "core://Scroll", "content": {"type": "core://Column", "children": []}}]},
  "screens": [{"id": "recipe", "route": "recipes://recipes/{id}", "data": "/v1/recipes/screens/recipe?id=$route.id",
    "item": "recipes://recipes/$route.id", "root": {"type": "core://Use", "component": "core:card", "children": []}}]
}
```

`title` names the layout (lists, accessibility) and is not drawn. `core:page_header` draws that header row (Back, the
title, actions as its children). In the root column a `core://Scroll`, a filling
`core://Box` or an inline `core://Layout` (or a `core://Gesture` around one) takes the height the other children
leave. A screen's `item` binds `$item`
throughout it (feature `screen_item`). `local://nav` and `local://screen` say where it is drawn and how its data load
is going ([Device data](#device-data)).

## Structure of a workspace

`core://Workspace` is porcelain: the pod serves it to a build that draws `page`, `scroll` and `nav_data` expanded
through `core:workspace` and `core:screen`, and builds since issue #126 draw no workspace of their own (features
`workspace`, `screens` and `title_scrolls` are in `features/removed/`, decision `workspace-drawn-by-the-pod`). Layouts
may still be written with it; prefer a `core://Page` for a new one.

```json
{
  "type": "core://Workspace",
  "id": "recipes",
  "title": "Recipes",
  "slots": [
    { "slot_id": "recipes", "provider_uri": "recipes://all", "content": { "type": "core://Column", "children": [] } }
  ],
  "screens": [
    {
      "id": "recipe",
      "route": "recipes://recipes/{id}",
      "title": "Recipe",
      "data": "/v1/recipes/screens/recipe?id=$route.id",
      "content": { "type": "core://Column", "children": [] },
      "footer": null
    }
  ]
}
```

- **Slots** are stacked top to bottom on the app's main page, which scrolls. The title is pinned in a bar above it,
  unless `title_scrolls` is true.
- **Screens** open when an `action.navigation.open` has a `target_uri` matching their `route` (the screens of the
  layout on top of the back stack, else home's). `{name}` segments
  capture route parameters, which `$route.<name>` substitutes anywhere in the screen. `data` is a pod path loaded
  when the screen opens; it answers `{entities, queries}`. `content` scrolls; `footer` stays at the bottom. Back (the
  arrow, Android Back, `action.navigation.back`) closes the screen.

## Bindings

| Syntax | Where | Means |
|---|---|---|
| `$item.<field>` | inside a List, Carousel or group-header template | a field of the current entity |
| `${<uri>#<field>}` | any text | a field of any entity, e.g. `${core://sources/pixelfed#signed_in}`; the URI may hold `$item.<field>` (bound first), e.g. `${collections://ingredients/$item.ingredient#name}` reads through a collection ref |
| `$route.<name>` | in a screen | a parameter captured from the route |
| `$form.<field>` | in intents and filters | the value a TextField, ChoiceChips or MediaPicker with that `field` edits; an intent is not sent while a referenced value is blank |
| `$form?.<field>` | in intents | the same, but a blank value binds to `""` instead of blocking |
| `$nonce` | in intents | a fresh value per dispatch (one per `action.sequence`), for temporary URIs and idempotency keys |
| `$attachments.<name>` | in a create | staged attachments: local URIs in `entity`, upload ids in `payload` |
| `$count` | in a Text with `count` | the number of matching entities |
| `${local://<entity>#<field>}`, `local:layouts` | anywhere | [device data](#device-data), with no feed |

Mutation expressions (`value_expr` of `action.mutation.optimistic` and `action.local.set`) are a small subset
evaluated against the entity before the change: a JSON literal, `entity.<field>`, `!entity.<field>`,
`entity.<field> ± N`, and `cond ? a : b` with those as branches (D4). For example
`entity.is_liked ? entity.like_count - 1 : entity.like_count + 1`.

## Nodes

| Node | Fields | Notes |
|---|---|---|
| `core://Page` | `id`, `title`, `root`, `screens` | A layout's root with no chrome from the app (feature `page`); only as the root. Screens: `{id, route, title?, data?, item?, root}`. |
| `core://Scroll` | `content`, `follow_end` | `content` scrolls in the height its parent leaves (feature `scroll`); `core://Sticky` pins at its top. Inside another scroll it doesn't scroll itself. Side by side in a `core://Row` that has a height (one in a filling box, or a page's column), each scrolls on its own in the row's full height. `follow_end`: after an action from outside it (a footer's Send), follow the content to its end once it grows. |
| `core://Column` | `children`, `spacing_dp`, `padding_h_dp`, `padding_v_dp`, `align` | Children stacked, full width. In a page's bounded height, a child that fills (a scroll, a filling box, an inline layout) takes the rest of it. |
| `core://Row` | `children`, `spacing_dp`, paddings, `align` | Side by side, vertically centred, no wrapping. A Spacer without a size takes the remaining width. Controls (Switch, Chip, Button, IconButton, sized Image or Spacer, Box) keep their natural width; text-like children (Text, Column, a vertical Container, a horizontal Container holding any of these, or a Conditional, Tappable or Gesture around one) take what is left, so long text wraps instead of pushing the controls off screen. A horizontal Container of controls only (chips, buttons, sized boxes) is a control too, so a chip in one stays put when the text beside it wraps. A child that fills height (a scroll, a filling box, or a box without `height_dp` around one) takes the row's full height whatever its `align`, and such a row fills the height its column leaves, as a column does. |
| `core://Container` | `orientation`, `padding_dp`, `children` | A plain stack; prefer Column and Row. |
| `core://Box` | `content`, `background`, `content_color`, `border_color`, `corner_dp`, paddings, `width_dp`, `height_dp`, `align`, `fill` | A surface. `fill` (feature `box_fill`): fills the space given, content placed by `align` both ways. A box without `height_dp` around a filling child (a scroll, a filling box) fills height too, so `width_dp` can size a column of a row; as a workspace's one slot, the whole screen with no title bar. Colours are `#RRGGBB` or a binding that resolves to one. |
| `core://Spacer` | `size_dp` | Empty space. |
| `core://Divider` | | A thin line. |
| `core://Sticky` | `content` | Scrolls until it reaches the top, then stays pinned while the rest of its parent scrolls under it. Not inside a List or Carousel template, or another Sticky. |
| `core://Text` | `value_expr`, `style`, `format`, `count` | Styles: `Display`, `Headline`, `Title`, `Label`, `Body`, `Caption`, `Secondary`. `format: {date: Relative\|Short, past?}` draws `YYYY-MM-DD` values against the viewer's clock ("tomorrow", "Overdue"). `count: {query_key, where?, zero_text?}` fills `$count`. |
| `core://Image` | `url`, `shape`, `size_dp`, `aspect_ratio`, `content_description`, `blurhash` | `shape`: `Square`, `Rounded`, `Circle`. A square of `size_dp`, or full width at `aspect_ratio`. |
| `core://Carousel` | `items`, `item_template`, `aspect_ratio`, `aspect_field`, `min_aspect`, `max_aspect`, `tag` | A swipeable pager over entity URIs, with page dots. |
| `core://List` | `query_key`, `item_template`, `empty_text`, `indent`, `filter`, `where`, `group_by`, `columns`, `spacing_dp`, `padding_h_dp`, `focus` | The entities of a query. `filter` searches as you type (every word in one of `fields`, ignoring case and accents); `where: {field, equals}` keeps exact matches, and a list of such conditions keeps items matching all of them (feature `list_where_all`, the same shape as `count`'s `where`); `group_by` draws a header where a field changes (`headers_only`, feature `list_group_headers`: only the headers, one per distinct value); `columns` (2–6) makes a grid; `focus` scrolls to one item and tints it. |
| `core://Conditional` | `value`, `content`, `equals` | `content` only when `value` is truthy, or, with `equals`, when it equals that text. Works with `$form` values to switch views. |
| `core://Button` | `label`, `on_click` | A text button. |
| `core://IconButton` | `icon`, `content_description`, `on_click`, `active`, `active_icon`, `active_tint`, `active_content_description`, `size_dp`, `tag` | `active` binds a bool (`$item.is_liked`) to switch icon and tint. Icon names are `protocol::ICONS` (`describe_vocabulary` lists them); the newest, `stapler` (a stapler from the side), needs a build with feature `icons_stapler`. |
| `core://Tappable` | `on_click`, `content`, `content_description`, `tag` | Makes any node tappable; buttons inside keep their own taps. Drawn pressed (slightly darker) while held. |
| `core://Gesture` | `content`, `on_tap`, `on_long_press`, `on_swipe_up`, `on_swipe_down`, `on_swipe_left`, `on_swipe_right`, `content_description` | `content` with gestures, each running its intent; at least one is set (feature `gesture`, decision `edge-overlays-and-gestures`). A swipe is a drag of at least 48 dp mostly one way (left and right on screen), run when the finger lifts. A pull the content can't scroll for counts, so a page inside still scrolls and pulling it down at its top is a swipe down; a drag an outer scroll takes doesn't, so put the gesture around what scrolls. Buttons inside keep their own taps. As a workspace's one slot around a filling node, it fills the screen too. |
| `core://Chip` | `text`, `icon`, `tone` | A small label; `tone` `Neutral` or `Warning`. |
| `core://Badge` | `text`, `icon` | A one-line note with an icon ("Bob boosted"). |
| `core://Progress` | `size_dp` | An indeterminate spinner. |
| `core://Switch` | `checked`, `on_click`, `content_description` | An on/off switch, on while `checked` (a binding) is truthy; a tap dispatches `on_click`. |
| `core://Layout` | `layout`, `show_when`, `equals`, `presentation` (`Inline`, `Sheet`, `Panel`, `Overlay`), `animation`, `params`, `height_dp`, `edge`, `size_dp` | Another layout of the pod (decision `layouts-not-apps`). `Inline` draws it in place: `height_dp` tall, the whole screen as a workspace's one slot, else as tall as its content (its slots only). `Sheet`, `Panel` and `Overlay` present it over the screen. An `Overlay` (feature `overlay_edge`, decision `edge-overlays-and-gestures`) comes from its `edge` (`top`, `bottom`, `start`, `end`; bottom when unset), `size_dp` tall (top, bottom) or wide (start, end), sliding in from that edge unless `animation` is `fade` or `none`; a `Sheet` is an overlay from the bottom, a `Panel` one from the start. `edge` and `size_dp` go only with `Overlay`. Every overlay closes when dragged back toward its edge past a quarter of its size or with a fling: a sheet (top or bottom) by its handle, on its inner side, or by its content once that can't scroll further that way; a panel (start or end) anywhere. With `show_when` (e.g. `$form.panel`, `${local://inbox#unread}`) it shows only while that holds (or equals `equals`); dismissing a sheet, panel or overlay clears that `$form` field. `params` bind as `$route.<name>` in it. `layout` is an id or `$item.id`. |
| `core://TextField` | `field`, `placeholder`, `max_length`, `on_submit`, `lines` | Edits `$form.<field>`; an x at its right end empties it while it has text. With `on_submit`: a send button that dispatches, then clears. Without: a search box, or a multi-line field with `lines`. |
| `core://ChoiceChips` | `field`, `options: [{label, value}]`, `padding_h_dp`, `remember` | A chip row editing `$form.<field>`. `remember` keeps the choice on the device. |
| `core://MediaPicker` | `field`, `max_items`, `label` | Picks photos; `$form.<field>` is the list of staged attachment ids. |

Icons (`protocol::ICONS`): `favorite`, `favorite_border`, `chat_bubble`, `bookmark`, `bookmark_border`, `repeat`,
`warning`, `send`, `arrow_back`, `close`, `flame`, `timer`, `add_photo`, `add`, `refresh`, `check`, `check_box`,
`check_box_outline_blank`, `delete`, `settings`, `notifications`.

A node type the client doesn't know is skipped, and its JSON kept, so old versions still parse.

## Intents

| Intent | Fields | Does |
|---|---|---|
| `action.navigation.open` | `target_uri`, `presentation` (`Push`, `Modal`, `Replace`, `Sheet`, `Panel`, `Overlay`), `layout`, `params`, `animation`, `edge`, `size_dp` | Without `layout`: opens the screen of the layout on top (else home's) whose route matches. With `layout` (feature `navigate_layout`): presents that layout on the back stack (Push: the whole screen with Back; Sheet, which the user can drag down to close; Panel; Overlay from `edge`, as for `core://Layout`), `params` as its `$route.<name>`, then its screen matching `target_uri`, if any. A screen opened while a Sheet, Panel or Overlay of the same layout is on top opens inside it: the frame's content slides aside for it, with Back and the screen's title at its top (decision `screens-inside-frames`). |
| `action.navigation.back` | | Pops the top of the back stack (a screen or a presented layout), as system Back does. |
| `action.mutation.optimistic` | `entity_uri`, `field`, `value_expr`, `endpoint`, `payload`, `also`, `rollback_value` | Changes fields at once and sends `payload` to `endpoint` through the outbox; a refusal restores each field's value from before the change. `rollback_value` (optional, `null` when left out) is what `field` gets back when it had no value before; leave it out (decision `optional-rollback-value`). |
| `action.mutation.create` | `query_key`, `entity_uri`, `entity`, `endpoint`, `payload`, `refresh_workspace`, `attachments`, `position` | Adds `entity` to the list at once, uploads attachments, sends the create, then reloads what lists the query. A refusal removes it. |
| `action.local.set` | `entity_uri`, `field`, `value_expr`, `also` | Changes fields on this device only (view state), in the engine too. Loads that started before it, or while a write was still on its way to the pod, keep it; the next load after every write is on the pod replaces it with the pod's value. |
| `action.data.load` | `path` | Loads and caches a data path. |
| `action.sequence` | `steps` | Runs intents in order as one tap, e.g. a create then `action.navigation.back`. |
| `action.link.open` | `url` | Opens an `http(s)://` URL in the browser. |
| `action.confirm` | `title`, `message`, `confirm_label`, `then` | Asks first: the app's own dialog with the layout's wording, a `confirm_label` button and Cancel; runs `then` only on confirm (in a sequence, the later steps wait too). Wrap `leave_pod`, `sign_out` and deletes in it (decision `confirm-intent`). |
| `action.auth.login` | `instance_url`, `app_path`, `token_path` | Starts an OAuth2 PKCE sign-in to an upstream. |
| `action.auth.session` | `path`, `payload` | Opens a pod session (the pod code screen). |
| `action.device` | `command`, `app` (or `layout`), `id`, `value` | Something the device does: `open_notification` (`id`: presents its layout and target), `mark_all_read`, `toggle_channel` (`app`, `id`), `sign_in` (`id` of a provider; `value`: another server, e.g. `$form?.server`), `sign_out`, `set_server` (`id` of a server option), `show_versions` (`app`: a layout; loads its versions into `local:versions`), `pin_version` (`id`: show that version on this device; kept across restarts until `follow_latest` or leaving the pod), `follow_latest`, `load_older_notifications` (the next page into `local:notifications`, feature `device_inbox_paging`), `mark_read` (`id`: one notification), `hide_layout` and `unhide_layout` (`app`: a layout, hidden from every device's `local:layouts`; feature `device_layout_controls`), `delete_layout` (`app`: deletes the layout on the pod; only inside `action.confirm`), `request_notification_permission` (Android's prompt to allow notifications, or the app's notification settings once the prompt can't show again; nothing on desktop; feature `device_notification_permission`), `leave_pod` (forget the pod on this device), `refresh`. Any layout may use any command. `open_app` and `show` were retired with the dock and drawer; `open_settings` and `open_inbox` are retired too: present those layouts with `action.navigation.open` (`settings` as a `Panel`, `inbox` as a `Sheet`). |

## Features and compatibility

Every piece of vocabulary above is a named feature, one file in `crates/protocol/features/`, and
`protocol::required_features` maps a layout to the features it uses. Clients send the features they draw in
`X-Construct-Features`, and the pod serves each client the newest version it can fully draw. The versions API marks
each version `compatible` or, for that client, `update_required` (uses something newer) or `retired` (uses something
removed). See decision [`named-vocabulary`](decisions/named-vocabulary.md).

- **Adding vocabulary** needs a client build: add the type or field to `protocol`, a feature file, an arm in
  `required_features`, drawing it in `crates/construct-render` (and `construct-sim` scripts if it adds
  interaction), and tests. Steps:
  [`crates/protocol/features/README.md`](../crates/protocol/features/README.md). Then layouts can use it; clients
  without it keep getting the previous version.
- **Removing vocabulary** never edits layout files: move the feature's file to `crates/protocol/features/removed/`,
  and only the versions that use it are retired.

## Device data

Feature `local_data` (decision `blueprints-and-home`): entities and a query the client fills itself, with no pod
request. Any layout may bind them; a custom app needs no feed for them.

| Entity or query | Fields |
|---|---|
| `local://connection` | `online`, `pending` (unsent changes), `label` ("Offline · 2 pending"), `attention` (offline or changes waiting) |
| `local://inbox` | `unread`, `label` ("3 unread"), `has_older` (the inbox has more than `local:notifications` holds), `loading` (an older page is loading) |
| `local://layouts` | `count`: the pod's layouts but `home` |
| `local:layouts` (query) | `local://layouts/<id>`: `id`, `title`, `icon`, `kind`, `detail`, `unread`, `version` ("v12 · latest", "v10 · pinned"), `hidden`; hidden ones left out. `local:apps` (`local://apps/<id>`) is its old name. |
| `local:all_layouts` (query) | The same rows with the hidden layouts too (`hidden: true`), e.g. for a "Hidden" list with `unhide_layout` buttons (feature `device_layout_controls`). |
| `local:notifications` (query) | The newest page, then the older pages `load_older_notifications` loaded: `id`, `app_id`, `app_title`, `title`, `body`, `when` ("5 min"), `day` ("Today"), `unread` |
| `local:channels` (query) | `app_id`, `app_title`, `id`, `title`, `description`, `enabled` |
| `local:sign_ins` (query) | `id`, `title`, `detail` ("@dana · pixelfed.example") |
| `local:providers` (query) | `id`, `title`, `custom_server` (another server may be typed), `server_placeholder`: sign-ins the pod offers that aren't signed in |
| `local:versions` (query) | the versions of the layout `show_versions` named, newest first: `version`, `label`, `detail`, `when`, `compatible`, `showing` |
| `local://versions` | `app_id`, `app_title`, `pinned` (or "" while following the latest), `loading` |
| `local:servers` (query) | `id`, `title`, `url`, `selected`: the servers this device can use |
| `local://device` | `platform`; `notification_permission`: meant to be `granted` or `denied` on Android (whether notifications can show); `""` today on every client (the Android app doesn't report it yet, decision `rust-clients`), and where it doesn't apply (desktop, simulator) |
| `local://nav` | Where the layout is drawn (feature `nav_data`): `can_go_back`, `presentation` (`root`, `push`, `sheet`, `panel`, `screen`, `inline`), `pushed`, `inline`, `in_frame` (in a sheet, panel or overlay; an overlay from the top or bottom says `sheet`, from the start or end `panel`) |
| `local://screen` | In a page screen (feature `nav_data`): `loading`, `error` ("" when none) of its `data` load |
| `local://theme` | The pod's theme colours for the app's mode (feature `theme`): `page` (a page's background), `card` (cards and rows on it), `card_border`, `muted_text`, `accent`, `accent_surface`, `muted_surface`, `success`, `success_surface`, `error`, `error_surface`, `highlight`, `like`, `unread`, each with a light and a dark value; set with MCP `set_theme`. Bind these instead of fixed `#RRGGBB` colours, which stay the same in dark mode (decision `dark-palette`; layout writes answer `warnings` for each). |
| `local://session` | `open`, `server`, `error`, `message`: the pod sign-in; `status`: an upstream sign-in in progress or failed |

A `core://Conditional` `equals` may bind the item (`"equals": "$item.id"`). A `core://Conditional` value may be such a binding: `{"value": "${local://layouts#count}", "equals": "0", ...}`.

## Porcelain: components

A component is a node tree with parameters, defined as data and expanded by the pod before serving, so the app never
sees one and changing one needs no app build. Use one anywhere a node goes:

```json
{ "type": "core://Use", "component": "core:card", "args": {"spacing": 6}, "children": [
  { "type": "core://Text", "style": "Label", "value_expr": "$item.title" } ] }
```

- **`core:` components** ship with the server: `crates/pod-core/components/<name>/NNNN.json`, `{"note", "component"}`,
  append-only like layout files. An adapter ships its own the same way, in `crates/app-*/components/<name>/` (its
  build script embeds them; `AppAdapter::component_files`); names are unique across crates (decision
  `adapter-components`). Every blueprint is one too: `core:<blueprint>` is its whole layout (only as a
  layout's root; the layout keeps its own id) and `core:<blueprint>/<part>` a part (`screen:<id>` in `screens`,
  `slot:<id>`, or a name from the file's `"parts"`). `{"type": "core://Ref", "app", "part"}` is the older spelling.
- **`pod:` components** are the pod's own, defined by an agent (MCP `define_component`, `copy_component`), versioned
  and deletable while no layout uses them.
- **A definition** is `{"doc", "params": [{"name", "type": "text|number|bool|uri|binding", "default"?, "required"?,
  "doc"}], "children"?: true, "template", "examples": [{"args", "children"?, "note"}]}`. In the template,
  `$arg.<name>` is an argument (a string that is only `"$arg.n"` keeps the argument's type; it may name a key too),
  `{"type": "core://Children"}` is where the caller's children go (`"at": i` for one), `{"type": "core://IfArg",
  "arg", "equals"?, "then", "else"?}` keeps a branch, and `{"type": "core://Fragment", "children"}` is several
  siblings. Every example must expand to plumbing.
- **Following and pinning.** Without `version` a use follows the component: the layout gets a new version when the
  component changes. With `version` it stays.
- **Seeing the result.** MCP `expand` shows what a layout, a component use or any node becomes, and the features it
  needs; `get_component` shows each example expanded.

The server components:

| Component | Parameters | Draws |
|---|---|---|
| `core:workspace` | `title`, `title_scrolls`, `fills_screen`; children: the content | A page as a workspace looks: the headline title (Back when pushed), the content scrolling under it, a little space. |
| `core:screen` | `title`, `data`, `has_footer`; children: content, footer | A screen as the app drew it: Back, title and spinner, the load's error with Try again, scrolling content, footer. Its `data` is only for Try again: the screen's own `data` (the same path) is what loads when it opens, and a screen that gives `core:screen` a `data` without loading it is refused. |
| `core:back_button` | | Back, shown while `local://nav#can_go_back`. |
| `core:header_bar` | `title`, `style`, paddings; children: actions | A title on the left and icon buttons on the right, on one line. |
| `core:page_header` | `title`, `style`, paddings; children: actions | A `core://Page`'s header on one line: Back (while it can go back), the title, and icon buttons on the right. |
| `core:section_header` | `text`, `style`, `padding_v`, `divider` | A section's padded heading, optionally under a divider. |
| `core:list_row` | `title`, `trailing`, `padding_v` | A row with a title and a quiet trailing text. |
| `core:card` | `spacing`, `border`; children | A bordered, rounded card of its children in a column. |
| `core:filled_card` | `background`, `color`, `spacing`, paddings; children | A filled, rounded card of its children in a column, its text in one colour (an item's heading, a status at a glance). |
| `core:pill` | `text`, `background`, `color`, `style`, `corner` | A small tinted tag (a date, a status, a count); `corner` defaults to 8. |
| `core:dot` | `color`, `size`, `corner` | A small coloured dot. |
| `core:warning_chip` | `text` | A warning chip. |
| `core:link_button` | `label`, `url` | A button opening a link, shown while the link is set. |
| `core:author_header` | `avatar`, `name`, `handle`, `size`, paddings | A round avatar beside the name and handle. |
| `core:toggle_action` | `field`, `icon`, `active_icon`, `label`, `active_label`, `endpoint`, `payload_field`, `tint`, `count_field`, `tag` | An icon button that flips a boolean field at once and tells an endpoint, optionally moving a count with it. |
| `core:stat` | `value`, `label` | A number with a label under it. |
| `core:pixelfed_sign_in` | `server`, `instance_url`, `other_servers`, `placeholder` | Pixelfed's signed-out screen: a sentence, "Sign in with Pixelfed", and a box for another server (shipped by `app-pixelfed`). |
| `core:pixelfed_post` | `focus` | A Pixelfed post and its comments on a post screen (route `$route.id`), optionally scrolled to and highlighting one comment; the content of the blueprint's `post`, `comments` and `comment_thread` screens (shipped by `app-pixelfed`). |
| `core:pixelfed_comment_box` | | The box that comments on the screen's post; those screens' footer (shipped by `app-pixelfed`). |
| `core:message_page` | `title`, `body`, `background`; children: actions | A whole page with a message in the middle and actions under it. |

Their definitions are the files; `get_component` reads them with examples. See decision
[`plumbing-and-porcelain`](decisions/plumbing-and-porcelain.md), and [`layout-parts`](decisions/layout-parts.md) for
how references to blueprints follow them.
