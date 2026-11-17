# The renderer's own controls follow the theme tokens a pod sets, and keep Material's colours otherwise

Date: 2026-11-17. Related: [dark-palette](dark-palette.md), [custom-theme-tokens](custom-theme-tokens.md),
issues #325, #326.

In light mode the renderer drew its own controls in fixed Material colours: a switch that is on and every
`core://Button` in `#6750A4` with white on it, a switch that is off in `#E6E0E9` with a `#79747E` outline. Dark
mode already took `accent` and `accent_surface` (decision `dark-palette`). So a pod that set its accent (an
evergreen, say) saw everything follow it in light mode except its switches and buttons, which stayed purple.

Considered:

- **Light `primary` is always the `accent` token.** The default `accent` is `#4A2C82` (it is text on accent
  surfaces), darker than `#6750A4`, so every pod without a theme would change: the issue asks that those look the
  same as before.
- **Change the default `accent` to `#6750A4`.** Same problem the other way round: every layout that binds
  `accent` for text would change.
- **A new built-in `primary` token** with Material's default. A pod that set `accent` would still get purple
  controls until it also set `primary`, which is the bug again.

Decision: a control colour follows a built-in token once the pod (or the device's `local://theme`) gives it a
colour other than that mode's default; while the token is its default, the control keeps today's Material colour.

- **Light mode.** A switch that is on and `core://Button` (`Theme::primary`) draw `accent` once it is set; the
  pod's bundle always carries every built-in token with its defaults filled in, so "set" means "not the built-in
  default for this mode" (compared ignoring case). A pod that sets `accent` to exactly its default keeps the
  Material purple, as a pod without a theme does.
- **The off switch, both modes.** Its track follows `muted_surface` and its outline and thumb `muted_text` once
  set, so a tinted theme doesn't keep a purple-grey switch.
- **Dark mode** keeps `accent` for `primary` as before.
- **`on_accent`, optional.** What goes on an accent fill (a button's label, an on switch's thumb) is the pod's
  own token `on_accent` when it has one (decision `custom-theme-tokens`; `set_theme {"on_accent": {"light": …,
  "dark": …}}`), else white in light mode and `accent_surface` in dark, as before. It isn't built in: a pod needs
  it only when its accent is light. Set both modes: one mode set stands in for the other, so a light-only
  `on_accent` also replaces dark's `accent_surface`.
- `pod_core`'s token table is unchanged, and so is every pod's bundle: the change is the renderer's alone, so it
  needs a new desktop and Android build, and older builds keep drawing the purple.

Also in this change (#325): a `core://Badge` with empty `text` draws only its icon, with no 4 dp gap and no empty
text beside it, so an icon-only badge centres where it is put; screen readers skip it.

Everything else drawn in `primary` follows too: a focused text field's border, its Send button and the text
cursor and selection. `core://ChoiceChips` already read `accent` and is unchanged. Not done: the warning bar's text
colour stays fixed.
