`core://Text` `markdown: true`: reads the bound text as Markdown (decision `text-markdown`) and draws it natively in
the text's style and colour: headings, paragraphs, bulleted, numbered and task lists (nested), quotes, fenced code
blocks in a monospace face on a tinted card, tables and rules, with **bold**, *italic*, `inline code`,
~~strikethrough~~ and links inside them. With `max_lines` only the inline styles are kept, the blocks run together
on that many lines (a row's preview). For text that arrives as Markdown, such as an agent's report. It doesn't gate
a layout: a client without it ignores the field and draws the Markdown as written.
