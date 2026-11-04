# Writing layouts

A layout is JSON: a tree of generic `core://` nodes with intents attached. The pod serves it; every client draws it
with the same renderer. This page covers the file format, how to publish a new version, and the vocabulary. The
types themselves are in `crates/protocol/src/ast.rs` (nodes) and `crates/protocol/src/intent.rs` (intents), and each
feature has a one-paragraph description in `crates/protocol/features/`.

## Publishing a new layout version

Built-in apps' layouts live in `layouts/<app>/NNNN.json`. Adding a file is the only way to change how a built-in app
looks.

1. Copy the newest file of the app to the next number (four digits, no gaps), e.g. `layouts/recipes/0005.json` to
   `layouts/recipes/0006.json`, and edit it.
2. The file is `{"note": "<one-line changelog>", "layout": <core://Workspace>}`. The note is what the app menu's
   version history shows. The root's `id` must be the app id. Don't declare features: the pod works them out. Older
   files carry a `"schema"` number; it is ignored.
3. Run `cargo test` (`-p app-pixelfed`, `-p app-recipes` or `-p app-watcher` is enough). It validates every file:
   structure, size, slots, routes, endpoints the adapter declares, and nesting rules.
4. Check it in the [simulator](simulator.md) and put screenshots in the PR.
5. Merge to `main`. On start, the pod appends the file as the app's next version. Connected clients get a `layout`
   event on their stream and refetch; a client that pinned an older version keeps it and is told a newer one exists.

There is no list of files to edit: each adapter's `build.rs` embeds its `layouts/<app>/` directory.

**Files are append-only.** Never edit, rename or delete a file that has been deployed: the pod refuses to start and
names the file. To undo a change, add a new file with the old content. Reformatting a file without changing its
layout or note is fine. If another branch took your number first (git reports an add/add conflict), merge both
layouts into the next number: each file is a whole snapshot.

After changing a `pixelfed_home` layout, regenerate the simulator fixtures:
`UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::`.

## Structure of a workspace

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
- **Screens** open when an `action.navigation.open` has a `target_uri` matching their `route`. `{name}` segments
  capture route parameters, which `$route.<name>` substitutes anywhere in the screen. `data` is a pod path loaded
  when the screen opens; it answers `{entities, queries}`. `content` scrolls; `footer` stays at the bottom. Back (the
  arrow, Android Back, `action.navigation.back`) closes the screen.

## Bindings

| Syntax | Where | Means |
|---|---|---|
| `$item.<field>` | inside a List, Carousel or group-header template | a field of the current entity |
| `${<uri>#<field>}` | any text | a field of any entity, e.g. `${core://sources/pixelfed#signed_in}` |
| `$route.<name>` | in a screen | a parameter captured from the route |
| `$form.<field>` | in intents and filters | the value a TextField, ChoiceChips or MediaPicker with that `field` edits; an intent is not sent while a referenced value is blank |
| `$form?.<field>` | in intents | the same, but a blank value binds to `""` instead of blocking |
| `$nonce` | in intents | a fresh value per dispatch (one per `action.sequence`), for temporary URIs and idempotency keys |
| `$attachments.<name>` | in a create | staged attachments: local URIs in `entity`, upload ids in `payload` |
| `$count` | in a Text with `count` | the number of matching entities |

Mutation expressions (`value_expr` of `action.mutation.optimistic` and `action.local.set`) are a small subset
evaluated against the entity before the change: a JSON literal, `entity.<field>`, `!entity.<field>`,
`entity.<field> ± N`, and `cond ? a : b` with those as branches (D4). For example
`entity.is_liked ? entity.like_count - 1 : entity.like_count + 1`.

## Nodes

| Node | Fields | Notes |
|---|---|---|
| `core://Column` | `children`, `spacing_dp`, `padding_h_dp`, `padding_v_dp`, `align` | Children stacked, full width. |
| `core://Row` | `children`, `spacing_dp`, paddings, `align` | Side by side, vertically centred, no wrapping. A Spacer without a size takes the remaining width. |
| `core://Container` | `orientation`, `padding_dp`, `children` | A plain stack; prefer Column and Row. |
| `core://Box` | `content`, `background`, `content_color`, `border_color`, `corner_dp`, paddings, `width_dp`, `height_dp`, `align` | A surface. Colours are `#RRGGBB` or a binding that resolves to one. |
| `core://Spacer` | `size_dp` | Empty space. |
| `core://Divider` | | A thin line. |
| `core://Sticky` | `content` | Scrolls until it reaches the top, then stays pinned while the rest of its parent scrolls under it. Not inside a List or Carousel template, or another Sticky. |
| `core://Text` | `value_expr`, `style`, `format`, `count` | Styles: `Display`, `Headline`, `Title`, `Label`, `Body`, `Caption`, `Secondary`. `format: {date: Relative\|Short, past?}` draws `YYYY-MM-DD` values against the viewer's clock ("tomorrow", "Overdue"). `count: {query_key, where?, zero_text?}` fills `$count`. |
| `core://Image` | `url`, `shape`, `size_dp`, `aspect_ratio`, `content_description`, `blurhash` | `shape`: `Square`, `Rounded`, `Circle`. A square of `size_dp`, or full width at `aspect_ratio`. |
| `core://Carousel` | `items`, `item_template`, `aspect_ratio`, `aspect_field`, `min_aspect`, `max_aspect`, `tag` | A swipeable pager over entity URIs, with page dots. |
| `core://List` | `query_key`, `item_template`, `empty_text`, `indent`, `filter`, `where`, `group_by`, `columns`, `spacing_dp`, `padding_h_dp`, `focus` | The entities of a query. `filter` searches as you type (every word in one of `fields`, ignoring case and accents); `where` keeps exact matches; `group_by` draws a header where a field changes; `columns` (2–6) makes a grid; `focus` scrolls to one item and tints it. |
| `core://Conditional` | `value`, `content`, `equals` | `content` only when `value` is truthy, or, with `equals`, when it equals that text. Works with `$form` values to switch views. |
| `core://Button` | `label`, `on_click` | A text button. |
| `core://IconButton` | `icon`, `content_description`, `on_click`, `active`, `active_icon`, `active_tint`, `active_content_description`, `size_dp`, `tag` | `active` binds a bool (`$item.is_liked`) to switch icon and tint. |
| `core://Tappable` | `on_click`, `content`, `content_description`, `tag` | Makes any node tappable; buttons inside keep their own taps. |
| `core://Chip` | `text`, `icon`, `tone` | A small label; `tone` `Neutral` or `Warning`. |
| `core://Badge` | `text`, `icon` | A one-line note with an icon ("Bob boosted"). |
| `core://Progress` | `size_dp` | An indeterminate spinner. |
| `core://TextField` | `field`, `placeholder`, `max_length`, `on_submit`, `lines` | Edits `$form.<field>`. With `on_submit`: a send button that dispatches, then clears. Without: a search box, or a multi-line field with `lines`. |
| `core://ChoiceChips` | `field`, `options: [{label, value}]`, `padding_h_dp`, `remember` | A chip row editing `$form.<field>`. `remember` keeps the choice on the device. |
| `core://MediaPicker` | `field`, `max_items`, `label` | Picks photos; `$form.<field>` is the list of staged attachment ids. |

Icons (`protocol::ICONS`): `favorite`, `favorite_border`, `chat_bubble`, `bookmark`, `bookmark_border`, `repeat`,
`warning`, `send`, `arrow_back`, `close`, `flame`, `timer`, `add_photo`, `add`, `refresh`, `check`, `check_box`,
`check_box_outline_blank`, `delete`.

A node type the client doesn't know is skipped, and its JSON kept, so old versions still parse.

## Intents

| Intent | Fields | Does |
|---|---|---|
| `action.navigation.open` | `target_uri`, `presentation` (`Push`, `Modal`, `Replace`) | Opens the screen whose route matches. |
| `action.navigation.back` | | Closes the top screen. |
| `action.mutation.optimistic` | `entity_uri`, `field`, `value_expr`, `rollback_value`, `endpoint`, `payload`, `also` | Changes fields at once and sends `payload` to `endpoint` through the outbox; a refusal rolls back. |
| `action.mutation.create` | `query_key`, `entity_uri`, `entity`, `endpoint`, `payload`, `refresh_workspace`, `attachments`, `position` | Adds `entity` to the list at once, uploads attachments, sends the create, then reloads what lists the query. A refusal removes it. |
| `action.local.set` | `entity_uri`, `field`, `value_expr`, `also` | Changes fields on this device only (view state). |
| `action.data.load` | `path` | Loads and caches a data path. |
| `action.sequence` | `steps` | Runs intents in order as one tap, e.g. a create then `action.navigation.back`. |
| `action.link.open` | `url` | Opens an `http(s)://` URL in the browser. |
| `action.auth.login` | `instance_url`, `app_path`, `token_path` | Starts an OAuth2 PKCE sign-in to an upstream. |
| `action.auth.session` | `path`, `payload` | Opens a pod session (the pod code screen). |

## Features and compatibility

Every piece of vocabulary above is a named feature, one file in `crates/protocol/features/`, and
`protocol::required_features` maps a layout to the features it uses. Clients send the features they draw in
`X-Construct-Features`, and the pod serves each client the newest version it can fully draw. The versions API marks
each version `compatible` or, for that client, `update_required` (uses something newer) or `retired` (uses something
removed). See decision [`named-vocabulary`](decisions/named-vocabulary.md).

- **Adding vocabulary** needs a client build: add the type or field to `protocol`, a feature file, an arm in
  `required_features`, the Kotlin model and renderer in `kmp-app/shared`, and tests. Steps:
  [`crates/protocol/features/README.md`](../crates/protocol/features/README.md). Then layouts can use it; clients
  without it keep getting the previous version.
- **Removing vocabulary** never edits layout files: move the feature's file to `crates/protocol/features/removed/`,
  and only the versions that use it are retired.

## Reusing built-in parts

A custom app can embed a part of a built-in layout by reference, and the pod keeps it current:

```json
{ "type": "core://Ref", "app": "pixelfed_home", "part": "screen:post" }
```

`part` is `screen:<id>`, `slot:<id>`, or a name from the layout file's `"parts"`. See decision
[`layout-parts`](decisions/layout-parts.md) and [custom-apps.md](custom-apps.md).
