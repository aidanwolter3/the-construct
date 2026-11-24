# The Taste source

The `taste` source (crate `source-taste`, decisions [`taste-data-source`](decisions/taste-data-source.md) and
[`taste-deck-and-map`](decisions/taste-deck-and-map.md)) shows the
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
| Feed `feed` | `taste:feed`, `taste:status` | The deck (kept pieces not reacted to yet), and the connection with its like and skip counts |
| Data path | `/v1/taste/screens/item?key={key}` | One piece (`taste://items/<key>`), `taste:similar:<key>` and `taste:contrast:<key>` |
| Data path | `/v1/taste/screens/steer?key={key}&q={q}` | `taste:steer:<key>`: the piece moved the way the words point |
| Data path | `/v1/taste/screens/search?q={q}` | `taste:search` and `taste://search/pod` |
| Data path | `/v1/taste/screens/likes` | `taste:likes` |
| Data path | `/v1/taste/screens/taste` | `taste:tastes`, their pieces `taste:taste_items` (each with its `taste`) and `taste://summary/pod` |
| Data path | `/v1/taste/screens/map?area={area}` | The taste map over an area (`taste://areas/<area>`) as 4 × 4 tiles, `taste:tiles:<area>` |
| Data path | `/v1/taste/screens/area?area={area}` | `taste:area:<area>`: pieces inside an area of the map |
| Action | `/v1/taste/react` | `{id, liked}`, `{id, skipped}` or `{id, kind}`: passed to the service |
| Action | `/v1/taste/more`, `/v1/taste/refresh` | Append the next page; start again with a fresh one |
| Action | `/v1/taste/summary` | Have the service put the taste into words |

An item's `key` is its service id made fit for a route (`shop.example:123` is `shop_example__123`). Items carry `image` (720 px),
`image_large` (1600 px) and `thumb` (360 px), all the store's own CDN URLs, `price_text`, `buy_url`, `host`, `liked`,
`skipped`, `description`, `chance_text` (41% your taste, once the service can tell), and in the feed `why_text`: On
the edge (the service can't tell whether the user would like it: what it most wants an answer to), Exploring
(somewhere they haven't been) or Probably yours (the older Near your taste · 2 and Adjacent still read).
`list_data_sources` lists every field.

**Photos.** An item has up to six photos, the ones of someone wearing it first: `image_0` to `image_5` (720 px, empty
past `photo_count`). A card shows one at a time without arithmetic in the layout: each photo `k` is a
`core://Conditional` on the device-only field `photo` (photo 0 while it's unset or 0, `equals: "k"` for the rest),
whose tap sets `photo` to `entity.next_<k>` with `action.local.set`; the pod serves `next_<k>`, the photo after `k`
wrapping to 0. `photo` is never served, so a reload shows the first photo again.

**The deck.** The feed is fetched from the service only when the pod has nothing waiting, when a reaction leaves
fewer than six pieces waiting (in the background, then a data event), on More and on Refresh: the service serves a
new page for every request, and a layout's feed is fetched again after every data event. `taste:feed` holds only
the pieces not liked or skipped yet; a reaction goes to the service, then marks the kept item, which leaves the
deck. The pod keeps the newest 60 reacted pieces for their feed reasons and drops older ones.

**The map.** The service lays the catalog out in two dimensions by how the photos look (alike pieces close
together), `0..1` each way. The pod names an area of it by its box in millionths, `x0-y0-x1-y1` (the whole map is
`0-0-1000000-1000000`), so a screen's route can carry it. A map screen cuts its area into 4 × 4 tiles
(`taste://tiles/<area>/<row>_<column>`): each has the service's tint (`color`: warm where the user likes what's
there, cool where they skip, grey where it doesn't know), a photo from it, `count_text`, `reactions_text` (♥ 2 · ✕ 1)
and `opens`, what a tap opens: a finer map of the tile, or its pieces (`taste://area/<area>`) when it holds six or
fewer, or nothing for an empty tile.

## The core layout

`@core/taste` (since `layouts/taste/0002.json`) is a deck: one piece at a time on a `core://SwipeCard` in a
`stack` list, swiped right to like and left to skip (it leaves the deck either way), with Skip, Details and Like
buttons on the card too. On a phone the photo is on top and its reason, photo count (2 / 3), brand, name, price,
chance and three lines of description are under it, all on screen without scrolling; on a wide window (`size_class`
`expanded`) the photo is on the left and the rest beside it. A tap on the photo shows the next one. Refresh is in
the header; Search, Likes, Taste and Map open screens. A piece's screen has its photos (tap for the next), Buy at
the store, the description, Like and Skip, "This, but…" (typed, or Relaxed, Dressier, Rugged) and its Similar and
Opposite pieces. Your taste shows each taste's words, brands and pieces, and the description the service writes.
The Map screen shows the whole map as tiles; a tile zooms in (a screen per area, Back zooms out), and See the pieces
here lists an area's pieces.

Since `layouts/taste/0003.json` (decision `scatter-and-tap-paging`, a build with features `carousel_tap_paging` and
`scatter`; older clients keep 0002): a card's and a piece's photos page on taps (the left third back, the right
third on, the middle opens the piece) with bars over the photo, every photo loaded up front; Skip and Like are round
buttons. Map opens the whole catalog as a scatter (`taste://scatter/pod`, data `/v1/taste/screens/scatter`:
`taste://maps/catalog` with its packed `points`, likes highlighted and skips muted, coloured by the model's chance),
with a legend: pinch or scroll to zoom, drag to move, tap a dot to open its piece (`taste://points/<n>`, data
`/v1/taste/screens/point?n=`, lists `taste:point-similar:<n>` and `taste:point-contrast:<n>`), and long-press (or
Select an area) and drag to open the pieces in a box (`taste://area/<box>`). Grid view is the tile map. The pod keeps
the service's whole map (`GET /api/map`) for 30 minutes, asks again 15 s after a reaction, and marks a reacted
piece's point at once. A reaction to a piece the service no longer has (404 or 410) takes it out of the deck and
answers `{ok: true, gone: true}`, so the card doesn't come back.

Use it whole from a pod's layout, `{"type": "core://Use", "ref": "@core/taste"}` with the feed `["taste", "feed"]`,
and link that from home; `taste:status`'s `counts_text` (12 liked · 30 skipped) makes a one-line summary there.

## The service's API

What the source calls, each with `Authorization: Bearer <token>`: `GET /api/feed?n=`, `POST /api/event {id, kind}`,
`GET /api/item/<id>` and its `/similar`, `/contrast` and `/steer?q=`, `GET /api/search?q=`, `GET /api/likes`,
`GET /api/taste`, `POST /api/taste/summary`, `GET /api/map` (`{ids, xy, cluster, thumb, liked, skipped, p}`: every
item's place, the liked and skipped ones by index and each one's chance or null), `GET /api/map/tiles?x0=&y0=&x1=&y1=&n=` (`{bounds, n, tiles}`, each tile
`{i, j, x0, y0, x1, y1, count, likes, skips, p, color, item}`, row by row) and `GET /api/map/items?x0=&y0=&x1=&y1=&n=`.
Items may carry `description`, `worn` (the first photo shows someone wearing it) and `p` (the chance the user likes
it, or null). Everything it answers is treated as untrusted display text: cleaned,
cut, and links kept only when they are `http(s)`.

## Tests

`cargo test -p source-taste` runs the source against a fake service (`tests/integration/fake.rs`) with made-up
products. The simulator's `taste` workspace draws the core layout over made-up data
(`crates/server/tests/data/simulator/taste_data.json`, made into fixtures by `fixtures::taste_bundles`; the `taste`
scenario swipes the deck, cycles a card's photos and zooms into the map).
