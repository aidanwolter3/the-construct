# Local sets hold over stale loads, and a relayout reuses unchanged list items

Date: 2026-11-08. Related: [composable-actions](composable-actions.md),
[rust-clients-on-the-engine](rust-clients-on-the-engine.md), [rust-render-engine](rust-render-engine.md).

A staples page (five `core://List`s over one 167-row collection) marks a row "In groceries" with
`action.sequence [action.local.set in_groceries, action.mutation.create groceries row]`. On a phone the row
stayed "+" for about two seconds after the tap, then turned muted. Two causes:

- **The local set never reached the engine.** The session (`construct-render`) applied `action.local.set` to its
  own copy of the data, but not to the engine's (`client-core`). The create that followed made the engine send a
  snapshot, which carries all of its data, and the session took the snapshot's entities over its own: the row
  went back to `in_groceries` 0 before the first frame drew it. It turned muted only once the pod had the create,
  recomputed its rollup and the engine reloaded the page (the outbox POST, the pod's `data` event, a full reload
  of the layout's data). That is the two seconds.
- **Every engine snapshot laid the whole page out again.** Each snapshot (the create, each load starting and
  ending, the outbox answer) invalidated the session, which expanded, bound, filtered and measured every row of
  every list again: about 8 ms on a desktop per snapshot, several times that on a phone, four or five times per tap.

## Decision

1. **The session hands a local set to its loader too** (`Loader::local_set`; `PodEngine` dispatches it to the
   engine), so the engine's snapshots carry the new values. `local://` entities stay the session's alone.
2. **The engine holds a local set over loads that can't know of it.** It records each set's fields with the
   state's `seq`. Every load (workspace fetch or data path) is stamped when it starts, under the `net` lock: the
   `seq` then, and whether the outbox was empty. An answer from a load that started before the set, or while a
   write was still on its way to the pod (usually the write the set shows early), gets the set fields again. The
   answer of a load that started after the set with every write on the pod replaces them, and the set is
   forgotten: the pod's own value (a rollup it recomputed, or nothing if it refused) wins from then on. Sign-out
   forgets them all.
3. **The session takes only what changed from a loader's news.** Entities and queries equal to what it has are
   left alone; news with nothing new keeps the frame as it is (no layout at all). Several snapshots between two
   frames were already merged into one `Changes`; now an unchanged one costs a comparison.
4. **A relayout reuses the list items whose data didn't change** (`SceneBuilder::relayout`). The frame before
   is kept (`Entry::stale`) instead of dropped. If it was built against the same layout roots, form, route,
   viewport, insets, focus, day, carousel pages and theme (`Env`), each list item is looked up by its semantic path
   and entity URI: when its entity, every other entity and query it read while it was built (recorded through
   `Ctx`), and its parent's axis, colour and depth are the same, its elements and taffy nodes are taken over as
   they are, measured layout included; the rest is built. Nodes not taken over are removed from the tree. A
   List's `where` values are worked out once per list rather than once per row.

## Consequences

- The tap's first frame shows the muted row, and nothing undoes it while the create travels; the pod's answer
  arrives without a flicker.
- On the staples page (desktop, release) a relayout after a one-row change went from about 8 ms to about 2 ms;
  `Laid::reuse` counts built and reused items, and `tests/integration/reuse.rs` checks both the counts and that
  the frame equals one laid out from scratch.
- Rasterizing is unchanged: the GPU draws the visible quads and text each frame, which costs what is on screen,
  not the list's length.
- A change to the form, the layout or the viewport still builds everything (they may change any row).
- Needs a new client build: the change is in the engine and the renderer.
