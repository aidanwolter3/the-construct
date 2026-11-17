# Wide screens: panels beside an embed, inline params, and hover

Date: 2026-11-17. Related: [screen-sizes](screen-sizes.md), [layouts-not-apps](layouts-not-apps.md),
[web-client](web-client.md), [plumbing-and-porcelain](plumbing-and-porcelain.md).

On a laptop, a home laid out with a sidebar and a detail (decision `screen-sizes`: a list beside an inline
`core://Layout` picked with `action.local.set`) breaks down one level deeper. A link inside the detail ("All
meals") pushes over the whole screen, so the sidebar and the detail both go. Two more gaps showed on the same
screen. An inline layout ignored its `params`, so a detail meant to show House or Yard drew `$route.area` instead.
And nothing reacted to the mouse, so a desktop window felt like a picture of a phone.

Considered for going deeper:

- **Each layout rewires its links for wide screens** (a `core://Conditional` on `size_class` around every link,
  setting local state that a third pane reads). Rejected: it means editing every layout a detail can show and
  every link in them, and it reaches one level only.
- **The app opens every push beside home on wide windows.** Rejected again for the reasons in `screen-sizes`: it
  changes how every existing layout draws.
- **The embed asks for it.** Taken: `core://Layout` gains `panels: true`.

Decision:

- **`panels: true`** (feature `layout_panels`) on an inline `core://Layout`, on an `expanded` window, makes the
  embed a panel host. A push from inside it opens the pushed layout as a panel to its right: an
  `action.navigation.open` of a layout with no `target_uri`, and not as a sheet, panel or overlay. The panel gets
  the open's `params` as its `$route`. A push from panel k opens beside panel k and closes any panels past it. The
  embed and its panels share the width equally with a hairline between each two. At most two columns show: deeper
  ones push the oldest out of sight to the left, and Back brings it back. Back (system Back, or a back button in a
  panel) closes the rightmost panel. A host that stops being drawn closes its panels: another view is picked, or
  the window narrows.
- The engine keeps the panels on the stack entry that holds the host. The host is identified by the semantic
  path of the embed's content (`<path>/layout:<id>`), and panel k by `<host>~k`. Each column is drawn as a
  semantic node with that id, so a tap inside it, on a tagged row too, finds its column through its ancestors. Panel
  layouts are loaded like any pushed layout, and their data stays live like an inline layout's.
- **It gates nothing.** A client without the feature, or a narrower window, pushes as before. A layout can set it
  for everyone; phones and older builds are unchanged.
- **Inline `params`** bind as `$route.<name>` in the embedded layout, as they always did for an opened one. Each
  value is bound against the embedding layout. This is a fix, not a feature: the field was documented and
  validated, but inline embeds dropped it.
- **Hover** is renderer behaviour, not vocabulary. With a mouse and no button down, on desktop and the web, the
  `core://Tappable` under the pointer gets half a press's wash and the pointer becomes a hand
  (`Session::hover`, `hover_end`). Touch never calls them, so phones draw exactly as before.

Selection that stays highlighted while its view shows needs no new vocabulary. A row tints itself with
`core://Conditional` on the same `local://` field that picks the view (a user's own component wraps that).
