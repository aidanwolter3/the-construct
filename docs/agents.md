# Agents: runs from an agent runner

The **Agents** data source (`agents`, `crates/app-agents`, decisions
[`agents-data-source`](decisions/agents-data-source.md), [`agents-runs`](decisions/agents-runs.md) and
[`agents-cap-v3`](decisions/agents-cap-v3.md)) shows and controls Claude Code work that an external **agent runner**
(the Foreman) does on cloud sandboxes (Sprites). The pod is the dashboard and the "start work" button; the work
itself is watched and steered in the Claude app.

Each fact has one owner (the Foreman's decision `runs-leases-and-verified-outcomes`):

- **The pod** owns the **work items**: Tasks rows and **schedules**, where each came from, and the notifications.
- **The runner** owns the **runs** (one attempt at one piece of work on one sandbox), the sandboxes, and each run's
  **outcome**, which it verifies against GitHub. The pod keeps a mirror of what it sends and decides nothing about
  them.

## The runner's protocol (CAP v3)

The contract is the Foreman's `docs/cap-v3.md`. The pod connects out to the runner; the runner never calls the pod.
Every request carries the pod's bearer token for the runner. The runner sends whole records, each with a `version`
that goes up on every change; the pod keeps the higher one, so a replay or a late copy changes nothing.

- `GET /v3/snapshot`: `{v: 3, cursor, oldest_seq, runs[], sprites[], checks[]}`. `runs` holds every run that hasn't
  ended, ended ones from the last 7 days, and ended ones whose outcome is still `pending` or `needs_review`.
- `GET /v3/events?since=<seq>&limit=<n>`: `{events, next, more}`, the event log as pages (kept 30 days). `410
  {error: "cursor_too_old"}` when `since` is below the snapshot's `oldest_seq` (`since = oldest_seq` is a page).
- `GET /v3/stream?since=<seq>`: the same events as server-sent events with `id: <seq>`, resumable with
  `Last-Event-ID`. Each is `{v: 3, seq, ts, type, record}`, `type` one of `run.updated`, `sprite.updated`,
  `check.updated`, and `outcome.updated` (`reminder: true` beside the whole run: the 24 h review reminder, kept by
  version like `run.updated`). A type the pod doesn't know is skipped.
- `GET /v3/runs/<id>/log`: `{entries: [{ts, kind, text}]}`, the run's history, oldest first.
- `POST /v3/commands`, a fixed set, each `{type, ...}`: `start_run {request_id, workspace, work_item_id,
  occurrence?, origin, title, prompt, model?, max_runtime_min?, resume_from?}` (202 `{run}`; the same `request_id`,
  or a work item's run that hasn't ended, answers that run with 200), `stop_run {run_id}`, `wake_sprite {workspace,
  minutes ≤ 60}`, `repair_sprite {workspace}`, `mark_reviewed {run_id}` (409 unless its outcome is
  `needs_review`). Errors are `{error}` with 400, 401, 404, 409 or 502.

A run's **execution** is its `state`: `queued` (with a `queue_reason`: `waiting_for_slot`, `sprite_waking`,
`sprite_degraded`, `sprite_down`), `launching`, `working` (`state_detail` e.g. "waiting on 1 background task"),
`needs_you` (`needs_you {reason, text}`), `parked` (needed you for 30 min; its sandbox may sleep), `stalled` (its
sandbox is suspended or degraded), `interrupted` (its process stopped; it is launched again, `attempt` of
`max_attempts`), `stopping`, `closing`, and `ended` with an `end_reason` (`reported`, `stopped`, `timed_out`, `lost`,
`start_failed`, `expired`). Its **outcome** is separate and can change after it ends: `none`, `pending` (`stage`
`ci`, `review` or `deploy`), `done`, `failed` (`reason` `ci`, `closed_unmerged`, `deploy`, `blocked`, `failed`) or
`needs_review` (no report and no pull request), with its `pr_url`, `deploy_url`, `claim_mismatch` and `reviewed`.
What the agent reported (`foreman report`) is its `claim`. A sandbox is `unprovisioned`, `provisioning`, `asleep`,
`waking`, `awake`, `degraded`, `suspended`, `down` or `updating`, with a `reason`, its slots, its preflight and its
disk failures in the last 7 days. A launch refused for memory alone leaves the run `queued` (`waiting_for_slot`)
and the sandbox `awake`, with `memory` in its preflight failures; the pod doesn't show that as trouble.

## In the pod

The runner is the pod's `agent_runner` jack (decision [`jacks`](decisions/jacks.md)): `list_jacks` shows whether it
is plugged and to which URL; `unplug_jack agent_runner` signs out, like `POST /v1/agents/sign-out`.

- **Sign-in:** the Agents layout's form posts the runner's URL and token to `POST /v1/agents/sign-in`. The pod asks
  the runner for a snapshot with it first (a refused token is 400 `runner_refused_token`), then stores the token
  sealed (`CONSTRUCT_SECRETS_KEY`) in `agents_runner`, and the snapshot with its cursor. The URL must be `https://` on
  a public host name (`CONSTRUCT_AGENTS_PRIVATE_HOSTS=1` allows any address, for local development). Requests follow
  no redirects. An answer over 4 MiB, or a stream message over 256 KiB, is refused, so a runner can't fill the shared
  server's memory. `POST /v1/agents/sign-out` forgets the runner and everything it sent. One runner per pod.
- **Following, always:** a pod signed in to a runner is opened when the server starts and stays open
  (`AppAdapter::keeps_open`), so its follower runs whether or not anyone uses the pod. The cursor is stored with the
  mirror (`agents_state.cursor`). After any gap (a restart, a dropped stream, a runner that was away) the follower
  reads the event log from the cursor in pages, saving each page and its cursor, then follows the stream from there,
  reconnecting with backoff (1 s doubling to 60 s). It takes a snapshot only when it has no cursor (the first
  sign-in) or the runner answers 410. While the runner can't be reached the feeds serve the last state, with
  `agents:status.stale` true and a "Can't reach …" line.
- **Feeds:** `sessions` (`agents:status` and `agents:sessions`: runs that need you or are parked, then running ones,
  then queued, then ended ones waiting on their outcome, then the rest, each newest first; entity
  `agents://sessions/<run id>`), `sandboxes` (down first, then degraded or suspended, then awake), `checks`,
  `schedules` (the pod's own, below); and `/v1/agents/sessions/timeline?id=<run id>` (`agents:timeline:<id>`), the
  run's log read from the runner when the screen opens (kept 10 s). `agents:status.summary_text` reads e.g. "1
  working · 1 needs you · 2 queued · 1 waiting" or "Nothing running". A run's `status_text` is one of Queued,
  Starting, Working, Needs you, Parked, Stalled, Restarting, Stopping, Finishing, Waiting on CI, Waiting on review,
  Waiting on deploy, Done, Reviewed, Needs review, CI failed, Closed unmerged, Deploy failed, Blocked, Failed,
  Stopped, Lost, Didn't start, Timed out, Expired or Ended; `queue_text`, `explain_text`, `stage_text`,
  `attempt_text` and `claim_mismatch_text` say more. A sandbox has `reason`, `slots_text`, `preflight_text` and
  `disk_text`. The entities and their fields are in the source's manifest (`describe_vocabulary`,
  `list_data_sources`).
- **Actions:** `/v1/agents/sessions/stop {run_id}` (`session_id` is read the same way, for older layouts),
  `/v1/agents/sandboxes/wake {workspace, minutes?}`, `/v1/agents/sandboxes/repair {workspace}` (asks first),
  `/v1/agents/tasks/start {workspace, title, prompt, task_id?, model?, max_runtime_min?}`, `/v1/agents/tasks/reopen`,
  `/v1/agents/sessions/review {run_id}`, and the schedule actions below. They need a valid pod session; runner
  refusals come back as 400, 404 or 409 `runner_refused`, and 502 otherwise (never 401, which would sign the device
  out).
- **Mark reviewed** sends `mark_reviewed`: the runner, which owns the outcome, sets it `done` with `reviewed`, and
  the run reads Reviewed. The answer's record goes into the mirror at once. A run that doesn't need review is 409
  `not_needing_review`. (Before CAP v3 this was a pod-only mark in `agents_reviewed`; that table is gone.)
- **Untrusted text:** everything the runner sends is plain display text, cleaned of control characters and capped.
  The links kept are `pr_url` (the outcome's and the claim's), only as `https://github.com/<owner>/<repo>/pull/<n>`;
  `deploy_url`, only as `https://github.com/<owner>/<repo>/actions/runs/<n>` (optionally `/job/<n>` or
  `/attempts/<n>`); and `claude_url`, only as `https://claude.ai/code/` and a path of plain segments; without a
  `claude_url`, "Open in Claude" is built from the run's `session_id` when it is a claude.ai one (`session_…`). Run
  ids are letters, digits, `_` and `-`.

## Schedules

Schedules are the pod's work items (`agents_schedules`): a name, a cron of five fields (minute hour day-of-month
month day-of-week; numbers, ranges, steps, lists, and names such as `mon` or `jan`), an IANA time zone (the pod's
when none is given), a workspace, a model (empty: the sandbox's default), a max runtime (1 to 480 minutes, default
240), on or paused, and the prompt. The runner keeps none.

- **The timer** runs in the follower on every pass (connected or not) and in the source's notification poll. For
  each due occurrence it records the occurrence first (`agents_occurrences`), then sends `start_run` with `origin:
  schedule`, `work_item_id` the schedule's id, `occurrence` the time it is for as RFC 3339 with its offset
  (`2026-11-15T03:52:00-07:00`, so the runner's 2 h expiry starts there) and `request_id` derived from both
  (`<schedule id>_20261115T035200m0700`), so a resend after a crash or while the runner was away is the same request
  and never a second run.
- **Missed:** an occurrence not sent within 2 hours of its time (the pod or the runner was down) is skipped as
  `missed`, and a "Missed: <name>" warning says so. A refused one (400, 404, 409) is `refused` and warns too.
- **Daylight saving:** a local time the clocks skip runs at the first instant after the gap; one they repeat runs
  once, at the first of the two.
- **Pause** drops what waits to be sent; **Resume**, or a new cron or zone, starts from now, so nothing paused counts
  as missed. **Run now** sends an occurrence named by this minute (`request_id` ending `_now`): a second tap in the
  same minute answers the same run.
- Actions: `/v1/agents/schedules/create {name, cron, timezone?, workspace, prompt, model?, max_runtime_min?,
  enabled?}`, `/update {name, new_name?, …}` (blank fields keep their value; `model: "default"` clears it),
  `/delete {name}`, `/run`, `/pause` and `/resume {name}`. The MCP tool `agents_schedules {op: list | create |
  update | delete | pause | resume | run, …}` does the same for Claude.
- The `schedules` feed (`agents://schedules/<name>`) has the definition, `next_run_text` (e.g. "in 3 h · Sun 03:52",
  in the schedule's zone), and the last run's state and outcome (`last_status_text` e.g. Waiting on CI, Done,
  Missed), from its newest occurrence and the run that answered it.

Schedule definitions were files in the runner's repository before CAP v3; they move to the pod at cutover (see the
decision). A schedule's prompt runs unattended with its workspace's access, so only a signed-in pod session or the
pod's owner through MCP can write one.

## The Agents blueprint

`agents` (`crates/app-agents/layouts/agents/`): the sign-in form until the pod is signed in; then a summary card
("1 working · 1 needs you · 2 queued · 1 waiting"), chips for Runs, Schedules, Sandboxes and Checks, a "Start a run"
form, and Sign out (asks first). A run's card shows its status (Parked, Stalled, Waiting on CI…), why it waits
(the queue reason, the background task), what it needs from you, its tool or what it did. A run's screen adds what
Parked, Stalled or Restarting means, the attempt, a warning when the report disagrees with GitHub, **Open in
Claude**, **Pull request**, **Deploy run**, **Stop** (on active runs, asks first), **Mark reviewed** on a run that
needs review, a **Follow up** form (Reopen) on an ended task run, and its log. Schedules have **Run now**, **Pause**
(asks first), **Resume** and **Edit** (a screen with the schedule, an editor whose fields show the current values
and keep them when left blank, and **Delete schedule**, which asks first), and a **New schedule** form. Sandboxes
show why one is degraded, suspended or down, its preflight, slots and a repeat-disk warning; an asleep or suspended
one has **Wake**, a down one **Repair** (asks first). It uses existing vocabulary only.

A layout made from it (`{"type": "core://Ref", "app": "agents", "part": "layout"}`) is linked from home; home can
show `${agents://runner/status#summary_text}` as its one-line summary.

## Notifications

In the layout that shows the source (decision [`notification-owner`](decisions/notification-owner.md)), once the pod
is signed in:

| Channel | Importance | Sends |
|---|---|---|
| `needs_you` "Needs you" | high | a run starts waiting for you (`needs_you` or `parked`, once per wait: parking is the same wait); a review still waiting after 24 h (inbox only, no push) |
| `sessions` "Finished runs" | default | a run's outcome turns `done` (what it did and its pull request); a run ends needing your review |
| `warnings` "Runner warnings" | high | a run ends lost, didn't start, timed out or expired, or its outcome fails; a sandbox goes down or fails its disk a second time in 7 days; a schedule's run is missed or refused; a security check turns to warn |

They come from the **events**, replayed ones included: a run that started and ended while the pod couldn't reach
the runner still notifies when the log is replayed. A wait that was answered within the same batch of events (a
replayed gap) isn't news and notifies nothing. Each is remembered by run (or sandbox, or check) and transition (the
last 400, in the stored state), so a restart, a replay or a repeated record notifies nothing twice. A run (or a
sandbox) the pod first sees in a snapshot already ended (or down) is history and notifies nothing.

## The Tasks wiring

Opt-in per collection, with the MCP tool `agents_link_tasks {collection, workspace, model?, max_runtime_min?}`
(`collection: null` unlinks). The collection needs a `title` and a choice `status` with the option `queued`; linking
adds the options `todo`, `queued`, `running`, `needs_you`, `waiting`, `review`, `done` and `failed` (none is removed)
and optional text fields `run_id`, `session_id`, `workspace`, `stage` and `diagnosis`, through Collections'
`apply_change`. A collection linked before an option or field existed gets it on the next pass. Then:

- **Only rows a person queued start runs** (issue #243). Collections records who last wrote each field
  (`collections_field_writers`: a filler or a model; a person's or Claude's write clears it). A queued row a filler
  made, or whose `status`, `title`, `notes`, `description`, `details`, `workspace`, `model` or `max_runtime_min` a
  filler or a model wrote last, isn't sent: it goes to `failed` with "Not started: the filler … wrote … last; set it
  yourself to start it". Setting those fields yourself (or through Claude) makes it startable.
- a row that becomes `queued` sends `start_run` once: `work_item_id` the row's id, `origin: task`, its title, its
  title and notes as the prompt (the runner adds its own preamble), its `workspace` (else the linked default), its
  `model` and `max_runtime_min` fields when it has them (else the link's), and a `request_id` stored before sending
  (`agents_launches`), so a send the runner didn't answer is sent again as the same request. The answer's run id goes
  in `run_id`. A refusal (400, 404, 409) sets `failed` with the reason in `diagnosis`; an unreachable runner leaves it
  queued for the next pass (every 30 s and on each change). A row queued again after its run ended (a retry) gets a
  new run.
- the row's status then follows its current run (the one `run_id` names, else the newest run whose `work_item_id` is
  the row's id) through one table:

  | current run | row status |
  |---|---|
  | queued, launching | queued |
  | working, stalled, interrupted, stopping, closing | running |
  | needs_you, parked | needs_you |
  | ended, outcome pending (or not worked out yet) | waiting, with `stage` ci, review or deploy |
  | outcome done | done |
  | outcome failed; or ended lost, start_failed, timed_out or expired with no outcome | failed |
  | outcome needs_review | review |

  When the outcome settles, what the run did and its PR link (or why it failed) are appended to `diagnosis`, once.
  The runner retries a launch whose process died itself (`interrupted`), so the pod no longer retries crashes.
- a row you set to `done` (or whose `done` box you tick) or `todo` keeps that status: its run only adds to
  `diagnosis`, and a run that hasn't ended gets `stop_run`, once.
- **Reopen:** `POST /v1/agents/tasks/reopen {id, message}` (or `{run_id, message}` from a run's screen) needs the
  row's last run to have ended. It sends `start_run` with `resume_from` the last run and `message` as the prompt; the
  new run continues the conversation in its worktree, and the row goes back to `queued` with the new `run_id`.
- **Mark reviewed** on the row's current run turns its outcome `done`, and the row follows it to `done`.

## Setting it up in a pod

1. Make a layout from the blueprint and link it from home (ask Claude: "add the Agents layout to home").
2. Open it and sign in with the runner's URL and token.
3. Optionally link a tasks collection: ask Claude to run `agents_link_tasks` with the collection and the default
   workspace.
4. Add schedules from the layout's Schedules chip, or ask Claude (`agents_schedules`).
