# HTTP API

The pod's core routes. Sources add their own: see [pixelfed.md](pixelfed.md#endpoints) and
[building.md](building.md#collections). Every call except those marked *public* needs
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
| `GET /v1/construct` | `{root, generation}`: home's element for the client's features. With the request header `x-construct-root: manifest` (decision `element-roles-and-reuse`), `root` is the hash of a manifest instead: `{"kind": "manifest", "home", "names": {name: hash}, "scale"?}`, naming every element home reaches at the entry of its history this client is served. The elements a manifest names hold references that follow a name without its hash (resolved through the manifest) and no examples, so an edit moves the edited element and the manifest, not every element above it. `scale` is the theme scale the pod set (`$theme.<name>` in components). For a client with feature `construct_swap`, `swaps` lists each recent edit group's names with their element before the group and after (`{group: {name: [before, after]}}`), which `POST /v1/objects` serves with the manifest, so the device swaps between them at once (decision `instant-swap`). The `construct` event and `GET /v1/construct/names/{name}` answer a manifest the same way (the latter with that name as its `home`). The engine asks for one; the web client and the simulator's live mode are sent home sealed. |
| `POST /v1/objects {have: [ids], want: [ids]}` | `{objects: {id: object}, more}`: what `want` reaches that `have` doesn't list, `want`'s order first; `more` when one answer couldn't carry all (ask again). |
| `GET /v1/objects/{id}` | One object (an id is `e1-` and the base32 SHA-256 of its canonical JSON): `{kind, title, icon, node, bind?, needs}`. A layout's `title` and `icon` are what devices show for it. |
| `GET /v1/construct/data/{id}?bind=<JSON>` | `{entities, queries}`: the data of the home feeds element `id` needs (its `needs.feeds` and those of home's feeds its tree binds now, so a core layout opened by its key gets the feeds home binds for it); with `bind` (a use site's bindings, `{"items": "collection:books"}`), also those its bound tree reads. Only home's feeds are loaded. An upstream that fails answers its source's `core://sources/<id>` entity with an `error`. |
| `GET /v1/construct/names/{name}` | `{name, el}`: a layout's element (by its name, or a core layout's key) for the client's features: a notification's layout that home doesn't reach is opened this way. `404 unknown_name`. |
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
| `notification` | `{id, channel_layout, channel, title, body, target_uri, layout, unread_total}` | A notification for any layout of the pod. `channel_layout` is the layout (or core layout) whose channel it is; `layout` is the layout to present it in. |
| `notifications_read` | the new unread counts | Notifications were marked read on some device. |

A `: keepalive` comment comes every 15 s. Streams opened without a session hear only `construct` events. A
session's stream ends when the session is revoked; one session may hold 8 streams (then 429 `too_many_streams`).

The pod's layouts are written through Claude (MCP `create_layout`, `update_layout`, `rename_layout`,
`delete_layout`, `restore_version`; [building.md](building.md#building-with-claude)): every write is
validated against the sources of home's feeds, and errors name a JSON pointer and what is allowed. Limits per pod: 50
layouts, 256 KiB per layout.

## Notifications and devices

| Call | Answer |
|---|---|
| `GET /v1/notifications?limit=0..100&before=<id>&layout=<id>` | `{notifications, unread_total, unread_by_layout, next_before?}`, newest first; switched-off channels are left out of both. Each notification may carry `subject`, the entity it is about. The same notifications are the rows of the `notifications` collection (`/v1/collections/notifications/rows`). |
| `POST /v1/notifications/read {"ids": [...]}` or `{"all": true, "layout"?: id}` | `{unread_total, unread_by_layout}`. |
| `GET /v1/notifications/channels` | `{channels: [{layout, id, title, description, default_enabled, importance, enabled}]}`; `layout` is the layout (or core layout) the channel belongs to; `enabled` is the channel's switch, for the whole pod (a row of the `notification_channels` collection). |
| `PUT /v1/devices/{id} {platform, name?, push?: {endpoint, p256dh, auth} \| null}` | Registers the device under the session; `push` sets or removes its Web Push subscription. A pod takes at most 20 new endpoints an hour (429 `rate_limited`); refreshing the one a device has doesn't count. |
| `PUT /v1/devices/{id}/channels {"<layout>.<channel>": bool}` | Turns channels on or off for the whole pod (`{id}` must be a registered device). |
| `DELETE /v1/devices/{id}` | Forgets the device. |
| `GET /v1/push/key` | `{vapid_public_key}`; 404 `push_unavailable` with `CONSTRUCT_PUSH=off`. |

How the pipeline works: [notifications.md](notifications.md).

## Uploads

| Call | Answer |
|---|---|
| `POST /v1/uploads` (raw body, `Idempotency-Key`) | `{upload_id}`. JPEG, PNG or WebP, checked by content; 413 over `CONSTRUCT_UPLOAD_MAX_BYTES`, 507 `upload_quota_exceeded` or `low_disk_space`. |
| `GET /v1/uploads/{id}` *public* | The file. The id is a 128-bit capability; it lives until a source releases it, or 24 h if never claimed. |
| `GET /v1/image?url=<url>` | A signed-in session: an image of a public `http(s)` site, fetched by the pod for the native clients as `/web/image` is for the browser (the device never fetches another host, issue #443); 400 for other URLs (a port other than the default too), 401 without a session; 429 `rate_limited` past 120 tries a minute per pod, counted with `/web/image`. |

How a create carries attachments: [posting.md](posting.md).

## Claude (MCP)

`POST /v1/mcp` is the pod's MCP endpoint, with OAuth discovery at `/.well-known/oauth-protected-resource` and
`/.well-known/oauth-authorization-server`, and `/oauth/register` (20 an hour per client address, 40 per IPv6 /48;
503 `temporarily_unavailable` when the data volume is nearly full), `/oauth/authorize` and `/oauth/token`. The
credential it issues reaches only `/v1/mcp`. See [building.md](building.md#building-with-claude). A tool's
answer is compact JSON (indenting it would about quadruple what a model reads); any tool call with `"pretty": true`
among its arguments gets it indented, for a person reading it.

The other way round, the pod is a client of remote MCP servers (decision [`mcp-jack`](decisions/mcp-jack.md)):
`GET /v1/mcp-servers/callback?code&state` *public* is where a server's OAuth sign-in comes back to (on a host,
under the pod's public prefix); it answers a short HTML page. See [building.md](building.md#mcp-servers).

## Web client

Only with `CONSTRUCT_WEB=1` (decision [`web-client`](decisions/web-client.md)); without it these paths are the pod's
like any other.

| Route | |
|---|---|
| `GET /web`, `GET /web/<file>`, `GET /web/fonts/<file>` *public* | The page, the built client and the renderer's fonts, from `CONSTRUCT_WEB_DIR` (plain file names only). The page's policy allows only its own scripts and WebAssembly. |
| `POST /web/session {code}` *public* | With `x-construct-client: web` (else 403 `web_client_only`): a new session of the pod `code` opens, as the cookie `construct_web` (`HttpOnly; SameSite=Strict; Path=/`, `Secure` behind HTTPS, as long as `CONSTRUCT_SESSION_TTL_DAYS`) -> `{ok}`; 401 `pod_not_found`; rate-limited like `POST /v1/pods/open`. |
| `POST /web/logout` | Deletes the browser's session (the cookie's, or a site's bearer) and clears the cookie. |
| `GET /web/site?origin=&return=&state=&challenge=` *public* | Only for an origin in `CONSTRUCT_SITE_ORIGINS` (decision [`site-origins`](decisions/site-origins.md)), a path `return` on it, and a PKCE S256 `challenge`: the page where the pod code is typed for that site; 400 otherwise. Its form (`POST /web/site`, rate-limited like `POST /v1/pods/open`) redirects (303) to `<origin><return>?construct_code=<code>&state=<state>`. |
| `POST /web/site/token {code, verifier}` *public* | From the site's `Origin` (else 403 `not_a_site`): the single-use code (5 minutes) and its PKCE verifier for a browser session of the pod -> `{session_token, expires_in}`, used as a bearer; 400 `invalid_grant`. |
| `GET /web/image?url=<url>` | With the cookie (or a site's bearer): an image of a public `http(s)` site, passed on (raster images only, not SVG, at most 10 MiB, sent with `nosniff` and a sandboxing CSP so one opened on its own runs nothing); 400 for other URLs, including a port other than the default; 429 `rate_limited` past 120 tries a minute per pod. |

The cookie stands for its session on every other route, but only on requests with `x-construct-client: web` and,
where the browser sends `Sec-Fetch-Site`, `same-origin`; a pod code put in the cookie is not taken, and `/v1/mcp`
answers it 403 `web_credential_scope`. Rotating the pod code ends every browser's session.

**Public links** (decisions [`public-web-links`](decisions/public-web-links.md),
[`public-links-share-the-closure`](decisions/public-links-share-the-closure.md)), only with `CONSTRUCT_WEB=1` and
`CONSTRUCT_PUBLIC_WEB=1`; anyone with the link reads them, and they never reach the pod. Each route also answers at a
chosen short path, `/<handle>/<slug>` (and `…/view`, `…/image`; the first form, `/@<handle>/<slug>`, redirects to it). `/<handle>/<slug>/<row>` is the page opening one row's screen, the row named by its name (decision `public-row-links`). With `CONSTRUCT_PUBLIC_WEB` unset they answer 503,
a page saying sharing is paused, and the copies stay stored. A page address with a trailing slash, `%40` for `@`, capitals or a `/web` prefix redirects (308) to its own address:

| Route | |
|---|---|
| `GET /s/<link>` *public* | The web client's page for the copy, titled with its title (`og:title`, `noindex`). |
| `GET /s/<link>/view` *public* | The copy, as stored: `{format: 2, layout, title, node, data, item?, params?, layouts?}`, the plumbing a visitor's page draws (the root and, by id, the layouts it opens and embeds) and the rows they show. A copy without `format` is format 1 (`{title, node, data, item?}`). 404 once revoked. |
| `GET /s/<link>/image?url=<url>` *public* | An image the copy names, passed on as `/web/image` does; a URL the copy doesn't name is 404. 429 `rate_limited` past 120 tries a minute per pod, counted apart from its own devices' `/web/image` and `/v1/image`. |

Pods make and revoke them with the MCP tools `share_publicly`, `list_public_links` and `unshare_publicly`
([building.md](building.md#public-links-for-people-without-a-pod)).

The host keeps only a SHA-256 of each share, public and calendar token (decision
[`link-tokens-hashed`](decisions/link-tokens-hashed.md)), so a backup holds no working link. A public or share link
is shown when it is made; later lists name it by its `id`, which `unshare_publicly` accepts. A page or feed of an
expired temporary pod answers 404 at once.

**Calendar feeds** (decision [`ical-feeds`](decisions/ical-feeds.md)), under the same settings: a calendar app
subscribes to the text the pod last rendered, never reaching the pod. With `CONSTRUCT_PUBLIC_WEB` unset it answers
503 and the feeds stay stored.

| Route | |
|---|---|
| `GET /ical/<token>.ics` *public* | The feed (`text/calendar; charset=utf-8`, RFC 5545: CRLF lines folded at 75 octets, times in UTC or whole days). 404 for an unknown, revoked or rotated token. |

Pods make, list, rotate and revoke them with the MCP tools `set_calendar_feed`, `list_calendar_feeds`,
`rotate_calendar_feed` and `revoke_calendar_feed` ([building.md](building.md#calendar-feeds)).

## Development and test routes

| Call | Answer |
|---|---|
| `POST /v1/pixelfed_sample_no_login/ack` | A stateless 200, for the `pixelfed_sample_no_login` app's buttons. |
| `POST /v1/test/fault {"status": 503, "count": 3}` | Only with `CONSTRUCT_TEST_HOOKS=1`: the next `count` POSTs answer `status` without being applied; `count: 0` clears it. |
| `GET /metrics` | Prometheus text, only on `CONSTRUCT_METRICS_ADDR`. |
