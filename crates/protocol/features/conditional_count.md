`core://Conditional` `count: {query_key, where?: [{field, equals}]}` (the shape of a `core://Text` `count`, without
`zero_text`): the content shows while the query has at least one entity matching every `where`, or, with `not`,
while it has none; while the query isn't loaded it shows nothing. `value` is unused (leave it out), e.g. to hide a
card around a list that is empty.
