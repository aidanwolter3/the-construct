# An agent filler can research each row of another collection, with checked citations

Date: 2026-11-18. Related: [collection-fillers](collection-fillers.md),
[web-and-agent-modules](web-and-agent-modules.md), [web-fetch-provenance](web-fetch-provenance.md),
[collection-computed-fields](collection-computed-fields.md),
[notifications-follow-their-alerts](notifications-follow-their-alerts.md).

A user wants one shopping experience: they list what they need ("3 V-neck undershirts", "2 pairs of jeans for work
and going out"), each item gets researched against their preferences for its category (sizes, colours, the
style they swiped right on), the options come back as a list with prices, photos, reviews and links to their
sources, they pick some into a bag, and prices are checked daily so a deal notifies them. A pod could do half of
this: a `query_field` filler fills each row's own fixed fields (three `opt1_*`/`opt2_*`/`opt3_*` sets) from one web
search, and an `agent` filler answers one prompt for a whole collection. Neither gives an item its own list of
options, neither carries the owner's preferences per category, and nothing checked the reviews a model quoted.
An alert notified once per row ever, so a second price drop never notified, and an expression couldn't compare
numbers or remember a lowest price.

Considered:

- **The external agent runner** (decision `agents-data-source`): a full session per item in a sandbox. Rejected
  for this: heavy, slow, and its output isn't rows; the in-pod agent already has the pod's own search, fetch,
  host policy, budgets and link provenance.
- **More fixed option fields per row.** Rejected: options are rows (a photo, reviews, a price history and an in-bag
  mark each), and a layout lists them like any rows.
- **A prompt template with placeholders** (`{name}`). Rejected: the row's fields and the context rows are
  appended by the pod, so a prompt stays plain text and every field (including lookups, e.g. the category's
  preferences) reaches the model without naming each.
- **A price-history collection** for deals. Rejected for now: an expression that reads its own last value gives a
  running lowest, and comparisons give the deal flag, with no new table.

Decision:

- **`for_each`** on an `agent` filler: `{collection, link, where?, from?, context?}`. Each live row of
  `collection` matching `where` is researched on its own: the request is the filler's prompt, then the row's
  fields (`from`, by name; default every field but true/false ones and refs) and the `context` rows (`[{collection,
  where?, fields?, limit?}]`, at most 5 lists of at most 100 rows). The answer is that row's rows in this
  collection: `replace` (by `key`) among the rows whose `link` (a ref field to `collection`, set by the pod and
  locked) names it, with its current rows shown to the model. Each researched row is one model call, counted toward
  `fills_per_day` and the pod's limit; a fill takes at most 10. A row is researched again when its text or the
  context changes (`refresh: once`, the default, using the per-row marks of `query_field` fillers) or on every fill
  (`every`); rows never researched go first, then the longest ago. A write to `collection` starts its `for_each`
  fillers 20 s later (so the row's computed fields settle), retrying while a fill runs. Rows of a researched row
  that is deleted are removed. Fields of the other collections are kept by name (renames leave aliases) and
  resolved when the fill runs.
- **`keep: [{field, equals}]`** on any filler that adds and removes rows: rows matching every condition stay when a
  refill leaves them out (an option someone put in their bag).
- **Checked citations.** With the pod's own web tools, a research answer is checked against what the task read
  (`web-extract`'s `tools::Seen::check_rows`): a value that is a link the task never saw becomes null, and in
  other text a line holding such a link, or quoting three or more words that are on no page or search result it
  read (case, spacing and punctuation aside), is left out. The request's own text (its current rows) counts as
  read. Without the `web_search` jack (the API's server-side tools) there is no text to check against; links are
  still limited to what the conversation holds.
- **Pictures.** `web_fetch` lists a page's main pictures (`og:image`, `twitter:image`, `image_src`, JSON-LD
  `image`, at most 4) after its text and marks them seen, so an image field gets the store's own photo (and passes
  the link check). The image search filler and `images?id=` picker stay for the rest.
- **Expressions** gain `*` and `<`, `<=`, `>`, `>=` (two numbers, or two texts in order, so `YYYY-MM-DD` dates
  compare), and an expression may read its own field's last value (other kinds still can't read themselves), so
  `price_low` can be a running lowest and `deal` a comparison of today's price with the researched one.
- **`repeat: true`** on an alert: a row that stops matching has its sent mark forgotten, so it notifies again the
  next time it matches; each such notification has its own dedupe key. The earlier notification still goes when
  its row stops matching (decision `notifications-follow-their-alerts`).

Not done: a per-category deal threshold read through a ref (a lookup of a lookup) is left to the layout's own
fields; the daily price check is an ordinary `url_field` fetch filler (`jsonld` `Product` `offers.price`), whose
fields must differ from the agent's (`price_now` beside the researched `price`). Research starts only from the
pod's own writes and its schedule; filler-written rows of the other collection are researched too, within the
budgets.
