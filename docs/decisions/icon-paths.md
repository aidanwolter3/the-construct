# An icon can be its own drawing: path data instead of a name

Date: 2026-11-13. Related: [named-vocabulary](named-vocabulary.md), [plumbing-and-porcelain](plumbing-and-porcelain.md),
D44, D51.

Icons are names from `protocol::ICONS`, each drawn from SVG path data compiled into the renderer. So every new icon
was new vocabulary: `flame` and `timer` (D44), `add_photo`, `add` and `refresh` (D51), and the named features
`todo_icons`, `icons_shell`, `icons_cart` and `icons_stapler`. Each needed a protocol change, a feature, a client
build and a renderer release before one layout could show it. Most were for one app's buttons (a cart for
Groceries, a stapler for Staples). The renderer already draws icons from path data, so the name only looked the
drawing up.

Decision: wherever a layout gives an icon (`core://IconButton` `icon` and `active_icon`, `core://Chip` and
`core://Badge` `icon`), it may give the drawing itself: `path:` and then SVG path data in a 24 × 24 box, drawn
filled and tinted like a named icon.

```json
{"type": "core://IconButton", "icon": "path:M19,13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z", "content_description": "Add", "on_click": {...}}
```

- Additive: names keep working and mean what they did. A layout that uses a drawing needs feature `icon_paths`,
  so older clients keep getting the newest version without one.
- The pod checks the data (`protocol::icon_path`): it starts with a move, holds only path commands and numbers,
  and is at most 2048 bytes. The renderer draws only data that passes the same check and caps a mask's size as for
  named icons. The GPU gives each drawing a glyph id after the named icons' and below the images' (232 free); one
  past the last free id is not drawn.
- Sharing one drawing between layouts is porcelain: a `pod:` or `core:` component carries it, so a new icon needs
  no client build.
- Considered: the pod serving a named icon set as data, with clients fetching names they don't know (rejected for
  now: a new store and a fetch for what a string in the layout already says); SVG documents (rejected: a parser
  and a much larger surface than one filled path).

Not yet: the named icons stay. Retiring the per-app ones (`icons_cart`, `icons_stapler`) for drawings in
components would be a breaking change under `named-vocabulary`, for a later decision.

Needs a client build (the renderer reads `path:` icons). No layout file changes.
