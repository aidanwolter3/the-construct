# A taste piece opens as a sheet: its own core layout, layouts only

Date: 2026-11-24. Related: [taste-data-source](taste-data-source.md), [taste-deck-and-map](taste-deck-and-map.md),
[layouts-not-apps](layouts-not-apps.md).

The owner asked to drop the taste deck's Details screen, "This, but…", the Opposite pieces, the liked and skipped
counts and the sign-out button (Settings signs out), to call skipped disliked, and to open a piece's similar
pieces with a swipe up on its card; and to build it from layouts, so it can change again without an app build.

Decision:

- **`taste_item`, a second core layout** of the source: the piece as a sheet (its photo, brand, title, price, Buy,
  like or dislike, and its similar pieces). The `taste` layout opens it with existing vocabulary: an
  `action.sequence` of `action.navigation.open {layout: taste_item, presentation: Sheet, params: {key}}` and
  `action.data.load` of the item's data path. It is the one item view: a swipe up on a deck card (`core://Gesture`
  `on_swipe_up` around the card; a horizontal drag is still the card's), a tap in the middle of its photo, and the
  likes, search and area pieces open it. A similar piece takes the sheet's place (back, then open), one sheet at a
  time. Map dots keep their pushed point screen, made compact like the sheet.
- **Read by its route.** A layout opened with `params` binds `$route` in text, intents and `where`, but not in a
  list's `query_key` (a screen's route is substituted into its whole root when it opens; a layout's is not). So the
  sheet reads its piece as `${taste://items/$route.key#<field>}`, and its similar pieces from one query of fixed
  name, `taste:sheet:similar`, which the item's data path now also answers: copies of the similar pieces at
  `taste://sheet-similar/<key>/<n>`, each with `of: <key>`, listed `where of equals $route.key`, so a sheet never
  shows another piece's list while its own loads.
- **Words:** what the owner reads says disliked (the counts texts, the map's legend, the ✕ button, the jack's
  summary, which now speaks to them); the wire keeps `skipped`. The pod routes the old layouts use (the item
  screen, steer, contrast) stay declared.

Consequences: no client build: the layout files (`taste/0004.json`, `taste_item/0001.json`) and the pod's extra
query ship with the server. A client older than `carousel_tap_paging` and `scatter` keeps `taste/0002`. If layouts
later bind `$route` in query keys, the fixed query can go.
