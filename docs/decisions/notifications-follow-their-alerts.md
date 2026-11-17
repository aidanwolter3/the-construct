# A notification says whether its event still matches an alert, and layouts filter on it

Date: 2026-11-17. Related: [notifications-in-a-collection](notifications-in-a-collection.md),
[collection-fillers](collection-fillers.md), [live-computed-fields](live-computed-fields.md).

An alert on the Equality Center's events was narrowed from every event to D&D, but the 18 notifications it had sent
for other events stayed in the inbox: an alert decides what is sent, and nothing withdrew what was sent before.
Notifications handed over from the core's table (decision `notifications-in-a-collection`) also had no `subject`,
so deleting one of those events wouldn't take its notification either. Considered: deleting a notification when
its alert stops matching (a removal rule in the alerts code), and a fact the layouts filter on, as channel switches
are. The developer chose the filter: what to show stays a layout's choice.

Decision: **the notification carries the fact; layouts filter; the counts follow.**

- A new compute kind, `subject` (`{kind: "subject", from, value}`): what the row an entity URI in text field
  `from` names serves as `value` (its fields by name and the derived names, such as `alert_matched`), whatever its
  collection. A row's alert match is worked out when it is served, never stored, and the existing lookups read
  stored fields through a ref into one collection, so neither could reach it. It is live: computed again when a
  write changes that row (`follow::settle` sees every row a transaction changed), when its collection's
  definition changes (an alert edited: `db::append_def` notes it), and hourly for answers that change with time
  alone (an alert's window of days).
- `notifications` gains `subject_alert_matched` (a bool, true by default: a notification whose subject can't be
  read, or whose collection has no alerts, stays shown). A pod's next open adds the field, gives handed-over
  notifications their `target_uri` as `subject` when it names a collection row, and computes every row.
- The inbox blueprint (file 0006) adds `{field: subject_alert_matched, equals: true}` to both lists. The store
  leaves the others out of the unread counts, the old `/v1/notifications` list and so the badge; a new
  notification always matches when sent.

Not done: a notification whose alert is removed while another alert still matches its row stays shown (the
match is "any alert of the collection", not the one that sent it). A user's own copy of the inbox gets the filter
only when it is edited to use it.
