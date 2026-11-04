`core://Text` `count: {query_key, where?: [{field, equals}], zero_text?}` counts the entities of a query the
client holds that match every `where` (as in `core://List`), and puts the number in place of each `$count` in
`value_expr` ("$count left"). With `zero_text`, a count of 0 shows that text instead ("All done").
