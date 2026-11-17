# The pod's notifications are a collection, and the inbox is an ordinary layout over it

Date: 2026-11-17. Related: D52, [collections](collections.md), [collection-primitives](collection-primitives.md),
[collection-reminders](collection-reminders.md), [list-where-all](list-where-all.md),
[layouts-not-apps](layouts-not-apps.md), [plumbing-and-porcelain](plumbing-and-porcelain.md).

The user asked for three changes: a channel turned off should leave the notifications view, a notification about
an event should go when the event does, and read notifications older than a day should move into a section that
starts closed. Each could have been one more rule in the core (filter by switch, delete on a hook, a fixed 24-hour
split computed by the renderer). They want the opposite: a pod's notifications should be theirs to shape, so the
server reports facts and offers general tools, and what to show is a layout's choice. Notifications lived in the
core's own table, reachable only through `/v1/notifications` and a device cache (`local:notifications`), so no
layout or MCP tool could filter, edit or extend them. Considered: keeping the table and adding facts to it
(`channel_enabled`, `subject`) plus a `remove_notifications` tool, which still leaves notifications outside
everything layouts and Claude already use for data; and moving them into a collection, which gives that for free
but needs a few things collections lacked.

Decision: **notifications are rows of a system collection**, and the core stores them through an interface.

- **The store is pluggable.** `pod_core::NotificationStore` (`notify_store.rs`) keeps notifications and channel
  switches: insert (skipping a `(layout, dedupe_key)` already stored), list, unread counts, mark read, switches,
  remove what is about a subject. The pipeline in `notify.rs` (checking channels, Web Push, the `notification` and
  `notifications_read` events) only calls it. A source offers one with `Source::notification_store`; without one
  the core's own tables serve (`CoreStore`), so `pod-core` still knows no source.
- **Collections is the store.** Two system collections every pod has (`source_collections::inbox`):
  `notifications` (title, body, channel_layout, channel, channel_key, layout, target_uri, image_url, group_key,
  subject, dedupe_key, at, read, read_at; `channel_enabled` and `channel_title` are live lookups into the channel's
  row) and `notification_channels` (key `<layout>.<channel>`, layout, channel, title, description, importance,
  enabled, default_enabled, remove_with_subject), kept current as sources declare channels. Layouts bind them as
  feeds, Claude edits them with the usual tools, and rows reach devices through the usual scoped `event: data`. Any
  write to either collection announces the unread counts again (`notifications_read`), so the badge follows a row
  edit. They don't count against a pod's collections or rows, and can't be deleted or have their fields renamed or
  removed, as for `events`.
- **Channel switches are the pod's.** One switch per channel for the whole pod (the user's choice; the core table
  `notification_switches` replaced per-device `device_channel_prefs`, a channel any device had off starts off).
  `PUT /v1/devices/{id}/channels` still sets them. A switched-off channel's notifications are still stored, but
  they are left out of `GET /v1/notifications` and its counts, go out on no stream, and are pushed to no one. The
  inbox hides them with `where channel_enabled = true`, so another layout may still show them.
- **A notification may name its subject** (`NewNotification::subject`, `Notification::subject`: an entity URI).
  Collections' alerts and reminders name their row. When a write deletes a row (`follow::settle` sees every row a
  transaction changed, a filler's removals included), the notifications about it are deleted in the same
  transaction, unless their channel's `remove_with_subject` is off. Other sources call `Pod::notifications_gone`.
- **`read_at` follows `read`**, set by the pod whichever way a row is marked read (a row update, an action, the
  old API).
- **Two generic Collections features** came with it: an **update action** (an action whose source is its own
  collection and that has only `update`: it writes `update` to every row matching `where`; `notifications` ships
  `mark_all_read`, and `clear_read`, a delete action), and **retention** (`Definition::keep {rows, days}`, applied
  when a write settles; `notifications` keeps the newest 500 of 30 days, as the core table did).
- **Vocabulary `list_where_age`**: a `where` condition may be `{field, older_than: "1d"}` (`s`, `m`, `h`, `d`,
  `w`) on the device's clock, and `not: true` inverts any condition, in a list and in a count. The list draws itself
  again when an item crosses the line.
- **The inbox** (`inbox` blueprint, file 0005) lists `collection:notifications` where `channel_enabled` is true and
  `read_at` is not more than a day ago, and the rest in `@core/collapsible`, a new server component: a header that
  opens and closes its children, kept open while the app runs (`local://collapsible/<key>`, set with
  `action.local.set`). Tapping a row marks it read (an optimistic row update) and `open_notification` now takes the
  row's URI. A blueprint's element now needs its own `feeds` (the inbox declares `collections/notifications`), so it
  loads them wherever it is drawn.
- **Hand-over:** a pod's first open with this build creates the collections and moves the core table's newest 500
  notifications and its switches into them, then empties those tables. Requests that come before the pod's start
  finished create the collections themselves.

Compatibility: `/v1/notifications`, `/v1/notifications/read`, the channel routes, the stream events and Web Push
keep their shapes (ids are now row ids, still strings; `before` pages by them), so builds before this one keep
working on the inbox they draw (file 0004, `local:notifications`), which now hides switched-off channels too. File
0005 needs `list_where_age`, so only new builds get it.

Not done: a device can no longer keep a channel on while another has it off. Deleting a whole collection doesn't take
its rows' notifications with it (only deleting rows does). A notification removed upstream (a deleted
Pixelfed post) goes only where its source calls `notifications_gone`; Pixelfed, Agents and Monitor don't name
subjects yet. The simulator's live mode (`remote::PodClient`) still resets `local://inbox` to 0 when it opens a
layout that doesn't show the inbox (the apps take the count from the engine).
