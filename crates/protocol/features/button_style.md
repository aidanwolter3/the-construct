`core://Button` `style: Primary|Secondary|Tonal|Text` and `size: Regular|Small`: one of the renderer's fixed button
faces (decision `one-button-node`). `Primary` (the default) is filled with the accent, `Secondary` outlined on the
card colour, `Tonal` filled with the accent's tint, `Text` accent text alone. Every style of a size has the same
height, padding and corner: `Regular` (the default) 40 dp tall, `Small` 32 dp. It doesn't gate a layout: a client
without it ignores both fields and draws a regular primary button.
