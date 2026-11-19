# One button node: styles and sizes in the renderer, not in layouts

Date: 2026-11-19. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md),
[named-vocabulary](named-vocabulary.md), [controls-follow-the-theme](controls-follow-the-theme.md). Issue #407.

Buttons in the same row came out at different heights and corners. On one pod's Tasks run screen, "Open in Claude"
was 142×40 dp and "Stop" beside it 60×36 dp. The plumbing had one button, `core://Button {label, on_click}`: filled
with the accent, nothing else. Any layout that wanted a second kind (outlined, tinted, small) built its own face from
a `core://Tappable` over a `core://Box`, and each agent chose its own padding, height and corner. A shared
component wouldn't have fixed this, since each pod's copy would still pick its own numbers.

Decision: `core://Button` gets `style` (`Primary` by default, `Secondary`, `Tonal`, `Text`) and `size` (`Regular` by
default, `Small`), feature `button_style`. The renderer owns every face. Each size has one height, padding and
corner, whatever the style: `Regular` is 40 dp tall with 24/10 dp padding (as before), `Small` 32 dp with 16/6. The corner is
half the height. Layouts pick a style and never a dimension:

- `Primary`: filled with the accent (`on_accent` text): the screen's main action.
- `Secondary`: outlined in `card_border` on `card`, in the text colour: an action beside the main one or a quiet
  destructive one (Stop, Cancel, Sign out).
- `Tonal`: filled with `accent_surface`, in the accent.
- `Text`: accent text with no fill.

`on_click` already takes any intent (`action.sequence`, `action.confirm`, `action.mutation.optimistic` with
`also`), so a hand-made face is never needed for its tap. The porcelain is `@core/button` (all four arguments) and
`@core/link_button`, which now takes `style` and `size` too.

Hand-made faces are advice, not errors. A layout or component write (`create_layout`, `update_layout`,
`validate_layout`, `define_component`, `update_component`) answers `warnings` for each `core://Tappable` whose content
is a filled or outlined, rounded `core://Box` straight around a `core://Text` (`validate::button_faces`). It points at
`core://Button`. Cards and tiles (a column inside, or a fixed width) are not flagged. Refusing these writes would
break pods that already have them.

The feature doesn't gate a layout. A client without it ignores both fields and draws a regular primary button, so
the button still works. The blueprints' buttons were already `core://Button`. New versions make the actions next to
the main one `Secondary`: Agents (Stop, Pause, Edit, Repair, Sign out of the runner, Delete schedule), Settings (Sign
out, Unplug, Leave pod on this device), Pixelfed (Discard beside Retry), and the sign-in screen's "Try a temporary
pod".

Not done here: pods' own copies and `@me/` components don't follow blueprints. Their hand-made faces stay until
someone edits them; the write warnings point them out.
