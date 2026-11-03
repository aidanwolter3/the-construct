# Each watched site has its own rules; the Watcher app only shows sites, rules and events

Date: 2026-11-03. Related: [watcher](watcher.md), [open-link-intent](open-link-intent.md), D52, issues #58, #63.

The first Watcher had sites and rules side by side, a rule applying to every site unless it named some. Using it, the
user found that unclear: which rules apply to which site, and why an event did or didn't notify. Considered: keep
global rules and show on each site the rules that apply (rejected: still two lists to reconcile), many-to-many
links between rules and sites (rejected: same problem, more UI).

Decision: a rule belongs to one site.

- **Data:** no new column: a rule's `targets` scope holds its one site. `watcher_set_rule` and
  `POST /v1/watcher/rules/create` take `target` (the site id); a rule for a site the pod doesn't watch is refused
  (422). Watching for the same thing on two sites is two rules; Claude writes them, so the duplication costs nothing.
  Adapter migration 2 gives every existing rule one site: a rule for every site (or several) becomes one per live
  site. A create without a site from an older layout's form gets the pod's only site, or stays for every site (shown
  as "All sites") until it is edited.
- **Served:** a site's entity has `rule_count` and `rules_text` ("Watching for Budget Cuts", "No rules yet"); its
  screen data adds `watcher:target:<id>:rules`. A rule's entity has `target_id` and `site`. An event's has
  `match_text` ("Improv Boulder · Budget Cuts").
- **Event text:** descriptions are served as plain text (Markdown headings, bold and links reduced to text; a first
  line that only repeats the site's name dropped; Meetup's feeds start with the group's name) and titles with single
  spaces. The tickets link is pulled out as `tickets_url` (a Markdown link whose text mentions tickets, else the first
  link to a known ticket seller). Stored text is unchanged, so matching still reads everything the site wrote.
- **App** (`layouts/watcher/0002.json`, needs feature `open_link`; older clients keep 0001): two tabs, Matches (every
  upcoming match, newest unseen marked "New") and Sites (each with its rules and health). A site's screen shows its
  rules and **every** upcoming event, matched ones on a tinted card with a "Match" chip. The event screen has a tinted
  header, the match and "Mark as seen", "Get tickets" and "Event page" buttons, and the plain-text description.
- **No editing in the app:** adding and changing sites and rules is Claude's job over MCP (the user's choice); the
  app keeps "Check now". The routes stay, for older layouts and custom apps.
- **Model settings are shared:** the `llm` extractor uses the pod's one model key and endpoint, the ones Collections'
  computed fields use (`CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_URL`), with the model from
  `CONSTRUCT_WATCHER_LLM_MODEL`, else `CONSTRUCT_INFER_MODEL`. This replaces `CONSTRUCT_WATCHER_ANTHROPIC_API_KEY`
  and `CONSTRUCT_WATCHER_LLM_URL` of decision `watcher`, which never shipped with a value.

Not done yet: editing a rule's site (a rule moves by being re-added); rules shared by several sites, if duplication
ever becomes a burden.
