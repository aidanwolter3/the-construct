`core://List` `merge_query_keys` and `sort_by` (decision `merged-lists`): the entities of more queries after
`query_key`'s, each once, with `where`, `filter` and the rest applied to them all; and the items in the order of a
field's value as text, equal or missing values keeping their order. A day's agenda of workouts and events in time
order: `"query_key": "collection:workouts", "merge_query_keys": ["collection:events"], "sort_by": "starts_at"`.

Fields: merge_query_keys, sort_by.

A list over several queries, sorted by a field
