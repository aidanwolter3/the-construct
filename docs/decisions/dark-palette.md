# Dark mode is a palette of theme tokens, and layouts use tokens instead of fixed colours

Date: 2026-11-07. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md),
[rust-render-engine](rust-render-engine.md), [rust-simulator](rust-simulator.md), issue #189.

Since the Rust clients follow the system's dark mode (#173), dark mode looked broken: the Compose app always drew
light, so nothing had been designed or checked in dark. The worst screens were a pod's own layouts (home, a
notifications list), and the blueprints did the same thing in smaller ways: each drew fixed light colours
(`#FFFFFF` cards, `#F8F9FA` pages, `#FFF3CF` highlights). In dark mode the engine's light text landed on white
boxes. The theme had no token for a card or for good-news and error tones, so a layout had nothing to use instead.
The engine's dialogs drew on `page`, and its dark roles were Material's baseline (`#141318`, `#D0BCFF`), not the
app's palette.

Considered: making the Rust clients draw light only, as the Compose app does. That hides the problem and gives up
a feature the clients already have.

Decision: dark mode is the theme's dark palette, and every colour in a layout is a theme token.

- **Tokens.** `card` (`#FFFFFF` / `#242428`: cards, rows and boxes on a page, and the engine's dialogs, sheets and
  menus), `muted_text` (`#6B6B76` / `#A3A3AD`), `success` (`#137333` / `#81C995`), `success_surface`
  (`#E6F4EA` / `#1E3325`), `error_surface` (`#FCE8E6` / `#3D2321`) and `highlight` (`#FFF3CF` / `#3B3320`) join
  `page`, `card_border`, `accent`, `accent_surface`, `muted_surface`, `like`, `unread` and `error`, whose light and
  dark values stay as they were. One table in `pod_core::porcelain::THEME_TOKENS`, mirrored in
  `construct-render`'s `THEME_DEFAULTS` and the shared UI's.
- **The engine's roles come from the palette in dark mode**: the background is `page`, primary is `accent` (with
  `accent_surface` on it). In both modes dialogs, sheets, panels and the edit bar draw on `card`, muted text is
  `muted_text` and the warning surface is `highlight`. Otherwise light mode keeps Material's roles, as the Compose
  app draws.
- **Each mode takes only its own values.** `core://theme` and `local://theme` give `<token>` for light and
  `<token>_dark` for dark; a theme set for light only keeps the dark defaults.
- **Clients take any token the pod sends**, not only the ones they know, so a later token needs only the pod. A
  build older than this one resolves an unknown token to nothing: the box draws no fill, the text keeps its
  colour. That is why the new tokens need no feature: a feature would not help anyway, because a layout's older
  versions expand with the newest components too, so an old client could not step back to a version without them.
- **`set_theme` stores only what the pod set**, so changing a default reaches every pod that didn't set that token.
  It also takes the flat form the bundle carries (`{"card": "#…", "card_dark": "#…"}`), and its answer describes
  every token.
- **Layouts use tokens.** New versions of the blueprints (`events`, `recipes`, `inbox`, `pixelfed`, re-recorded
  `shell` and `agents`) and of `core:card` (now filled with `card`), `core:pill`, `core:message_page`,
  `core:screen` and `pixelfed_post` bind tokens instead of hex. `core://List` focus colours and
  `core://IconButton` `active_tint` take a binding too, as `core://Box` colours already did.
- **Agents are told.** `create_layout`, `update_layout` and `validate_layout` answer `warnings` for every fixed
  `#RRGGBB` colour (naming the token when one has that light colour); they are advice, never a refusal.
- The simulator's `mode dark` / `mode light` command draws either mode, so a PR can show both.

Not done: the Like heart's tint stays `#ED4956` in the blueprints (older builds can't bind `active_tint`, and the
red reads in both modes); the Agents and server-monitoring adapters still put fixed status colours in their data,
drawn as white text on a solid colour, which reads in both modes. The developer's phone is the final check.
