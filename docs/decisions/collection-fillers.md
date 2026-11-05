# Collections fill themselves from the web with fillers and alerts; the Watcher becomes the Events app on them

Date: 2026-11-04. Related: D40, D52, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[collections](collections.md), [collection-computed-fields](collection-computed-fields.md),
[collection-reminders](collection-reminders.md), [watcher](watcher.md) (superseded here),
[watcher-rules-per-site](watcher-rules-per-site.md) (superseded here),
[upstream-credentials-sealed-at-rest](upstream-credentials-sealed-at-rest.md), [text-dates-and-counts](text-dates-and-counts.md).

A user wants short-lived custom apps over live data from the web: up-to-date election results, the prices of a
few things they are shopping for. No built-in source has that data, and writing one is adapter code the developer
ships. Collections could hold it, but only people and Claude wrote rows, so nothing kept it current.

The Watcher already did this for one shape of data: it fetched websites on a schedule from the pod, skipped
unchanged pages, extracted events with cheap extractors (a model only as a fallback), matched rules and notified.
But it had its own tables, its own pipeline and its own rules, and its output had one shape.

How data reaches a layout:

- **Live adapters** (Pixelfed): each feed request goes upstream with the signed-in person's login. The data is
  that person's, fresh on every open; writes go upstream.
- **Stored sources**: Collections, Recipes (`recipes_*`) and, before this, the Watcher (`watcher_*`).

Layouts don't see the difference: every source offers feeds, data paths and actions answering the same entities.
The missing piece was one level down, in how stored rows are *filled*.

Considered:

- **Claude writes a data source** (code that fetches and maps). Rejected, as in #45 and decision `collections`:
  code, upstream calls and secrets from a prompt.
- **Stored JavaScript** that fetches and returns rows. Rejected for the same reason; a pure, sandboxed transform
  stays open as a later step if recipes prove too weak.
- **A model call on every refresh.** Rejected as the default: cost grows with refresh rate × collections × pods,
  and answers drift between runs. A model writes the recipe (through Claude), and is an explicit, budgeted kind.
- **Fetching on the clients.** Rejected as for the Watcher: fills must run while no client is open.
- **Moving Pixelfed into a filled collection.** Rejected: its data is per person and its writes go upstream.
- **Keeping the Watcher next to fillers.** Rejected: two pipelines that fetch, extract, schedule, report health
  and notify. Its parts were already the general ones.

Decision: a collection may have **fillers**, which write its rows, and **alerts**, which notify when a row matches.
Both are data in the definition: Claude declares them over MCP, the pod runs them. The Watcher becomes the
**Events** app on a system collection with one filler per website.

## Fillers

`definition.fillers`, at most 50 per collection, each `{id, title, kind, …, mode, key?, every_minutes?,
on_open_minutes?, upcoming?}` (`crates/app-collections/src/filler.rs`; run by `fill.rs`). Each row remembers the
filler that wrote it (`collections_rows.filler`), and a filler only touches its own rows, so several sites fill one
collection side by side.

- **`fetch`** (no model call per fill): `url`, or `url_field` (each row's own page fills that row), `extract`,
  `items`, `types`, `script`, `zone`, `map` (field → path, or CSS selector), `headers` (a value may name a sealed
  secret, `{secret:<name>}`), `details`, `hint`.
  - Extractors (`crates/web-extract`, shared): `events` (the Watcher's: an ics feed with `TZID`s resolved, or a
    page's schema.org events; normalized to `id`, `title`, `start`, `end`, `venue`, `description` as plain text,
    `url`, `tickets_url`), `json`, `embedded-json` (`<script id>`, default `__NEXT_DATA__`), `jsonld` (any
    schema.org `types`), `rss` (and Atom), `ics` (raw `VEVENT` properties), `selectors` (CSS), and `llm`: a model
    reads the page's text and answers the fields (`fields`, `hint`), only when the page changed.
  - A small path language picks items and values (`races[*].candidates`, `offers.price`; a key holding a list is
    looked into).
  - Fetch is the Watcher's: conditional `GET`, size and time caps, public hosts only (unless the operator's
    `CONSTRUCT_FILL_PRIVATE_HOSTS=1`), at least a second between requests to one host from a pod. A 304 or an
    unchanged body ends the fill with no work, unless the recipe changed (its fingerprint).
  - Values are coerced to the fields' types; web forms are read (`$1,299.00` as a number, RFC 2822 and iCalendar
    dates, a schema.org enumeration URL as a choice); a time without an offset is placed in `zone`. A value that
    doesn't fit is skipped and counted.
  - `upcoming` (a date or datetime field) leaves out items that have passed. `details` (`link`, `extract`, `map`,
    `text`) reads each new row's own page, at most 10 a fill (the Watcher's event pages, for lineups).
- **`agent`** (costs model calls): `prompt`, `fields`, `max_searches` (≤ 20), `max_fetches` (≤ 30),
  `fills_per_day`. One fill is a Messages API conversation with the server-side web search and fetch tools, ending
  in a strict `save_rows` tool whose schema is the fields; `pause_turn` resumes. Budgets: the filler's
  `fills_per_day` and the pod's `CONSTRUCT_FILL_AGENT_DAILY` (default 20). Behind the `FillAgent` trait (tests use a
  fake), which also reads pages for `llm`.
- **Modes:** `replace` (the filler's rows are its latest answer; with a `key` rows keep their ids and unfilled
  fields; rows it no longer lists are deleted for good, not kept 30 days: they were never anyone's) or `upsert` by
  `key` (rows not in the answer stay; a person's delete sticks).
- **Locks:** fields a filler writes can't be set by apps or `edit_rows`; a `replace` filler's rows can't be added,
  deleted or restored by hand. Computed fields compute on filled rows as after any write.
- **When:** `every_minutes` (fetch 15…10080, agent 60…10080) through the adapter's poll; `on_open_minutes`, and an
  overdue schedule, when an app showing the collection opens; `collections_refresh`; a layout's
  `POST /v1/collections/<c>/refresh {filler?}`; and after a filler is set. Failures back off (doubling from the
  interval, at most a day, at least `Retry-After`).
- **Health** per filler: pending, ok, failing, empty (three fills found nothing), off, unavailable (no model).
  Stored in `collections_fills` with the validators, body hash and model fills today.
- **Temporary pods** get `collections.fill = 0`: they can't fetch or ask a model.

## Alerts

`definition.alerts`, at most 100 per collection (`alerts.rs`): `any`, `all`, `none` phrases (case and spacing
ignored) over the text fields (or `fields`), optionally only one filler's rows, `days` and `within_days` on a date
or datetime field; a dated row stops matching six hours after its date. A matching row notifies once per alert
(`collections_alerts_sent`), after a fill and on the poll; a row matching several alerts notifies once per run; more
than 10 in a run arrive quietly. Text: templates with `{field}`, served keys (`{start_text}`), `{alert}` and
`{filler}`. An alert is a channel of the custom app showing the collection (as reminders); a system collection's
alerts use its built-in app's channel. Rows are served with `alert_matched` and `alert_names`.

## Datetimes and what layouts get

- A **`datetime`** field type: `2026-11-06T19:30-07:00` (the source's wall clock with its offset when known, or a
  date alone), ordered by instant, served with `<name>_date` (`YYYY-MM-DD`, for `date_format`), `<name>_time`
  (`7:30 PM`) and `<name>_text` (`Fri Nov 6 · 7:30 PM`), written by the pod as the Watcher did (clients format only
  plain dates, decision `text-dates-and-counts`).
- The collection feed also fills `collection:<c>:fillers` (each filler's title, URL, health, `failing`,
  `alerts_text`) and `collection:<c>:alerted`. New data paths: `/v1/collections/<c>/fillers/<f>` (the filler, its
  rows, its alerts) and `/v1/collections/<c>/row?id=<id>` (one row's screen).
- MCP: `define_collection` takes `fillers` and `alerts`; `apply_change` ops `set_filler` (add, or change by id),
  `remove_filler` (its rows stay, unlocked; its alerts go), `add_alert`, `update_alert`, `remove_alert`; new tools
  `collections_preview` (try a recipe, nothing stored; without a map, sample items to write paths from),
  `collections_status`, `collections_refresh`, `collections_set_secret` (sealed with the server key, never shown
  back, sent only to the host it was stored for: [collection-secret-hosts](collection-secret-hosts.md)).

## The Events app

The Watcher's app keeps its id (`watcher`; renamed `events` by decision
[rename-apps-and-folders](rename-apps-and-folders.md)), so it stays where people pinned it, and its notification channel
(`matches`), so their choice holds; it is titled **Events**. Its feed is the system collection `events`
(`source_id`, `title`, `start`, `end`, `venue`, `description`, `url`, `tickets_url`, `seen`), which every pod gets
on start. A system collection's fields can't be renamed, removed or retyped (the app reads them); fields can be
added. Layout 0003 shows Matches, Sites (health, alerts), a site's events and an event, from collection queries.

- **The move:** when a pod starts, its live Watcher sites become fetch fillers (`ics`/`jsonld`/`auto` → `events`,
  `llm` → `llm`; zone, interval, `details`) and its live rules become alerts for their site (a venue becomes a phrase
  the event must contain), once (`watcher.imported`). Each moved filler's first fill records its matches without
  notifying, so nothing is announced twice. Seen marks don't move.
- The Watcher's pipeline, extractors, rules and MCP tools are gone (Claude uses the Collections tools; the MCP
  instructions give the recipe for a site). Its routes stay declared for layouts 0001 and 0002 and answer 410; its
  `watcher_*` tables stay, unread, for their migrations.

## Consequences

- No protocol change and no client build: new data and layout files only. Clients that can't draw layout 0003
  (no `open_link`) keep 0001, which now shows nothing.
- Schema: `collections` adapter migration 4 (a `filler` column on rows, `collections_fills`,
  `collections_alerts_sent`, `collections_secrets`), additive on staging's databases.
- The pod's outbound calls grow: fetch fillers (public hosts only) and model fills (budgeted).

Not done yet:

- A sandboxed transform for data recipes can't reshape (its own decision: code from a prompt).
- Headless fetching for pages that render with JavaScript; sites that block bots stay out of reach.
- Keeping a filled value's history (a price over time); a filler writing several collections.
- Seeing and editing fillers and alerts in an app (they are Claude's, as the Watcher's sites were).
- Dropping the `watcher_*` tables once every pod has moved (done by decision
  [rename-apps-and-folders](rename-apps-and-folders.md)).
