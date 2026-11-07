# Notifications

Apps send notifications through their pod: likes and comments from Pixelfed, matches from the Events app, reminders
from a collection. They reach every device of the pod: an in-app inbox, OS notifications while an app runs, and Web
Push when the Android app is closed. Decision D52 in [DECISIONS.md](DECISIONS.md) records the design and what was
weighed; the original build plan is in [history/notifications-plan.md](history/notifications-plan.md).

```
 upstream ─poll─▶ adapter (knows the app) ── NewNotification{channel, title, body, target_uri, dedupe_key}
                                                    │
 host scheduler ─wakes due pods─▶ pod-core: store once ─┬─▶ `notification` event on open event streams
                                                        └─▶ Web Push to registered devices (per-device channels)
 engine: cache, unread counts, live-arrival callback ─▶ inbox, badge, OS notifications
 tap ─▶ open the app, then action.navigation.open {target_uri} against that app's own routes
```

## Who knows what

Only an app's adapter knows where its notifications come from. The pod, the engine and the clients see an app id, a
**channel** the adapter declares ("Likes"), ready-made title and body text, an optional image, a `target_uri`, and
the `layout` to present it in: the pod's own layout built on that blueprint (a whole reference to it, or copied from
it), else the blueprint's id (decision `layouts-not-apps`). The client presents that layout and opens the screen of it
whose route matches `target_uri`, like any navigation. Adding notifications to a new
app needs adapter code only.

## Sources

- **Polling:** an adapter implements `AppAdapter::poll_notifications` and `poll_interval`, and keeps its cursor in
  its own tables. Pixelfed polls `GET /api/v1/notifications` every 2 minutes; the first poll stores the recent
  backlog as already read, so nothing floods in. Collections uses the same poll to send due reminders and to run
  scheduled fills, whose alerts notify. Its channels belong to the custom app showing the collection. Server
  monitoring looks at its checks every 5 minutes and notifies the custom apps built on it when one turns to warning.
- **From a stream:** the Agents source follows its runner's event stream and notifies as events arrive (a session
  that needs you, finished or failed sessions, runner warnings; [agents.md](agents.md)).
- **From a route:** an adapter calls `AppState::notify` from its own handlers.
- **Waking pods:** pods open lazily, so the host's scheduler checks every 30 s and opens pods whose
  `host.db` `pods.next_wake_at` is due (8 at a time). A pod polls only while it has a device seen in the last 30 days,
  so idle and temporary pods cost no upstream calls. Failed polls back off from 1 minute to 1 hour; a revoked
  sign-in stops polls until someone signs in again.

## Delivery

1. Each notification is stored once per pod, keyed by `(app_id, dedupe_key)`, so repeated polls are harmless. The
   newest 500 are kept for 30 days.
2. It goes out as a `notification` event on every open events stream that has a session.
3. It is sent by Web Push (RFC 8291 encryption, VAPID) to every registered device that wants its channel and has no
   stream open. Payloads are end-to-end encrypted, so push services see no text.

Push endpoints are supplied by devices, so the pod only sends to `https://` URLs on the default port of a public
host, resolving public addresses only. A 404 or 410 from the push service drops the subscription, as do 5 failures
in a row. Limits per pod: 20 devices, 60 deliveries a minute. The routes are in [api.md](api.md#notifications-and-devices).

## Clients

- **Engine:** sends a stable device id (`X-Construct-Device`) and registers the device when its stream connects. It
  caches the newest 200 notifications, the channels and the unread counts, so the inbox and badge work offline;
  marking read is optimistic and queued. A listener fires only for live arrivals, so one notification never alerts
  twice. Sign-out unregisters the device and clears the cache.
- **Layouts:** the `inbox` blueprint lists every notification (`local:notifications`), grouped by day; tapping one
  presents its layout and target and marks it read. The `settings` blueprint lists the channels with switches (stored
  per device on the pod). Any layout can open them, or bind `${local://inbox#unread}` for a badge.
- **Android:** asks for `POST_NOTIFICATIONS` once; layouts read whether notifications can show from
  `local://device#notification_permission` (`granted`, `denied`; `""` on desktop) and ask again with the
  `action.device` command `request_notification_permission`, which shows the system prompt, or the app's notification
  settings once Android won't show the prompt again ([layouts.md](layouts.md)); creates OS channels `<app_id>.<channel>` grouped per app; shows
  live arrivals while in the background; and receives closed-app push through
  [UnifiedPush](https://unifiedpush.org): a distributor app such as ntfy if installed, otherwise the embedded FCM
  distributor via Play services and the UnifiedPush project's public gateway. No Firebase project or Google
  credentials are involved; build with `-PnoFcmFallback` to leave the fallback out. Each server gets its own push
  registration (decision [`android-push-registration-per-server`](decisions/android-push-registration-per-server.md)).
- **Desktop:** tray notifications while running and unfocused, and a dock badge where supported. There is no
  closed-app push on desktop.

## Trying it

- `server pods notify-test <name>` sends a test notification to a pod's inbox and devices.
- Against the fake Pixelfed, `POST /_admin/notifications` injects one upstream ([testing.md](testing.md#fake-pixelfed)).
- In the simulator, `notify <app_id> <channel> <title>`, `open-inbox`, `click <text>` (a row of the inbox) and
  `toggle-channel <app>.<channel>` script the UI ([simulator.md](simulator.md)).

## Not done yet

Closed-app notifications on desktop; removing an Android notification when it is read on another device; per-app
notification screens inside layouts; upstream push instead of polling where an upstream offers it. The Android
permission prompt, channels, UnifiedPush and the FCM fallback have not been checked on a device yet. The Rust Android app
(`construct-render`) receives push through UnifiedPush too, with its own keys and decryption, and the same embedded
FCM fallback (decision [`android-native-push-embedded-fcm`](decisions/android-native-push-embedded-fcm.md)).
