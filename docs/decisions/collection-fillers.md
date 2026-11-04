# Collections can be filled by the pod from a fetch recipe or an agent, not only by people

Date: 2026-11-04. Status: proposed, nothing built yet. Related: D40, D52,
[custom-apps-and-data-sources](custom-apps-and-data-sources.md), [collections](collections.md),
[collection-computed-fields](collection-computed-fields.md), [collection-reminders](collection-reminders.md),
[watcher](watcher.md), [watcher-rules-per-site](watcher-rules-per-site.md),
[upstream-credentials-sealed-at-rest](upstream-credentials-sealed-at-rest.md).

A user wants short-lived custom apps over live data from the web: up-to-date election results, or the prices of a
few things they are shopping for. No built-in source has that data, and writing one is adapter code the developer
ships. Collections can hold it, but only people and Claude write rows (the app's actions, `add_row`, `edit_rows`),
so nothing keeps it current while nobody is talking to Claude.

The pod already has one piece of this. The Watcher fetches pages on a schedule from the pod (so it runs with no
client open), skips unchanged pages (`ETag`, body hash), extracts with fixed, cheap extractors (`ics`, `jsonld`)
and calls a model only as a fallback. But it writes into its own tables, and its output has one shape (events).

How data reaches a layout today:

- **Live adapters** (Pixelfed): each feed request goes upstream with the signed-in person's own login, and the
  answer is mapped to entities. The pod stores no posts; writes (likes, comments) go upstream through the
  adapter's actions. The data belongs to one person and is fresh on every open.
- **Stored sources**: Collections (generic, declared over MCP), Recipes (`recipes_*`) and the Watcher
  (`watcher_*`), each in its own tables.

Layouts don't see the difference: every source offers feeds, data paths and actions answering the same entities
(decision `custom-apps-and-data-sources`). The missing piece is one level down, in how stored data is *filled*.

Considered:

- **Claude writes a data source** (code that fetches and maps). Rejected again, for the reasons in #45 and
  decision `collections`: code, upstream calls and secrets from a prompt.
- **Stored JavaScript** that fetches and returns rows. Rejected as the first step for the same reason. A narrow
  form stays open as a later escape hatch (below).
- **A model call on every refresh** ("summarize the latest results"). Rejected as the default: cost grows with
  refresh rate × collections × pods, and answers drift between runs. A model is used when a recipe is *written*
  and as an explicit, budgeted kind of filler, never by default.
- **Fetching on the clients.** Rejected as for the Watcher: refreshes must run while no client is open, and
  browsers' CORS and per-device work would duplicate it per device.
- **Moving Pixelfed into a filled collection.** Rejected: its data is per person, a timeline is a moving window
  rather than data to keep, and writes would need write-through to Pixelfed and back. Live adapters stay
  adapters.

Decision: a collection may name a **filler**, the thing that writes its rows. People and Claude remain the default
(no filler). A filler is data, not code: Claude declares it over MCP, the pod runs it. Two kinds to start, plus the
Watcher moved onto the same mechanism later.

## Fillers

A filler is stored with the collection definition (`collections_*`, a new migration of the `collections` adapter)
as `{kind, …, schedule, mode}`:

- **`schedule`**: an interval (15 min to 7 days, as Watcher targets) and/or `on_open: <minutes>`: when an app
  showing the collection is opened and the last fill is older than that, a fill starts in the background. The
  refreshed rows reach devices through the usual data event. `on_open` is what makes results night work without
  polling every few minutes all day.
- **`mode`**: `replace` (the rows are the latest answer: a results table) or `upsert` by a key field (rows keep
  their identity and any fields people set: a shopping list whose `price` is refreshed but whose `notes` and
  `bought` are the person's). Fields the filler writes are marked filled; the app's actions and `edit_rows` may
  not change them, so a refresh never overwrites a person's edit silently.
- **Health**, as the Watcher's: pending, ok, failing (with the error), empty; reported by `describe_collection`
  and a new `collections_status` MCP tool, so Claude says so when a filler breaks. Errors back off as the
  Watcher's do, at least `Retry-After`.

### `fetch`: a declarative recipe

`{kind: "fetch", url, params?, headers?, secret?, extract, map}`:

- **Fetch** is the Watcher's: `GET`, conditional headers, a body hash that ends an unchanged fill with no work,
  size and time caps, public addresses only (`pod_core::net::public_builder`). `secret` names a sealed credential
  (decision `upstream-credentials-sealed-at-rest`) sent as one header, for APIs that need a key; it is never
  shown to layouts, MCP or the recipe.
- **Extract**, a fixed set: `json`, `jsonld` (any schema.org type, not only `Event`: `Product`/`Offer` give
  price and availability on most shop pages), `rss`, `ics`, `embedded-json` (page state such as Next.js'
  `__NEXT_DATA__`) and `selectors` (CSS selectors for plain HTML). The Watcher's extractors move into a shared
  crate so both use one implementation.
- **Map**: a path per field into each extracted item (a small JSONPath subset: `results[*].candidate`,
  `offers.price`), coerced to the field's type as computed fields are. Computed fields (`expr`, `lookup`,
  `model`) then run on the filled rows as on any other, so a recipe needs no transforms of its own.
- **Several URLs**: `urls` (a list, or a field of another collection: "fetch each product page in `wishlist`")
  for the shopping case, capped per fill.

Claude writes a recipe with a new `collections_preview` MCP tool (fetch, extract and map now, store nothing,
show the rows), as `watcher_preview` does, and picks the cheapest extractor that gives the fields. **The model
writes the scraper; it does not run it.** Each later fill is an HTTP request.

### `agent`: a model with a prompt and a budget

`{kind: "agent", prompt, tools: ["web_search", "fetch"], budget}` for data that has no stable page to read:
"the five best-reviewed 27-inch monitors under $400 and where each is cheapest". A fill runs a model with
those tools (the Messages API, behind a trait with a fake for tests, as `FieldModel`), and its answer must fit
the collection's fields as structured output; rows that don't parse are dropped and counted in health.

- **Budget** is required: a maximum number of fills a day and of tool calls per fill. The default schedule is
  manual (Claude or a "Refresh" action starts it); an interval is allowed but counted against the budget.
- The operator configures the model as for `model` computes; without one, agent fillers report themselves
  unavailable.
- The usual pattern is both kinds: an agent finds the items once (or weekly) and a fetch recipe keeps their
  prices current, upserting by URL.

### Not code, for now

If recipes prove too weak (joining two responses, reshaping odd data), the next step is a **pure transform**:
`transform(body) -> rows`, JavaScript run in an embedded sandbox (QuickJS or WASM) with no network, secrets,
clock or randomness, and CPU and memory limits; the pod still does every fetch, only to the recipe's hosts. That
reverses "no code from a prompt" and needs its own decision; this one doesn't take it.

## The Watcher

Later, the Watcher becomes a filled collection plus rules: its targets are `fetch` fillers with the event
extractors into a pod-owned `events` collection (`upsert` by event id), and its rules are evaluated over rows, as
collection reminders are. That gives custom apps its events for free and leaves one fetch path to harden. It is a
separate migration with its own decision, since its tables, health and notifications are deployed.

## Limits and safety

- **Temporary pods**: fillers off (a pod setting `collections.fill = 0` at creation, next to `watcher.fetch` and
  `collections.model`): anyone can make one, so it must not make the server fetch URLs or spend the model key.
- **Caps** per pod (temporary pods excluded): 20 fillers, the Watcher's fetch caps per fill, the existing row and
  size caps of decision `collections` (a fill that would exceed them stops and reports it), and an agent budget
  ceiling per pod set by the operator.
- **Upstream politeness**: a minimum interval per host across a pod's fillers, a fixed `User-Agent` naming The
  Construct, and `Retry-After` honoured. Sites that render with JavaScript or block bots stay out of reach until
  headless fetching exists (a "Not yet" of decision `watcher`).

## Consequences

- No protocol change and no client build: filled collections are ordinary collection feeds; "Refresh" is an
  action of the collections source.
- New in the `collections` adapter: a migration (filler definition, fill state), the scheduler (the adapter's
  poll, as the Watcher's), `collections_preview` and `collections_status`, and `define_collection` /
  `apply_change` ops to set and remove a filler.
- The Watcher's extractors and fetch code move to a shared crate.

Not yet decided:

- the JSONPath subset, and whether `map` needs anything an `expr` compute can't do afterwards;
- whether an agent filler may write to several collections in one fill;
- whether filled rows keep history (the previous value of `price`) or only the latest;
- how a person sees a filler's health in the app (a generic part, or only through Claude).
