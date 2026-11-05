`core://Layout {layout, show_when?, equals?, presentation?, animation?, params?, height_dp?}`: another layout of the
pod drawn in place (`Inline`), or presented over the screen as a `Sheet` or `Panel` while `show_when` holds (decision
`layouts-not-apps`).

Fields: layout, show_when?, equals?, presentation?: Inline|Sheet|Panel, animation?: none|fade|slide|sheet_up, params?: {name: value}, height_dp?.

Another layout drawn here: Inline (the default) in place (height_dp tall, or as tall as its content; as a workspace's one slot, the whole screen); Sheet or Panel over the screen. show_when (e.g. $form.panel, ${local://inbox#unread}) shows it only while truthy (or equal to equals); dismissing a Sheet or Panel clears that $form field. params bind as $route.<name> in it. layout is an id or a binding ($item.id).
