# Writing layouts

A layout is JSON: a tree of generic `core://` nodes with intents attached. The pod serves it; every client draws it
with the same renderer. The vocabulary has two layers (decision
[`plumbing-and-porcelain`](decisions/plumbing-and-porcelain.md)): **plumbing**, the nodes, intents and device data the
app draws ([Nodes](#nodes), [Intents](#intents), [Device data](#device-data)), and **porcelain**, components defined
as data in terms of plumbing that the pod expands before serving ([Porcelain: components](#porcelain-components)). This page covers the file format, how to publish a new version, and the vocabulary. The
types themselves are in `crates/protocol/src/ast.rs` (nodes) and `crates/protocol/src/intent.rs` (intents), and each
feature has a one-paragraph description in `crates/protocol/features/`.

## Publishing a new layout version

Built-in layouts (the core layouts) live in their source crate, `crates/source-*/layouts/<layout>/NNNN.json`
(Pixelfed's in `crates/source-pixelfed/layouts/pixelfed/`, Events' in `crates/source-events/layouts/events/`).
Adding a file is the only way to change how a core layout looks.

1. Copy the newest file of the app to the next number (four digits, no gaps), e.g.
   `crates/source-events/layouts/events/0006.json` to `.../events/0007.json`, and edit it.
2. The file is `{"note": "<one-line changelog>", "layout": <core://Workspace or core://Page>}`. It may use server
   components (`core://Use` of `@core/<name>`); the pod records the layout they expand to. The note is what the
   core layout's history shows. The root's `id` must be the core layout's id. Don't declare features: the pod works them out. Older
   files carry a `"schema"` number; it is ignored.
3. Run `cargo test` (`-p source-pixelfed` or `-p source-events` is enough). It validates every file:
   structure, size, slots, routes, endpoints the source declares, and nesting rules.
4. Check it in the [simulator](simulator.md) and put screenshots in the PR.
5. Merge to `main`. When a pod opens, it moves the core layout's name (`@core/<layout>`) to the new file, keeping
   the older files in its history, and re-seals what follows it. Connected clients get a `construct` event with
   their new root and fetch the objects they lack; a client too old to draw the new file stays on the newest it can.

There is no list of files to edit: each source crate's `build.rs` embeds its `layouts/<layout>/`
directories.

**Files are append-only.** Never edit or rename a file that has been deployed: the pod refuses to start and names
the file. To undo a change, add a new file with the old content. Reformatting a file without changing its
layout or note is fine. If another branch took your number first (git reports an add/add conflict), merge both
layouts into the next number: each file is a whole snapshot.

**Old files can be deleted** once no client needs them, oldest first (decision `retired-layout-files`); each
core layout keeps at least its newest file. Its files then start above `0001`. A pod that recorded the deleted files
keeps them in its name's history; a new pod's history starts from the oldest file left, so a client too old to draw
that file draws nothing there.

After changing a `pixelfed` layout, regenerate the simulator fixtures:
`UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::`.

## Structure of a page

Plumbing (feature `page`): the root is any node, and the app draws nothing around it. A page draws its own header
and Back, and wraps what scrolls in `core://Scroll`:

```json
{
  "type": "core://Page", "id": "events", "title": "Events",
  "root": {"type": "core://Column", "children": [
    {"type": "core://Row", "padding_h_dp": 12, "children": [
      {"type": "core://Use", "ref": "@core/back_button"},
      {"type": "core://Text", "style": "Title", "value_expr": "Events"}, {"type": "core://Spacer"},
      {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
       "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}]},
    {"type": "core://Scroll", "content": {"type": "core://Column", "children": []}}]},
  "screens": [{"id": "event", "route": "collections://events/{id}", "data": "/v1/collections/events/row?id=$route.id",
    "item": "collections://events/$route.id", "root": {"type": "core://Use", "ref": "@core/card", "children": []}}]
}
```

`title` names the layout (lists, accessibility) and is not drawn. `@core/page_header` draws that header row (Back, the
title, actions as its children). In the root column a `core://Scroll`, a filling
`core://Box` or an inline `core://Layout` (or a `core://Gesture` around one) takes the height the other children
leave. Inside a `core://Scroll` an inline layout is as tall as its content, so one in a scrolling page needs no
`height_dp`; a filling box there takes the room short content leaves and is otherwise as tall as its own content. A screen's `item` binds `$item`
throughout it (feature `screen_item`). `local://nav` and `local://screen` say where it is drawn and how its data load
is going ([Device data](#device-data)).

## Structure of a workspace

`core://Workspace` is porcelain: the pod serves it to a build that draws `page`, `scroll` and `nav_data` expanded
through `@core/workspace` and `@core/screen`, and builds since issue #126 draw no workspace of their own (features
`workspace`, `screens` and `title_scrolls` are in `features/removed/`, decision `workspace-drawn-by-the-pod`). Layouts
may still be written with it; prefer a `core://Page` for a new one.

```json
{
  "type": "core://Workspace",
  "id": "events",
  "title": "Events",
  "slots": [
    { "slot_id": "matches", "provider_uri": "events://matches", "content": { "type": "core://Column", "children": [] } }
  ],
  "screens": [
    {
      "id": "event",
      "route": "collections://events/{id}",
      "title": "Event",
      "data": "/v1/collections/events/row?id=$route.id",
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

## Screens of every size

One layout serves every window (decision `screen-sizes`): the pod never sends a different layout per size. A layout
that doesn't adapt to the window draws at most 480 points wide, so a phone layout stays phone-sized in a browser or
a pane (decision `phone-width-layouts`): centred on a screen of its own, and at the start of its room when embedded
or in a panel, so panes build up from the left (a phone layout's panel is phone wide, beside the column before it). Using any of the following anywhere in a
layout's own tree (not in the layouts it embeds) makes it adapt, and it gets the whole width to use as it says:

- `max_width_dp` on a `core://Column` keeps a page readable on a desktop: at most that wide, centred.
- `min_column_dp` on a `core://List` fills a grid with as many columns as fit.
- `${local://device#size_class}` (`compact`, `medium`, `expanded`) with `core://Conditional` shows different
  things by size; `width_dp` and `height_dp` are the window's size in points. A conditional on
  `${local://device#platform}` (a fork for the `web`) counts too.
- `panels: true` on an inline `core://Layout`.
- `@core/two_pane` puts a list beside its detail on an `expanded` window.

Check a layout at several sizes in the simulator (`--screen phone|tablet|desktop`, or `screen <size>` in a script)
or with `render_layout_preview`.

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
| `${local://<entity>#<field>}`, `local:notifications` | anywhere | [device data](#device-data), with no feed |

Mutation expressions (`value_expr` of `action.mutation.optimistic` and `action.local.set`) are a small subset
evaluated against the entity before the change: a JSON literal, `entity.<field>`, `!entity.<field>`,
`entity.<field> ± N`, and `cond ? a : b` with those as branches (D4). For example
`entity.is_liked ? entity.like_count - 1 : entity.like_count + 1`.

## Nodes

| Node | Fields | Notes |
|---|---|---|
| `core://Page` | `id`, `title`, `root`, `screens` | A layout's root with no chrome from the app (feature `page`); only as the root. Screens: `{id, route, title?, data?, item?, root}`. |
| `core://Scroll` | `content`, `follow_end` | `content` scrolls in the height its parent leaves (feature `scroll`); `core://Sticky` pins at its top. Inside another scroll it doesn't scroll itself. Side by side in a `core://Row` that has a height (one in a filling box, or a page's column), each scrolls on its own in the row's full height. `follow_end`: after an action from outside it (a footer's Send), follow the content to its end once it grows. |
| `core://Column` | `children`, `spacing_dp`, `padding_h_dp`, `padding_v_dp`, `align`, `justify`, `max_width_dp` | Children stacked, full width; `max_width_dp` (feature `max_width`, decision `screen-sizes`) stops it at that width, centred in the room it is given, so a page reads as a column on a wide window. In a page's bounded height, a child that fills (a scroll, a filling box, an inline layout) takes the rest of it; in a scroll's content an inline layout is as tall as its content, and another filling child takes what short content leaves, else its own height. `justify` (feature `justify`: `start`, `center`, `end`, `space_between`, `space_evenly`) places the children down the column, which then fills the height it is given. |
| `core://Row` | `children`, `spacing_dp`, paddings, `align` | Side by side, vertically centred, no wrapping. A Spacer without a size takes the remaining width. Controls (Switch, Chip, Button, IconButton, sized Image or Spacer, Box) keep their natural width; text-like children (Text, Column, a Container holding any of these, or a Conditional, Tappable or Gesture around one) take what is left, so long text wraps instead of pushing the controls off screen. A Container of controls only (chips, buttons, sized boxes), either orientation, is a control too, as is a nested Row of controls only (no text, no unsized Spacer), so a chip in one stays put and keeps its width when the text beside it wraps. A child that fills height (a scroll, a filling box, or a box without `height_dp` around one) takes the row's full height whatever its `align`, and such a row fills the height its column leaves, as a column does. `justify` (feature `justify`: `start`, `center`, `end`, `space_between`, `space_evenly`) places the children along the row when they leave room, on any width (four fixed tiles centred: `"justify": "center"`, no spacers); children too wide for the row start at its padding whatever it says. |
| `core://Container` | `orientation`, `padding_dp`, `children` | A plain stack; prefer Column and Row. |
| `core://Box` | `content`, `background`, `content_color`, `border_color`, `border_style`, `corner_dp`, paddings, `width_dp`, `height_dp`, `align`, `fill` | A surface. `border_style` (feature `box_border_style`): `Solid` (default) or `Dashed`, even dashes round the corners; it doesn't gate the layout, a client without it draws the border solid (decision `dashed-box-borders`). `fill` (feature `box_fill`): fills the space given, content placed by `align` both ways. A box without `height_dp` around a filling child (a scroll, a filling box) fills height too, so `width_dp` can size a column of a row; as a workspace's one slot, the whole screen with no title bar. Colours are `#RRGGBB` or a binding that resolves to one. |
| `core://Spacer` | `size_dp` | Empty space. |
| `core://Divider` | | A thin line. |
| `core://Sticky` | `content` | Scrolls until it reaches the top, then stays pinned while the rest of its parent scrolls under it. Not inside a List or Carousel template, or another Sticky. |
| `core://Text` | `value_expr`, `style`, `format`, `count`, `weight`, `caps`, `max_lines`, `strike`, `markdown` | Styles: `Display`, `Headline`, `Title`, `Label`, `Body`, `Caption`, `Secondary`. `format: {date: Relative\|Short, past?}` draws `YYYY-MM-DD` values against the viewer's clock ("tomorrow", "Overdue"). `count: {query_key, where?, zero_text?}` fills `$count`; with `sum: "<field>"` (feature `count_sum`, decision `rings-sums-and-week-strips`) it also adds up that field over the same entities, filling `$sum_money` (a price: `$518.84`) and `$sum` (a plain number): a bag's total. `Display` is the serif face (Lora): a dated page's big date, a blank pod's name. `weight: Regular\|Medium\|SemiBold\|Bold` (feature `text_weight`) overrides the style's weight (`Label` is bold, the rest regular). `caps: true` (feature `text_caps`) draws capitals with a little letter spacing, for eyebrow labels ("MOVE OFTEN · THIS WEEK"); screen readers read it as written. `max_lines: 1..20` (feature `text_max_lines`) ends a longer text in an ellipsis. `strike: true` (feature `text_strike`, decision `text-strike`) draws a line through each line of the text, in its colour: a bought item on a shopping list. `markdown: true` (feature `text_markdown`, decision `text-markdown`) reads the bound text as Markdown and draws it natively in the text's style and colour: headings, paragraphs, bulleted, numbered and task lists, quotes, fenced code on a tinted card, tables and rules, with bold, italic, `code`, strikethrough and links (in the theme's `accent`, not tappable) inside them; with `max_lines` only the inline styles are kept, the blocks run together on that many lines (a row's preview). Use it for text that arrives as Markdown, such as an agent's report, never for text people type as plain words. A client without these five ignores them and draws the text in its style (Markdown as written). |
| `core://Image` | `url`, `shape`, `size_dp`, `aspect_ratio`, `content_description`, `blurhash` | `shape`: `Square`, `Rounded`, `Circle`. A square of `size_dp`, or full width at `aspect_ratio`. |
| `core://Carousel` | `items`, `item_template`, `aspect_ratio`, `aspect_field`, `min_aspect`, `max_aspect`, `tag`, `tap_paging`, `on_tap` | A swipeable pager over entity URIs, with page dots. With `tap_paging: true` (feature `carousel_tap_paging`, decision `scatter-and-tap-paging`) it pages on taps instead, as stories do: the left third back, the right third on (wrapping), the middle third runs `on_tap`; bars over the top show the page, and a sideways drag goes to what is around it (a `core://SwipeCard`). Every page's images load up front, and a turn waits (at most 1.5 s) with the current page showing until the next page's photo is ready. Don't `tag` a carousel in a list's template: copies would share the id. |
| `core://Scatter` | `points`, `colors`, `none_color`, `highlight_color`, `muted_color`, `select`, `on_tap`, `on_select`, `aspect_ratio`, `fill`, `tag`, `content_description` | A scatter plot of up to tens of thousands of points (feature `scatter`, decision `scatter-and-tap-paging`). `points` binds one string, the points packed six bytes each (`protocol::scatter`: x, y, a value along the `colors` ramp (low, middle, high) or none, a mark: plain, highlighted (larger, ringed) or muted). Pinch or the wheel zooms and a drag pans, in place. A tap near a point runs `on_tap` with `$point.index`, `$point.x` and `$point.y`; while `select` is truthy, or after a long press, a drag draws a box and its release runs `on_select` with `$select.x0`, `$select.y0`, `$select.x1`, `$select.y1` (0..1) and `$select.box` (`x0-y0-x1-y1` in millionths, fit for a route). Square (or `aspect_ratio`) at its parent's width, or the space given with `fill`. |
| `core://List` | `query_key`, `item_template`, `empty_text`, `indent`, `filter`, `where`, `group_by`, `columns`, `min_column_dp`, `spacing_dp`, `padding_h_dp`, `focus`, `stack`, `trailing`, `merge_query_keys`, `sort_by` | The entities of a query; with `merge_query_keys` (feature `list_merge`, decision `merged-lists`) those of more queries after them, each once, `where` and the rest applying to all, and with `sort_by` in the order of a field's value as text (equal or missing values keep their order): a day's workouts and events as one agenda by start time. `filter` searches as you type (every word in one of `fields`, ignoring case and accents); `where: {field, equals}` keeps exact matches, and a list of such conditions keeps items matching all of them (feature `list_where_all`, the same shape as `count`'s `where`); a condition `{field, older_than: "1d"}` (`s`, `m`, `h`, `d`, `w`) keeps items whose date or time field is more than that long ago on the device's clock, and `not: true` inverts any condition, in a list or a count (feature `list_where_age`, decision `notifications-in-a-collection`): `[{field: "read_at", older_than: "1d", not: true}]` keeps the unread and the recently read; `group_by` draws a header where a field changes (`headers_only`, feature `list_group_headers`: only the headers, one per distinct value); `columns` (2–7; 7 for a week, decision `rings-sums-and-week-strips`) makes a grid; `min_column_dp` (80–1024, feature `list_auto_columns`, decision `screen-sizes`) instead makes as many equal columns as fit at least that wide, so a phone shows one and a desktop several (group headers and the empty text span the width); `focus` scrolls to one item and tints it. A list has no limit or date filter of its own: for "the next three" or "today's", bind a Collections view's query (`collection:<c>:view:<v>`, worked out on the pod: [building.md](building.md#collections)). `stack: true` makes it a deck (feature `list_stack`, decision `swipe-cards`): only the first item is drawn, with the next one behind it, as tall as it and a little narrower, peeking out 10 dp below; a deck has no `columns`, `min_column_dp`, `group_by` or `trailing`. `trailing` (a node, feature `list_trailing`) is one more cell after the items, such as an add button that ends a grid: it goes in the next cell (in a grid, right after the last item, in its row), is bound like the list rather than an item (no `$item`), and is drawn when the list is empty too (not while its query is loading). |
| `core://Conditional` | `value`, `content`, `equals`, `date`, `time`, `not`, `count` | `content` only when `value` is truthy, or, with `equals`, when it equals that text. Works with `$form` values to switch views. With `date: Past\|Today\|Future`, `value` is a date compared with the device's clock: see [Dates and times](#dates-and-times). `not: true` (feature `conditional_not`) inverts the test: an "else" branch. `count: {query_key, where?}` (feature `conditional_count`, the shape of a Text's `count`) tests a query instead of `value` (leave out `value`, `equals` and `date`): the content shows while the query has an entity matching every `where`, or, with `not`, while it has none; nothing shows while the query isn't loaded. E.g. hide a card around an empty list. |
| `core://Button` | `label`, `on_click`, `style`, `size` | A text button running `on_click` (any intent: `action.sequence`, `action.confirm`, `action.mutation.optimistic` with `also`, ...). `style` (feature `button_style`, decision `one-button-node`): `Primary` (the default, filled with the accent: the screen's main action), `Secondary` (outlined on the `card` colour in the text colour: an action beside the main one, Cancel, Stop, Sign out), `Tonal` (the `accent_surface` tint, in the accent) or `Text` (accent text alone). `size`: `Regular` (the default, 40 dp tall) or `Small` (32 dp). The renderer owns every face's height, padding and corner, so buttons side by side always match: every button is a `core://Button` (or `@core/button`), never a `core://Tappable` over a `core://Box` (layout and component writes answer `warnings` for one). A client without the feature ignores both fields and draws a regular primary button. |
| `core://IconButton` | `icon`, `content_description`, `on_click`, `active`, `active_icon`, `active_tint`, `active_content_description`, `size_dp`, `tag` | `active` binds a bool (`$item.is_liked`) to switch icon and tint. Icon names are `protocol::ICONS` (`describe_vocabulary` lists them); the newest, `stapler` (a stapler from the side), needs a build with feature `icons_stapler`. Any icon can instead be its drawing, `path:<SVG path data>` (below). |
| `core://Tappable` | `on_click`, `content`, `content_description`, `tag` | Makes any node tappable; buttons inside keep their own taps. Drawn pressed (slightly darker) while held. |
| `core://Gesture` | `content`, `on_tap`, `on_long_press`, `on_swipe_up`, `on_swipe_down`, `on_swipe_left`, `on_swipe_right`, `content_description` | `content` with gestures, each running its intent; at least one is set (feature `gesture`, decision `edge-overlays-and-gestures`). A swipe is a drag of at least 48 dp mostly one way (left and right on screen), run when the finger lifts. A pull the content can't scroll for counts, so a page inside still scrolls and pulling it down at its top is a swipe down; a drag an outer scroll takes doesn't, so put the gesture around what scrolls. Buttons inside keep their own taps. As a workspace's one slot around a filling node, it fills the screen too. |
| `core://SwipeCard` | `content`, `on_swipe_left`, `on_swipe_right`, `threshold_dp`, `content_description` | A card that follows the finger sideways (feature `swipe_card`, decision `swipe-cards`); at least one side is set. Past `threshold_dp` (default 96, at least 24) it is washed with the colour of the side it would go (right: theme `success`, left: `error`); let go there, or flung that way past half of it, it flies off and runs that side's intent, then is back in its place (the next card of a deck shows there). Let go before, it springs back. A swipe that isn't a drag (a screen reader's, the simulator's) runs the side's intent straight away. Put it as a `stack` list's `item_template`, with intents that take the item out of the list (a verdict field the list's `where` filters on), for a deck. |
| `core://Chip` | `text`, `icon`, `tone` | A small label; `tone` `Neutral` or `Warning`. |
| `core://Badge` | `text`, `icon` | A one-line note with an icon ("Bob boosted"); with `text` `""` just the icon, centred where it is put. |
| `core://Progress` | `size_dp`, `value`, `max`, `color`, `shape`, `label` | An indeterminate spinner; with `value` (feature `progress_value`, decision `determinate-progress`) a determinate bar as wide as it is given, `size_dp` tall (default 6), filled `value / max` (max default 1, clamped) in `color` (default the theme's primary) over a quiet track, read out as a percentage: a lesson's place (`"value": "$item.reading_card", "max": "$item.card_count"`), a week's goal. With `shape: Ring` as well (feature `progress_ring`, decision `rings-sums-and-week-strips`) it is a ring `size_dp` across (default 56), the fraction drawn clockwise from the top, with `label` (bound text such as `"$item.done/5"`) in its middle. |
| `core://Bars` | `query_key`, `bar`, `label`, `color`, `value`, `hatched`, `where`, `height_dp`, `scale_min`, `field`, `on_click`, `merge` | A stacked bar chart (feature `bars`, decision `device-versions`). Items with the same `bar` field value make one bar, in the order their first item comes; each item is a segment from the bottom up, `value` (a field, default 1) tall and coloured `color` (`$item.color`); a true `hatched` field draws it as a dashed outline. `label` (bound to the bar's first item) goes under it; the axis reaches the tallest bar, at least `scale_min`; `height_dp` defaults to 140. `where` filters items as a list's does. With `field`, a tap on a bar sets `$form.<field>` to its `bar` value and draws it picked (the last bar while unset), so a list beside it can show that bar's items; else a tap runs `on_click` with `$item` the bar's first item. With `merge` (a field, feature `bars_merge`), neighbouring segments whose items have the same value of it and the same hatching draw as one, as tall as their values together. Colour series from the theme's chart colours, `${local://theme#chart_$item.series}` (decision `chart-colours`). E.g. the `monitor` source's `monitor:device_days`, merged by `build`. |
| `core://Switch` | `checked`, `on_click`, `content_description` | An on/off switch, on while `checked` (a binding) is truthy; a tap dispatches `on_click`. |
| `core://Layout` | `layout`, `show_when`, `equals`, `presentation` (`Inline`, `Sheet`, `Panel`, `Overlay`), `animation`, `params`, `height_dp`, `edge`, `size_dp`, `panels` | Another layout of the pod (decision `layouts-not-apps`). `Inline` draws it in place: `height_dp` tall, the whole screen as a workspace's one slot, else as tall as its content (its slots only), and in a scrolling column always as tall as its content. `Sheet`, `Panel` and `Overlay` present it over the screen. An `Overlay` (feature `overlay_edge`, decision `edge-overlays-and-gestures`) comes from its `edge` (`top`, `bottom`, `start`, `end`; bottom when unset), `size_dp` tall (top, bottom) or wide (start, end), sliding in from that edge unless `animation` is `fade` or `none`; a `Sheet` is an overlay from the bottom, a `Panel` one from the start. `edge` and `size_dp` go only with `Overlay`. Every overlay closes when dragged back toward its edge past a quarter of its size or with a fling: a sheet (top or bottom) by its handle, on its inner side, or by its content once that can't scroll further that way; a panel (start or end) anywhere. With `show_when` (e.g. `$form.panel`, `${local://inbox#unread}`) it shows only while that holds (or equals `equals`); dismissing a sheet, panel or overlay clears that `$form` field. `params` bind as `$route.<name>` in it, inline too (each value bound where the embed is). A screen an inline layout opens by address (`action.navigation.open` of a `target_uri` its own `screens` route, no layout: a post, a row's details) opens from that layout, the innermost embed first, over the whole window; the screens of the layout around it (home) are only the fallback. `panels: true` (feature `layout_panels`, decision `wide-screen-panels`), inline on an `expanded` window: a layout pushed (`action.navigation.open` of a layout, no `target_uri`, no sheet, panel or overlay) from inside it opens as a panel to its right instead of over the screen, with the open's `params` as its `$route`, and one pushed from a panel opens right of that panel, closing any past it; a screen a panel's layout opens by address (`action.navigation.open` of a `target_uri` its `screens` route, no layout: a post, a New post) opens from that layout over the whole window, Back returning to the panels; the embed and its panels share the width equally with a hairline between them (a phone layout's column is instead phone wide, beside the one before it: decision `phone-width-layouts`), at most two columns showing (older ones go out of sight to the left until Back reaches them). Back (system Back, or `action.navigation.back` tapped in a panel) closes the rightmost panel; picking something that stops drawing the embed closes its panels. On a narrower window, and on clients without the feature, the push is a push. `layout` is an id or `$item.id`. |
| `core://TextField` | `field`, `placeholder`, `max_length`, `on_submit`, `lines`, `secret` | Edits `$form.<field>`; an x at its right end empties it while it has text. With `on_submit`: a send button that dispatches, then clears. Without: a search box, or a multi-line field with `lines`. `secret: true` (feature `text_field_secret`) hides what is typed (an API key, a password): one dot per character, no Copy or Cut, a password keyboard, never read out; one line, no `on_submit`. |
| `core://ChoiceChips` | `field`, `options: [{label, value}]`, `padding_h_dp`, `remember` | A chip row editing `$form.<field>`; chips past the screen's edge scroll sideways. `remember` keeps the choice on the device. |
| `core://MediaPicker` | `field`, `max_items`, `label` | Picks photos; `$form.<field>` is the list of staged attachment ids. |

Icons (`protocol::ICONS`): `favorite`, `favorite_border`, `chat_bubble`, `bookmark`, `bookmark_border`, `repeat`,
`warning`, `send`, `arrow_back`, `close`, `flame`, `timer`, `add_photo`, `add`, `refresh`, `check`, `check_box`,
`check_box_outline_blank`, `delete`, `settings`, `notifications`, `shopping_cart`, `stapler`.

An icon no name covers is given as its drawing (feature `icon_paths`, decision `icon-paths`): `"icon":
"path:<SVG path data>"` on `core://IconButton` (and `active_icon`), `core://Chip` and `core://Badge`. The data is
drawn filled in a 24 × 24 box, tinted like a named icon, and must start with a move (`M`/`m`), hold only path
commands and numbers, and be at most 2048 bytes. Write a smooth curve that repeats (`s` with more than four
numbers) as `c`: the rasterizer bends a repeated `s` out of shape. To use one drawing in several places, put it in a
`pod:` component.

A node type the client doesn't know is skipped, and its JSON kept, so old versions still parse.

## Intents

| Intent | Fields | Does |
|---|---|---|
| `action.navigation.open` | `target_uri`, `presentation` (`Push`, `Modal`, `Replace`, `Sheet`, `Panel`, `Overlay`), `layout`, `params`, `animation`, `edge`, `size_dp` | Without `layout`: opens the screen of the layout on top (else home's) whose route matches. With `layout` (feature `navigate_layout`): presents that layout on the back stack (Push: the whole screen with Back; Sheet, which the user can drag down to close; Panel; Overlay from `edge`, as for `core://Layout`), `params` as its `$route.<name>`, then its screen matching `target_uri`, if any. A screen opened while a Sheet, Panel or Overlay of the same layout is on top opens inside it: the frame's content slides aside for it, with Back and the screen's title at its top (decision `screens-inside-frames`). |
| `action.navigation.back` | | Pops the top of the back stack (a screen or a presented layout), as system Back does. |
| `action.navigation.pop_to` | `edge` | Back to the nearest entry of the back stack with that edge key (a layout id), every entry above closing with its own transition; nothing when none has it (feature `navigate_pop_to`). On a pod switched to the construct, a link back to a layout that leads to this one is written this way: elements only go forward. |
| `action.navigation.pop_to_root` | | Back to home (feature `navigate_pop_to`). |
| `action.mutation.optimistic` | `entity_uri`, `field`, `value_expr`, `endpoint`, `payload`, `also`, `rollback_value` | Changes fields at once and sends `payload` to `endpoint` through the outbox; a refusal restores each field's value from before the change. `rollback_value` (optional, `null` when left out) is what `field` gets back when it had no value before; leave it out (decision `optional-rollback-value`). |
| `action.mutation.create` | `query_key`, `entity_uri`, `entity`, `endpoint`, `payload`, `refresh_workspace`, `attachments`, `position` | Adds `entity` to the list at once, uploads attachments, sends the create, then reloads what lists the query. A refusal removes it. |
| `action.local.set` | `entity_uri`, `field`, `value_expr`, `also` | Changes fields on this device only (view state), in the engine too. Loads that started before it, or while a write was still on its way to the pod, keep it; the next load after every write is on the pod replaces it with the pod's value. |
| `action.data.load` | `path` | Loads and caches a data path. |
| `action.sequence` | `steps` | Runs intents in order as one tap, e.g. a create then `action.navigation.back`. |
| `action.link.open` | `url` | Opens an `http(s)://` URL in the browser. |
| `action.form.clear` | `fields`, `focus?` | Empties those form values (`$form.<field>`) on the device and, with `focus`, keeps typing in that field (feature `form_clear`). After an Add in an `action.sequence`, the boxes empty, ready for the next one; the create reads its `$form` values first. Nothing is sent to the pod. |
| `action.confirm` | `title`, `message`, `confirm_label`, `then` | Asks first: the app's own dialog with the layout's wording, a `confirm_label` button and Cancel; runs `then` only on confirm (in a sequence, the later steps wait too). Wrap `leave_pod`, `sign_out` and deletes in it (decision `confirm-intent`). |
| `action.auth.login` | `instance_url`, `app_path`, `token_path` | Starts an OAuth2 PKCE sign-in to an upstream. On a server other than its provider's own (`local:providers` `server`), the app shows the host and asks first (decision `sign-in-host-dialog`). |
| `action.auth.session` | `path`, `payload` | Opens a pod session (the pod code screen). |
| `action.device` | `command`, `layout`, `id`, `value` | Something the device does: `open_notification` (`id`: presents its layout and target), `mark_all_read`, `toggle_channel` (`layout`, `id`), `sign_in` (`id` of a provider; `value`: another server the user typed, `$form?.server`: a server written into the layout is ignored, and the app shows its host and asks before opening the browser), `sign_out`, `set_server` (`id` of one of the platform's own servers, `local:servers`; the app asks before switching), `load_older_notifications` (the next page into `local:notifications`, feature `device_inbox_paging`), `mark_read` (`id`: one notification), `request_notification_permission` (Android's prompt to allow notifications, or the app's notification settings once the prompt can't show again; nothing on desktop; feature `device_notification_permission`), `leave_pod` (forget the pod on this device), `refresh`. Any layout may use any command. `open_app` and `show` were retired with the dock and drawer, and `show_versions`, `pin_version`, `follow_latest`, `hide_layout`, `unhide_layout` and `delete_layout` with the old layout system (decision `one-layout-system`); `open_settings` and `open_inbox` are retired too: present those layouts with `action.navigation.open` (`settings` as a `Panel`, `inbox` as a `Sheet`). |

## Elements and names

A pod stores layouts and components as immutable elements behind names (decisions
[`content-addressed-construct`](decisions/content-addressed-construct.md),
[`one-layout-system`](decisions/one-layout-system.md)). The pod normalizes what is written:

- A **reference** is `"ref"` on a `core://Use` (a component, expanded in place), a `core://Layout` (an embed) or an
  `action.navigation.open` (a link): a name, `"ref": "@core/card"`, or sealed, `{"el": "e1-…", "name", "pin",
  "as"?}`. Names: a layout's id (`home`, `c-…`), `@me/<name>` (a component of the pod's, `pod:<name>` before),
  `@core/<name>` (a server component or a core layout), `@core/<layout>/<part>`. The authoring
  spellings `component` (on a use) and `layout: <id>` (on an embed or link) still work and are written as
  references; `core://Ref` is refused. An unpinned reference
  follows its name: when the name moves, every element that follows it is sealed again (`pin: true` keeps it).
- **Edges**: an embed or a link is keyed by its `as`, else its name (a link to a core layout keeps its id, `as:
  "settings"`); `local://nav` and `pop_to` use that key.
- **Forward only**: a write that would make a layout reach itself is refused (`cycle`). A way back is
  `action.navigation.pop_to` or Back.
- **Inputs**: inside a component's template `$in.<name>` reads an argument as `$arg.<name>` does, and a use may pass
  them as `bind` instead of `args`. Home's feeds are its `bind`; every layout may still read home's feeds directly.
- **Bindings between layouts** (feature `layout_bind`): a layout may read `$in.<name>` too (a list's `query_key`,
  a text), and each `core://Layout` or `action.navigation.open` that leads to it binds them with `bind`:
  `{"type": "core://Layout", "ref": {"name": "c-shelf", "as": "shelf-books"}, "bind": {"items":
  "collection:books"}}`. Only home binds the pod's data (query keys of home's feeds, or any value); another layout
  passes its own inputs on (`"bind": {"items": "$in.items"}`). A use site that leaves an input unbound is refused
  (`unbound_input`), as is a new input its use sites don't bind yet (bind it there first), and a value other than
  `$in.<name>` outside home (`bind_outside_home`). The same layout bound two ways needs two edge keys (`as`); a key
  draws the binding of the first use site that reaches it from home. The device fills the inputs in and loads only
  the data they bind.
- **No versions on the device**: a name's history is the agent's (`get_layout`, `restore_version`).

## Features and compatibility

Every piece of vocabulary above is a named feature, one file in `crates/protocol/features/`, and
`protocol::required_features` maps a layout to the features it uses. Clients send the features they draw in
`X-Construct-Features`, and the pod seals each client, for every name, the newest element it can fully draw (one
that uses nothing newer, and nothing removed). A component's features are those of its template as any expansion
draws it: `core://IfArg` counts as both its branches, `core://Fragment` as its children and `core://Children` as
nothing (they once counted as the removed `legacy_nodes`, so a client was sealed an older element of a component
that had one; the `construct` migration step `component_features` derives stored components' features again). See
decision [`named-vocabulary`](decisions/named-vocabulary.md).

- **Adding vocabulary** needs a client build: add the type or field to `protocol`, a feature file, an arm in
  `required_features`, drawing it in `crates/construct-render` (and `construct-sim` scripts if it adds
  interaction), and tests. Steps:
  [`crates/protocol/features/README.md`](../crates/protocol/features/README.md). Then layouts can use it; clients
  without it keep getting the previous element.
- **Removing vocabulary** never edits layout files: move the feature's file to `crates/protocol/features/removed/`,
  and only the elements that use it are retired.

## Device data

Feature `local_data` (decision `blueprints-and-home`): entities and a query the client fills itself, with no pod
request. Any layout may bind them; a layout needs no feed for them.

| Entity or query | Fields |
|---|---|
| `local://connection` | `online`, `pending` (unsent changes), `label` ("Offline · 2 pending"), `attention` (offline or changes waiting) |
| `local://inbox` | `unread`, `label` ("3 unread"), `has_older` (the inbox has more than `local:notifications` holds), `loading` (an older page is loading) |
| `local:notifications` (query) | The newest page, then the older pages `load_older_notifications` loaded: `id`, `channel_layout`, `app_title` (its layout's title), `title`, `body`, `when` ("5 min"), `day` ("Today"), `unread` |
| `local:channels` (query) | `layout`, `app_title`, `id`, `title`, `description`, `enabled` |
| `local:sign_ins` (query) | `id`, `title`, `detail` ("@dana · pixelfed.example") |
| `local:providers` (query) | `id`, `title`, `server` (the provider's own server), `custom_server` (another server may be typed), `server_placeholder`: sign-ins the pod offers that aren't signed in |
| `local:servers` (query) | `id`, `title`, `url`, `selected`: the servers this device can use |
| `local://device` | `width_dp`, `height_dp` and `size_class` (feature `device_size`, decision `screen-sizes`): the window's size in points and its class by width, `compact` (under 600), `medium` (600–839) or `expanded` (840 and up), following resizes; show things by size with `core://Conditional` on `${local://device#size_class}` and `equals`. `platform`; `notification_permission`: meant to be `granted` or `denied` on Android (whether notifications can show); `""` today on every client (the Android app doesn't report it yet, decision `rust-clients`), and where it doesn't apply (desktop, simulator) |
| `local://nav` | Where the layout is drawn (feature `nav_data`): `can_go_back`, `presentation` (`root`, `push`, `sheet`, `panel`, `screen`, `inline`), `pushed`, `inline`, `in_frame` (in a sheet, panel or overlay; an overlay from the top or bottom says `sheet`, from the start or end `panel`) |
| `local://screen` | In a page screen (feature `nav_data`): `loading`, `error` ("" when none) of its `data` load |
| `local://theme` | The pod's theme colours for the app's mode (feature `theme`): `page` (a page's background), `card` (cards and rows on it), `card_border`, `muted_text`, `accent`, `accent_surface`, `muted_surface`, `success`, `success_surface`, `error`, `error_surface`, `highlight`, `like`, `unread`, and a chart's series `chart_1` to `chart_8` and `chart_other` (decision `chart-colours`: bind `${local://theme#chart_$item.series}`), each with a light and a dark value; set with MCP `set_theme`, plus up to 40 tokens a pod names itself (decision `custom-theme-tokens`): an area of a pod (meals, workouts) is best a pair, `<area>` for its icons, tags and progress and `<area>_soft` for their tinted backgrounds, used on the area's screens and its tile on home alike, while `accent` stays the buttons' colour. Once a pod sets `accent`, switches and `core://Button` draw in it in light mode too, and an optional `on_accent` of its own colours what goes on them (decision `controls-follow-the-theme`). Bind these instead of fixed `#RRGGBB` colours, which stay the same in dark mode (decision `dark-palette`; layout writes answer `warnings` for each). |
| `local://<anything else>` | The layout's own values on this device: `action.local.set` writes them (only these: the entities above, `local://servers`, `session`, `device`, `providers`, `sign_ins`, `connection`, `notifications`, `inbox`, `channels`, `theme`, `nav` and `screen`, are the device's own, which layouts read but never set; the pod refuses such a write and clients ignore it, decision `device-owned-local-state`), any binding reads them (`${local://shell#tab}`). They last while the app runs; with `"keep": true` on the set (feature `local_keep`) the device keeps the fields across starts and puts them back before its first frame, per pod, at most 200 values of 1 KB, never sent to the pod. For what belongs to a device, not to the pod's data: the tab that is open, a filter. |
| `local://session` | `open`, `server`, `error`, `message`: the pod sign-in; `status`: an upstream sign-in in progress or failed |

A `core://Conditional` `equals` may bind the item (`"equals": "$item.id"`). A `core://Conditional` value may be such a binding: `{"value": "${local://inbox#unread}", "equals": "0", ...}`. With `"not": true` the same test shows its content only while something is unread.

### Dates and times

Dates are compared and drawn by the device, from its own clock and time zone, so a screen stays right on the next
day and needs no job on the pod to flip a flag (decisions `text-dates-and-counts`, `date-conditions`):

- `core://Text` `format` draws bound `YYYY-MM-DD` values as "tomorrow" or "Nov 5" (see the table above).
- `core://Conditional` `date` (feature `conditional_date`) shows its content while the bound moment has come
  (`Past`), falls on today (`Today`), or is still ahead (`Future`). The value is a `YYYY-MM-DD` date, a local
  `YYYY-MM-DDTHH:MM[:SS]`, or an instant with an offset (`2026-11-16T16:00:00Z`, as a `datetime` field holds).
  `time` binds a wall-clock time to a bare date (`15:45`, `9:00`, `9:00 AM`, `3:45pm`); a value that isn't a time
  ("Bonus", empty) leaves the bare date. A bare date is `Past` once its day is over and `Future` until it begins,
  so on the day itself only `Today` holds; with a time, `Past` holds from that minute on and `Future` until it.
  A value that isn't a date shows nothing. `equals` may go with it (both must hold); `time` needs `date`.
- The device draws the screen again by itself at the minute an answer changes (the bound time, or midnight), also
  when nobody touches it.

```json
{"type": "core://Conditional", "value": "$item.date", "time": "$item.ends", "date": "Past",
 "content": {"type": "core://Text", "style": "Label", "value_expr": "Tap to check off"}}
```

A pod's MCP previews read the clock in the pod's time zone (`set_time_zone`); the simulator's `--now` and `clock`
pin it.

## Porcelain: components

A component is a node tree with parameters, defined as data and expanded by the pod before serving, so the app never
sees one and changing one needs no app build. Use one anywhere a node goes:

```json
{ "type": "core://Use", "ref": "@core/card", "args": {"spacing": 6}, "children": [
  { "type": "core://Text", "style": "Label", "value_expr": "$item.title" } ] }
```

- **`@core/` components** ship with the server: `crates/pod-core/components/<name>/NNNN.json`, `{"note", "component"}`,
  append-only like layout files. A source ships its own the same way, in `crates/source-*/components/<name>/` (its
  build script embeds them; `Source::component_files`); names are unique across crates (decision
  `adapter-components`). Every core layout is one too: `@core/<layout>` is its whole layout (only as a
  layout's root; the layout keeps its own id) and `@core/<layout>/<part>` a part (`screen:<id>` in `screens`,
  `slot:<id>`, or a name from the file's `"parts"`).
- **`@me/` components** are the pod's own, defined by an agent (MCP `define_component`, `copy_component`), versioned
  and deletable while no layout uses them.
- **A definition** is `{"doc", "params": [{"name", "type": "text|number|bool|uri|binding|json", "default"?,
  "required"?, "doc"}], "children"?: true, "template", "examples": [{"args", "children"?, "note"}]}`. A `json`
  parameter takes any JSON value but null, for the parts of a node that are objects or lists: an intent (`on_click`),
  a `count` query, `where` conditions (decision `component-json-params`; the app must list feature
  `component_json_params` to expand a component that has one). In the template,
  `$arg.<name>` is an argument (a string that is only `"$arg.n"` keeps the argument's type; it may name a key too),
  `{"type": "core://Children"}` is where the caller's children go (`"at": i` for one), `{"type": "core://IfArg",
  "arg", "equals"?, "then", "else"?}` keeps a branch (`equals` may be another argument), and `{"type":
  "core://Fragment", "children"}` is several siblings. Three more need feature `component_templates` (decision
  `element-roles-and-reuse`; the pod records it on an element that uses one, and a client without it is sealed
  the newest entry of the name it can read):
  - `{"type": "core://Each", "arg": "<a json list argument>", "as": "<name>", "template": <node>}` repeats the
    template once per entry (at most 200). Inside it `$arg.<name>` is the entry when it is one value,
    `$arg.<name>_<key>` a key of an entry that is an object, and `$arg.<name>_index` its place from 0.
  - `{"type": "core://Match", "arg" | "value", "cases": {"<key>": <node>}, "else"?: <node>}` keeps one piece by a
    key. With `arg`, the case the argument names is chosen when the component is expanded. With `value` (a
    binding), every case stays under a `core://Conditional` `equals`, and `else` under a `not` for each key.
  - **Named slots**: `{"type": "core://Children", "slot": "<name>"}` takes the nodes a use passes as `"slots":
    {"<name>": [nodes]}` (the name may come from an argument); `{"type": "core://IfArg", "slot": "<name>",
    "then"}` keeps a piece while the use filled that slot. `children` stays the unnamed slot. An example may pass
    `slots` too.
  References never ride inside a `json` argument: a component that takes layouts takes them as `core://Layout`
  nodes in a slot (`@core/tab_shell`), so every reference is still at one of the three sites the pod follows. Every example must expand to plumbing a pod would save and serve: within the limits layouts are held
  to (`corner_dp` at most 512, known icons, …). A server component that doesn't stops the server from starting
  and names the file and example.
- **Following and pinning.** Without `version` a use follows the component: the layout gets a new version when the
  component changes. With `version` it stays.
- **Seeing the result.** MCP `expand` shows what a layout, a component use or any node becomes, and the features it
  needs; `get_component` shows each example expanded.

The server components:

| Component | Parameters | Draws |
|---|---|---|
| `@core/workspace` | `title`, `title_scrolls`, `fills_screen`; children: the content | A page as a workspace looks: the headline title (Back when pushed), the content scrolling under it, a little space. |
| `@core/screen` | `title`, `data`, `has_footer`; children: content, footer | A screen as the app drew it: Back, title and spinner, the load's error with Try again, scrolling content, footer. Its `data` is only for Try again: the screen's own `data` (the same path) is what loads when it opens, and a screen that gives `@core/screen` a `data` without loading it is refused. |
| `@core/back_button` | | Back, shown while `local://nav#can_go_back`. |
| `@core/header_bar` | `title`, `style`, paddings; children: actions | A title on the left and icon buttons on the right, on one line. |
| `@core/page_header` | `title`, `style`, paddings; children: actions | A `core://Page`'s header on one line: Back (while it can go back), the title, and icon buttons on the right. |
| `@core/scroll_page` | `title`, `spacing`, paddings; children: the content | A whole page with its own header: on the page colour, `@core/page_header` and a divider, then its children in a column that scrolls; `spacing` and the paddings are left out while 0. The root of a `core://Page` or of one of its screens. |
| `@core/section_header` | `text`, `style`, `padding_v`, `divider` | A section's padded heading, optionally under a divider. |
| `@core/list_row` | `title`, `trailing`, `padding_v` | A row with a title and a quiet trailing text. |
| `@core/divided_item` | `inset`; children | A list item set off from the next: its children in a column with a divider under them, edge to edge or inset by `inset`. |
| `@core/footer_bar` | paddings; children | A bar along the bottom of a page: a divider, then its children on one centred row (a count, a Spacer, a button). |
| `@core/card` | `spacing`, `border`, `background`, `corner`; children | A bordered, rounded card of its children in a column; `corner` defaults to 14. |
| `@core/filled_card` | `background`, `color`, `spacing`, paddings; children | A filled, rounded card of its children in a column, its text in one colour (an item's heading, a status at a glance). |
| `@core/chevron_card` | `background`, `color`, `border`; children | A rounded card that opens something: its children on one centred row and a › at the end; `color` and `border` are left out while empty. Wrap it in `core://Tappable`. |
| `@core/pill` | `text`, `background`, `color`, `style`, `corner` | A small tinted tag (a date, a status, a count); `corner` defaults to 8. |
| `@core/dot` | `color`, `size`, `corner` | A small coloured dot. |
| `@core/section_label` | `text` | A quiet section heading above a grouped card: Label text in `muted_text`, inset 4 dp. |
| `@core/muted_note` | `text`, `style`, `count` | A quiet line of muted text inset 4 dp (Secondary by default): what an empty section says, or a hint under a heading. Given `count` (a list's `{query_key, where?}`, a `json` argument), it is the whole empty state: shown only while that list is empty. |
| `@core/muted_text` | `text`, `style`, `max_lines`, `when` | A text in `muted_text` with no padding of its own (Secondary by default, `max_lines` left out while 0): a row's details or trailing date, a caption beside a heading, a sentence in a card. Given `when` (a binding such as `$item.notes`), it shows only while that value is set: an optional field's line in one use, with no `core://Conditional` around it. |
| `@core/tinted_text` | `text`, `color`, `style`, `weight`, `max_lines` | A text in one colour (a theme token) with no padding of its own (Secondary by default, `weight` and `max_lines` left out while empty): an event's time in `accent`, an error line, a heading in a section's hue. For `muted_text` use `@core/muted_text`. |
| `@core/text_link` | `label`, `description`, `on_click` | A quiet text link: accent Label text padded 4 dp across and 8 dp down, running `on_click` (a `json` argument: any intent). A header's action in words, or Clear beside a list. |
| `@core/grouped_card` | `corner`; children | Rows grouped in one rounded card (`card` colour, `card_border` edge, `corner` defaults to 18); the rows bring their own dividers. |
| `@core/tinted_icon` | `icon`, `tint`, `tint_bg`, `size` | An icon centred in a tinted circle (`accent` on `accent_surface`, 34 dp by default). Until the app has issue #325's renderer fix, an icon-only Badge, and so this icon, sits 2 dp left of centre. |
| `@core/date_block` | `weekday`, `day` | A compact date: the short weekday (Caption, muted) over the day of the month (Title), centred in 36 dp. |
| `@core/icon_row` | `icon`, `tint`, `tint_bg`, `title`, `meta`, `trailing`, `chevron` | A list row: `@core/tinted_icon`, a bold title with a muted line under it while `meta` is set, optional quiet `trailing` text and a ›. |
| `@core/eyebrow` | `text`, `color` | A small label in capitals over a heading (what a sheet is about, a lesson's course, a card's kind): Caption text, SemiBold, with `caps`, in `accent` unless `color` is given. |
| `@core/title_meta` | `title`, `meta` | A bold title with one muted line under it, always shown (a name and its details, a heading and a sentence): Label text over Secondary text in `muted_text`, 2 dp apart. |
| `@core/two_pane` | `list_width` (360); children: the list, the detail | On an `expanded` window the list at `list_width` beside the detail, which fills the rest; on a narrower one the list alone. Each pane brings its own `core://Scroll`. A row usually selects on a wide window (`action.local.set` on `local://<key>#selected`, with a JSON literal such as `"\"recipes\""`; the detail shows the matching `core://Layout` with `show_when` and `equals`) and pushes on a narrow one: its two intents in a `core://Conditional` on `${local://device#size_class}` equals `expanded` and one with `not: true` too. |
| `@core/collapsible` | `key`, `title`, `show_label`, `hide_label`; children | A section that starts closed: a header (the title, Show or Hide) that opens and closes its children, kept open while the app runs (`local://collapsible/<key>#open`: `"true"` open, `""` or `"false"` closed). |
| `@core/warning_chip` | `text` | A warning chip. |
| `@core/button` | `label`, `on_click`, `style`, `size` | A `core://Button` with its `style` and `size` (`on_click` a `json` argument: any intent). |
| `@core/link_button` | `label`, `url`, `style`, `size` | A button opening a link, shown while the link is set, in any button style and size. |
| `@core/labeled_text` | `label`, `text`, `padding_h` | A small label above a text (an item's About or Description on its screen), shown while the text is set. |
| `@core/author_header` | `avatar`, `name`, `handle`, `size`, paddings | A round avatar beside the name and handle. |
| `@core/toggle_action` | `field`, `icon`, `active_icon`, `label`, `active_label`, `endpoint`, `payload_field`, `tint`, `count_field`, `tag` | An icon button that flips a boolean field at once and tells an endpoint, optionally moving a count with it. |
| `@core/stat` | `value`, `label` | A number with a label under it. |
| `@core/pixelfed_sign_in` | `server`, `instance_url`, `other_servers`, `placeholder` | Pixelfed's signed-out screen: a sentence, "Sign in with Pixelfed", and a box for another server (shipped by `source-pixelfed`). |
| `@core/pixelfed_post` | `focus` | A Pixelfed post and its comments on a post screen (route `$route.id`), optionally scrolled to and highlighting one comment; the content of the core layout's `post`, `comments` and `comment_thread` screens (shipped by `source-pixelfed`). |
| `@core/pixelfed_comment_box` | | The box that comments on the screen's post; those screens' footer (shipped by `source-pixelfed`). |
| `@core/message_page` | `title`, `body`, `background`; children: actions | A whole page with a message in the middle and actions under it. |
| `@core/screen_scaffold` | `title`, `spacing`, `padding_h`, `data`, `follow_end`; children: the content; slots `actions`, `footer` | The usual page or screen: a pinned header (Back, the title, a spinner while its data loads, the `actions` on the right), the children in a padded column that scrolls, and the `footer` pinned under it; with `data`, a failed load says so with Try again. |
| `@core/tab_shell` | `tabs` (`[{key, title, icon}]`), `first`, `state`, `rail_width`; a slot per tab key; slots `rail_top`, `rail_bottom` | A whole app's shell: one list of tabs is a dock along the bottom of a phone and a rail down the side of an `expanded` window, with the open tab's slot (usually one `core://Layout`) under or beside it. The open tab is kept on the device (`local://shell#tab`, feature `local_keep`); before any choice the `first` tab shows. |
| `@core/grouped_list` | `corner`; children | Rows in one rounded card with hairlines only between them: a `core://List` or `core://Column` with `spacing_dp` 1 whose rows draw on the card colour (`@core/item_row` does). |
| `@core/item_row` | `title`, `meta`, `meta_lines`, `title_color`, `leading`, `trailing`, `chevron`, `background`; children: the leading child, then the trailing one | A row with room at both ends: a leading child (an icon, a checkbox, a picture), a title with a muted line under it, then with `trailing` its second child and a › with `chevron`; with `leading` false the first child trails. On the card colour, for `@core/grouped_list`. |
| `@core/titled_section` | `text`, `space_above`; children | A section: space above, a `@core/section_label` heading, then its children. |
| `@core/callout` | `title`, `text`, `tone`, `background`, `color`, `corner`; children | A tinted panel that says one thing in a tone: a heading in the tone's colour, a text, then its children. |
| `@core/empty_state` | `title`, `text`, `icon`, `tone`, `corner`; children | What a list or screen shows with nothing in it yet: a tinted icon, a title, one sentence on why or when it fills, then its children (one action), centred in a card. |
| `@core/counted_list` | `count`, `empty`, `corner`; children: the list | A list that is its rows in a `@core/grouped_list` while `count` (the list's `{query_key, where?}`) has rows, and the `empty` line while it has none. |
| `@core/checkable_row` | `title`, `field`, `endpoint`, `meta`, `description`, `tint`; children: at the end | A row that ticks a yes/no field of `$item` at a tap (`{id, <field>}` to the endpoint): a ring, or a filled tick with the title struck through. |
| `@core/choice_chip` | `label`, `value`, `chosen`, `on_click`, `tint`, `on_tint`, `description` | One choice among several: filled while `chosen` (a binding) equals `value`, outlined otherwise; `on_click` chooses it (a mutation of a row's field, or `action.local.set` for a draft). |

`@core/pill` and `@core/tinted_icon` also take `tone` (`accent`, `success`, `error`, `muted`: the theme's pair for
it) in place of two colours, `@core/card` its paddings (`padding_h`, `padding_v`), and `@core/page_header` a `weight`,
a least `height` and a `loading` spinner.

What each is for, in a line, is in `components/catalog.json` beside the files (and each source's `catalog.json`
for what it ships): MCP `find_elements` answers from it, by a query, a role, a scope or a category (`primitive`,
`row`, `container`, `scaffold`, `pattern`, `layout`). A test fails when something shipped has no entry.

Their definitions are the files; `get_component` reads them with examples. See decision
[`plumbing-and-porcelain`](decisions/plumbing-and-porcelain.md), and [`layout-parts`](decisions/layout-parts.md) for
how references to core layouts follow them.
