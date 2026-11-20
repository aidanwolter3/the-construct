`$theme.<name>` in a component's template (decision `element-roles-and-reuse`): a number or word of the
pod's theme scale, put in when the component is expanded: `corner_s`, `corner_m`, `corner_l`, `pad_s`,
`pad_m`, `pad_l` (dp) and `title_weight` (Regular, Medium, SemiBold or Bold). The pod sets them with MCP
`set_theme {scale}` and sends them in the construct's manifest; unset ones have the defaults every build
knows. The engine expands components, so it must know the form to read such a definition; the pod records
this feature on the element that has one.
