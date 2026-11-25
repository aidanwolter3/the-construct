# Devices report how the owner uses their screens to their own pod, names only

Date: 2026-11-25. Related: [device-versions](device-versions.md), [anticipate](anticipate.md),
[mcp-access-levels](mcp-access-levels.md).

Walks prove a journey can work; nothing said whether the owner uses it, where they give up, which paths they repeat
that no journey covers, or whether a change made a journey easier. The pod saw writes, not taps or screens.

Considered: deriving use from row edits (only writes; most journeys are reading); a per-device switch in Settings (a
device-owned entity needs a new device command and new vocabulary); recording in the renderer's session directly
(previews and walks drive the same session).

Decision:

- **What:** `protocol::usage::UsageEvent`: `screen` (layout opened, from where), `tap` (the node's identity: its
  authored tag, else its tree path with list indices taken out; the intent's kind; the layout it opens, or the
  endpoint's kind and collection; whether it navigated, sent, changed or did nothing), `back` (how long the screen
  was open), `result` (a write's outcome, from the outbox). Names from the layout and enums only: never bound text,
  values, route or param values, typed text, row ids or screenshots. Labels and templates are author text and may
  hold literals.
- **Where it's recorded:** `Loader::usage`, default nothing. Only the engine's loader (`PodEngine`) records, so
  previews, walks and the simulator never do. The engine keeps events in its store (`usage_events`, at most 5,000,
  oldest dropped) and sends them in batches of 500 to `POST /v1/usage` every 15 minutes and when the app comes back
  to the foreground; a pod without the route (404, 405: an older server) turns it off for that engine; a refusal
  drops the batch; a network error keeps it.
- **On the pod:** the `usage` jack, a core switch off by default and locked on temporary pods, is the only switch.
  Unplugged, the route answers 204 and keeps nothing. Only the owner's own device sessions may post (not an MCP
  credential, a public visitor or the pod's own requests). Raw events are kept 14 days (a forgotten device's go
  with it), daily totals 180.
- **Read:** `usage_report` (a layout's opens, where from, taps and dead taps per node, backtracks; a journey's
  starts, completions, rate and median time, by its first screen and its collection) and `usage_paths` (frequent
  sequences of 2 to 4 screens, at least 3 times), at `edit` and in Anticipate's Build grant only. A judgement of a
  change needs at least 20 starts on each side.
- **Client build:** new desktop, Android and web builds report; older ones report nothing and keep working.

Not done: the web client's own loader reports nothing (it doesn't run the engine); a per-device opt-out.
