# Agents: Claude Code sessions from an agent runner

The **Agents** data source (`agents`, `crates/app-agents`, decision
[`agents-data-source`](decisions/agents-data-source.md)) shows and controls Claude Code sessions that an external
**agent runner** starts on cloud sandboxes. The pod is the dashboard and the "start work" button; the sessions
themselves are watched and steered in the Claude app.

## The runner's protocol (CAP v1)

The pod connects out to the runner; the runner never calls the pod. Every request carries the pod's bearer token for
the runner.

- `GET /v1/snapshot`: `{v, cursor, sprites[], sessions[], checks[], schedules[]}`, the state now. A schedule is
  `{name, cron, timezone, workspace, model, max_runtime_minutes, enabled, paused, next_run, last_run}`, `last_run`
  `null` or `{run_key, due_at, status, error, session_id, summary, cost_usd}`; times are milliseconds.
- `GET /v1/stream?since=<cursor>`: server-sent events with `id: <seq>`, resumable with `Last-Event-ID`. Each event
  is `{v: 1, seq, ts, type, sprite?, session_id?, task_id?, status?, data}`. Types: `task.launched`,
  `sprite.state` (awake/asleep), `session.started`, `session.status`, `tool.used`, `permission.requested`,
  `session.finished` `{summary, pr_url, cost_usd, input_tokens, output_tokens, duration_s, reason}`,
  `session.failed` `{error}`, `session.link` `{claude_url, workspace?}` (the session's page in the Claude app),
  `security.check` `{id, title, detail}` (ok/warn), `schedule.state` `{schedule}`
  (paused/active), `schedule.missed` `{schedule, error}`, `schedule.timeout` `{schedule, max_runtime_minutes,
  stopped}`.
- `POST /v1/commands`, a fixed set: `wake {workspace, minutes}`, `start_task {workspace, task_id, title, prompt}`
  (429 at the concurrency limit), `stop_session {session_id}`, `run_schedule {name}`, `pause_schedule {name}`,
  `resume_schedule {name}`. Errors are `{error}` with 400, 401, 404, 429 or 502.

A snapshot's session may carry `workspace` and `claude_url`; `session.started` may carry `workspace` in its data.

Session statuses: `starting`, `working`, `waiting_permission`, `waiting_input`, `idle`, `finished`, `failed`,
`stale`. Only `starting` and `working` count as working in `agents:status` (`working_count`, `summary_text`), and
`waiting_permission` and `waiting_input` as needing you; `idle` counts as neither, since each sandbox's Remote
Control keeps an idle standby session open and a finished run's session stays open idle. A scheduled run's `task_id` is `schedule/<name>/<local minute or manual-…>`.

## In the pod

- **Sign-in:** the Agents layout's form posts the runner's URL and token to `POST /v1/agents/sign-in`. The pod asks
  the runner for a snapshot with it first (a refused token is 400 `runner_refused_token`), then stores the token
  sealed (`CONSTRUCT_SECRETS_KEY`) in `agents_runner`. The URL must be `https://` on a public host name
  (`CONSTRUCT_AGENTS_PRIVATE_HOSTS=1` allows any address, for local development). Requests follow no redirects.
  `POST /v1/agents/sign-out` forgets the runner and everything it sent. One runner per pod.
- **Following:** a background task per open pod takes a snapshot (at sign-in, and when the last good answer is over
  10 minutes old), then follows the stream from the cursor, reconnecting with backoff (1 s doubling to 60 s). Each
  event updates the stored state and the session's timeline (`agents_events`, the last 200 per session), and the
  pod's clients reload. Sandboxes and schedules come only with a snapshot (a redeployed runner can change a
  schedule's time without an event), so the pod reads them again from a snapshot on every reconnect and every 10
  minutes while following; sessions, checks and the cursor stay as the stream left them. While the runner can't be
  reached the feeds serve the last state, with `agents:status.stale` true and a "Can't reach …" line.
- **Feeds:** `sessions` (`agents:status` and `agents:sessions`: the ones that need you, then working, idle and ended
  ones, each newest first), `sandboxes`, `checks`, `schedules`; and `/v1/agents/sessions/timeline?id=<id>` (`agents:timeline:<id>`). The
  entities and their fields are in the source's manifest (`describe_vocabulary`, `list_data_sources`).
- **Actions:** `/v1/agents/sessions/stop`, `/v1/agents/sandboxes/wake`, `/v1/agents/tasks/start`,
  `/v1/agents/schedules/run`, `/pause` and `/resume`. They need a valid pod session; runner refusals come back as
  429 `runner_busy`, 400 or 404 `runner_refused`, and 502 otherwise (never 401, which would sign the device out).
- **Untrusted text:** everything the runner sends is plain display text, cleaned of control characters and capped.
  The links kept are `pr_url`, only as `https://github.com/<owner>/<repo>/pull/<n>`, and `claude_url`, only as
  `https://claude.ai/code/` and a path of plain segments; without a `claude_url`, "Open in Claude" is built from the
  session id when it is a claude.ai one (`session_…`).

## The Agents blueprint

`agents` (`crates/app-agents/layouts/agents/`): the sign-in form until the pod is signed in; then a summary card
("2 working · 1 needs you"), chips for Sessions, Schedules, Sandboxes and Checks, a "Start a session" form, and Sign
out (asks first). A session's screen shows its status, tool, summary, error, **Open in Claude** and **Pull request**
links, **Stop** (asks first) and its timeline. Schedules have **Run now**, **Pause** (asks first) and **Resume**;
sandboxes that are asleep have **Wake**. It uses existing vocabulary only.

A layout made from it (`{"type": "core://Ref", "app": "agents", "part": "layout"}`) is linked from home; home can
show `${agents://runner/status#summary_text}` as its one-line summary.

## Notifications

In the layout that shows the source (decision [`notification-owner`](decisions/notification-owner.md)), once the pod
is signed in:

| Channel | Importance | Sends |
|---|---|---|
| `needs_you` "Needs you" | high | a session starts waiting for a permission |
| `sessions` "Finished sessions" | default | a session finishes (summary and PR link) or fails |
| `warnings` "Runner warnings" | high | a security check turns to warn (again only after it was ok), a schedule is missed or runs past its limit (once per run: a repeated event for the same run is ignored) |

A run is the event's `session_id` (else `task_id`) for a timeout and its `task_id` for a missed run; the pod
remembers the last 200 notified runs in its stored state. The follower sends them as events arrive, while the pod is
open; the source's 60-second poll keeps the follower running for pods with a registered device.

## The Tasks wiring

Opt-in per collection, with the MCP tool `agents_link_tasks {collection, workspace}` (`collection: null` unlinks).
The collection needs a `title` and a choice `status` with the option `queued`; linking adds the options `todo`,
`started`, `review` and `failed` and optional text fields `session_id`, `workspace` and `diagnosis`, through
Collections' `apply_change`. Then:

- a row that becomes `queued` sends `start_task` with the row's id as `task_id`, its title and notes as the prompt,
  and its `workspace` (else the linked default). At the runner's limit (429) it stays queued and is sent again
  later; a refusal (400, 404) sets `failed` with the reason in `diagnosis`.
- `session.started` sets `status = started` and `session_id`;
- `session.finished` appends the summary and PR link to `diagnosis` and sets `status = review`;
- `session.failed` sets `status = todo` and appends the error.
- a row marked done (`status = done`, or a `done` box ticked) whose session is still open sends `stop_session` once,
  and the feeds show that session as finished whatever the runner last said (a session whose sandbox is gone can't be
  stopped, and would otherwise wait for you forever). A done row keeps its status whatever its session sends after.

## Setting it up in a pod

1. Make a layout from the blueprint and link it from home (ask Claude: "add the Agents layout to home").
2. Open it and sign in with the runner's URL and token.
3. Optionally link a tasks collection: ask Claude to run `agents_link_tasks` with the collection and the default
   workspace.
