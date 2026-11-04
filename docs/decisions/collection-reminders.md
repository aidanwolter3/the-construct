# Collections remind the user through declarative rules, and the pod has an IANA time zone

Date: 2026-11-04. Related: D52 ([`notifications.md`](../notifications.md)), [collections](collections.md) (open
question 3), [collection-computed-fields](collection-computed-fields.md), [watcher](watcher.md), issue #57, issue #66.

A todo app built on Collections couldn't say "this is due today": a collection holds data, no logic, and nothing in
the pod looked at a row's date. Three things were missing: a way to say *when* to notify without stored code, a
notification that belongs to a custom app (D52 channels were static, declared at startup by built-in apps), and a
time zone (a `date` has none; "due today at 9" needs the user's).

Considered:
- Stored code or a trigger language on collections. Rejected (decision `collections`): no code from a prompt.
- A row-level "notify at" timestamp field. Rejected: the app would have to compute it in every write, and "unless
  done" would need a second write when the todo is ticked.
- Per-rule time zones or fixed UTC offsets (as the watcher has today). Rejected: offsets are wrong across daylight
  saving (issue #66), and the zone is the user's, not the collection's.
- One static Collections channel shown under no app. Rejected: the inbox and settings group by app, and a tap must
  open the todo app.

Decision:

## Reminders are data in the definition

A collection definition gains `reminders` (at most 5), versioned and restored with it. Claude sets them with
`apply_change` ops:

- `{op: add_reminder, field, title, body?, at?, unless?, label?, app?}`, `{op: update_reminder, reminder, ...}`,
  `{op: remove_reminder, reminder}`.
- `field`: a `date` field. `at`: `HH:MM`, default `09:00`. `unless`: `{field, equals}`, the value coerced to the
  field's type (`null`: empty). `title`/`body`: the notification's text, `{field}` replaced by the row's value.
  `label`: the channel's name in notification settings (default `<Title>: <field>`). `app`: a custom app id.
- Fields are stored by id, so renames keep working; removing a field a reminder reads is refused until the reminder
  changes; restoring an older version drops a reminder whose fields are gone.

## Evaluation

The Collections adapter polls for notifications (D52 `poll_notifications`, so only while the pod has a registered
device). A row is owed one notification per date value when that date at `at` in the pod's zone has passed and
`unless` doesn't match *then*: a todo ticked before its time never notifies. `collections_reminders_sent` (adapter
migration 3, append-only) records `(collection, reminder, row, date)`, so nothing repeats after the core's 30-day
retention and a changed date notifies again. Not sent: dates before the day the reminder was added (no backlog
announced), reminders more than 7 days late (the pod had no device meanwhile), more than 50 per poll (the rest follow a
minute later). The poll comes back at the next reminder's moment, at most 15 minutes later (rows added meanwhile), or
in an hour when no collection has a reminder. A notification opens `collections://<c>/<row>` in its app (the app opens
when its layout has no route for it), grouped by reminder.

## Custom apps get notification channels (pod-core, generic)

- `AppAdapter::notifies_custom_apps` and `custom_notification_channels(store)`: an adapter declares channels at run
  time for live custom apps. The core drops any other (not a live custom app, bad id, a duplicate). Such an adapter
  may poll without static channels.
- `AppState::notification_channels` (and so `notify`, Web Push prefs, `GET /v1/notifications/channels`,
  `PUT /v1/devices/{id}/channels`) includes them; unread counts and `GET /v1/notifications?app=` count live custom
  apps too. No wire or schema change: clients already treat `app_id` and channel ids as opaque.
- Each reminder is one channel, its id the reminder's pod-unique id (`n` + 10 base32 characters), of the reminder's
  `app` or else the oldest live custom app listing the collection as a feed. With no such app nothing is sent (and
  nothing is recorded, so it sends once an app appears, within the 7 days).

## The pod's time zone

A pod setting `time_zone` holds an IANA name (`America/Denver`), set by Claude with the core MCP tool `set_time_zone`
(which also answers the current one; UTC when unset). `pod_core::tz` validates and resolves it with `jiff` and its
bundled zone database (servers may have none), so daylight saving is right. Any adapter can read it; issue #66 (the
watcher's fixed offsets) can use the same module and dependency.

Not done yet: the user can't set the zone in the app (only through Claude); repeating reminders ("every Monday"),
reminders relative to a date ("a day before"), and reminders on text timestamps are not supported; a reminder can't
yet be snoozed or acted on from the notification.
