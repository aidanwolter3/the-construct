# Android: one push registration per server, and notification taps through a private activity

Date: 2026-11-03. Related: D29 (storage per server), D52 / [`../NOTIFICATIONS.md`](../NOTIFICATIONS.md), security
audit #31 (L11, L12).

The Android app had one UnifiedPush registration (the connector's default instance) and handed its endpoint and Web
Push keys to every server the user picked in the menu (`Push.resend` on each new session). Every pod the user had
tried therefore held the same endpoint, which links the device across servers, and each of them could push to it.
Separately, `MainActivity` is exported (launcher, OAuth redirect), and it acted on the notification extras of any
intent, so any installed app could mark notifications read or navigate the app.

Decision:

- **One UnifiedPush instance per server**, named `server:<base url>` (`Push.instanceFor`). Endpoints and keys are
  saved per instance, and a session is only ever given its own server's registration. Push events are routed by
  instance: the live engine handles them only when it is that instance's server; any other server's push becomes an
  OS notification (even with the app in the foreground, since the in-app bell shows only the open server).
- The old shared registration is dropped once on upgrade (keys deleted, default instance unregistered). Each server
  registers again the next time its session sets up notifications, so a server the user doesn't reopen stops
  pushing to this device.
- **Notification taps go through `NotificationTapActivity`** (not exported), which forwards them to `MainActivity`
  with a random per-process nonce; `MainActivity` ignores notification extras without it. A tap on a notification
  from another server than the open one only brings the app up (no cross-server navigation yet).

Not done: switching to the tapped notification's server automatically.
