# Text strikethrough

Date: 2026-11-17. Related: [text-weight-caps-lines](text-weight-caps-lines.md),
[named-vocabulary](named-vocabulary.md).

A shopping list wants a bought item crossed out rather than hidden, and a to-do list a done task. A pod could fake
it (a thin `core://Box` laid over the text), but that can't follow the text's width or its wrapped lines, so it
belongs in the plumbing next to `weight`, `caps` and `max_lines`.

Considered: cosmic-text has no strikethrough decoration in the version the renderer uses, and drawing the line as
a scene quad would need the shaped lines' widths at layout time, which only the renderers know (they shape at the
frame's scale).

Decision:

- `core://Text` gets `strike: bool` (feature `text_strike`). Each laid-out line that has glyphs gets a solid line in
  the text's own colour, 1 dp thick (whole device pixels), from its first glyph's left to its last glyph's right,
  0.3 em above the baseline (through the middle of the lowercase letters). It works with `weight`, `caps` and
  `max_lines`; screen readers read the text as written.
- The lines are worked out once, where the text is shaped (`text::strike_lines`), in whole device pixels, so the
  CPU renderer (which fills them) and the GPU renderer (which draws each as a solid custom glyph of the text's own
  area, taking its colour and clip) draw the same pixels.
- Like the other text styling, it doesn't gate a layout: a client without it ignores the field (serde default)
  and draws the text plain.

Not done: underline, or a strike colour or thickness of its own.
