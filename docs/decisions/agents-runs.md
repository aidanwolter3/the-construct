# The Agents source mirrors the runner's runs over CAP v2

Date: 2026-11-09. Related: [agents-data-source](agents-data-source.md) (superseded in part),
[notification-owner](notification-owner.md), [named-migrations](named-migrations.md), D50, D52.

Under CAP v1 the pod replayed the runner's delta events through its own reducer to guess each Claude session's
status, and so did the runner and the sandbox daemon, each with their own rules. They disagreed, and the bugs came
from there: Claude sessions never really end (Remote Control keeps them open), so a finished session later showed
"Needs you" when Claude's idle notification arrived; Stop changed nothing in the books; silence read as "still
working"; standby sessions were counted; Tasks rows stuck at `started`. The pod also followed the runner only while
it was open, so notifications came late. Considered: fixing the reducer's rules again (the third time; the runner
still couldn't tell the pod what it knew); polling snapshots only (no prompt notifications).

Decision:

- **The run** is the unit: one attempt at one piece of work on one sandbox, keyed by the runner's id. The runner
  (the Foreman) alone decides its state (`queued`, `starting`, `working`, `needs_you`, `ended` with an end reason)
  and the agent reports its outcome (`done`, `review`, `blocked`, `failed`). The pod talks only CAP v2.
- **The pod is a mirror.** CAP v2 sends whole records with a `version`; a snapshot replaces the stored records, an
  event's record replaces the stored one only when its version is higher. Nothing is derived: the session reducer,
  its `idle`/standby/`needs_you` rules and the done-task workaround (PR #218) are gone. Applying stays pure and
  returns effects, now worked out by comparing a record before and after.
- **Feeds keep their names** (`sessions` with `agents:status` and `agents:sessions`, `sandboxes`, `checks`,
  `schedules`) and a session entity is now a run (`agents://sessions/<run id>`), so layouts made from the blueprint
  keep binding. A run's log is read from the runner on demand (`GET /v2/runs/<id>/log`, kept 10 s); the stored event
  timeline is dropped (migration step `cap_v2`, which also empties the stored state so a v2 snapshot is taken).
- **Notifications by transition:** a run entering `needs_you` (once per entry), ending with an outcome or report,
  or ending lost, unstarted, timed out or expired; a check turning to warn. Remembered by run and transition across
  restarts.
- **Tasks follow their run through one table** (queued → queued; starting, working → running; needs_you →
  needs_you; outcome done → done; outcome review or ended reported, exited or stopped without one → review; outcome
  failed or blocked, or ended lost, start_failed, timed_out or expired → failed), with the outcome or reason appended
  to `diagnosis` once per run. A row the person set to done or todo keeps it, and its unfinished run gets `stop_run`
  once. Reopen (`/v1/agents/tasks/reopen`) starts a run that resumes an ended one (`resume_from`). The runner
  queues, so there are no 429 retries.
- **Always on:** a new `AppAdapter::keeps_open(store)` lets an adapter keep its pod open. `AppState::poll_due`
  answers `KEEP_OPEN_WAKE` (0, always due) for such a pod even without a device, so the host's scheduler opens it
  at server start and `resume` starts the follower. The Agents source keeps a pod open while it is signed in to a
  runner.

Limits: a pod first signed in before this change and opened by nobody since has no `next_wake_at` in `host.db` yet,
so it is opened on its first request or poll after the deploy and from then on at every start. A run that started and
ended entirely while the pod couldn't reach the runner notifies nothing (it arrives in a snapshot already ended).
Waking a sleeping sandbox before "Open in Claude" is the runner's job, not the pod's. Not testable here: real
sandboxes sleeping and waking, and the Claude app link after a resume.
