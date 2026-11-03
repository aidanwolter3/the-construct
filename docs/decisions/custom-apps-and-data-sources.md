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
`timeline:home`), so it declares no entity scheme or query key of its own; P1b should not offer dev sources to
custom apps.

Not done yet (later PRs of issue #45):

- **P1b:** custom apps in the pod (`custom_apps` table, `/v1/custom-apps` API, `validate::custom_layout` with query
  key and entity scheme checks and no `action.auth.*`, caps, hidden apps, `/v1/apps` `kind`/`hidden`). With them,
  composition becomes lenient for custom apps (a failing feed is recorded and skipped) and adds
  `core://sources/<id>` = `{title, signed_in, error?}` so a layout can hint "Connect Pixelfed in Settings". Built-in
  apps stay strict and get no such entity, so their answers don't change.
- **P1c:** client support (Custom marker, delete, hide, sign in from Settings).
- **P2:** `POST /v1/mcp` for Claude with a host-issued, scoped credential (decision `mcp-connector`).
- **P3:** layout `parts` and `core://Ref`, `ref_update` versions, copies with an update notice (decision
  `layout-parts`).
- `action.auth.login` paths in built-in layout files are not checked against a manifest; none uses one today (the
  signed-out screens are code).
