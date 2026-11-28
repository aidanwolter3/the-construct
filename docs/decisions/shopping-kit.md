# Shopping is a kit of collections: taste fillers explore a need, snapshots keep a pick's price

Date: 2026-11-27. Related: [research-per-row](research-per-row.md), [search-product-data](search-product-data.md),
[taste-data-source](taste-data-source.md), [element-roles-and-reuse](element-roles-and-reuse.md),
[collection-computed-fields](collection-computed-fields.md), [live-computed-fields](live-computed-fields.md).

The owner wants one shopping flow: tell their agent "I want a full-length mirror", see it on the list as nothing
chosen yet, explore what's out there (first the kinds of thing, then narrowing toward one), choose it, and have
its price watched, with a notification when it drops. Their pod had pieces of it spread over four hand-made
collections and three fillers held together by prompt text, and the taste service, which already knows how
clothes resemble each other, sat beside it unused.

Considered:

- **A shopping source** (its own crate, tables and routes, like Taste). The flow's rounds, shortlist and price
  history are easy in code, but the owner loses what Collections give: fields they add, screens they copy,
  `add_row` for the agent, edits by Claude. Rejected by the owner.
- **Collections as they were.** A taste call has no filler; a `for_each` research waits 20 s and costs a model
  call per row; a price is one value, not a history; and a computed field couldn't read another, so a status
  line over rollups, or a deal over the lowest price, couldn't be written.
- **Collections, grown where they fall short, shipped as a kit.** Taken.

Decision:

- **`taste` fillers.** `{kind: "taste", for_each: {collection, link, where?}, request: {part: field}, map: {field:
  part}, copy?}`: for each row of another collection, its fields make an explore request (`query`, `like`,
  `unlike`, `exclude` as piece keys, `distance`, `spread`, `group`, `n`, `price_max`) and the pieces answered are
  its rows here, linked back (the `Fetch::Each` store of research per row). Collections asks through an
  `Explorer` trait the server fills with the taste source's (`source_taste::explore`), so Collections needn't
  depend on Taste (Taste depends on Collections). A row is explored once, as soon as it is written (someone is
  waiting on the screen), and never again on its own, not even when the filler changes: its pieces carry
  reactions. No model and no web.
- **The taste service's `POST /api/explore`** (the taste repository): stateless, around an anchor (the liked
  pieces' centre pushed away from the disliked ones, or the query's text), `distance` a percentile rank among
  the pieces matching the query (so 0.3 means the same in a dense catalog and a sparse one), `spread` how varied
  the answer is, `group` clusters with labels from the words that set them apart. The pod keeps no session:
  the rounds are rows.
- **Snapshots.** `snapshots: [{into, link, day, fields}]` on a definition copies some fields into a row of
  another collection per row and day, on every write and once a day, so a value has a history: rollups give its
  lowest and highest, `core://Bars` charts it. Considered an append mode on fillers (only fills would write
  history, not a person's edit) and a windowed rollup (not needed: the lowest since the pick was chosen is the
  rollup over all its days).
- **Chains of instant computes.** A computed field may read an earlier computed field (in the definition's order)
  that isn't a model's answer; it is worked out in the same write, and a live recompute follows the chain in
  order. Model answers stay excluded (they come later, in the background).
- **Kits of several collections.** A kit may hold several collections under fixed names (its layouts read them)
  and the changes that need them all (rollups over the rows that point back, alerts); `use_kit` makes what is
  missing and skips what is there, so using it twice changes nothing.
- **The `shopping` kit:** `shop_categories` (Clothes explored, Home and Other found on the web by an agent
  filler), `shop_needs`, `shop_rounds` (the narrowing rule as expressions: distance 0.5, 0.3, 0.18, 0.11 unless
  `how_far` is chosen), `shop_pieces` (♥, ✕ and the shortlist as fields), `shop_picks` (a daily `query_field`
  price check while watched, the deal and back-in-stock alerts, About this pick from a search), `shop_prices`
  (the snapshots), with the layouts `@core/shopping`, `@core/shopping_need` and `@core/shopping_explore`.

Consequences: no client build (existing vocabulary). A kit layout belongs to Collections and may only call
Collections, so a piece opens on its store's page rather than Taste's piece sheet, and choosing a pick isn't sent
to the taste service as a like. The taste service's catalog is clothes, so Home and Other needs find options on
the web until it has more domains. The owner's existing shopping collections stay as they are; moving their rows
into the kit is done on their pod, not by a migration.
