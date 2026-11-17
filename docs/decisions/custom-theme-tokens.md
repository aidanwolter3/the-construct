# A pod may name theme colours of its own, each with a light and a dark value

Date: 2026-11-17. Related: [dark-palette](dark-palette.md), [plumbing-and-porcelain](plumbing-and-porcelain.md).

The theme had a fixed set of tokens (`page`, `card`, `accent`, …). A pod that styles its own layouts needs more
named colours than those: a colour per category or priority, the fill of a toolbar, a tag's tint. Without a token it
writes a fixed `#RRGGBB`, which stays the same in dark mode (layout writes warn about each one), or it reuses a
built-in token that means something else.

The apps already take any token from the `core://theme` entity (`<token>` light, `<token>_dark`), so a token newer
than an app build still binds. Only the pod refused names it didn't know.

Decision: `set_theme` also takes tokens of the pod's own. A name that isn't built in, matches
`[a-z][a-z0-9_]{0,31}` and doesn't end in `_dark` adds one; the pod keeps at most 40. A token of its own needs at
least one colour: one mode set stands in for the other, so the bundle always carries both `<token>` and
`<token>_dark` and the dark mode never falls back to nothing. `{token: null}` removes one of the pod's own, or puts
a built-in back to its defaults. `set_theme` without arguments lists both kinds; its `tokens` answer marks the
pod's own.

- No client build: the renderer and engine already pass any token through.
- Nothing checks `${local://theme#<token>}` names in layouts; an unknown token draws nothing, as before.
- Built-in tokens keep their defaults and meanings; the renderer's own controls (switches, buttons) still read only
  built-in ones.
