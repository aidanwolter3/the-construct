# Image search fills a row's picture, with no model

Date: 2026-11-15. Related: [web-and-agent-modules](web-and-agent-modules.md), [jacks](jacks.md),
[jacks-in-settings](jacks-in-settings.md), [collection-fillers](collection-fillers.md).

Some collections need a picture per row: the Style deck wants a product photo for each piece, and shops such as
J.Crew and L.L.Bean block the pod's page fetches, so a `url_field` filler can't read the photo from the product page.
The `web_search` jack (a Brave key, plugged from Settings) only ran web searches, which return pages, not images.

Options: a model picking among image results (a model call per row, for a choice the search's ranking already makes
well), an image tool for agent fills (an agent fill writes rows of its own; it doesn't fill rows people typed), or
the `query_field` filler with an image search in place of the web search and the model.

Decision: **`extract: "images"` on a `query_field` fetch filler.** Each row's query (its `query_field` text) is
searched for images with the pod's `web_search` jack, and `map` fills fields from the first result: `image` (the
full-size picture's URL), `thumbnail`, `page` (the page it is on), `title` or `source` (the site's host). No model is
asked, so no model budget is spent and the `model` jack isn't needed. The web module (`web_extract::search`) gains
`search_images`: Brave's `/res/v1/images/search` (safe search strict) or SearXNG's `images` category, through the host
policy, cached apart from web searches. DuckDuckGo's image results need a token scraped from its pages, so it has no
image search; a fill with it plugged fails saying so. Everything else is the `query_field` filler's: `refresh: once`
by default (a row is searched when its query first appears or changes), `on_empty: keep`, failed rows listed in
`collections_status`. Validation names the image parts a `map` may use, keeps `extract: "images"` to `query_field`
fillers, and refuses filling the query field itself.

Not yet: picking among several images (only the first is used); an image search tool for agent fills; previewing an
images filler (`collections_preview` refuses it: it searches each row).
