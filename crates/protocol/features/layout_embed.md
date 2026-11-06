`core://Layout {layout, show_when?, equals?, presentation?, animation?, params?, height_dp?, edge?, size_dp?}`:
another layout of the pod drawn in place (`Inline`), or presented over the screen as a `Sheet`, `Panel` or `Overlay`
while `show_when` holds (decision `layouts-not-apps`).

Fields: layout, show_when?, equals?, presentation?: Inline|Sheet|Panel|Overlay, animation?: none|fade|slide|sheet_up, params?: {name: value}, height_dp?, edge?: top|bottom|start|end, size_dp?.

Another layout drawn here: Inline (the default) in place (height_dp tall, or as tall as its content; as a workspace's one slot, the whole screen); Sheet (from the bottom), Panel (from the start) or Overlay (from edge, bottom when unset; size_dp tall for top and bottom, wide for start and end) over the screen. An overlay slides in from its edge (or fades, or appears with animation fade or none), and dragging it back toward its edge dismisses it. show_when (e.g. $form.panel, ${local://inbox#unread}) shows it only while truthy (or equal to equals); dismissing a Sheet, Panel or Overlay clears that $form field. params bind as $route.<name> in it. layout is an id or a binding ($item.id).
