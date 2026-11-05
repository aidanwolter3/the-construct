`core://Page`: a layout whose root is any node, drawn with no chrome from the app (no title, back arrow or insets),
and screens (`{id, route, title?, data?, root}`) that each draw their own root (decision `plumbing-and-porcelain`).

Fields: id, title, root, screens?: [{id, route, title?, data?, item?, root}].

The root of a plumbing layout: draws root over the whole space with no title, back arrow or padding from the app.
Draw your own header (a Row with a back core://IconButton shown while ${local://nav#can_go_back}), and wrap what
should scroll in core://Scroll. Screens open as a workspace's do and draw their own root.
