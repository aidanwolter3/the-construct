# One list over several queries, sorted by a field

Date: 2026-11-20. Related: [list-where-all](list-where-all.md), [named-vocabulary](named-vocabulary.md).

A UX audit of the developer's pod found its Calendar listing a day's workouts above its events: two lists, each in
its own order, so Sunday read 9 AM swim, 2 PM hike, 9 AM masters, 11 AM hike. A list showed one query, in the
order the pod sent it, and a layout had no way to interleave two collections.

Decision: **`core://List` takes `merge_query_keys` (more queries whose entities follow `query_key`'s, each once) and
`sort_by` (a field whose value, as text, orders the items; a stable sort, so equal or missing values keep the
queries' order)** (feature `list_merge`). `where`, `filter`, grouping and the rest apply to the merged items. Items of
different collections share the item template, which tells them apart with a `core://Conditional` on a field.

Text order is enough for what the pods sort: ISO dates and times (`2026-11-22T09:00`), zero-padded clocks, names.
A pod gives two collections a common field to sort and filter on with computed fields (an `expr` joining a date and
a 24-hour start).

Rejected: merging on the pod into a new query (each pair of collections would need a source change or a stored
view, and an edit in one collection would refetch the merged list); a numeric or descending sort (nothing needs it
yet; text order covers dates).

Consequences: a new desktop, Android and web build draws it; a layout that uses it is served to older builds as its
previous version. The pod counts the merged queries' feeds as bound, as it does `query_key`'s. When a collection's definition changes, the
pod checks a merged list's bindings (`$item.<f>`, `where`, a mutation of `$item.uri`) against all its collections: a
field of any of them passes, since the template tells their rows apart (`source-collections` `bindings.rs`). Before
that the check read every row as `query_key`'s, which refused any change to that collection while such a list
used a field only another had.
