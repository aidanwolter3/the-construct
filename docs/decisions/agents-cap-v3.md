# The pod follows the runner over CAP v3 and owns schedules as work items

Date: 2026-11-15. Related: [agents-runs](agents-runs.md) (superseded in part), [agents-reviewed](agents-reviewed.md)
(superseded), [agents-data-source](agents-data-source.md), [named-migrations](named-migrations.md),
[notification-owner](notification-owner.md); the Foreman's decision `runs-leases-and-verified-outcomes` and its
contract `docs/cap-v3.md` (aidanwolter3/foreman). Issue #243.

A week of real use showed the pod's side of the runner integration breaking in three ways. Notifications were worked
out by comparing snapshots, and the follower took a snapshot whenever its last answer was over 10 minutes old, so a
run that started and ended while the pod couldn't reach the runner notified nothing. A run's state mixed what the
agent said with what happened: a run "lost" had shipped its PR, one "done" had hit a storage error, and "Mark
reviewed" had to be a pod-only mark because the runner's record of an ended run never changed. Schedules were files
in the runner's repository, changeable only by a merged PR, and their runs weren't work items the pod could follow,
reopen or review. And any row of a linked Tasks collection that turned `queued` started a run, including rows a
filler wrote from web content (#243).

The Foreman's decision splits ownership: the runner owns runs, sandboxes, leases and each run's outcome (verified
against GitHub); the pod owns work items (tasks and schedules), where each came from, and the notifications. Both
sides change together, with no fallback: CAP v2 is removed on both.

Considered: keeping a v2 fallback (two protocols to test, for a cutover done once while nothing runs); keeping
schedules in the runner and adding commands to edit them (the runner would still own work it can't follow up);
for #243, refusing to link a collection with fillers that can write the fields a run is made from (simplest, but
rules out a useful combination, and a `url_field` filler fills people's rows too), or a `proposed` state a person
must confirm (new vocabulary for the layouts).

Decision:

- **CAP v3 client** (`crates/source-agents/src/cap.rs`): snapshot, `/v3/events` pages, `/v3/stream`, the fixed command
  set `start_run {request_id, work_item_id, occurrence?, origin, model?, max_runtime_min?, resume_from?}`,
  `stop_run`, `wake_sprite` (at most 60 min), `repair_sprite`, `mark_reviewed`. The safety limits stay: size caps, no
  redirects, https on public hosts, untrusted text cleaned, and links allow-listed (`outcome.pr_url` and the claim's
  as pull requests, `outcome.deploy_url` as a GitHub Actions run).
- **The contract's clarifications** (the "Changes during implementation" of `cap-v3.md`): `since = oldest_seq`
  is a page and only `since < oldest_seq` is 410; `outcome.updated` carries the whole run and `reminder: true` and
  is kept by version like `run.updated`; event types the pod doesn't know are skipped; a sandbox whose only
  preflight failure is `memory` stays awake and isn't shown as trouble.
- **A stored cursor, replayed after any gap.** The cursor has its own column (`agents_state.cursor`). The follower
  takes a snapshot only without a cursor or on 410, else reads the log from the cursor in pages, saving each page with
  its cursor, then follows the stream. Records are kept by version from every source (a snapshot no longer
  overwrites a newer stored record).
- **Notifications from events.** Each event's record is compared with the stored one; a run new to the pod that
  already ended is news when it comes as an event (it ran while the pod was away) and history when it comes in a
  snapshot. A wait answered within the same batch is dropped. Kept per run and transition (the last 400 keys). The
  channels stay: `needs_you` for a run that starts waiting (parking is the same wait) and the 24 h review reminder
  (inbox only, no push); `sessions` when the outcome turns `done` or the run needs review; `warnings` for `lost`,
  `start_failed`, `timed_out`, `expired`, a failed outcome, a sandbox going `down` or failing its disk twice in 7
  days, and a schedule's missed or refused occurrence.
- **Schedules are the pod's** (`agents_schedules`, `agents_occurrences`). A timer in the follower (every pass,
  connected or not) and in the notification poll records each due occurrence, then sends `start_run` with `origin:
  schedule`, the schedule's id as `work_item_id`, the time it is for as `occurrence` (RFC 3339
  with its offset, so the runner can place it and start its 2 h expiry there) and a `request_id` derived from
  both, so a resend never duplicates. Not sent within 2 h: `missed`, with a warning. Daylight saving: a
  skipped local time runs just after the gap, a repeated one once. The cron is read by a small module on the zone
  database the pod already ships (`pod_core::tz`, jiff): no new dependency. Pause drops what waits; resume and a new
  time start from now. Create, edit, delete, run now, pause and resume are pod actions and the MCP tool
  `agents_schedules`; the blueprint has an editor.
- **Tasks follow the new table** (queued, launching → queued; working, stalled, interrupted, stopping, closing →
  running; needs_you, parked → needs_you; ended with the outcome pending → `waiting` with its `stage`; done → done;
  failed, or ended lost, start_failed, timed_out or expired → failed; needs_review → review). Linking adds `waiting`
  and `stage`, and an already-linked collection gets them on the next pass. `start_run` carries a `request_id` stored
  before sending, the row's (or the link's) model and max runtime; Reopen uses `resume_from`. The pod's own retry of
  a crash at launch is gone: the runner relaunches a dead process itself.
- **Origin (#243).** Collections records who last wrote each field (`collections_field_writers`, migration step
  `field_writers`): a filler (`filler:<id>`) or a model's computed answer (`model`); a person's or Claude's write
  through the app, MCP or a layout action clears it. A queued row a filler made, or whose status, title, notes,
  workspace, model or runtime a filler or a model wrote last, is failed with the reason instead of started.
- **Mark reviewed goes to the runner** (`mark_reviewed`); the pod-only mark is gone ([agents-reviewed](agents-reviewed.md)
  is superseded).
- **Migration step `cap_v3`** (a Rust step): keeps only the notices already sent from the v2 state (so nothing is
  notified twice), sets the cursor to 0 (the first v3 answer is a snapshot), clears the launches, gives them a
  `request_id`, drops `agents_reviewed`, and adds the schedule tables.
- **A new blueprint version** (`0005`) shows Parked, Stalled, Waiting on CI / review / deploy, the queue reason,
  troubled sandboxes with their reason and Repair (asks first), and the schedule editor, with existing vocabulary
  only: no client build.

Cutover (both sides at once, while no runs are active): deploy the Foreman's v3 and this together; the pod's
follower takes a v3 snapshot on its own. The schedules the runner kept as files are created on the pod with
`agents_schedules` (their prompts are not in this repository). Linked collections gain `waiting` and `stage` by
themselves.

Limits: a schedule changed while one of its occurrences waits to be sent keeps that occurrence as it was taken up.
An undo (`undo_edit`) of a filler's write doesn't change who is recorded as its writer. Not testable
here: a real runner, real sandboxes sleeping and waking, and GitHub settling outcomes; the fake runner in the tests
speaks the contract.
