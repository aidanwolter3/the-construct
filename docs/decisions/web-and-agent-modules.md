# Web access goes through one web module, model calls through one agent module

Date: 2026-11-09. Related: [collection-fillers](collection-fillers.md), [details-llm](details-llm.md),
[collection-computed-fields](collection-computed-fields.md), [jacks](jacks.md).

A shopping list's "where to buy" filler searched DuckDuckGo hourly per item (a `url_field` filler with `extract:
"llm"`). The results page changed on every fetch, so the model read it every time (23 model fills in a day);
DuckDuckGo then answered with a block page, the filler read it as zero items, reported `ok`, and wrote empty fields
over good recommendations. Each piece of policy that would have stopped it lived somewhere else or nowhere: host
spacing in `fill.rs`, backoff per filler (another filler still hit a blocking site), no block detection, two
Messages API clients (computed fields and fillers) with their own HTTP setup, prompts in `app-collections`, and
budgets for agent fillers only.

Considered: patching the filler (fixes one feature); a headless browser (heavier, and the problem is policy, not
rendering); keeping the API's server-side web tools for agents (they bypass the pod's host rules).

Decision: two modules, one direction:

- **Web module** (`web-extract`): talks to websites and owns the prompts for web content.
  - `fetch` reports blocks: a 403 or 429, or a short page with a challenge marker (captcha, "unusual traffic",
    DuckDuckGo's bot page), is `FetchError { blocked: true }`, never content.
  - `hosts`: one policy per host, pod-wide: 1 s spacing, 500 requests a day (20 for search pages scraped without
    an API), and a shared backoff after a block (1 h doubling to a day, at least `Retry-After`).
  - `search`: Brave (API key), SearXNG (`format=json`) or DuckDuckGo HTML, from the pod's `web_search` jack;
    answers cached a day.
  - `tasks`: reading a page into rows, picking one row's fields from search results, research as rows. Answers are
    checked against what was fetched: a URL not in the page or the results is dropped, stray JSON in a value is
    cut.
  - `tools`: the pod's own `web_search` and `web_fetch` for research loops.
- **Agent module** (`crates/agent`, `construct-agent`): runs every model call and knows nothing about pages. A
  task names a feature and a tier (cheap: computed fields, `CONSTRUCT_INFER_MODEL`; capable: everything else,
  `CONSTRUCT_FILL_MODEL`). It refuses with the `model` jack unplugged, counts calls toward daily budgets per
  feature and per pod (`agent_usage`; computed fields included now), retries 408/429/5xx/529 and timeouts with
  jittered backoff, runs the tool loop (`pause_turn`, client tools, a finish tool), and records the metrics.

Fillers change accordingly (Collections, no new layout features):

- A per-row filler (`url_field`, and the new `query_field`) never erases a row on a miss: a blocked or failed page
  writes nothing, and an empty answer writes nothing unless `on_empty: "clear"`.
- `refresh: "once" | "every"` (default `every` for `url_field`, `once` for `query_field`); `collections_row_fills`
  records each row's source and content key, so `once` fills a row when its URL or query is new or changed and
  `every` skips the model when a page's text didn't change.
- `query_field` (with `extract: "llm"` and `fields`): each row's text is searched with the pod's `web_search` jack
  and a model picks the row's fields from the results. People may add rows.
- Agent fillers use the pod's own search and fetch when `web_search` is plugged, else the API's server-side web
  tools as before.

Not yet: a persistent page cache (search answers are cached in memory); prices from product pages (search
results rarely carry them).
