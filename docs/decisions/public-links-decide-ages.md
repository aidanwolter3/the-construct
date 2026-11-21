# A public link decides a list's age on the pod's clock as the copy is made

Date: 2026-11-21. Related: [public-web-links](public-web-links.md),
[public-links-share-the-closure](public-links-share-the-closure.md), [site-origins](site-origins.md). Issue #446.

A shared events page (`where: [{field: "start", older_than: "6h", not: true}]`, the upcoming events) showed "No
matched events coming up" to every visitor while the owner's screen listed them. A copy treated every `older_than`
as a condition it can't decide and, failing closed (issue #446), left out every row behind it: an events list
always comes out empty.

An age is not like a form value or an unbound name: it depends only on the row's date and the clock, and the clock
only moves forward. A row an age hides now (an event already past, for "not older than") never shows later, and a
row it shows now that ages out is hidden by the visitor's page, which tests the same `where` on its own clock, as
the owner's does. So deciding it now carries exactly the rows the owner's screen shows, and nothing it will show
later without a review.

Decision: **when a copy is made, a list's `where` `older_than` (with or without `not`) is decided on the pod's
clock, in the pod's time zone (`set_time_zone`, else UTC), as the renderer's `Where` decides it**: a value more
than that long ago is older, an empty or unreadable value or a duration that doesn't read never is. Rows it shows
go out with the fields read; rows it hides stay home.

- Only list ages (`where`, counts' `where`). A `core://Conditional` `date` test and other conditions that can't be
  decided still fail closed.
- A row that later passes an age the other way round (`older_than` without `not`: it becomes old) is not in the
  copy until the link is made again; with `new_rows: "publish"` the next refresh carries it.
- The review id is a hash of the copy, so a row crossing the line between the review and the confirm asks again.
