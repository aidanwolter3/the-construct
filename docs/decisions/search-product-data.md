# A search answer's product data joins its snippet, and an `every` query row asks again only when its results change

Date: 2026-11-18. Related: [research-per-row](research-per-row.md), `query_field` fillers.

A daily price check (`refresh: every`) on a shopping collection could not read most stores: their pages refuse the
pod (403, 429). Moving it to `query_field` with `extract: llm` searched for each item and let the model pick a
price from the results, but two things kept it from working:

- **A row was asked once.** The row's key was the result URLs, which barely change day to day, so a later run
  skipped the model even when a snippet showed a new price. And a search was cached for a day, so `every` read
  yesterday's answer.
- **Brave's snippets rarely name a price.** Brave can attach a result's schema.org product data (`product`,
  `schemas`) and `extra_snippets`, but the parser kept only title, url and description.

Considered:

- **A paid shopping API.** A new provider and key for one collection.
- **Ask `extra_snippets` in every Brave request.** A plan without it may refuse the request, which would break all
  searches. Not asked; read when Brave sends it.
- **Read what Brave already sends.** Taken.

Decision:

- **Brave results' price and stock facts join the snippet**, as `key: value` pairs (`price`, `lowPrice`,
  `highPrice`, `original_price`, `priceCurrency`, `availability`, at most 8), read wherever they sit in `product`
  and `schemas`, then any `extra_snippets` line with a `$`. The model reads them like any snippet; no new field.
- **A `query_field` row's key is the text the model would read** (`results_text`), so a changed snippet asks the
  model again and unchanged results don't.
- **`refresh: every` searches afresh** each run, bypassing the day-long search cache.

Not yet: whether Brave sends product data for these stores is only seen on a live pod.
