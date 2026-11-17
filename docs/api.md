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
| `POST /v1/pods/temporary` *public* | `{session_token, expires_at, pod}`; 404 when disabled, 429, 503 `temporary_pods_full` or `low_disk_space`. The caller's address, its `X-Construct-Client` label and its `User-Agent` are recorded with the pod. |
| `DELETE /v1/pods/temporary` | With a temporary pod's token: deletes that pod now (slot and files) -> `{deleted}`; 401 without a live pod token, 403 `not_temporary` for an admin pod. |
| `GET /v1/auth/sign-ins` | Every upstream sign-in on the pod, and the ones it offers. |
| `POST /v1/auth/logout` | Deletes the session and its sign-ins. |
| `GET /healthz` *public* | `ok`. |

Images load without a bearer from `/v1/p/<pod id>/…`, which reaches only the paths sources mark public (such as
the Pixelfed media proxy and uploads).

## The construct and the event stream

The pod's layouts are a content-addressed construct (decisions `content-addressed-construct`, `one-layout-system`).
A client sends the features it draws in `X-Construct-Features`; the pod seals it, for each name, the newest element
that uses only those.

| Call | Answer |
|---|---|
| `GET /v1/construct` | `{root, generation}`: home's element for the client's features. |
| `POST /v1/objects {have: [ids], want: [ids]}` | `{objects: {id: object}, more}`: what `want` reaches that `have` doesn't list, `want`'s order first; `more` when one answer couldn't carry all (ask again). |
| `GET /v1/objects/{id}` | One object (an id is `e1-` and the base32 SHA-256 of its canonical JSON): `{kind, title, icon, node, bind?, needs}`. A layout's `title` and `icon` are what devices show for it. |
| `GET /v1/construct/data/{id}?bind=<JSON>` | `{entities, queries}`: the data of the home feeds element `id` needs (its `needs.feeds` and those of home's feeds its tree binds now, so a blueprint opened by its key gets the feeds home binds for it); with `bind` (a use site's bindings, `{"items": "collection:books"}`), also those its bound tree reads. Only home's feeds are loaded. An upstream that fails answers its source's `core://sources/<id>` entity with an `error`. |
| `GET /v1/construct/names/{name}` | `{name, el}`: a layout's element (by its name, or a blueprint's key) for the client's features: a notification's layout that home doesn't reach is opened this way. `404 unknown_name`. |
| `GET /v1/construct/events` | Server-sent events, below. |

A bearer the pod no longer knows gets 401 with `X-Construct-Session: invalid`, and the client forgets it. On a
server with many pods, a caller without a pod token is answered a one-element construct (a page that asks for a pod)
on these read calls, and `401 pod_required` elsewhere.

Objects are answered only from the pod's own store: an element another pod shared is readable only after this pod
adopted it (MCP `adopt`, decision [`sharing`](decisions/sharing.md)), and knowing another pod's hash grants
nothing. An adopted element is drawn like any other; its edge's `bind` maps its inputs to this pod's data (a
collection name, and field inputs `<input>__<field>` to this pod's field names).

**Events.**

| Event | Data | When |
|---|---|---|
| `construct` | `{root, generation}` | First, and whenever home's root for this client moves (any layout or component it reaches changed). |
| `data` | `{workspace_id: "home"}`, and when the pod knows what changed: `source`, `changes` (Collections: `[{collection, rows}]`), `uri_prefixes`, `query_prefixes` | Data of a source of home's feeds changed for this session (e.g. a post finished uploading). With prefixes, reload only data holding an entity URI that starts with a `uri_prefix`, or a query whose key is a `query_prefix` or starts with one and `:`; without, reload everything. The device named by a write's `x-construct-device` isn't sent the event of its own write when the pod changed nothing else (decision `scoped-data-events`). |
| `notification` | `{id, channel_layout, channel, title, body, target_uri, layout, unread_total}` | A notification for any layout of the pod. `channel_layout` is the layout (or blueprint) whose channel it is; `layout` is the layout to present it in. |
| `notifications_read` | the new unread counts | Notifications were marked read on some device. |

A `: keepalive` comment comes every 15 s. Streams opened without a session hear only `construct` events. A
session's stream ends when the session is revoked; one session may hold 8 streams (then 429 `too_many_streams`).

The pod's layouts are written through Claude (MCP `create_layout`, `update_layout`, `rename_layout`,
`delete_layout`, `restore_version`; [custom-apps.md](custom-apps.md#building-apps-with-claude)): every write is
validated against the sources of home's feeds, and errors name a JSON pointer and what is allowed. Limits per pod: 50
layouts, 256 KiB per layout.

## Notifications and devices

| Call | Answer |
|---|---|
| `GET /v1/notifications?limit=0..100&before=<id>&layout=<id>` | `{notifications, unread_total, unread_by_layout, next_before?}`, newest first; switched-off channels are left out of both. Each notification may carry `subject`, the entity it is about. The same notifications are the rows of the `notifications` collection (`/v1/collections/notifications/rows`). |
| `POST /v1/notifications/read {"ids": [...]}` or `{"all": true, "layout"?: id}` | `{unread_total, unread_by_layout}`. |
| `GET /v1/notifications/channels` | `{channels: [{layout, id, title, description, default_enabled, importance, enabled}]}`; `layout` is the layout (or blueprint) the channel belongs to; `enabled` is the channel's switch, for the whole pod (a row of the `notification_channels` collection). |
| `PUT /v1/devices/{id} {platform, name?, push?: {endpoint, p256dh, auth} \| null}` | Registers the device under the session; `push` sets or removes its Web Push subscription. |
| `PUT /v1/devices/{id}/channels {"<layout>.<channel>": bool}` | Turns channels on or off for the whole pod (`{id}` must be a registered device). |
| `DELETE /v1/devices/{id}` | Forgets the device. |
| `GET /v1/push/key` | `{vapid_public_key}`; 404 `push_unavailable` with `CONSTRUCT_PUSH=off`. |

How the pipeline works: [notifications.md](notifications.md).

## Uploads

| Call | Answer |
|---|---|
| `POST /v1/uploads` (raw body, `Idempotency-Key`) | `{upload_id}`. JPEG, PNG or WebP, checked by content; 413 over `CONSTRUCT_UPLOAD_MAX_BYTES`, 507 `upload_quota_exceeded` or `low_disk_space`. |
| `GET /v1/uploads/{id}` *public* | The file. The id is a 128-bit capability; it lives until a source releases it, or 24 h if never claimed. |

How a create carries attachments: [posting.md](posting.md).

## Claude (MCP)

`POST /v1/mcp` is the pod's MCP endpoint, with OAuth discovery at `/.well-known/oauth-protected-resource` and
`/.well-known/oauth-authorization-server`, and `/oauth/register` (20 an hour per client address, 40 per IPv6 /48;
503 `temporarily_unavailable` when the data volume is nearly full), `/oauth/authorize` and `/oauth/token`. The
credential it issues reaches only `/v1/mcp`. See [custom-apps.md](custom-apps.md#building-apps-with-claude).

The other way round, the pod is a client of remote MCP servers (decision [`mcp-jack`](decisions/mcp-jack.md)):
`GET /v1/mcp-servers/callback?code&state` *public* is where a server's OAuth sign-in comes back to (on a host,
under the pod's public prefix); it answers a short HTML page. See [custom-apps.md](custom-apps.md#mcp-servers).

## Development and test routes

| Call | Answer |
|---|---|
| `POST /v1/pixelfed_sample_no_login/ack` | A stateless 200, for the `pixelfed_sample_no_login` app's buttons. |
| `POST /v1/test/fault {"status": 503, "count": 3}` | Only with `CONSTRUCT_TEST_HOOKS=1`: the next `count` POSTs answer `status` without being applied; `count: 0` clears it. |
| `GET /metrics` | Prometheus text, only on `CONSTRUCT_METRICS_ADDR`. |
