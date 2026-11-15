# People pick a row's picture from its image results

Date: 2026-11-15. Related: [image-search](image-search.md), [collection-fillers](collection-fillers.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md).

An `images` filler fills a row's picture from the first image result. For a product name that is usually right;
for a piece described in someone's own words ("Mustard zip hoodie") it is often another item. A field a filler fills
can't be set by hand, and people had no way to choose a better picture.

Options: upload one's own photo (the exact piece, but uploads reach collections only through creates today, so
changing a row's photo needs new client vocabulary and a build), or choose among the search's other results.

Decision: **a data path that lists a row's image results**, `GET /v1/collections/<c>/images?id=<row>`: the
collection's `images` filler (the first, or `&filler=<id>`) searches the row's query, through the same cache as the
fill, and answers up to 10 results as entities `collections://<c>/<row>/images/<n>` (`image`, `thumbnail`, `page`,
`title`, `source`, `n`, `row_id`) in the query `collection:<c>:images:<row>`. No query, no `web_search` jack or a
failed search is an empty list; no such row or no images filler is a 404. Picking adds no endpoint: a tap saves the
picture's URL into a plain field with the ordinary row `update`, and the field the layout draws is an `expr` that
prefers it (`entity.my_photo ? entity.my_photo : entity.found_photo`), so the filler keeps filling its own field and
a pick is never overwritten. No client build: it is a pod data path and layout data.

Not yet: uploading one's own photo for an existing row (needs attachments on updates); checking that each listed
picture still loads (the fill checks the one it keeps; the list shows the index's).
