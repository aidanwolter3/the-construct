# Notifications

Sources send notifications through their pod: likes and comments from Pixelfed, matches from the Events app, reminders
from a collection. They reach every device of the pod: an in-app inbox, OS notifications while an app runs, and Web
Push when the Android app is closed. Decision D52 in [DECISIONS.md](DECISIONS.md) records the design and what was
weighed; the original build plan is in [history/notifications-plan.md](history/notifications-plan.md). They are
kept as rows of the pod's `notifications` collection, which any layout can bind and Claude can edit (decision
[`notifications-in-a-collection`](decisions/notifications-in-a-collection.md)).

```
 upstream ─poll─▶ source (its upstream) ──── NewNotification{channel, title, body, target_uri, dedupe_key}
                                                    │
 host scheduler ─wakes due pods─▶ pod-core: store once (NotificationStore: the `notifications` collection)
                                    │ channel switched on? ┬─▶ `notification` event on open event streams
                                    │                      └─▶ Web Push to registered devices
                                    └─▶ rows reach layouts as `event: data` (the inbox binds the collection)
 engine: unread counts, live-arrival callback ─▶ badge, OS notifications
 tap ─▶ open its layout, then action.navigation.open {target_uri} against that layout's own routes
```

## Who knows what

Only a source knows where its notifications come from. The pod, the engine and the clients see a layout, a
**channel** the source declares ("Likes"), ready-made title and body text, an optional image, a `target_uri`, and
the `layout` to present it in, which the pod fills in (decision `layouts-not-apps`, issue #337):

1. with a `target_uri`, the first of the pod's layouts with a screen whose `route` matches it: the channel's own
   layout (`channel_layout`, when it is a pod layout), then the pod's layout built on that blueprint, then every
   other layout, home last (so an Agents run opens in whichever layout routes `agents://sessions/{id}`);
2. otherwise the channel's own layout, else the pod's layout built on its blueprint: the first that uses it whole
   (`core://Use` of `@core/<blueprint>`), else the first copy of it (`copy_of`, as `list_layouts` says);
3. else the blueprint's id.

The pod resolves it when it stores a notification, when `GET /v1/notifications` lists one without it, and each time
the pod opens for rows whose `layout` is empty or names no layout of the pod (a blueprint, or a layout since deleted);
a row already naming a pod layout keeps it. The client presents that layout and opens the screen of it whose route
matches `target_uri`, like any navigation, so it needs nothing of channels. Adding notifications to a new source
needs source code only.

## Sources

- **Polling:** a source implements `Source::poll_notifications` and `poll_interval`, and keeps its cursor in
  its own tables. Pixelfed polls `GET /api/v1/notifications` every 2 minutes; the first poll stores the recent
  backlog as already read, so nothing floods in. Collections uses the same poll to send due reminders and to run
  scheduled fills, whose alerts notify. Its channels belong to the custom app showing the collection. Server
  monitoring looks at its checks every 5 minutes and notifies the custom apps built on it when one turns to warning.
- **From a stream:** the Agents source follows its runner's event stream and notifies as records change (a run
  that needs you, a run that ends with a report, runner warnings; [agents.md](agents.md)).
- **From a route:** a source calls `AppState::notify` from its own handlers.
- **Waking pods:** pods open lazily, so the host's scheduler checks every 30 s and opens pods whose
  `host.db` `pods.next_wake_at` is due (8 at a time). A pod polls only while it has a device seen in the last 30 days,
  so idle and temporary pods cost no upstream calls. A pod a source keeps open (`Source::keeps_open`, e.g. one
  signed in to an agent runner) is always due, so it is opened when the server starts. Failed polls back off from 1 minute to 1 hour; a revoked
  sign-in stops polls until someone signs in again.

## Storage

`pod-core` stores through a `NotificationStore` (`notify_store.rs`); Collections provides it
(`source_collections::inbox`), and without Collections the core's own tables serve. Two system collections every
pod has:

- **`notifications`**: one row per notification: `title`, `body`, `channel_layout`, `channel`, `channel_key`
  (`<layout>.<channel>`), `layout` (the layout it opens), `target_uri`, `image_url`, `group_key`, `subject`,
  `dedupe_key`, `at` (when it happened), `read`, `read_at` (set by the pod when `read` turns true), and two live
  lookups into its channel's row, `channel_enabled` and `channel_title`, and `subject_alert_matched`: false once
  the row it is about no longer matches an alert of its collection (a `subject` compute reading that row's
  `alert_matched`, live: when the row or its collection's alerts change, and hourly; decision
  [`notifications-follow-their-alerts`](decisions/notifications-follow-their-alerts.md)). The unread counts and
  the old list leave out notifications whose `subject_alert_matched` is false. Ordered by `at`, newest
  first. Actions:
  `mark_all_read` (an update action) and `clear_read` (a delete action). It keeps the newest 500 of the last 30
  days (`keep`).
- **`notification_channels`**: one row per declared channel, kept current as sources declare them: `key`, `layout`,
  `channel`, `title`, `description`, `importance`, `enabled` (the pod-wide switch), `default_enabled`,
  `remove_with_subject`.

Both are bound, filtered and edited like any collection (marking read is setting `read`, removing is deleting a
row); any write to them announces the unread counts again. They don't count against the pod's collection or row
caps and can't be deleted. A pod's first open with them moves the core table's notifications and switches in.

## Delivery

1. Each notification is stored once per pod, keyed by `(layout, dedupe_key)`, so repeated polls are harmless (a
   row the user deleted isn't stored again). `layout` is the layout (or blueprint) whose channel it is: on the wire
   a channel's `layout` and a notification's `channel_layout` (`app_id` before decision
   [`one-layout-system`](decisions/one-layout-system.md)); a notification's `layout` is the layout it opens.
2. **Channel switches are the pod's**, one per channel for every device. A switched-off channel's notifications
   are stored but alert no one: no stream event, no push, and `GET /v1/notifications` and the unread counts leave
   them out. Layouts decide whether to show them (`channel_enabled`).
3. It goes out as a `notification` event on every open events stream that has a session.
4. It is sent by Web Push (RFC 8291 encryption, VAPID) to every registered device that has no stream open.
   Payloads are end-to-end encrypted, so push services see no text.
5. **It goes with its subject.** A notification may name the entity it is about (`subject`; Collections' alerts and
   reminders name their row). When that row is deleted (by a person, an action, or a filler that no longer lists
   it, e.g. an event that is gone), its notifications are deleted in the same write, unless the channel's
   `remove_with_subject` is off. Other sources call `Pod::notifications_gone(uris)`.

Push endpoints are supplied by devices, so the pod only sends to `https://` URLs on the default port of a public
host, resolving public addresses only. A 404 or 410 from the push service drops the subscription, as do 5 failures
in a row. Limits per pod: 20 devices, 60 deliveries a minute. The routes are in [api.md](api.md#notifications-and-devices).

## Clients

- **Engine:** sends a stable device id (`X-Construct-Device`) and registers the device when its stream connects. It
  caches the newest 200 notifications (for builds whose inbox reads `local:notifications`), the channels and the
  unread counts, so the badge works offline; marking read is optimistic and queued. A listener fires only for live
  arrivals, so one notification never alerts twice. Sign-out unregisters the device and clears the cache.
- **Layouts:** the `inbox` blueprint (file 0007, a `core://Page` built from the core components: decision
  [`element-roles-and-reuse`](decisions/element-roles-and-reuse.md)) binds `collection:notifications` and shows the
  notifications whose channel is on and whose event still matches its alert in three groups, each a card of rows
  (`@core/grouped_list`): **New** (unread, with the unread count), **Earlier** (read within the last day) and, in a
  section that starts closed (`@core/collapsible`), **Older** (read more than a day ago: `where` `older_than`,
  feature `list_where_age`). A row has a bell (`@core/tinted_icon`, in the accent while unread and muted once read:
  channels carry no icon of their own yet), its channel, day and time, the title, the body on at most two lines, and
  a dot while unread. Tapping one marks its row read and presents its layout and target (`open_notification` with
  the row's URI). "Mark all read" is in the header while something is unread, and "Manage channels" under the
  groups opens Settings as a panel. Builds that can't draw it get file 0006 (the same lists grouped by day), and
  builds before `list_where_age` file 0004 (device data).
  The `settings` blueprint lists the channels with switches (for the whole pod). Any layout can open them, bind the
  collections for its own view of notifications, or bind `${local://inbox#unread}` for a badge.
- **Android:** asks for `POST_NOTIFICATIONS` once; layouts can ask again with the
  `action.device` command `request_notification_permission`, which shows the system prompt, or the app's notification
  settings once Android won't show the prompt again ([layouts.md](layouts.md)) (`local://device#notification_permission`
  reads `""` for now); creates the OS channel `<layout>.<channel>` when its first notification arrives; and receives
  push, also while the app is closed, through
  [UnifiedPush](https://unifiedpush.org): a distributor app such as ntfy if installed, otherwise the embedded FCM
  distributor via Play services, which registers with the pod's VAPID key so the pod sends Web Push straight to
  FCM's own Web Push endpoint (`fcm.googleapis.com/fcm/send/...`). No Firebase project, gateway or Google
  credentials are involved. Each server gets its own push
  registration (decision [`android-push-registration-per-server`](decisions/android-push-registration-per-server.md)).
- **Desktop:** no OS notifications, tray icon or dock badge yet (the Compose desktop app had them; decision
  `rust-clients`): the inbox shows them.

## Trying it

- `server pods notify-test <name>` sends a test notification to a pod's inbox and devices.
- Against the fake Pixelfed, `POST /_admin/notifications` injects one upstream ([testing.md](testing.md#fake-pixelfed)).
- In the simulator, `notify <layout> <channel> <title>`, `open-inbox`, `click <text>` (a row of the inbox) and
  `toggle-channel <layout>.<channel>` script the UI ([simulator.md](simulator.md)).

## Not done yet

Per-device channel switches (one switch per pod since decision `notifications-in-a-collection`); subjects from
Pixelfed, Agents and Monitor; desktop notifications; Android channels created from the pod's channel list (titles, importance, groups) rather
than on arrival; removing an Android notification when it is read on another device; per-app notification screens
inside layouts; upstream push instead of polling where an upstream offers it. The Android app decrypts push with its
own keys (decisions [`android-native-push-unifiedpush`](decisions/android-native-push-unifiedpush.md),
[`android-native-push-embedded-fcm`](decisions/android-native-push-embedded-fcm.md)).
