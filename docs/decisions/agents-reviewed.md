# A run is marked reviewed on the pod, not on the runner

Date: 2026-11-11. Related: [agents-runs](agents-runs.md), [named-migrations](named-migrations.md).

A run that ends with the outcome `review` (or ends by itself without a report) reads "Needs review" until it leaves
the runner's snapshot, 7 days after it ended. Once a person has dealt with it (merged its pull requests, say), the card
keeps asking. A task run's row can be set to done, but a scheduled run has no row, and the runner's record never
changes after a run ends. Considered: a CAP command for the runner to record the review (the runner has none, and
the run is over, so its record is final); keeping the mark in the mirrored state (the follower saves its own copy
of that state whole, so a mark written beside it would be overwritten).

Decision:

- **Mark reviewed** (`POST /v1/agents/sessions/review {run_id}`) is a pod-side acknowledgement. It applies only to
  an ended run that needs review (409 `not_needing_review` otherwise, 404 for a run the pod doesn't list), and
  repeating it is harmless. The runner is told nothing and its record stays as it is: the pod still decides nothing
  about a run's state, it only shows its own acknowledgement beside it.
- **Stored in its own table**, `agents_reviewed (run_id, at)` (migration step `reviewed`), read into the state when
  the feeds are built. Saving the mirror drops the marks of runs no longer in it, so a mark lasts as long as its run
  is listed; signing out drops them all.
- **Shown** as `status_text` "Reviewed" (green), `needs_review` false and `reviewed` true on the run's entity, and on
  a schedule whose last run it is. The blueprint's run screen has a "Mark reviewed" button while `needs_review`.
- **A linked task row** in `review` whose current run is the one marked is set to `done`. A row in any other status
  is left alone.

Limits: the button is on the run's screen only, not its card (the card is one tappable). Marking can't be undone
from the layout; the mark goes with the run.
