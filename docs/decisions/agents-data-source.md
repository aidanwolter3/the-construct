# An `agents` data source follows an external agent runner over CAP v1

Date: 2026-11-07. Related: [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[server-monitoring-alerts](server-monitoring-alerts.md), [notification-owner](notification-owner.md),
[upstream-credentials-sealed-at-rest](upstream-credentials-sealed-at-rest.md), [collections](collections.md). Issue #165.

An agent runner, a separate service, starts Claude Code sessions on cloud sandboxes and reports what they do over
the Construct Agent Protocol (CAP v1: a snapshot, an event stream, a fixed set of commands). The owner wants the pod
to be the dashboard and the "start work" button, with notifications when a session needs them, and a Tasks
collection whose queued rows start sessions. Considered: the runner calling the pod (it would need a way in to every
pod, and a pod would have to trust it); polling the snapshot alone (no timelines, and finished sessions between polls
would be missed); a sign-in provider like Pixelfed's (those are per device session and start an OAuth flow; the
runner's token is the pod's, used in the background).

Decision:

- **A new adapter, `app-agents`,** a data source named `agents` with an `agents` blueprint. The pod connects out;
  the runner never calls it. Feeds `sessions` (with `agents:status`), `sandboxes`, `checks` and `schedules`; a
  timeline data path; actions for stop, wake, start, and run, pause and resume a schedule. Built on existing
  vocabulary only: no client build.
- **The pod's sign-in to the runner** is a form in the blueprint posting `{url, token}` to `/v1/agents/sign-in`,
  checked against the runner, stored sealed in the adapter's table (one runner per pod). It is not a
  `SignInProvider`: those are per device session, and the follower needs the token with no session at hand.
  `https://` public hosts only, no redirects, never logged (the `Runner` debug output is the host).
- **A follower task per open pod** takes a snapshot when the stored state is missing or over 10 minutes old, then
  follows the stream from the stored cursor (`since` and `Last-Event-ID`), reconnecting with backoff. The state is
  kept, so the feeds answer while the runner is away and say so (`stale`). It holds the pod only while working on an
  answer, so a closed pod ends it.
- **Applying is pure:** a snapshot or an event updates the state and returns the effects it owes (notifications,
  task-row changes), so the rules are unit-tested without a runner.
- **Notifications** follow [server-monitoring-alerts](server-monitoring-alerts.md): three channels (`needs_you`,
  `sessions`, `warnings`) in the layout that shows the source, sent by the follower as events arrive. A security
  check notifies when it turns to warn and again only after it was ok; a session waiting for permission once per
  wait. Schedules: missed and timed-out runs warn.
- **Counts mean running now:** `agents:status` counts `starting` and `working` sessions as working and waiting ones
  as needing you; `idle` is neither (Remote Control's standby sessions and finished runs' open sessions are idle),
  so an idle runner says "Nothing running". `agents:sessions` lists idle sessions after working ones.
- **Schedules** are shown and can be run now, paused and resumed. Their next run's day and time read in the
  schedule's own `timezone` (the pod's when it has none or an unknown one). Their definitions (prompt, cron,
  workspace) stay in the runner's reviewed configuration: CAP has no command to create or change one, so a leaked
  pod token can't plant a recurring prompt. Pause asks first.
- **Tasks wiring** is opt-in per collection with the MCP tool `agents_link_tasks`, which adds the status options
  and fields it needs through Collections' `apply_change`. Rows are read with Collections' tables and written with
  `app_collections::update_row` (a layout's update, checks and computed fields included), the one addition to
  `app-collections`. A sent `start_task` is remembered (`agents_launches`) so a queued row is sent once; a 429 leaves
  it queued for the next look (every 30 s and on each Collections change).
- **Runner text is untrusted:** plain text, cleaned and capped; the only link is a GitHub pull request URL that
  passes a strict check, and "Open in Claude" is built from the session id.

Limits: the follower runs while the pod is open (any request, or the 60-second notification poll for a pod with a
registered device), so a pod nobody uses misses notifications until it opens; the stream's replay then catches up.
The token field in the form is plain text (there is no password field in the vocabulary yet). One runner per pod.
