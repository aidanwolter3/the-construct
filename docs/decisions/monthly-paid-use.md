# Paid use by the month: a pod's and the server's, kept apart

Date: 2026-11-26. Related: [server-monitoring](server-monitoring.md), [jacks](jacks.md),
[web-and-agent-modules](web-and-agent-modules.md), [determinate-progress](determinate-progress.md).

The developer asked for a monthly usage bar on Server health: how many web searches the pod made this month against
Brave's free credit (1,000 searches, $5 a month), and then for every paid service the Construct drives, with what
is per pod told apart from what is per server. The monitoring counters couldn't answer it: they live in memory,
restart from zero and only reach back a day.

Decision: **every request to a service billed per use is counted when it is made, by UTC calendar month, in two
ledgers** (`pod_core::paid_use`, a `paid_use` table of month, kind and count):

- **The pod's** (its own store): its web search provider's requests (`search:<provider>`, made with the pod's own
  key: `web_extract::search` counts a real request, never a cache hit) and the model requests it made
  (`model:calls`, tokens by direction, the model's own web searches and fetches), counted in `Agent::post`.
- **The server's** (`host.db`, through `set_server_ledger`): every pod's model requests, since all of them spend
  the server's one key. Searches stay out of it: a pod's provider and key are its own.

The `monitor` source's `paid_use` feed serves `monitor://paid_use/pod` in any pod and `monitor://paid_use/server`
only where monitoring is on (the whole server's use, like the `overview` feed). Server health shows them under
"Usage this month" in a Server part and a This pod part, each card tagged Server or Pod, with Brave as a
determinate bar against its free searches and a dollar estimate (its price is fixed per search).

Why counts and not dollars for the model: its price depends on the model and changes; the tokens by direction are
what a bill is made of, and the developer can read them against the provider's console. Why not Fly.io: Fly has no
API for an app's bill, so the screen says the hosting bill is on Fly's dashboard rather than guess. The taste
service and Foreman report no usage to the Construct, so they aren't shown.

Consequences: counting starts at the deploy, so a month already under way shows "counted since" its first day
counted. Thirteen months are kept. No new client build is needed: a client without `progress_value` gets the
previous layout version.
