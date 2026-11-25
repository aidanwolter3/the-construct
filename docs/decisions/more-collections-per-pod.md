# A pod may have 50 collections

Superseded by [collections-cap-100](collections-cap-100.md).

Date: 2026-11-17. Related: [collections](collections.md) (its Limits), [pod-storage-limits](pod-storage-limits.md).

A pod that keeps most of its life in collections (meals, recipes and their lines, groceries, workouts, a log, a
calendar's days, wardrobe cards, …) reached the cap of 20 and could not add one more. The cap on the number of
collections was never what bounds a pod's size: 5,000 live rows and 4 MiB of values across all collections, the
definition history per collection and the pod's storage quota do that, and they stay as they are.

Decision: the cap is 50 collections per pod, for every pod (`source_collections::Caps::collections`); the other
limits in [collections](collections.md#limits) are unchanged. Supersedes only that number there.
