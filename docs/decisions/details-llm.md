# `details` may read each new row's page with a model (`extract: "llm"`)

Date: 2026-11-09. Related: [collection-fillers](collection-fillers.md), [map-path-filters](map-path-filters.md).

`details` reads each new row's own page once, with a path extractor (`events`, `jsonld`, ...) or, when that finds
nothing, the page's whole readable text (`text`). Many event pages (Downtown Boulder's, for one) have no structured
data, so only the text fallback worked, and it stored the whole page, site menu included: a keyword alert on the
field then matched every row (the menu said "Gallery"). The alternative was an `agent` filler that re-reads every
event page on every fill, costing model calls for pages already seen.

Considered: a model compute on the field (it can't read a page, only the row's fields); cleaning the page text
(menus and footers differ per site); an `llm` fetch filler with `url_field` (it re-reads every row's page on every
fill, not only new rows').

Decision: `details` also takes `extract: "llm"` with `fields` (the fields the model answers) and an optional `hint`,
like an `llm` fetch filler but for one row's page: the page's text goes to the same page reader, and the first row
it answers fills those fields. It runs only for rows a fill creates (at most `MAX_DETAILS` a fill), so each page
costs one model call, once. `map` and `text` don't go with it. Without a model configured, it reads nothing.
