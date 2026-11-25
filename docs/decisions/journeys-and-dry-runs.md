# Journeys are replayed on the owner's screens, with writes run as dry runs

Date: 2026-11-25. Related: [previews-read-like-a-device](previews-read-like-a-device.md),
[rust-simulator](rust-simulator.md), [edit-groups](edit-groups.md), [anticipate](anticipate.md).

To know whether a journey through the owner's screens works ("add a recipe, put its ingredients on Groceries"), an
agent drove previews tap by tap and judged each screen itself. Previews send no writes, so a journey that writes
could only be shown to have a button, not to do the right thing; and a model's judgement of a walk isn't
repeatable, so "it worked yesterday" meant nothing.

Considered: a scratch pod with a copy of the owner's data (copies data, drifts from the real pod); walks only up to
the first write (the flagship journey writes twice); a separate dry-run API per source (two write paths that drift).

Decision:

- `walk_journey {script, start?, group?}` replays a script of the simulator's commands (the parser moves from
  `construct-sim` into `construct_render::script`, shared by both; new: `expect-write <collection> {fields}` and
  targets `<target> in tag:<parent>`) on a preview session of the owner's real screens, deterministically: each step
  is `ok`, `broken` (a target isn't there, a write was refused or doesn't match), `data` (an assertion over live rows
  failed), `unverified` or `error`. The clock reads the pod's zone at the walk's start.
- **Dry runs.** A tap's write goes through the pod's own router, as a device's would, with a `DryRun` in the
  request's task (`pod_core::dryrun`). A source that says it can (`Source::dry_runs`; Collections for row writes and
  actions) runs its real write path in a transaction it always rolls back, first putting back the rows the walk's
  earlier writes would have written, and records every row the transaction touched (by name). Nothing after a
  commit runs, because there is none: fillers, model fields, reminders, alerts, events to devices, idempotency
  records, agent links. A path no source dry-runs isn't sent; its step is `unverified`. The rows join the walk's
  data, so later screens show them.
- `group` walks a staged edit group: its staged elements serve its names, the live store everything else.
- Every walk is stored 30 days (`journey_walks`) with the construct hash it walked, and when it failed the layout it
  failed on and the collection its `expect-write` names; `stage_edit_group {walk_id}` reads them, so the server,
  not the caller, says where a fix may change a binding (that screen) and what it may newly call.

Not done: dry runs for other sources (Pixelfed, Agents); refreshing a screen's view queries after a dry run (a
created row joins its collection's plain list); node-level failing pointers (the failing screen is the unit).
