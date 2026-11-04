# A built-in watcher source polls websites for events and notifies matches; sites and rules are rows

Superseded by [collection-fillers](collection-fillers.md): the Watcher became the Events app on Collections.

Partly superseded by [watcher-rules-per-site](watcher-rules-per-site.md) (rules belong to one site; shared model
settings; the app no longer edits sites and rules).

Date: 2026-11-03. Related: D40, D52, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[collections](collections.md), [mcp-connector](mcp-connector.md), issue #58.

The user wants a notification when a website lists an event they care about (Improv Boulder lists a show with the
house team Budget Cuts; the lineup is only in each event's description on Meetup). No aggregated feed exists for
these, so it has to work across many sites, and adding a site must not need a server push. Considered: a data
source per site (rejected: code per site), storing targets and rules as a Collection as the issue suggested
(rejected for now: a built-in source can't depend on a collection a user declares and may change; the poller needs
typed columns for its own state such as `ETag`, hashes, backoff and health), and fetching from the clients
(rejected: polling must run while no client is open).

Decision: one built-in adapter, **watcher** (`crates/app-watcher`, source and app `watcher`). A website is a
*target* and a filter is a *rule*: both are rows in the watcher's own tables, edited from the app's routes or by
Claude over MCP, so new sites need no deploy. There is no stored code: extractors are a fixed set, rules are
declarative.

## Pipeline

- **Targets** (`watcher_targets`): URL, title, check interval (15 min to 7 days, default 6 h), extractor, a hint for
  the model, `details` (fetch each new event's own page once), an optional zone (an IANA name such as `America/Denver`, or a fixed UTC offset from targets saved before
  zones had names; the name wins), and the poll state (next
  check, `ETag`/`Last-Modified`, body hash, failures, empty runs, last error).
- **Fetch:** plain `GET` with conditional headers, 3 MiB cap, 30 s timeout. Only public addresses
  (`pod_core::net::public_builder`) unless the server sets `CONSTRUCT_WATCHER_PRIVATE_HOSTS=1`. A 304 or an
  unchanged body hash ends the check: no extraction, so no model call. Errors back off from the interval doubling
  to a day, at least `Retry-After`. Temporary pods get the pod setting `watcher.fetch = 0` (anyone can make one, so
  it must not make the server fetch URLs of their choosing).
- **Extractors** (`extract.rs`): `ics` (VEVENTs), `jsonld` (schema.org `Event`s, through `@graph`, `ItemList`s and
  nested events; most event sites, Meetup included, embed these), `llm` (the page's text to the Messages API with a
  JSON schema as structured output; the model's dates must parse or the event is dropped), and `auto` (the default:
  `ics` for a calendar body, else `jsonld`, else `llm` when the server has a model). The model is configured by the
  operator (`CONSTRUCT_WATCHER_ANTHROPIC_API_KEY`, `…_LLM_MODEL`, `…_LLM_URL`); without a key `llm` reports itself
  unavailable in the target's health.
- **Normalize:** every extractor yields the same event: title, start, end, venue, description, url. Identity is the
  source's own id when it has one (iCalendar `UID`, JSON-LD `@id`/`url`), else a hash of the normalized title, the
  date and the venue, so a re-scraped or reworded listing is the same event. Past events are dropped.
- **Dates** (`when.rs`): an event keeps the source's wall clock and its offset when the source gave one; an
  iCalendar `DTSTART;TZID=America/Denver` gets that zone's offset on that date (a zone name we can't resolve, such
  as a Windows one, is left to the target's zone). A time without an offset is placed in the target's zone, with
  daylight saving when it is named (`pod_core::tz`'s built-in database: the server image needs no zoneinfo).
  Display text (`Fri Nov 6 · 7:30 PM · The End`) is written by the pod from that wall clock (layouts can't format
  dates), shifted to the target's zone when both are known.
- **Rules** (`watcher_rules`): any-of, all-of and none-of phrases, a venue, days of the week, a window in days, and
  the targets they apply to. Matching ignores case over the title, venue and description. Rules run on new and
  changed events, and a new or changed rule runs on every upcoming event.
- **Notify:** `watcher_matches` records each (event, rule) match and when it was notified. After a check, and after
  a rule write, unnotified matches become one notification per event (`watcher:<event id>` dedupe key, channel
  `watcher.matches`, target `watcher://events/<id>`, which the layout's event screen opens); more than 10 from one
  run arrive quietly.
- **Scheduling:** checks run in the background, one run per pod: the adapter's notification poll (every minute
  while a device is registered, D52) starts one, and so does opening the app when a target is overdue (a pod with no
  devices still refreshes) and "Check now". A model call can take longer than the 20 s poll limit, so the poll only
  starts the run and the run notifies through `AppState::notify` itself.
- **Health** per target: pending, ok (`4 upcoming events · checked 5 min ago`), failing (with the error), empty
  (no events in the last 3 checks) or archived; the app marks failing and empty ones "Needs attention", and
  `watcher_status` reports them to Claude. Silent breakage is the main failure mode.

## Surfaces

- **App** `watcher` (layout `layouts/watcher/0001.json`, existing vocabulary only): upcoming matches; an event
  screen; "Sites & rules" (sites with health, add a site by URL, rules with remove, add a rule from keywords and
  exclusions); a site screen with its upcoming events, "Check now" and "Stop watching".
- **Source** (custom apps may use it too): feeds `matches` and `targets`, data paths for the event and site screens,
  actions to create and update sites and rules (archive with `archived: true`, no delete intent), refresh, and mark
  seen. No sign-in: the data is the pod's.
- **MCP:** `watcher_status`, `watcher_preview` (fetch and extract now, nothing stored: how Claude picks the cheapest
  extractor that works), `watcher_set_target`, `watcher_set_rule`, `watcher_refresh`.
- **Caps:** 50 live targets and 100 live rules per pod, 200 events per page, 10 event pages per check. Events are
  deleted 30 days after they are over.

No schema bump and no new vocabulary: the app is a layout file and adapter code. Clients list it like any app.

Not done yet:

- **Opening a link** ("Get tickets", "View on Meetup"): no intent opens an external URL; the event screen shows
  the URL as text. That is new client vocabulary (a named layout feature and a client build): issue #63.
- **Extractors** `rss`, `embedded-json` (e.g. Next.js page state) and `selectors`, and headless-browser fetching for
  pages that render with JavaScript. Matching against the raw page text as well as the extracted fields: issue #65.
- **Editing a rule or site in place** from the app: there is no prefilled text field; the app removes and re-adds,
  Claude edits with the MCP tools.
- **Day-of reminders** for a matched event.
- **The Improv Boulder target itself**: a user's data, added over MCP (`watcher_preview` first), not shipped:
  issue #64.
