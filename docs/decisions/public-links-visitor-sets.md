# A public link counts a row field as the visitor's only as far as the copy's own sets reach it

Date: 2026-11-22. Related: [public-web-links](public-web-links.md),
[public-links-decide-ages](public-links-decide-ages.md). Issue #446.

A copy treats a condition on a row field that its own `action.local.set` changes as the visitor's to decide: either
way can be drawn, as on the owner's screen a tap away, so what it guards goes out. It recorded only the field's name,
from any kept set, and then counted that field of every row of every list as the visitor's. A diary row with a "Hide"
button (`field: "private"`, `value_expr: "true"`) and its body under `$item.private` `not: true` therefore sent the
body of every private row to anyone with the link: no tap on the owner's screen (or the visitor's page) ever shows it.
A set of one fixed row (`notes://diary/9`) opened that field of every other row too.

Decision: **a kept `action.local.set` makes a condition the visitor's only for the rows it writes and only when a value
it writes changes the answer.**

- **Which rows**: a set whose `entity_uri` has no binding writes that one row; one with a binding (`$item.id`, a
  route parameter) may write any row, as before.
- **Which values**: a set whose `value_expr` (or an `also` field's) is a JSON literal writes that value. A
  `core://Conditional` or `show_when` on `$item.<field>` is decided for the row's own value and each literal the sets
  that reach it write: the same answer for all is that answer (a "Hide" leaves a private row hidden), different
  answers are the visitor's. A computed value (`!entity.open`, a toggle) or a binding may be anything, so the field is
  the visitor's, as before.
- Everything else (a `$form` value, a `local://` entity, a `where`) is unchanged.
