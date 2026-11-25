# A pod may have 100 collections

Date: 2026-11-25. Related: [more-collections-per-pod](more-collections-per-pod.md) (which this supersedes),
[collections](collections.md) (its Limits), [pod-storage-limits](pod-storage-limits.md).

A pod that keeps its meals, recipes, groceries, workouts, wardrobe, courses and calendar in collections reached the
cap of 50 again: adding pinned recipes and their ingredients (two small collections) needed two empty ones deleted
first. As [more-collections-per-pod](more-collections-per-pod.md) found, the number of collections is not what
bounds a pod's size: 5,000 live rows and 4 MiB of values across all collections, the definition history per
collection and the pod's storage quota do that, and they stay as they are. Small collections (a lookup table, a
one-row state) cost a definition and nothing more, and splitting data the way it is shaped is what the model wants.

Decision: the cap is 100 collections per pod, for every pod (`source_collections::Caps::collections`); the system
collections (`notifications`, `notification_channels`) still don't count, and the other limits in
[collections](collections.md#limits) are unchanged.

Not yet: the 5,000-row limit is now the nearer one for a pod with a large recipe book (recipe lines and steps are
most of its rows); raising it is a separate decision about storage, not about the count.
