A `where` condition (of a `core://List`, or a `core://Text` `count`) may be `{field, older_than: "1d"}` (`s`, `m`,
`h`, `d` or `w`): items whose field is a date or time more than that long ago on the device's clock (an empty value
never matches); `not: true` inverts a condition. For sections such as "read more than a day ago".
