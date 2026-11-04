# Notifications from apps: build plan (D52)

> **Archived.** This is the plan the notifications pipeline was built from, kept for history. How it works today is
> in [`../notifications.md`](../notifications.md). D52 in [`../DECISIONS.md`](../DECISIONS.md) records what changed
> while building: there is no wake hook, the FCM fallback needs no Firebase project, and the engine's offline
> behaviour differs. Section 10's phases shipped together; "PROGRESS.md" and `SCHEMA_VERSION` no longer exist.

## Context
The goal: Pixelfed notifications (likes, comments, follows, ...) from the pod, on the user's devices, including when the app
is closed. Today nothing exists for this: no notification code anywhere, pod-core runs no background work (pods open
lazily, only the host's temp-pod sweeper ticks), SSE carries only `layout` events, sessions record no device, and
Android has no service/push/notification permission.

Hard constraint (D34/D35/D40): **only `crates/app-pixelfed` may know Pixelfed.** pod-core, pod-host, the server
binary, client-core, the FFI and the shared UI handle *notifications in general*: an opaque app id, channel ids the
adapter declares, display text the adapter writes, and a navigation target the client resolves against the app's
own layout routes (same as any `action.navigation.open`).

Scope chosen: full (in-app inbox → OS notifications while running → closed-app push), transport =
standard **Web Push** (RFC 8030/8291/8292) delivered through **UnifiedPush** on Android, with an FCM-backed
distributor as the fallback for phones without one. The pod never holds Google credentials.

## Architecture at a glance
```
 Pixelfed ──poll /api/v1/notifications──▶ app-pixelfed (adapter: knows Pixelfed)
                                              │  NewNotification{channel, title, body, target_uri, dedupe_key}
                                              ▼
 pod-host scheduler ─wakes due pods─▶ pod-core Notifier (generic): store row ─┬─▶ SSE `notification` event (open apps)
                                                                              └─▶ Web Push to registered devices
                                                                                    (per-device channel prefs)
 client-core: cache + unread counts + on_notification callback ─▶ shared UI inbox / Android & desktop OS notifications
 tap ─▶ open app_id, dispatch action.navigation.open{target_uri} (route matched in that app's layout)
```

## 1. Wire types (`crates/protocol`, new `notify.rs`)
Generic, schemaless where possible; **no `SCHEMA_VERSION` bump** (no layout vocabulary changes; old clients ignore the
new SSE event and never call the new endpoints).
- `NotificationChannel { app_id, id, title, description, default_enabled: bool, importance: Low|Default|High }` —
  declared by adapters, shown in settings, mapped to Android channels as `"<app_id>.<id>"`.
- `Notification { id (pod-assigned, e.g. ULID), app_id, channel, title, body, image_url?, target_uri?,
  group_key?, created_at, read: bool }`. `title`/`body` are ready-made strings (like comment `meta`, D35) — clients
  never format app data. `image_url` uses the pod's public prefix (D46) so images load without a bearer.
- `PushPayload` — the encrypted body sent to devices: `{v:1, pod_hint, notification: Notification, unread_total}`,
  kept under the 4 KB Web Push limit (body truncated by the core).

## 2. Adapter interface (`crates/pod-core/src/adapter.rs`)
Three optional methods, defaults = "no notifications", so recipes/StaticAdapter are untouched:
```rust
fn notification_channels(&self) -> Vec<NotificationChannel> { vec![] }   // startup, checked by Catalog
fn poll_notifications<'a>(&'a self, pod: &'a Pod, ctx: &'a PollCtx) -> BoxFuture<'a, PollOutcome> { none() }
fn poll_interval(&self) -> Option<Duration> { None }                        // None = adapter doesn't poll
```
- `PollOutcome { new: Vec<NewNotification>, next_in: Option<Duration> /*override*/, status: Ok|Transient(err)|SignInGone(provider) }`.
- `NewNotification { app_id, channel, dedupe_key, title, body, image_url?, target_uri?, group_key?, created_at, quiet: bool }`
  (`quiet` = store as already-read, never push — used for the first-poll backfill).
- Push-in path for adapters whose upstream can call us (webhooks, a future Mastodon-style push subscription):
  `AppState::notify(&self, Vec<NewNotification>)` — same pipeline as polling. Polling is just the scheduler calling
  the adapter and feeding `notify`.
- Adapter keeps its own cursor in its own tables (`<adapter>_*`), not in core tables.
- `Catalog::new` checks: channel `app_id` is one of the adapter's apps; channel ids unique per app; an adapter with
  `poll_interval` declares ≥1 channel.

## 3. Pod core (`crates/pod-core`, new `notify.rs` + `devices.rs`; core migration v14)
Tables (one SQLite per pod, so no pod id column):
- `notifications(id PK, app_id, channel, dedupe_key, title, body, image_url, target_uri, group_key, created_at,
  read_at, UNIQUE(app_id, dedupe_key))` — insert-or-ignore makes polling idempotent. Retention: keep 500 / 30 days,
  pruned on insert.
- `notification_polls(adapter PK, next_at, failures, last_error)` — scheduler state + backoff (1 min doubling to 1 h
  on `Transient`; `SignInGone` stops polling until that provider is signed in again).
- `devices(id PK /*client-generated UUID*/, token_hash → sessions ON DELETE CASCADE, platform, name, created_at,
  last_seen, push_endpoint, push_p256dh, push_auth, push_failures)`.
- `device_channel_prefs(device_id, app_id, channel, enabled, PK(...))` — absent row = channel's `default_enabled`.
- This is also the first per-device record (D46 "Not yet": per-device credentials); keep it scoped to notifications.

`Notifier` (in `AppState`): `notify()` → insert rows in one transaction → for each new non-quiet row:
1. broadcast on a new `notify_tx: broadcast::Sender<NotifyEvent>` (like `layout_tx`, D22);
2. enqueue Web Push to each device with a push subscription whose prefs allow `(app_id, channel)`, skipping
   devices that currently hold an open SSE stream (tracked by `X-Construct-Device` on the stream) — they show it
   themselves. Sends run on a bounded task set; 404/410 deletes the subscription; 5 consecutive failures disables it.
3. `poll_due(&self)` runs every adapter whose `notification_polls.next_at` has passed (timeout 20 s each), feeds
   results to `notify`, updates backoff. Polls only when the pod has a reason: ≥1 device registered **or** an SSE
   listener in the last 24 h (saves upstream calls for idle and temporary pods).
4. `next_wake(&self) -> Option<i64>` — earliest `next_at`, for the host.

HTTP (all need the pod token / session; under `/v1/`, not app-prefixed because they're core):
- `GET /v1/notifications?before=&limit=&app=` → `{notifications, unread_total, unread_by_app}`
- `POST /v1/notifications/read {ids? | up_to? , app?}`
- `GET /v1/notifications/channels` → every adapter's channels (+ this device's prefs if `X-Construct-Device`)
- `PUT /v1/devices/{id} {platform, name, push?: {endpoint, p256dh, auth}}` (upsert; binds device to caller's session)
- `DELETE /v1/devices/{id}`; `PUT /v1/devices/{id}/channels {"<app>.<channel>": bool, ...}`
- `GET /v1/push/key` → the server's VAPID public key (the client needs it to subscribe).
- SSE: the existing `/v1/workspaces/{id}/events` stream also emits `event: notification` `{id, app_id, channel,
  title, body, target_uri, unread_total}` and `event: notifications_read {unread_total}` for **all** apps of the pod.
  Additive (old engines ignore unknown events, client-core `on_push_event`), one connection per client, and works on
  any open app. Events need a valid session (unlike layout events, which may be anonymous — D22).
- Optional later: `GET /v1/notifications/data` returns a `DataBundle` (`core://notifications/<id>` entities, query
  `notifications:<app_id>`) so an app's layout can show its own notification screen with existing nodes.

Web Push sender: `pod_core::push` using `web-push-native` (pure RFC 8291 encryption + VAPID JWT, no HTTP stack) over
reqwest. **The endpoint is client-supplied, so the POST goes through the guarded public client** (D45 resolver: public
`https://` names only, no redirects to private space). Move that resolver/client builder from `app-pixelfed::upstream`
into a generic `pod_core::net` module so both use it.

## 4. Host (`crates/pod-host`, `crates/server`)
- Pods open lazily, so someone must wake them. `host.db` migration: `pods.next_wake_at INTEGER NULL`.
- `Host::spawn_scheduler()` (next to `spawn_sweeper`): every 30 s, select pods with `next_wake_at <= now`, open each
  (existing `Host::pod`), call `pod.poll_due()`, write back `pod.next_wake()`. A pod also writes its `next_wake` to the
  host after any request that changes it (device registered, sign-in completed) via a small callback the factory
  passes into `AppState` (`WakeHook`) — pod-core never sees host.db.
- Concurrency cap (e.g. 8 pods at once) and jitter so pods don't hit upstreams in lockstep.
- VAPID key pair is server-wide: `CONSTRUCT_VAPID_PRIVATE_KEY` (Fly secret) or, if unset, generated once and kept in
  `host.db`; `CONSTRUCT_VAPID_SUBJECT` (mailto/https). Passed to every pod via the factory. README "Server runtime
  configuration" documents both. Staging's pod machine is always on (`deploy/fly/pod.toml` min 1, auto-stop off), so
  the scheduler runs continuously.
- `server pods` CLI: `server pods notify-test <pod>` inserts a test notification (core-only, no app knowledge).

## 5. Pixelfed adapter (`crates/app-pixelfed`) — the only Pixelfed-aware part
- Channels on `pixelfed_home`: `likes`, `comments` (comments + replies), `mentions`, `follows`, `boosts`, `other`
  (unknown types; default off). Titles like "Likes", descriptions "When someone likes your post".
- `poll_interval`: 2 min (fake on staging: same; it auto-starts on request).
- `Upstream::notifications(token, min_id, limit=40)` → `GET /api/v1/notifications?min_id=…` (scope `read` already
  granted; no re-auth needed). Paginate until caught up, cap 3 pages per poll.
- Poll each stored account (`pixelfed_account`, one per instance) with `with_token` (refresh-on-401 + `RefreshLock`
  via `pod.local`). A surviving 401 → `SignInGone("pixelfed")`, mirroring `Hydration::StaleSession`.
- Adapter migration 2: `pixelfed_notify_cursor(instance PK, last_id, initialized_at)`. First poll for an instance:
  store the newest 20 as `quiet` (inbox history, no push), set the cursor — no backlog flood.
- Mapping (all in the adapter): `favourite|like` → likes, "@bob liked your post"; `comment|mention` with a status
  in reply to ours → comments; `mention` → mentions; `follow` → follows; `reblog|share` → boosts; else `other`
  with the upstream's text. `dedupe_key = "<instance>:<notification id>"`. `target_uri`: post-related →
  `pixelfed://posts/<status id>/comments` (route already in the comments screen of the current layouts; comments and
  replies now open `pixelfed://posts/<post>/comments/<comment>`, see decision `comment-threads-from-notifications`); follow →
  none for now (no profile screen; tapping just opens the app). `image_url` = author avatar through the media proxy
  (`MediaPolicy`, public prefix). `group_key = "<status id>:<type>"` so Android can bundle "3 people liked…".
- `fake-pixelfed/`: `GET /api/v1/notifications` (min_id/max_id/limit, Mastodon shape) backed by in-memory
  notifications; existing state changes by *other* fixture accounts create them; `POST /_admin/notifications`
  injects one (type, from, status) for tests; faults apply as for other routes.

## 6. Client engine (`crates/client-core`, `crates/client-ffi`)
- Device identity: engine generates a UUID on first run, stored in `cache.db` (`device` table, store v6), sent as
  `X-Construct-Device` on all calls and the SSE stream.
- Store v6 `notifications` cache (last 200) + `notification_channels` cache, so the inbox works offline.
- `events.rs`: handle `notification` / `notifications_read` events → update cache → `notify()` observers, and call a
  new `NotificationListener::on_notification(FfiNotification)` (live arrivals only, never replays) so platform code
  can post an OS notification.
- Snapshot gains `notifications_json` `{unread_total, unread_by_app}` (cheap; drives badges). Full lists via calls.
- FFI (`EngineHandle`): `set_notification_listener`, `list_notifications(before, limit, app)`, `mark_notifications_read(ids_json | app)`,
  `notification_channels()`, `set_channel_enabled(app, channel, bool)`, `register_push(endpoint, p256dh, auth)` /
  `unregister_push()`, `device_id()`, `push_public_key()`, and `handle_push_message(bytes) -> FfiNotification?` for
  platforms that hand raw decrypted payloads to the engine (updates cache + unread count without a network round-trip).
- On sign-out / `forget_session`: `DELETE /v1/devices/{id}` and clear the notification cache (it holds private text).
- Refresh `unread` on reconnect (`GET /v1/notifications?limit=0`) to recover from missed events.

## 7. Shared UI (`kmp-app/shared/src/commonMain`) — generic, no app names
- Bell button next to the menu with an unread dot/count (from `notifications_json`; test tag `inbox-badge`).
- Inbox sheet: notifications from all apps, grouped by day, each row = app icon (from `/v1/apps` catalog) + title +
  body + relative time; unread rows emphasized; "Mark all read". Tap → `ConstructController.openNotification(n)`:
  open `n.app_id` if not current, wait for its workspace, then dispatch `action.navigation.open {target_uri}` through
  the normal routing (D35); unmatched/no target = just open the app. Marks it read.
- Settings → "Notifications": per app, the pod's channels with switches (`set_channel_enabled`), plus the OS
  permission state.
- `ConstructEngineBridge` (common interface) grows the matching methods; `FakeEngineBridge` (simulator) serves
  notifications from a new fixture `kmp-app/desktopApp/src/sim/resources/notifications.json` and a sim command `notify <app> <channel> <title>`
  to inject a live one. New sim commands: `open-inbox`, `click-notification <text>`.

## 8. Platforms
**Android (`kmp-app/androidApp`)**
- Manifest: `POST_NOTIFICATIONS` (asked when the user first enables notifications, Android 13+).
- Notification channels created dynamically from `notification_channels()`: id `"<app_id>.<channel>"`, grouped per
  app (`NotificationChannelGroup` titled with the app title), importance from the pod. Re-synced on each start;
  channels the pod no longer lists are deleted.
- Posting: tag = notification id (so push and SSE arrivals of the same id replace, not duplicate), group = `group_key`,
  content intent → `MainActivity` with extras `{app_id, target_uri, id}` → `openNotification`.
- **UnifiedPush**: `org.unifiedpush.android:connector` (3.x: generates Web Push keys and decrypts RFC 8291 payloads).
  `PushService` subclass: `onNewEndpoint` → `register_push(endpoint, p256dh, auth)`; `onMessage` → decrypted bytes →
  `handle_push_message` (if engine alive) or parse `PushPayload` directly → post notification; `onUnregistered` →
  `unregister_push`. Registering needs the pod's VAPID key (`push_public_key`). Works when the app process is dead.
- **FCM fallback**: `org.unifiedpush.android:embedded-fcm-distributor` used only when no UnifiedPush distributor is
  installed. Needs a Firebase project + `google-services.json` in the Android build (a CI secret / gradle property,
  absent = fallback off) and a Web-Push→FCM gateway (the UnifiedPush project's public one, or self-hosted later). The
  pod is unchanged either way — it only ever speaks Web Push to an endpoint URL.
- While the app is open, SSE arrivals post the OS notification only if the app is backgrounded; in the foreground the
  bell badge updates instead.

**Desktop (`kmp-app/desktopApp`)**
- While running: Compose `Tray` + `TrayState.sendNotification(title, body)` on `on_notification`; dock/taskbar badge
  via `java.awt.Taskbar.setIconBadge(unread)` where supported. Clicking the tray brings the window forward.
- No closed-app push on desktop (no OS push channel for a JVM app without a store); recorded as a limitation. Later
  option: keep running in the tray after the window closes.

## 9. Security & privacy
- Notification text can be private: SSE `notification` events and all `/v1/notifications*` routes require a valid
  session (pod token); Web Push payloads are end-to-end encrypted (RFC 8291) so the push service / FCM sees nothing.
- Devices bind to the caller's session; deleting/rotating the pod token (`server pods rotate`) cascades devices, so a
  lost phone stops getting pushes on rotation. `DELETE /v1/devices/{id}` from settings removes one.
- Client-supplied push endpoints go only through the guarded public client (SSRF, D45/D46 review), rate-limited per
  pod (e.g. 60 pushes/min), with a cap on devices per pod (20).
- Temporary pods: notifications allowed (they only reach the default/fake instance), push allowed but they expire in
  24 h; the scheduler skips pods marked for deletion.

## 10. Phasing (each phase shippable, `main` stays green)
1. **Core pipeline**: protocol types, adapter methods, core migration v14, Notifier + `notify`, routes, SSE event,
   host scheduler + `next_wake_at`, `server pods notify-test`. Tests with a test adapter in pod-core.
2. **Pixelfed source**: fake-pixelfed endpoint + admin injection, `Upstream::notifications`, adapter migration 3,
   mapping, channels; e2e: inject on fake → appears in `GET /v1/notifications` and on SSE.
3. **Engine + inbox UI**: store v6, device id, events, FFI, bell + inbox + settings, simulator fixture/commands.
4. **OS notifications while running**: Android channels/permission/posting from SSE; desktop tray + badge.
5. **Web Push**: `pod_core::net` extraction, VAPID config, `push` sender, device push subscriptions, skip-if-SSE;
   Android UnifiedPush connector; then the FCM embedded distributor behind a build property.
6. Optional: `/v1/notifications/data` for per-app notification screens in layouts.
Each phase gets a `docs/DECISIONS.md` entry (D52 = this design; later entries for decisions made while building) and
a PROGRESS.md note; README gets the routes, env vars and sim commands.

## Files to touch (representative)
- `crates/protocol/src/notify.rs` (new), `lib.rs`
- `crates/pod-core/src/adapter.rs` (methods + Catalog checks), `notify.rs`, `devices.rs`, `push.rs`, `net.rs` (new),
  `store.rs` (migration v14), `layouts.rs` (SSE event in `events` handler), `lib.rs` (`AppState` fields, `notify`,
  `poll_due`, `next_wake`, `WakeHook`)
- `crates/pod-host/src/lib.rs` (`spawn_scheduler`, host.db `next_wake_at`, VAPID keys), `crates/server/src/main.rs`,
  `crates/server/src/lib.rs` (`pod_factory` wiring, `pods notify-test`)
- `crates/app-pixelfed/src/adapter.rs`, `upstream.rs` (`notifications`), `db.rs` (migration 2), new `notify.rs` (mapping)
- `fake-pixelfed/src/lib.rs`; `crates/e2e`
- `crates/client-core/src/{lib.rs,events.rs,store.rs}`, `crates/client-ffi/src/lib.rs` + regenerated Kotlin bindings
- `kmp-app/shared/src/commonMain/...` (`ConstructEngineBridge`, `ConstructApp.kt`, `Menu.kt`, new `Inbox.kt`),
  `kmp-app/desktopApp/.../Main.kt` + `sim/` (FakeEngineBridge, SimScript), `kmp-app/androidApp/` (manifest,
  `MainActivity.kt`, new `PushService.kt`, `Notifications.kt`)
- Reuse: `AppState::local` (per-pod locks), `with_token`/`RefreshLock`/`refresh_account` (routes.rs),
  `MediaPolicy` + `pod_core::public_prefix` (avatar URLs), `Store::migrate_adapter`, `broadcast` pattern of
  `layout_tx`, `Host::spawn_sweeper` pattern, client-core `SseParser`/`on_push_event`.

## Verification
- `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- pod-core unit tests with a fake adapter: dedupe, quiet backfill, retention, backoff, SignInGone stop, prefs filter,
  skip-push-when-SSE, device cascade on session delete; a source-scan test already asserts pod-core names no app.
- Push: a test Web Push receiver (axum route in tests) that decrypts with the device keys and checks the payload;
  404/410 drops the subscription; SSRF guard refuses private endpoints.
- e2e (`scripts/e2e.sh`): fake-pixelfed `POST /_admin/notifications` → within one scheduler tick it appears in
  `GET /v1/notifications`, on the SSE stream, and at the test push receiver; tapping target route exists in the
  layout (`pixelfed://posts/{id}/comments`).
- Simulator: `-Pscript="notify pixelfed_home likes '@bob liked your post'; shot badge; open-inbox; shot inbox;
  click-notification @bob; wait-text Comments; shot target"` in fixture mode, and live against a local pod with the
  fake upstream.
- Staging: temporary pod token → sign in to the fake → inject via fake admin → check `GET /v1/notifications`.
- Needs the developer's Mac/devices (cloud sessions can't): Android permission prompt, channels, UnifiedPush with
  ntfy and the FCM fallback with the app killed; desktop tray notification and dock badge; a check against a real
  Pixelfed instance (D20 local stack) to confirm notification type names.
