# Architecture

The Construct is a server-driven UI system. A server, the **pod**, decides what every screen looks like and holds the
user's data and sign-ins. Native clients draw whatever the pod sends with one shared renderer and one shared Rust
engine, and know nothing about any particular app.

```
                         ┌──────────────── server (crates/server) ─────────────────┐
  Pixelfed, web pages,   │  pod-host: one process, many pods, routed by pod token   │
  the Messages API  ◀───▶│  ┌──────────── pod (crates/pod-core) ────────────────┐   │
                         │  │ layouts, versions, sessions, sign-ins, SQLite      │   │
                         │  │ adapters: app-pixelfed, app-recipes,               │   │
                         │  │           app-collections, app-events, app-monitor│   │
                         │  │           app-agents                               │   │
                         │  └────────────────────────────────────────────────────┘   │
                         └──────────────▲──────────────────────────▲──────────────────┘
                     HTTP: layouts +    │                          │  SSE: layout, data,
                     entities, actions  │                          │  notification events
                         ┌──────────────┴──────────────────────────┴──────────────────┐
                         │ engine (crates/client-core, Rust): SQLite cache, outbox,   │
                         │ sign-in, event stream; exposed through UniFFI (client-ffi) │
                         ├────────────────────────────────────────────────────────────┤
                         │ shared Compose UI (app/shared): draws UiNode trees     │
                         ├───────────────┬───────────────────┬────────────────────────┤
                         │ desktop (JVM) │ Android           │ headless simulator     │
                         └───────────────┴───────────────────┴────────────────────────┘
```

## The rule everything follows

**Only adapters know apps.** `pod-core`, `pod-host`, the engine, the FFI and the shared UI handle layouts, entities,
intents, uploads and notifications *in general* (decisions D34, D35, D40). Everything specific to Pixelfed lives in
`crates/app-pixelfed`; everything specific to recipes lives in `crates/app-recipes`; and so on. A test
(`the_core_names_no_app`) checks that the core names no app.

The payoff: most product changes ship from the pod alone, as a new layout file plus adapter code, with no client
release. Only new *vocabulary* (a node type, an intent, a field on one) needs a new client build.

## Pods and the host

One server process hosts many **pods**, one per user (D46). Each pod is its own SQLite database
(`pods/<id>/server.db`) holding that user's sessions, upstream sign-ins, layout history, custom apps and app data.
`host.db` lists the pods and the SHA-256 of each pod token. A request names its pod with its bearer token; without
one, a caller sees only the "Open your pod" screen. Temporary pods (24 h, deleted afterwards) let anyone try a
deployed server. Details: [server.md](server.md#pods).

## Adapters and data sources

An adapter implements `pod_core::AppAdapter` (`crates/pod-core/src/adapter.rs`). It can:

- declare **apps** (an id, a title, an icon, its layout directory, the feeds it shows);
- declare a **data source** (`SourceManifest`): named feeds, screen data paths, action endpoints, and documentation
  of its entity fields. Layouts may only call what a manifest declares (decision `custom-apps-and-data-sources`);
- add its own **routes** (sign-in, mutations, media proxies) and its own **tables** (`<adapter>_*`, migrated by the
  core);
- declare **notification channels** and poll an upstream for new notifications (D52);
- **resume** background jobs when a pod opens (D50).

The server binary picks the adapters at compile time (`server::adapters`). Apps in this repository:

| App id | Title | Adapter | What it is |
|---|---|---|---|
| `pixelfed` | Pixelfed | `app-pixelfed` | A Pixelfed client: feed, carousels, likes, bookmarks, threaded comments, photo posting, notifications |
| `pixelfed_sample_no_login` | Pixelfed sample | `app-pixelfed` (static) | A one-post sample that needs no sign-in; listed only with `CONSTRUCT_SHOW_DEV_APPS=1` |
| `recipes` | Recipes | `app-recipes` | A recipe collection with search, categories and recipe cards (D41) |
| `events` | Events | `app-events` | Upcoming events from websites the user watches, with alerts (decision `collection-fillers`) |
| `shell` | Home | `app-shell` | The home blueprint: a blank page with Notifications and Settings, which every pod's `home` references |
| `settings` | Settings | `app-shell` | Settings as a layout over device data: connection, layouts and versions, sign-ins, channels, server |
| `inbox` | Notifications | `app-shell` | The notification inbox as a layout over device data |
| `agents` | Agents | `app-agents` | Claude Code sessions, schedules, sandboxes and checks of an external agent runner ([agents.md](agents.md)) |

These are **blueprints** (decision `blueprints-and-home`): served by id and updated by deploys, but not listed.
There are no apps at runtime (decision `layouts-not-apps`): a pod has **layouts**, starting with `home`, the root
every client opens, which embeds (`core://Layout`) and presents (`action.navigation.open {layout}`) the others. A new
pod has only `home`, a reference to the `shell` blueprint; the user's agent adds layouts that reference or copy
blueprints. Home owns every feed; each layout binds any of them, and loads only the ones it binds.

Two more sources have no app of their own: **Collections** (`app-collections`), typed data that custom apps declare,
and **Server monitoring** (`app-monitor`). The **Agents** source (`app-agents`) follows an external agent runner
([agents.md](agents.md)). Users build their own apps on top of all of these; see
[custom-apps.md](custom-apps.md).

## Layouts

A layout is a tree of generic `core://` nodes (`protocol::UiNode`) with intents (`protocol::ActionIntent`) attached,
rooted at a `core://Page` (any node, no chrome from the app) or a `core://Workspace`, which can also declare
**screens** opened by route. Layouts bind to entities by URI and field (`$item.like_count`,
`${pixelfed://posts/1#caption}`) and never contain app logic.

- **Plumbing and porcelain** (decision `plumbing-and-porcelain`). Plumbing is what the app draws: the nodes, intents
  and device data, each a named feature. Porcelain is **components** defined as data in terms of plumbing and
  expanded by the pod (`pod_core::parts`, `pod_core::components`) before serving: the server's (`core:`, files in
  `crates/pod-core/components/` or an adapter's `crates/app-*/components/`, and every blueprint and its parts) and the pod's own (`pod:`, made by an agent). A
  `core://Workspace` is porcelain too: an app that draws the plumbing gets it as a `core://Page`
  (`core:workspace`, `core:screen`; `pod_core::porcelain`), older apps the workspace. The app has no workspace
  drawing of its own (decision `workspace-drawn-by-the-pod`).

- **Layouts are code.** Built-in apps' layouts are files, `crates/<adapter>/layouts/<app>/NNNN.json`, embedded in the binary and
  appended to each app's history at startup. Files are append-only: the pod refuses to start if a deployed one
  changed (D32).
- **Every version stays servable.** Each piece of vocabulary is a named **feature**
  (`crates/protocol/features/<name>.md`). A client sends the features it can draw in `X-Construct-Features`, and the
  pod serves it the newest version that uses only those (decision `named-vocabulary`). Old clients keep working on
  the last layout they can draw; new layouts reach them when they update.
- **Clients only read.** They can view an older version on their own device, but nothing changes a built-in layout
  over HTTP. Custom apps are the one writable kind, through the pod's API or Claude.

How to write one: [layouts.md](layouts.md).

## Wire bundle

`GET /v1/workspaces/<app>` answers a normalized bundle (`protocol::WorkspaceSpec`):

```json
{
  "workspace": { "type": "core://Workspace", "id": "pixelfed", "title": "Pixelfed", "slots": [], "screens": [] },
  "entities": { "pixelfed://posts/101": { "id": "101", "caption": "Sunrise over Millbrook", "is_liked": false } },
  "queries":  { "timeline:home": ["pixelfed://posts/101"] }
}
```

Entities are keyed by URI and shared across screens; queries are ordered lists of URIs that `core://List` draws. A
screen's `data` path answers the same `{entities, queries}` shape (`protocol::DataBundle`).

## The engine

`crates/client-core` (`ConstructEngine`) is the client's brain, written once in Rust and used by every client through
UniFFI (`crates/client-ffi`; Kotlin bindings in `app/shared/src/jvmCommonMain`).

- **Cache.** Every workspace, data load and entity is written to SQLite (`cache.db`), so the last screen shows at once
  on launch and offline (D13).
- **Optimistic actions and the outbox.** A tap applies its change to the cached entity and queues the request in the
  same transaction. A background worker sends queued rows oldest first. A 4xx rolls the change back (later queued
  changes are re-applied on top); a 5xx, 408, 429 or network error retries with backoff (2 s doubling to 60 s). A
  refetch re-applies changes still pending, so nothing visibly flickers (D14, D35).
- **Sign-in.** The engine runs OAuth2 PKCE for upstream sign-ins itself and keeps only a pod session in a platform
  secure store; upstream tokens never leave the pod (D17, D19).
- **One engine for every layout** (decision `layouts-not-apps`). The engine opens on the root layout (`home`);
  `open_layout` loads any other layout into the same store, cached like a data path (`layout:<id>`), and reports it
  in the snapshot's `layouts_json`.
- **Event stream.** While online, the engine keeps `GET /v1/workspaces/<root>/events` open; home's stream carries
  every layout's events, starting with every layout's current version so a reconnect catches up. `layout` events
  trigger a refetch of that layout when a newer drawable version exists;
  `deleted` marks it gone; `data` events say the data changed (D50); `notification` events feed the inbox (D52).
- **Snapshots.** The UI observes immutable state snapshots (layout, entities, connectivity, pending count, sign-in
  state, unread counts) and dispatches intents back as JSON.

## The clients

`app/shared/src/commonMain` is one Compose Multiplatform UI used by every client: the node renderer, the root layout
(`home`, or a bundled sign-in layout before a pod is open), and one back stack of presented layouts (pushed, sheets,
side panels, animated in and out) and screens, popped by system Back (decision `layouts-not-apps`). Settings and the
inbox are layouts too. Platform code is thin:

- **Desktop** (`:desktopApp`, JVM): loads the engine through JNA; the loopback OAuth redirect; tray notifications.
- **Android** (`:androidApp`): the engine cross-compiled to `libclient_ffi.so`; App Link sign-in redirect; Android
  notification channels and closed-app push through UnifiedPush.
- **Simulator** (`construct-sim`): the Rust engine's session drawn headlessly on the CPU to PNGs from a script,
  against fixtures or a live pod. It is how UI changes are checked; see [simulator.md](simulator.md). The Compose
  app's own simulator (`:desktopApp:simulate`) stays until the clients cut over (issue #166).

More on running them: [clients.md](clients.md).

**The Rust rendering engine** (`crates/construct-render`, decision `rust-render-engine`) draws the same plumbing
without the JVM: taffy layout, a scene of SDF quads and text, a CPU rasterizer and a wgpu pipeline. The pod uses it
for agents' layout previews over MCP; a desktop window and an Android NativeActivity are prototypes, run by the
same engine as the Compose clients (decision `rust-clients-on-the-engine`). See [rendering.md](rendering.md).

## Background work on the server

Pods open lazily on their first request. The host's scheduler wakes pods that have work due (`host.db`
`pods.next_wake_at`): notification polls, collection fillers that keep rows current from the web, and reminders. A
pod polls upstreams only while it has a device seen in the last 30 days, so idle and temporary pods cost nothing.

## Where decisions are recorded

Why things are the way they are: [DECISIONS.md](DECISIONS.md) (D1–D52, historical, numbered) and
[decisions/](decisions/) (newer ones, one file per topic). The original design document, written before any code,
is kept in [history/original-design.md](history/original-design.md); much of it no longer applies.
