`core://Conditional` `not: true`: inverts the test, so the content shows while `value` is not truthy, doesn't equal
`equals`, the `date` test fails, or the `count` query is empty: an "else" branch, e.g. `{"value":
"${local://inbox#unread}", "equals": "0", "not": true}` shows while something is unread.
