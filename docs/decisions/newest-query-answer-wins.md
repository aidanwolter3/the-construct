# A query key several cached answers list is the newest answer's

Date: 2026-11-16. Related: D34, [scoped-data-events](scoped-data-events.md),
[content-addressed-construct](content-addressed-construct.md).

A pod's Events layout stopped showing new matches on a phone after the pod moved to the construct: twenty rows a
filler added were on the pod, the per-site screen showed them, a fresh preview showed them, and the layout's own
load came back with them, but its list kept an older copy, through restarts. Query keys are one namespace on the
device: every layout that binds a feed carries that feed's query keys (`collection:events:alerted`), and the
engine overlaid the cached answers in path order, so the last `layout:<id>` path won however old its answer was.
An answer cached before the switch (the built-in `layout:events`, which nothing on the construct opens again)
sorted after every `layout:c-…` and won every time; a scoped `event: data` reloads only what is on screen, the
rest marked stale for this run only, and nothing ever dropped a layout's answer except a 404.

Decision:

- **The newest answer wins.** Each answer (the workspace's and each data path's) gets a load order when it arrives;
  `effective_queries` overlays them oldest first, so a key's value is the newest answer's. Answers restored from
  the cache at launch are older than anything fetched since (order 0; among themselves, path order as before).
- **Restored answers are stale.** At launch every restored data path is marked stale: it is shown at once (offline
  too) and loaded again once it is on screen.
- **Layouts the construct no longer reaches are forgotten.** After each construct sync the engine drops the cached
  answer (and the cached layout) of every `layout:<id>` the root doesn't reach and that wasn't opened by name,
  unless it is on screen (it is marked gone there, as before).

Considered: resolving each layout's queries from its own `layout:<id>` answer first (cleaner, but the renderer and
screens read one query map, and a screen's `data` path is not tied to a layout); pruning only the layouts a sync
reports gone (misses answers cached before the switch, which the first sync on a device never saw).

Needs a new desktop and Android build.
