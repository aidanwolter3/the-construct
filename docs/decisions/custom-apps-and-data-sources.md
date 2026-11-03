# Adapters are data sources; an app is a layout plus a list of feeds

Date: 2026-11-03. Related: D22–D25, D32, D34, D36–D38, D40, D46, D50, issue #45.

Every app's layout is a repo file, and an app's root data came from one adapter's `hydrate(app_id)`. A user can't
change how an app looks without a deploy, and no app can show two adapters' data side by side (a "Kitchen" app with
recipes next to Pixelfed posts). Issue #45 plans user-owned **custom apps**, stored in the pod and edited through
Claude, in five PRs (P1a, P1b, P1c, P2, P3); its PRD and architecture review were approved. This records the part
that changes the adapter interface, landed first as P1a, and the rules the later phases build on.

Considered: letting a custom app name any pod path (rejected: a layout could then call routes no layout was meant
to, such as `/v1/pods/*`), and composing data on the client (rejected: needs a client build and an engine that knows
several sources per app). The pod composing, against a declared manifest, keeps today's clients unchanged.

Decision:

- **A data source is an adapter seen from a layout.** `AppAdapter::manifest()` returns a `SourceManifest`
  (`pod_core::source`): a title, the sign-in its data needs, its entity URI schemes, its named **feeds** (with the
  query keys each fills), its screen **data paths** (`GET`, answering a `DataBundle`), its **actions** (`POST`
  endpoints for mutations and creates) and docs of its entities' fields. The source id is the adapter's `name()`.
  Patterns name variable parts `{name}`, always a whole path segment, query value or query-key segment
  (`/v1/pixelfed/screens/post?id={id}`, `post:{id}`).
- **`hydrate(app_id)` is replaced by `feed(feed_name)`.** `AppInfo` gains `feeds: &[(source, feed)]`, e.g.
  Pixelfed `[("pixelfed", "home")]` and Recipes `[("recipes", "all")]`. `StaticAdapter` has one feed,
  `static_app::FEED` (`all`), and declares its extra routes with `with_manifest`.
- **The pod composes.** `GET /v1/workspaces/:id` keeps the app-level `AuthPolicy::SignIn` check, then runs the app's
  feeds concurrently and merges their entities and queries in feed order. A feed whose source needs a sign-in the
  caller lacks is skipped. Any other answer than data ends the request as before (the first in feed order wins): a
  `Screen` is served; a `StaleSession` on a pinned session detaches that source's sign-in, otherwise 401; an `Error`
  passes through.
- **Data events are keyed by source.** `AppState::publish_data(source, session)`; every app that draws on the source
  (its feeds' sources plus its own adapter, `Catalog::app_sources`) gets `event: data`. The wire event is unchanged
  (`{"workspace_id": <app>}`).
- **Startup checks (`Catalog::new`):** valid manifests (names `[a-z0-9_]+`, valid patterns, unique feed names); entity
  schemes unique across sources, `core` reserved; query-key patterns of different sources don't overlap; a source's
  sign-in exists; every app feed names a declared feed of an existing source, once.
- **Capability check on layout files:** every screen `data` and `action.data.load` path of a layout file fits a data
  path, and every mutation and create endpoint fits an action, of the app's sources. The pod refuses to start
  otherwise, and the error names the path and what the sources offer. A manifest must therefore keep every path a
  deployed layout uses, like the routes themselves.

Built-in answers are byte-identical (bodies and headers), fixtures regenerate unchanged, and no URL, layout file or
`SCHEMA_VERSION` changed: no client build. The `pixelfed_demo` source reuses Pixelfed's shapes (`pixelfed://` URIs,
`timeline:home`), so it declares no entity scheme or query key of its own, and it is marked `dev`: custom apps may use
it only where dev apps are shown.

## Custom apps on the pod (P1b)

- **Storage:** core migration v15: `custom_apps(id, title, icon, feeds_json, created_at, updated_at, deleted_at)`;
  `app_versions` gains `author` (`user` | `claude`, NULL for files) and `meta_json`. Ids are `c-` plus 10 base32
  characters; built-in app ids may not contain `-` (checked at startup). Versions are ordinary `app_versions` rows
  (`source` `custom` or `restore`, the request's note), so serving, schema negotiation, SSE and the versions API work
  unchanged once the core resolves an app through `AppState::app(id) -> AppRef` (`Builtin` | `Custom`) instead of the
  static catalog. A version is recorded as written for its vocabulary's schema, raised to the oldest one current
  clients draw.
- **API** (`pod_core::custom`, README "Custom apps"): create, edit with compare-and-append (`base_version`, 409
  `stale_base`), rename/re-icon/re-feed (the latest layout must still fit), restore (appends a copy), soft delete
  (purged after 30 days by the hourly sweeper; open event streams get `event: deleted` and end), a dry-run validate,
  and hide/show for any app (`pod_settings` `apps.hidden`; `GET /v1/apps` lists hidden apps only with `?all=1`, and
  every row now has `kind` and `hidden`). The pod sets a written layout's root `id`.
- **Who writes:** the pod token's pinned session (later also the MCP credential). Any valid session may delete and
  hide, which is all clients do.
- **Validation** (`validate::custom_layout`): the structural checks; no unknown node type or intent; screen `data`
  and `action.data.load` paths fit the sources' data paths; mutation and create endpoints fit their actions; no
  `action.auth.*`; every `query_key` fits a key the sources fill; entity URIs (`entity_uri`, `${uri#field}`) use a
  declared scheme or `core://sources/…`; no other `/v1/` path anywhere (so not `/v1/auth/logout`, `/v1/devices`,
  `/v1/pods/*`, `/v1/uploads`). Errors carry the JSON pointer (`at`) and the allowed alternatives.
- **Composition** is lenient for custom apps: a failing feed is skipped and recorded; a stale sign-in on the pod
  token's session is detached and the rest served (any other session still gets the 401). Their root gets one
  `core://sources/<id>` entity per source, `{id, title, signed_in, error?}`. Built-in apps stay strict and get none.
- **Caps** (the same on temporary pods): 50 live apps, 500 versions per app (the oldest pruned, numbers keep
  counting), 256 KiB per layout (the existing layout limit), 60 writes a minute per pod (in memory).
- **App icons:** `photo`, `recipe`, `dashboard` (clients draw unknown names as a generic icon).

A custom layout can only call routes its sources declare, with the caller's own session: no more than curl with the
same token could do. A misleading button label is an accepted risk.

## Clients (P1c)

- **Apps list:** the engine lists `GET /v1/apps?all=1` (older pods ignore `all`); the menu marks custom apps
  **Custom** (also in the version picker), keeps hidden apps under "Show hidden apps (N)", and each row's ⋮ menu
  hides/shows any app and deletes a custom one after a confirmation (engine/FFI `set_app_hidden`,
  `delete_custom_app`). Pinning stays client-local.
- **A deleted app on screen:** the engine marks `layout_json` `{"app_id", "gone": true}` on the SSE `deleted`
  event, on a workspace 404, and (through a refetch) when its events stream gets a 404. The controller then opens
  the platform's default app (`ConstructController(defaultAppId = …)`) and shows "This app was deleted."; without
  one it stays on a placeholder that says so. No new snapshot field, so the FFI record is unchanged.
- **Sign in from Settings:** `GET /v1/auth/sign-ins` also lists `providers`, `{id, title, signed_in, intent?,
  server_placeholder?}`, from the new `AppAdapter::sign_in_start` (Pixelfed: the default instance's
  `action.auth.login`, plus a server box when other instances are allowed, D45). Settings shows "Sign in to
  <title>" for each one the session lacks and dispatches that intent. This closes a "Not yet" of decision
  `settings-page-and-sign-ins`.
- **Simulator:** the fixture catalog has a custom "Kitchen" app (`c-kitchen2dm`, `fixtures/custom_kitchen_bundle.json`);
  `shot` now draws popups (menus, dialogs) over the screen.
- `SCHEMA_VERSION` is unchanged (no new node or intent), but the app needs a new build for the menu changes; older
  builds keep working (they see custom apps as ordinary apps and can't delete them).

Claude edits custom apps through `POST /v1/mcp` with a host-issued, pod-scoped credential: decision
[mcp-connector](mcp-connector.md) (P2).

Custom layouts reuse built-in parts by reference (`core://Ref`), and copies hear about updates: decision
[layout-parts](layout-parts.md) (P3).

Custom apps keep data of their own (a todo list) in the Collections source, which Claude declares data in: decision
[collections](collections.md) (issue #54).

Not done yet:

- `action.auth.login` paths in built-in layout files are not checked against a manifest; none uses one today (the
  signed-out screens are code).
