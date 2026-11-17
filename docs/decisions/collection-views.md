# A collection declares views, small derived lists the pod serves as their own query keys

Date: 2026-11-17. Related: [collections](collections.md), [collection-reminders](collection-reminders.md),
[date-conditions](date-conditions.md), [list-where-all](list-where-all.md),
[scoped-data-events](scoped-data-events.md).

A `core://List` binds a whole collection (`collection:<c>`, every row in the collection's order) and can only keep
exact `where` matches. Layouts keep needing short derived lists: the next three upcoming rows, today's rows, next
week's. The list has no limit and no relative-date filter, so a page either drew everything or a scheduled job
flipped flags on rows to say which were "next".

Considered:

- **A `limit` and a date filter on `core://List`.** New vocabulary: every desktop and Android build would need an
  update before a pod could use it, and older clients would refuse the layout until then.
- **Computed flags on rows** (`is_upcoming`), recomputed by the pod each day. A field per question, written to every
  row each day and pushed to every device; "the next three" can't be a per-row value at all.
- **Views declared on the collection** and computed when the list is served.

Decision: a collection's definition has `views` (data, like `order_by`, reminders, alerts and fillers), each
`{name, where?, date?, order_by?, descending?, limit?}`, served in the collection's bundle (its feed and its
`/rows` data path) as `collection:<c>:view:<name>`, the URIs of the rows it keeps; the rows' entities are the ones
the bundle already carries.

- `where`: every `{field, equals}` must match, compared as text as a list's `where` compares (`Condition`, the same
  type a `count`'s `where` uses).
- `date: {field, from_days?, to_days?}` on a date or datetime field: the row's day from `from_days` to `to_days`
  days after today in the pod's time zone (`set_time_zone`; UTC when unset), either bound open. A datetime with an
  offset counts by its date in the pod's zone, one without by the date it was written with; an empty date never
  matches.
- `order_by` (else the collection's order) and `descending` sort with the collection's own comparison; `limit`
  (1–200) keeps the first rows. At most 10 views per collection.
- Fields are stored by id, so a rename keeps a view; removing a field a view reads is refused until the view
  changes (as for alerts), and restoring an older version drops views whose fields are gone.
- Layout checks treat a view key as a list of its collection (`$item.<field>` must be a field), and refuse a view
  that doesn't exist, naming the ones that do; since every layout is checked again on a definition change, a
  `remove_view` that a layout still lists is refused.

Why on the pod: it ships with the server alone (query keys are opaque strings to the engine, and the query key
pattern is in the Collections manifest), works the same on every client, and is generic: any pod's collection can
declare views; nothing in it knows an app.

Limits:

- Views are worked out when the list is served or its rows change (the scoped data event for `collection:<c>`
  covers its view keys). A dated view would go stale across midnight, so the Collections poll, once the pod's local
  day has changed, tells devices to reload `collection:<c>:view` of each collection with a dated view, and sleeps
  until the next local midnight. The poll runs only while a device is signed in; a device that was offline
  reloads on its next open.
- "Today" is the pod's, not the viewer's (unlike `conditional_date`, decision `date-conditions`): a device in
  another time zone sees the pod owner's day. Times within a day ("still to come today") are not a view's job; a
  layout can still gate rows with `conditional_date`.
- An optimistic create adds the new row to `collection:<c>` at once; a view shows it after the reload that follows.
- Sharing a layout that lists a view (decision `sharing`) does not turn the view into an input yet.
