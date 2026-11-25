# A pod's collections may hold 20,000 rows and 16 MiB of values

Date: 2026-11-25. Related: [collections](collections.md) (its Limits), [collections-cap-100](collections-cap-100.md),
[pod-storage-limits](pod-storage-limits.md).

A pod with a recipe book reached about 4,100 of its 5,000 live rows: each recipe brings its ingredient lines and steps
(about 15 rows a recipe), so roughly forty more recipes would have filled it, with nothing else left for a log, a
grocery list or a calendar. The row and value caps were set when a pod held a few small lists. They are not what
protects the server: each pod's whole database is held to its storage quota (768 MiB for an admin pod, 8 MiB for a
temporary one) by SQLite's own page limit, which every table counts.

Decision: per pod, temporary pods too (`source_collections::Caps`), collections hold at most **20,000 live rows** and
**16 MiB of values**. What is kept for undoing stays at twice the bytes cap (now 32 MiB), so deleting and re-adding
still can't grow a pod past it. A temporary pod still stops at its 8 MiB database long before either cap. One
`edit_rows` call or action still touches at most 5,000 rows: a preview is kept in memory for an hour, so a larger import
is several calls. Supersedes only those two numbers in [collections](collections.md#limits).
