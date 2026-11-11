# HTTP API

The pod's core routes. Adapters add their own: see [pixelfed.md](pixelfed.md#endpoints) and
[custom-apps.md](custom-apps.md#collections). Every call except those marked *public* needs
`Authorization: Bearer <pod session>`. Errors answer `{"error": "<code>", ...}` with a matching status.

Clients also send `X-Construct-Features` (the [layout features](layouts.md#features-and-compatibility) they draw)
and `X-Construct-Device` (a client-made device id, used by notifications).

## Pods and sessions

| Call | Answer |
|---|---|
| `POST /v1/pods/open {"token": "cpod_…"}` *public* | `{session_token}`; 401 `pod_not_found`. 10 tries a minute per client address (IPv4 address or IPv6 /64), 50 per IPv6 /48. |
| `POST /v1/pods/temporary` *public* | `{session_token, expires_at, pod}`; 404 when disabled, 429, 503 `temporary_pods_full` or `low_disk_space`. |
| `GET /v1/auth/sign-ins` | Every upstream sign-in on the pod, and the ones it offers. |
| `POST /v1/auth/logout` | Deletes the session and its sign-ins. |
| `GET /healthz` *public* | `ok`. |

Images load without a bearer from `/v1/p/<pod id>/…`, which reaches only the paths adapters mark public (such as
the Pixelfed media proxy and uploads).

## Apps, layouts and the event stream

| Call | Answer |
|---|---|
| `GET /v1/apps[?all=1][&blueprints=1]` | `{apps: [{id, title, icon, kind, latest_version, latest_compatible_version, updated_at, updates_from?}]}`: the pod's own apps, `home` first. Hidden apps only with `all=1`; the blueprints (`kind: builtin`) only with `blueprints=1` (decision `blueprints-and-home`). |
| `GET /v1/apps/{id}/versions[?limit=1..100&before=N]` | `{app_id, latest_version, latest_compatible_version, versions: [{version, created_at, source, note?, features, compatible, incompatible?}], next_before?}`, newest first. |
| `GET /v1/apps/{id}/versions/{n}` | One version with its `layout`. |
| `PUT /v1/apps/{id}/hidden {hidden}` | Hides or shows any app in the app list. |
| `GET /v1/workspaces/{id}` | The app's bundle (`{workspace, entities, queries}`) at the newest version the client can draw, with `X-Construct-Layout-Version`. A stale session gets 401 with a signed-out bundle and `X-Construct-Session: invalid`. |
| `GET /v1/workspaces/{id}/events` | Server-sent events, below. |

`latest_compatible_version` is what opening the app serves this client (`null` if no version fits).
`incompatible` is `update_required` (the version uses a feature this client doesn't draw) or `retired` (it uses a
removed one). A request without `X-Construct-Features` is served the latest version.

**Events.** The stream's first message is the current version, then:

| Event | Data | When |
|---|---|---|
| `layout` | `{version, workspace_id}` | A newer version this client can draw exists. |
| `data` | `{workspace_id}`, and when the pod knows what changed: `source`, `changes` (Collections: `[{collection, rows}]`), `uri_prefixes`, `query_prefixes` | The app's data changed for this session (e.g. a post finished uploading). With prefixes, reload only data holding an entity URI that starts with a `uri_prefix`, or a query whose key is a `query_prefix` or starts with one and `:`; without, reload everything. The device named by a write's `x-construct-device` isn't sent the event of its own write when the pod changed nothing else (decision `scoped-data-events`). |
| `notification` | `{id, app_id, channel, title, body, target_uri, layout, unread_total}` | A notification for any app of the pod; `layout` is the layout to present it in. |
| `notifications_read` | the new unread counts | Notifications were marked read on some device. |
| `deleted` | `{workspace_id}` | The custom app was deleted. |

`home`'s stream carries every layout of the pod (decision `layouts-not-apps`): `layout` and `deleted` events of any
layout (by `workspace_id`), and `data` events for every source of home's feeds. Right after home's first message
(and again after the server drops events for a slow reader) it sends a `layout` event with every other layout's current
version and a `deleted` event for each deleted layout not purged yet. So a client that wasn't listening (in the
background, offline, the server restarting) catches up; it ignores layouts it hasn't opened and versions it has.

A `: keepalive` comment comes every 15 s. Streams opened without a session hear only `layout` events. A session's
stream ends when the session is revoked; one session may hold 8 streams (then 429 `too_many_streams`).

## Custom apps

The pod's own layouts (decision `layouts-not-apps`; the routes keep their app-era names). Home owns the feeds: `feeds`
on another layout are added to home's.

| Call | Answer |
|---|---|
| `POST /v1/custom-apps {title, icon, feeds?, note?, layout}` | Creates a layout; the pod sets the layout root's `id`; `feeds` join home's. |
| `GET /v1/custom-apps/{id}` | Its feeds (home's: the scope it binds) and latest layout. |
| `PUT /v1/custom-apps/{id}/layout {layout, note?, base_version?}` | A new version; 409 `stale_base` if `base_version` isn't the latest. |
| `PATCH /v1/custom-apps/{id} {title?, icon?, feeds?}` | Renames; on home, replaces the feeds (409 `feed_in_use` while another layout binds one it drops); on another layout, adds feeds to home's. |
| `POST /v1/custom-apps/{id}/restore {version}` | Appends a copy of an older version. |
| `POST /v1/custom-apps/{id}/dismiss-update {version}` | Stops reporting `updates_from` for a copied part. |
| `DELETE /v1/custom-apps/{id}` | Soft delete, purged after 30 days. |
| `POST /v1/custom-apps/validate {feeds, layout}` | A dry run. |

Every write is validated against the sources of home's feeds; errors name a JSON pointer and what is allowed. Limits per
pod: 50 apps, 500 versions per app (oldest pruned), 256 KiB per layout, 60 writes a minute.

## Notifications and devices

| Call | Answer |
|---|---|
| `GET /v1/notifications?limit=0..100&before=<id>&app=<id>` | `{notifications, unread_total, unread_by_app, next_before?}`, newest first. |
| `POST /v1/notifications/read {"ids": [...]}` or `{"all": true, "app"?: id}` | `{unread_total, unread_by_app}`. |
| `GET /v1/notifications/channels` | `{channels: [{app_id, id, title, description, default_enabled, importance, enabled}]}`; `enabled` is the calling device's choice. |
| `PUT /v1/devices/{id} {platform, name?, push?: {endpoint, p256dh, auth} \| null}` | Registers the device under the session; `push` sets or removes its Web Push subscription. |
| `PUT /v1/devices/{id}/channels {"<app>.<channel>": bool}` | The device's channel choices. |
| `DELETE /v1/devices/{id}` | Forgets the device. |
| `GET /v1/push/key` | `{vapid_public_key}`; 404 `push_unavailable` with `CONSTRUCT_PUSH=off`. |

How the pipeline works: [notifications.md](notifications.md).

## Uploads

| Call | Answer |
|---|---|
| `POST /v1/uploads` (raw body, `Idempotency-Key`) | `{upload_id}`. JPEG, PNG or WebP, checked by content; 413 over `CONSTRUCT_UPLOAD_MAX_BYTES`, 507 `upload_quota_exceeded` or `low_disk_space`. |
| `GET /v1/uploads/{id}` *public* | The file. The id is a 128-bit capability; it lives until an adapter releases it, or 24 h if never claimed. |

How a create carries attachments: [posting.md](posting.md).

## Claude (MCP)

`POST /v1/mcp` is the pod's MCP endpoint, with OAuth discovery at `/.well-known/oauth-protected-resource` and
`/.well-known/oauth-authorization-server`, and `/oauth/register` (20 an hour per client address, 40 per IPv6 /48;
503 `temporarily_unavailable` when the data volume is nearly full), `/oauth/authorize` and `/oauth/token`. The
credential it issues reaches only `/v1/mcp`. See [custom-apps.md](custom-apps.md#building-apps-with-claude).

## Development and test routes

| Call | Answer |
|---|---|
| `POST /v1/pixelfed_sample_no_login/ack` | A stateless 200, for the `pixelfed_sample_no_login` app's buttons. |
| `POST /v1/test/fault {"status": 503, "count": 3}` | Only with `CONSTRUCT_TEST_HOOKS=1`: the next `count` POSTs answer `status` without being applied; `count: 0` clears it. |
| `GET /metrics` | Prometheus text, only on `CONSTRUCT_METRICS_ADDR`. |
