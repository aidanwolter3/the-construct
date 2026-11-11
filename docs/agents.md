# Agents: runs from an agent runner

The **Agents** data source (`agents`, `crates/app-agents`, decisions
[`agents-data-source`](decisions/agents-data-source.md) and [`agents-runs`](decisions/agents-runs.md)) shows and
controls Claude Code work that an external **agent runner** (the Foreman) does on cloud sandboxes. The pod is the
dashboard and the "start work" button; the work itself is watched and steered in the Claude app.

The unit is the **run**: one attempt at one piece of work on one sandbox. The runner alone decides a run's state; the
pod keeps a mirror of what it sends and decides nothing.

## The runner's protocol (CAP v2)

The pod connects out to the runner; the runner never calls the pod. Every request carries the pod's bearer token for
the runner. The runner sends whole records, each with a `version` that goes up on every change; the pod keeps the
higher one.

- `GET /v2/snapshot`: `{v: 2, cursor, runs[], sprites[], schedules[], checks[]}`. `runs` holds every run that hasn't
  ended and ended ones from the last 7 days.
- `GET /v2/stream?since=<cursor>`: server-sent events with `id: <seq>`, resumable with `Last-Event-ID`. Each is
  `{v: 2, seq, ts, type, record}`, `type` one of `run.updated`, `sprite.updated`, `schedule.updated`,
  `check.updated`, `record` the whole record.
- `GET /v2/runs/<id>/log`: `{entries: [{ts, kind, text}]}`, the run's history, oldest first.
- `POST /v2/commands`, a fixed set, each `{type, ...}`: `start_run {workspace, task_id, title, prompt,
  resume_from?}` (202 `{run}`; 200 with the task's run that hasn't ended), `stop_run {run_id}`, `wake_sprite
  {workspace, minutes}`, `run_schedule`, `pause_schedule`, `resume_schedule {name}`. Errors are `{error}` with 400,
  401, 404, 409 or 502. The runner queues work itself: there is no "busy" answer.

A run: `{id, version, task_id, title, origin, workspace, sprite, state, needs_you {reason, text}, end_reason,
outcome {status, summary, pr_url}, error, asleep, current_tool, summary, pr_url, claude_url, session_id,
resumed_from, cost_usd, input_tokens, output_tokens, queued_at, started_at, updated_at, ended_at}` (times in
milliseconds). States: `queued`, `starting`, `working`, `needs_you` (reason `permission`, `question` or `stopped`)
and `ended` (`end_reason` `reported`, `exited`, `stopped`, `timed_out`, `lost`, `start_failed` or `expired`). The
agent reports its `outcome` (`done`, `review`, `blocked`, `failed`); it survives the run ending. A run can be
`asleep` (its sandbox sleeps; frozen, not ended). A sprite is `{name, workspace, version, state, agentd_version,
last_heartbeat_at}`, state `unprovisioned`, `asleep`, `waking`, `awake`, `unreachable` or `updating`. A schedule
carries `last_run {run_id, due_at, state, end_reason, outcome}`. A scheduled run's `task_id` is
`schedule/<name>/<minute>`.

## In the pod

The runner is the pod's `agent_runner` jack (decision [`jacks`](decisions/jacks.md)): `list_jacks` shows whether it
is plugged and to which URL; `unplug_jack agent_runner` signs out, like `POST /v1/agents/sign-out`.

- **Sign-in:** the Agents layout's form posts the runner's URL and token to `POST /v1/agents/sign-in`. The pod asks
  the runner for a snapshot with it first (a refused token is 400 `runner_refused_token`), then stores the token
  sealed (`CONSTRUCT_SECRETS_KEY`) in `agents_runner`. The URL must be `https://` on a public host name
  (`CONSTRUCT_AGENTS_PRIVATE_HOSTS=1` allows any address, for local development). Requests follow no redirects. An
  answer over 4 MiB, or a stream message over 256 KiB, is refused, so a runner can't fill the shared server's memory.
  `POST /v1/agents/sign-out` forgets the runner and everything it sent. One runner per pod.
- **Following, always:** a pod signed in to a runner is opened when the server starts and stays open
  (`AppAdapter::keeps_open`), so its follower runs whether or not anyone uses the pod. The follower takes a snapshot
  (at sign-in, and when the last good answer is over 10 minutes old), then follows the stream from the cursor,
  reconnecting with backoff (1 s doubling to 60 s). A snapshot replaces the stored records; an event's record
  replaces the stored one only when its version is higher, so replays and late copies change nothing. While the
  runner can't be reached the feeds serve the last state, with `agents:status.stale` true and a "Can't reach …" line.
- **Feeds:** `sessions` (`agents:status` and `agents:sessions`: the runs that need you, then starting and working,
  then queued, then ended ones, each newest first; entity `agents://sessions/<run id>`), `sandboxes`, `checks`,
  `schedules`; and `/v1/agents/sessions/timeline?id=<run id>` (`agents:timeline:<id>`), the run's log read from the
  runner when the screen opens (kept 10 s). `agents:status.summary_text` reads e.g. "1 working · 1 needs you · 2
  queued" or "Nothing running". A run's `status_text` is one of Queued, Starting, Working, Needs you, Done, Needs
  review, Blocked, Failed, Stopped, Lost, Didn't start, Timed out, Expired, or Reviewed for a run that needed review
  and was marked reviewed on this pod (`needs_review` turns false, `reviewed` true). The entities and their fields are
  in the source's manifest (`describe_vocabulary`, `list_data_sources`).
- **Actions:** `/v1/agents/sessions/stop {run_id}` (`session_id` is read the same way, for older layouts),
  `/v1/agents/sandboxes/wake`, `/v1/agents/tasks/start`, `/v1/agents/tasks/reopen`, `/v1/agents/schedules/run`,
  `/pause` and `/resume`; and `/v1/agents/sessions/review {run_id}`, which tells the runner nothing. They need a
  valid pod session; runner refusals come back as 400, 404 or 409 `runner_refused`, and 502 otherwise (never 401,
  which would sign the device out).
- **Mark reviewed** (decision [`agents-reviewed`](decisions/agents-reviewed.md)): `/v1/agents/sessions/review`
  acknowledges an ended run that needs review, on this pod only (409 `not_needing_review` for any other run). The
  mark is kept in `agents_reviewed` until the run leaves the runner's snapshot (or the pod signs out). A linked task
  row in `review` whose current run it is goes to `done`.
- **Untrusted text:** everything the runner sends is plain display text, cleaned of control characters and capped.
  The links kept are `pr_url`, only as `https://github.com/<owner>/<repo>/pull/<n>`, and `claude_url`, only as
  `https://claude.ai/code/` and a path of plain segments; without a `claude_url`, "Open in Claude" is built from the
  run's `session_id` when it is a claude.ai one (`session_…`). Run ids are letters, digits, `_` and `-`.

## The Agents blueprint

`agents` (`crates/app-agents/layouts/agents/`): the sign-in form until the pod is signed in; then a summary card
("1 working · 1 needs you · 2 queued"), chips for Runs, Schedules, Sandboxes and Checks, a "Start a run" form, and
Sign out (asks first). A run's card shows its status, Asleep, what it needs from you (e.g. "Has a question" and the
question), its tool or outcome. A run's screen adds **Open in Claude**, **Pull request**, **Stop** (on active runs,
asks first), **Mark reviewed** on an ended run that needs review, a **Follow up** form (Reopen) on an ended task run,
and its log. Schedules have **Run now**, **Pause**
(asks first) and **Resume**; sandboxes that aren't awake have **Wake**. It uses existing vocabulary only.

A layout made from it (`{"type": "core://Ref", "app": "agents", "part": "layout"}`) is linked from home; home can
show `${agents://runner/status#summary_text}` as its one-line summary.

## Notifications

In the layout that shows the source (decision [`notification-owner`](decisions/notification-owner.md)), once the pod
is signed in:

| Channel | Importance | Sends |
|---|---|---|
| `needs_you` "Needs you" | high | a run enters `needs_you` (the question, the permission, or Claude's last message), once per entry |
| `sessions` "Finished runs" | default | a run ends with an outcome or a report (its summary and PR link) |
| `warnings` "Runner warnings" | high | a run ends lost, didn't start, timed out or expired; a security check turns to warn |

Each is worked out by comparing a run's stored record with the new one, and remembered by run and transition (the
last 200, in the stored state), so a restart or a repeated record notifies nothing twice. A run the pod first sees in
a snapshot already ended is history and notifies nothing.

## The Tasks wiring

Opt-in per collection, with the MCP tool `agents_link_tasks {collection, workspace}` (`collection: null` unlinks).
The collection needs a `title` and a choice `status` with the option `queued`; linking adds the options `todo`,
`queued`, `running`, `needs_you`, `review`, `done` and `failed` (none is removed, so an old `started` stays) and
optional text fields `run_id`, `session_id`, `workspace` and `diagnosis`, through Collections' `apply_change`. Then:

- a row that becomes `queued` sends `start_run` once, with the row's id as `task_id`, its title, its title and notes
  as the prompt (the runner adds its own preamble), and its `workspace` (else the linked default); the answer's run
  id goes in `run_id`. A refusal (400, 404, 409) sets `failed` with the reason in `diagnosis`; an unreachable runner
  leaves it queued for the next pass (every 30 s and on each change). A row queued again after its run ended (a
  retry) gets a new run.
- the row's status then follows its current run (the one `run_id` names, else the newest run whose `task_id` is the
  row's id) through one table:

  | current run | row status |
  |---|---|
  | queued | queued |
  | starting, working | running |
  | needs_you | needs_you |
  | ended, outcome done | done |
  | ended, outcome review; or reported, exited or stopped without an outcome | review |
  | outcome failed or blocked; or lost, start_failed, timed_out or expired | failed |

  When the run ends, its summary and PR link (or why it failed) are appended to `diagnosis`, once.
- a row you set to `done` (or whose `done` box you tick) or `todo` keeps that status: its run only adds to
  `diagnosis`, and a run that hasn't ended gets `stop_run`, once.
- **Reopen:** `POST /v1/agents/tasks/reopen {id, message}` (or `{run_id, message}` from a run's screen) needs the
  row's last run to have ended. It sends `start_run` with `resume_from` the last run and `message` as the prompt; the
  new run continues the conversation in its worktree, and the row goes back to `queued` with the new `run_id`.
- **Mark reviewed** on the row's current run sets a row in `review` to `done`.

## Setting it up in a pod

1. Make a layout from the blueprint and link it from home (ask Claude: "add the Agents layout to home").
2. Open it and sign in with the runner's URL and token.
3. Optionally link a tasks collection: ask Claude to run `agents_link_tasks` with the collection and the default
   workspace.
