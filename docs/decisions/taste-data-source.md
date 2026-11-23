# A `taste` data source shows the feed of an external taste service

Date: 2026-11-23. Related: [agents-data-source](agents-data-source.md), [jacks](jacks.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[upstream-credentials-sealed-at-rest](upstream-credentials-sealed-at-rest.md).

The owner runs a taste service of their own, a separate HTTP service: it crawls clothing stores, embeds every
product photo, and serves a feed that learns what they like from likes and skips while deliberately showing pieces
near their tastes, adjacent to them and far from them. They want to browse it in the Construct. A layout's buttons
can only call the pod's own endpoints, so likes and skips need a way from the pod to the service. Considered: a
Collections table filled from the service's feed by a fetch filler, with likes kept as fields on its rows and read
back by the service with a pod token (no pod code, but a new page only on Refresh or a schedule, and the owner hands
the service a pod token); and a source that talks to the service itself, like the agents source does with its runner.
The owner chose the source.

Decision:

- **A new adapter, `source-taste`,** a data source named `taste` with a `taste` core layout. The pod connects out;
  the service never calls it. Built on existing vocabulary only: no client build.
- **Sign-in like the agents runner's:** a form in the core layout posts `{url, token}` to `/v1/taste/sign-in`, checked
  with `GET /api/taste`, stored in the adapter's table with the token sealed (one service per pod). `https://` public
  hosts only, no redirects; `CONSTRUCT_TASTE_PRIVATE_HOSTS=1` lifts that for tests and local development. An account
  jack, `taste`, shows it in Settings; temporary pods have it locked, as they have `agent_runner`.
- **The pod keeps the feed.** The service answers a new page for every `GET /api/feed` (it records what it served,
  which its exploration uses), while a layout's feed is fetched again on every data event. So the feed answers the
  items the pod kept (`taste_items`), fetching the first page only when there are none; More appends the next page and
  Refresh replaces them. A reaction (`/v1/taste/react`) goes to the service first, then marks the kept item, so the
  feed shows it at once and the next fetch agrees. At most 240 items are kept, the oldest going first.
- **Everything else is read when it is shown:** an item with its similar and opposite pieces, "this, but …"
  (steer), search, the likes and the taste page are data paths that call the service each time; steer and search
  are loaded with `action.data.load`, the words in the path.
- **Item ids are `<store host>:<product id>`** (`shop.example:123`), while a screen's route captures only letters,
  digits, `_` and `-`, and query keys use `:` between their parts. So entities, routes and keys use a `key` with the
  host's dots as `_` and the colon as `__` (`shop_example__123`; host names hold no `_`, so it turns back); the
  service is always sent the id itself.
- **Images are the stores' own CDN URLs** at a few widths (`width=` on the URL); the clients load them through the
  pod's image proxy, as they do every image that isn't the pod's, so the source needs no media route of its own.

Consequences: no client build. The pod holds a token that reaches the owner's taste service; signing out (or
unplugging the jack) forgets it and the kept feed. A search or steer phrase with `&` or `#` in it is cut there,
since the client puts `$form` values into the load path as typed.
