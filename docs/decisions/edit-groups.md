# Agents change screens as edit groups: staged, applied in one step, undone in one tap

Date: 2026-11-25. Related: [content-addressed-construct](content-addressed-construct.md),
[element-roles-and-reuse](element-roles-and-reuse.md), [mcp-grants](mcp-grants.md),
[journeys-and-dry-runs](journeys-and-dry-runs.md), [anticipate](anticipate.md).

An agent that changes the owner's screens unattended needs the change judged before it is live and undone in one
tap after. The pod keeps history per name (`restore_version`) and per collection edit (`undo_edit`), but nothing
tied the writes of one change together, nothing could be walked before it went live, and neither undo was reachable
from a device. "Change sets" already names `apply_change`'s layout-and-collection writes.

Considered: undo by restoring the old root hash alone (also undoes the owner's own edits since, and pins old core
layouts); a revert keyed on element hashes (every write re-seals what embeds it, up to home, so the hashes move with
any edit anywhere); groups that include collection definitions (another store's transaction; their changes stay
proposals).

Decision:

- An **edit group** is up to four writes to the pod's own layouts and `@me/` components, each by its authored
  name and the version it is based on (`base_seq`). `stage_edit_group` makes them inside a savepoint it rolls back:
  nothing is live; it keeps the staged names and elements (for `walk_journey {group}`) and answers a report: the
  expanded diff per layout, the followers it re-seals, and `additive`: only adds, plus a binding or target replaced
  inside the failing screen of the stored walk it names (`walk_id`), with added nodes that call only endpoints the
  screen already calls (or one another of the pod's screens calls, under the collection the walk's `expect-write`
  names), link to no new host, add no delete and show no text bound to an agent's rows. A re-sealed reference is the
  same reference.
- **Apply** (`apply_edit_group`, or the app's `POST /v1/edit-groups/apply`) makes the same writes through
  `construct_write::put` in one transaction, only when every authored name is still at `base_seq` and home is still
  the element it was when staged (else `stale_base` / `stale_stage`: stage and walk again); under a grant only when
  additive and unprotected. It records a checkpoint first.
- **Revert** puts each authored name back, its references sealed again to what they name now (so an embedded
  component edited since keeps its edit), only when its newest version that isn't only a re-seal is the one the
  group wrote; else nothing moves and the group is `conflict`. `revert_edit_group {since}` undoes every group
  applied since a time, newest first, keeping the owner's own edits.
- A **checkpoint** is home's element before an applied group. Checkpoints, groups and the elements they need are
  roots of the pod's garbage collection (`construct_pins`): history is trimmed to 500 entries a name, so a stored
  hash alone would point at nothing. Kept 90 days or the newest 30. `restore_checkpoint` puts every pod name back as
  it was then (from the root's closure, else the name's history at that time; `@core/` names stay current; home
  keeps the feeds it binds now as well as those it bound then; each name is put back on its own savepoint and
  failures are reported per name), after a dry run that lists what moves, the owner's own edits it undoes and what
  home would no longer reach. It records a checkpoint of now first, so it can be undone too, and marks the groups it
  supersedes reverted. Rows and collection definitions are not covered.
- On the device: Settings' manifest declares `GET /v1/edit-groups/screen` (`edit_groups://<id>`,
  `checkpoints://<id>`) and `POST /v1/edit-groups/apply|revert`, `/v1/checkpoints/restore`, for the owner's own
  sessions only (an MCP credential uses the tools); an adopted layout can't call them. Existing vocabulary: no
  client build.

Not done: groups over collection definitions; a restore that also rolls rows back.

Since then (the owner, the same day): restyles, trials and redo.

- **Safe, not only additive.** `stage_edit_group` also reports `safe`. A safe change may move, regroup or restyle
  anything, as long as everything the screen had stays: every endpoint it called, every layout or screen it opened,
  every list and every value it bound from the owner's data (theme colours aside). Its new nodes still do nothing
  new. A grant's `apply_edit_group` now requires `safe` instead of `additive`.
- **Trials.** `apply_edit_group {id, trial: true}` makes it live as a trial. Applying a trial again (the app's
  Keep, the same route) makes it the owner's.
- **Redo.** Applying an undone group redoes it: each authored name goes back to the element the group wrote, only
  while it is still where the undo left it (else `conflict`). Undo and redo move names between the same elements,
  which devices already hold, so swapping back and forth to compare is fast. The app's routes are the same
  (`/v1/edit-groups/apply` applies, keeps or redoes; `/revert` undoes); the screen entity gains `can_keep` and
  `can_redo`.
