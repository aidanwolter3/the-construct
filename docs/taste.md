# The Taste source

The `taste` source (crate `source-taste`, decision [`taste-data-source`](decisions/taste-data-source.md)) shows the
feed of the owner's **taste service** in the pod: clothes from real stores, each with its photo and a link to buy
it, ranked by a service that learns what they like from likes and skips and keeps showing them things they haven't
seen. The service is a separate HTTP service (its own repository); the pod connects out to it and keeps the feed
it was served.

## Signing in

The `taste` core layout opens on a form for the service's address (`https://…`) and its token (the service's
`TASTE_TOKEN`). `POST /v1/taste/sign-in {url, token}` checks them with `GET /api/taste` and stores the token sealed.
Settings shows the `taste` jack as plugged once signed in; unplugging it, or the layout's Sign out
(`/v1/taste/sign-out`), forgets the service and the kept feed. Temporary pods have the jack locked.

The address must be `https://` on a public host. For a service on this machine (development, tests), run the server
with `CONSTRUCT_TASTE_PRIVATE_HOSTS=1`.

## What the source offers

| Kind | Path or key | What |
|---|---|---|
| Feed `feed` | `taste:feed`, `taste:status` | The kept feed, and the connection with its like and skip counts |
| Data path | `/v1/taste/screens/item?key={key}` | One piece (`taste://items/<key>`), `taste:similar:<key>` and `taste:contrast:<key>` |
| Data path | `/v1/taste/screens/steer?key={key}&q={q}` | `taste:steer:<key>`: the piece moved the way the words point |
| Data path | `/v1/taste/screens/search?q={q}` | `taste:search` and `taste://search/pod` |
| Data path | `/v1/taste/screens/likes` | `taste:likes` |
| Data path | `/v1/taste/screens/taste` | `taste:tastes`, their pieces `taste:taste_items` (each with its `taste`) and `taste://summary/pod` |
| Action | `/v1/taste/react` | `{id, liked}`, `{id, skipped}` or `{id, kind}`: passed to the service |
| Action | `/v1/taste/more`, `/v1/taste/refresh` | Append the next page; start again with a fresh one |
| Action | `/v1/taste/summary` | Have the service put the taste into words |

An item's `key` is its service id made fit for a route (`shop.example:123` is `shop_example__123`). Items carry `image` (720 px),
`image_large` (1600 px) and `thumb` (360 px), all the store's own CDN URLs, `price_text`, `buy_url`, `host`, `liked`,
`skipped`, and in the feed `why_text` (Near your taste · 2, Adjacent, Exploring). `list_data_sources` lists every
field.

The feed is fetched from the service only when the pod has none kept, on More and on Refresh: the service serves a
new page for every request, and a layout's feed is fetched again after every data event. A reaction goes to the
service, then marks the kept item.

## The core layout

`@core/taste` is a page: the feed as a grid of big photos (one column on a phone, more on a wide window), each with
why it is there, its brand, name and price, Like and Skip (a skipped piece leaves the feed); Show more at the end;
Refresh in the header. A piece's screen has the large photo, Buy at the store, Like and Skip, "This, but…" (typed,
or Relaxed, Dressier, Rugged) and its Similar and Opposite pieces. Search, Likes and Your taste (each taste's words,
brands and pieces, and the description the service writes) are screens of their own.

Use it whole from a pod's layout, `{"type": "core://Use", "ref": "@core/taste"}` with the feed `["taste", "feed"]`,
and link that from home; `taste:status`'s `counts_text` (12 liked · 30 skipped) makes a one-line summary there.

## The service's API

What the source calls, each with `Authorization: Bearer <token>`: `GET /api/feed?n=`, `POST /api/event {id, kind}`,
`GET /api/item/<id>` and its `/similar`, `/contrast` and `/steer?q=`, `GET /api/search?q=`, `GET /api/likes`,
`GET /api/taste`, `POST /api/taste/summary`. Everything it answers is treated as untrusted display text: cleaned,
cut, and links kept only when they are `http(s)`.

## Tests

`cargo test -p source-taste` runs the source against a fake service (`tests/integration/fake.rs`) with made-up
products. The simulator's `taste` and `taste_item` workspaces draw the core layout over made-up data (the `taste`
scenario).
