# The Taste source

The `taste` source (crate `source-taste`, decisions [`taste-data-source`](decisions/taste-data-source.md),
[`taste-deck-and-map`](decisions/taste-deck-and-map.md) and [`taste-item-sheet`](decisions/taste-item-sheet.md)) shows the
feed of the owner's **taste service** in the pod: clothes from real stores, each with its photo and a link to buy
it, ranked by a service that learns what they like from likes and dislikes and keeps showing them things they haven't
seen. The service is a separate HTTP service (its own repository); the pod connects out to it and keeps the feed
it was served.

## Signing in

The `taste` core layout opens on a form for the service's address (`https://…`) and its token (the service's
`TASTE_TOKEN`). `POST /v1/taste/sign-in {url, token}` checks them with `GET /api/taste` and stores the token sealed.
Settings shows the `taste` jack as plugged once signed in; unplugging it (Settings signs out there), or
`/v1/taste/sign-out`, forgets the service, the kept feed and the kept map; a new sign-in forgets the map too. Neither
forgets the owner's taste history (below), which the next service is given. Temporary pods have the jack locked.

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
| Data path | `/v1/taste/screens/one-taste?n={n}` | One taste to browse: `taste://tastes/<n>` (with `count_text`) and `taste:one-taste:<n>`, its likes (`liked` true) then the nearest pieces not judged yet, up to 60 |
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

## The taste history

The owner's likes and dislikes are the pod's (decision `taste-history-in-the-pod`): `taste_reactions` holds every
reaction in order, `taste_served` how often each piece has been served, and `taste_history` the history's name (`log`,
made once per pod). The service keeps only a cache of it. Every request names it, `X-Taste-State: <log>/<n>` (n: the
reactions in it); when the service doesn't hold that version it answers 409 `state_unknown`, and the pod sends the
whole history with `PUT /api/state` and asks again. A reaction is added to the history first and sent naming the
version it makes; one the service refuses is taken back out. Each page fetched counts its pieces as served.

The first time a pod meets a service that can hand its own history over (`GET /api/state`, no header), it takes that
history over, once: how a pod keeps the reactions made before it owned them. Until then, and with a service from
before (no `/api/state`, asked again every ten minutes), requests name no history and the service uses its own.

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

Since `layouts/taste/0004.json` (decision `taste-item-sheet`; layouts only, no new build): a swipe up on a deck
card, or a tap in the middle of its photo, opens the piece as a sheet, the `taste_item` core layout (its photo,
brand, title, price, Buy, Like and Dislike, and its Similar pieces; a similar piece takes the sheet's place). It is
the one item view: Likes, Search and an area's pieces open it too (map dots keep their point screen, made compact).
The card says "⌃ Swipe up for similar" under the photo. Gone: the Details screen and button, "This, but…", Opposite,
the liked and disliked counts, and the sign-out button. The owner reads "disliked" where the wire says `skipped`.
The sheet is opened with `{intent: action.sequence, steps: [{intent: action.navigation.open, layout: taste_item,
presentation: Sheet, params: {key}}, {intent: action.data.load, path: /v1/taste/screens/item?key=<key>}]}` and reads
its piece as `${taste://items/$route.key#<field>}` and its similar pieces from `taste:sheet:similar` (copies tagged
`of: <key>`, listed `where of equals $route.key`).

Since `layouts/taste/0005.json` (layouts only, no new build): the deck's card has no ✕ and ♥ buttons; it is liked by
swiping it right and disliked by swiping it left (the sheet keeps its Like and Dislike buttons), and the description
takes their room, six lines on a phone. The main view doesn't scroll: the root is a header (`core:page_header`, with
the refresh action) over a plain column, not the scrolling `core:screen_scaffold` (the signed-out form keeps a
scroll of its own). Nothing in the vocabulary sizes a photo by the screen's height, so the phone card's photo is
0.88 wide for its height: with the longest description the service sends (300 characters) the card fits a 360 × 740
phone under its system bars and leaves room on a 390 × 844 one (the simulator scenario checks both). On a shorter
screen the card's bottom is cut off rather than scrolled.

Since `layouts/taste/0006.json`: the phone card's text block (brand, two lines of title, price, six lines of
description) is a box of fixed height (184 dp), so every card in the deck is as tall as the longest. Before, a card
behind the top one with a longer description was taller and its last line showed below the top card; now only its
edge peeks out, and a short text leaves its room at the bottom of the card.

Since `layouts/taste/0007.json`: each taste on Your taste has a Browse button (its title opens it too) that pushes
`taste://tastes/{n}`, a screen with the taste's words, brands and kinds of garment over a scrolling grid of its pieces
(two columns on a phone, more on a wide window): its likes first, marked ♥, then the catalog's pieces nearest the taste
that haven't been liked or disliked. A piece opens the piece sheet. Since `layouts/taste_item/0002.json` the sheet
leads with the piece's photos, a full-width carousel paged by tapping its left or right third (at most 380 dp wide),
then its brand, title, price, Buy, Like and Dislike, its description and the similar pieces; the deck's swipe up, the
Likes, Search and area grids open the same sheet. A carousel can't name a piece by the sheet's route, so the item's
data path also answers it as `taste:sheet:piece`, which the sheet lists where `key` equals `$route.key`, paging
`$item.photos`.

Since `layouts/taste/0008.json` and `layouts/taste_item/0003.json` (decision `taste-fit-check`; layouts only, no
new build): a garment's card says whether it fits, a pill beside its reason (in place of the swipe hint) and, above
the description, the size it was judged on with its measurements; the sheet shows both and where the chart came from.
Since `layouts/taste/0010.json`, Your taste says it too: a strip's thumbnail carries Fits or Won't fit (no room for
Not sure), and a taste's own grid (Browse) the whole verdict under the price. See [Fit](#fit).

Use it whole from a pod's layout, `{"type": "core://Use", "ref": "@core/taste"}` with the feed `["taste", "feed"]`,
and link that from home; `taste:status`'s `counts_text` (12 liked · 30 disliked) makes a one-line summary there.

## Fit

Whether a piece fits the owner (decision `taste-fit-check`, `src/fit.rs`). Their measurements are rows of the
collection `fit_targets`: `kind` (knit top, woven top, jacket, trousers, shorts), `measure` (chest, the pit to pit
width; length; shoulder; sleeve, from the centre of the back; waist; rise; thigh; inseam; leg opening; body chest and
body waist for charts of body sizes) and `low` and `high` in inches, a garment laid flat (either may be empty: shorts
take `inseam` low 7.5 and borrow the trousers' waist, rise and thigh). With no rows for a kind there is no check.

Which pieces: the deck's next four, any piece whose sheet opens (first) and the first eight of a taste's grid when it
opens (Browse on Your taste; the taste page itself only shows verdicts already kept), when they are garments (from `category`
and the title: shoes, bags, hats and the like are not; long sleeves are told from the kind and the title). Each is
checked in the background, one at a time per pod, and its entity reloaded (a scoped data event) when kept:

1. The piece's own page (`buy_url`) is fetched through the web module (host spacing, daily caps and backoff; no
   search). A store that refuses the plain fetch (403 or 429: many shops refuse a server's address) is read in the
   server's headless browser instead (decision `rendered-fetches`; at most 40 of the pod's renders a day), and its pages
   go straight to the browser for 6 hours; only a refusal there backs the host off. The lines around its size words, when they look like a chart (a size word near two lines of numbers), go
   to the cheap model with a JSON schema (feature `taste_fit`, 200 calls a day), which answers the chart as numbers.
2. With no chart there, the first same-site link that looks like a size guide is read the same way, and kept as the
   brand's chart for that site and kind (`taste_fit_brands`, 30 days; none found, 7).
3. The chart is kept (`taste_fit`) in inches, flat across (an all-round chest, waist, thigh or leg opening halved),
   sleeves from the centre back (half the shoulders added to a seam-to-cuff sleeve, which is also assumed of a long
   sleeve under 30 inches). A piece is checked again after 30 days, a page without a chart after 7, a failed fetch after
   6 hours; a page text the model has read is kept by hash (`taste_fit_reads`) and never sent again.

The verdict is worked out whenever a list is served, against the targets as they are then, on the size with the
fewest misses (¼ inch of slack): `fits` (Fits in M), `no` (Won't fit: sleeves 33 in, the first miss of length,
sleeves, pits, shoulders; or waist, rise, inseam, thigh, leg opening) or `unsure` (Not sure: no length listed, when a
measure that decides it is missing: length, sleeves and pits for tops, waist and inseam for bottoms; a body chart says
the size it suggests; No size chart when there is none). Items then carry `fit_state`, `fit_checked`, `fit_text`,
`fit_tone` (success, error, muted), `fit_note` (Item chart · M: length 29½ ✓ · sleeves 35 ✓ · pits 20 ✓; Brand chart,
a guess, from a size guide) with `fit_noted`, and `fit_source_text`. The pod needs the `model` and `web` jacks plugged
and a cheap model configured; without them nothing is checked.

## The service's API

What the source calls, each with `Authorization: Bearer <token>` and, once the pod owns its history,
`X-Taste-State: <log>/<n>`: `PUT /api/state {log, events: [{id, kind, t}], served: {id: n}}` after a 409
`state_unknown`, `GET /api/state` (no header: the service's own history, taken over once), `GET /api/feed?n=`,
`POST /api/event {id, kind}`,
`GET /api/item/<id>` and its `/similar`, `/contrast` and `/steer?q=`, `GET /api/search?q=`, `GET /api/likes`,
`GET /api/taste`, `GET /api/taste/<n>?n=` (one taste: `{index, size, attributes, brands, categories, items, more}`,
404 for no such taste), `POST /api/taste/summary`, `GET /api/map` (`{ids, xy, cluster, thumb, liked, skipped, p}`: every
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
