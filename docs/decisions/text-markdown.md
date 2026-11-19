# Markdown text, drawn by the renderer

Date: 2026-11-19. Related: [text-strike](text-strike.md), [text-weight-caps-lines](text-weight-caps-lines.md),
[agents-data-source](agents-data-source.md), [named-vocabulary](named-vocabulary.md).

Agents report in Markdown: the runner's run summaries, reports and questions arrive with `## headings`, `- lists`,
`**bold**`, `` `code` `` and fenced blocks, and a `core://Text` showed the markup as written. A layout can't build
the structure itself: the text is data, bound at draw time, and a layout is fixed.

Considered:

- **The pod turns Markdown into nodes** (a list of blocks as entities, drawn by a `core://List` with conditionals
  per kind). No client build, but a paragraph's bold or code needs styled runs inside one text, which the
  vocabulary doesn't have, and every source with Markdown would repeat the work.
- **The renderer expands it into `UiNode`s** and draws those. The text it holds would then pass through binding
  again, where untrusted text containing `${…}` or `$item.` would be read as a placeholder.
- **The renderer reads it into blocks and lays them out with its own boxes and texts** (chosen).

Decision:

- `core://Text` gets `markdown: bool` (feature `text_markdown`). The bound text is read by
  `construct_render::markdown` (no dependency): ATX and setext headings, paragraphs, bulleted, numbered and task
  lists nested by indentation, quotes, fenced code, GitHub tables and rules; inside them bold, italic, code,
  strikethrough, links, autolinks and bare `http(s)://` addresses, and backslash escapes. HTML stays text. A
  single line break stays a line break, as in a GitHub comment, because status text is written that way. Anything
  it doesn't know is text; it can't fail, and nesting deeper than 8 is read as text.
- The blocks are laid out in the text's style and colour from the renderer's existing primitives: headings scaled
  from the style's size (1.43, 1.25, 1.1 ×) and weight, items with a drawn dot (a ring when nested), a number or a
  task box (☑ in the accent, its text muted; ☐), quotes behind an accent bar in muted text, code on a
  `muted_surface` card, tables in a bordered box of equal cells with a tinted header row, rules in `card_border`.
  A markdown text without `max_lines` spans its parent's width, so code cards and tables do.
- Inline styles are spans of the text (`text::RichSpan`) shaped in one cosmic-text buffer, so wrapping, measuring
  and both renderers agree: bold at weight 700, italic slanted (the bundled faces have no italic cut), code in a
  bundled monospace face (DejaVu Sans Mono subset, 47 KB) at 0.86 of the size, links in the theme's `accent`,
  strikethrough by the same `text::strike_lines` as `strike`, over the struck glyphs only.
- With `max_lines` the blocks are flattened into one styled text (headings bold, items with their bullets) on that
  many lines: a row's preview.
- Screen readers, the simulator's `assert-text` and previews get the flattened text without markup, as one node.
- Links are not tappable: a span can't take a tap of its own, and the text is often untrusted (the Agents source
  checks the few links it lets open). It doesn't gate a layout: a client without it draws the Markdown as written.
- The Agents blueprint draws the agent's prose (`summary`, `outcome_summary`, `needs_you_text`, `follow_up`,
  `last_summary`) as Markdown from layout version 7. Timeline lines stay plain: they are one-line tool and state
  logs, where `__init__.py` should not turn bold.

Not done: tappable links, a background behind inline code (the renderers shape at the frame's scale, so the span's
boxes aren't known at layout), images, HTML, footnotes, and syntax colouring in code blocks.
