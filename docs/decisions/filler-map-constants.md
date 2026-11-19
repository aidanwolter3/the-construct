# A filler's map may set a constant (`=<text>`)

Date: 2026-11-19. Related: [map-path-filters](map-path-filters.md), [collection-fillers](collection-fillers.md).

A filler fills fields only from what it reads, so every row of a source gets the same values only if the source
says so. The pod's own calendar (an `mcp` filler over Proton Calendar) feeds the Events collection beside sites'
listings, and its rows are the user's own plans: they should arrive marked `going`, which a toggle on other
sources' rows sets by hand. Proton's events carry nothing that reads as true, so no path could fill it.

Considered: a compute on the field (a computed field can't be filled by a filler, and it would take `going` away
from every other source's toggle); a model compute (a model call per row for a fixed value); a filler-level
`set: {field: value}` option (one more key in every recipe kind, when the map already names a field and where
its value comes from).

Decision: a map value starting with `=` is a constant: every row gets the text after it (`{going: "=true"}`),
coerced to the field's type like any other value. It lives in `web-extract`'s `items::extract`, so every
extractor (`selectors` too) and every filler kind that maps items (`fetch`, `mcp`, `range`, `details`) gets it. A
row whose other fields all came out empty is still dropped, so a constant never makes a row on its own. Like any
mapped field, a constant one is filled by the filler and can't be set by hand on its rows.
