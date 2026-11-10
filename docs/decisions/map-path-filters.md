# Map paths may end in filters (`| link`, `| text`)

Date: 2026-11-09. Related: [collection-fillers](collection-fillers.md).

Some feeds put the value a filler needs inside another one. Google Calendar's public iCalendar feeds (Downtown
Boulder's citywide calendars, for one) leave `URL` empty and put the event's page in `DESCRIPTION` as an HTML link,
often wrapped in a Google redirect. A fetch filler could map neither the link nor clean text, so `details` (which
reads each new row's own page once) had no link to follow. The workaround was an agent filler that re-read every
event page on every fill, costing model calls for pages already seen.

Considered: an `expr` compute on the field (computed fields can't fill a field a filler writes, and expressions
don't do text search); a model compute (a model call per row for something a few lines of code do); a regex
filter (more general, but recipes are written by models and people on phones: a named filter is easier to get
right, and a new need can add a new name).

Decision: a map path (not an `items` path) may end in filters, each after a `|`, applied in order to the value it
picks: `link` (the first `http(s)` link: an `href`, else a bare URL, with a `google.com/url?q=` redirect unwrapped;
nothing if there is none) and `text` (HTML tags dropped, entities decoded). So `{url: "description | link"}` fills
the link a description only links to, and `details: {link: "url", ...}` then reads that page once per new row. They
live in `web-extract`'s path language, so every path-based extractor (`events`, `ics`, `json`, `embedded-json`,
`jsonld`, `rss`) and `details` get them; `selectors` maps are CSS and don't. An unknown filter is refused when the
recipe is checked.
