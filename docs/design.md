# The Construct: design

This is the whole design of The Construct in one place, explained from the top down. It starts with what the project
is for, then names the big pieces and how they talk to each other, then opens each piece in turn: its goal, its
interfaces, and the smaller pieces it is made of. It assumes you know some programming, but nothing about this
codebase.

## How to read this

Read it breadth first. Part 1 is the goals, Part 2 the map of the big pieces, and Parts 3 to 8 each take one piece
apart. Part 9 is for working on the code. Each part is a short course of lessons; each lesson begins with a
one-paragraph summary, so reading only the summaries gives a fair picture. Terms are defined in the
[Glossary](#glossary) at the end.

The same lessons are a course you can take on a phone: **[Inside The Construct](https://the-construct-pod.fly.dev/aidan/design)**,
drawn by The Construct itself from this file (see [Keeping this current](#keeping-this-current)).

This document explains how things fit. The guides go deeper on each subject: [layouts.md](layouts.md) (the layout
vocabulary), [building.md](building.md) (layouts, Collections, sharing, jacks), [api.md](api.md) (the HTTP
API), [server.md](server.md), [clients.md](clients.md), [rendering.md](rendering.md), [agents.md](agents.md),
[taste.md](taste.md), [notifications.md](notifications.md), [testing.md](testing.md) and [simulator.md](simulator.md). Why each choice
was made is in the decisions: [DECISIONS.md](DECISIONS.md) (D1–D52) and [decisions/](decisions/).

## Part 1 · Why it exists

The Construct is a way to have native apps that are entirely your own: their screens and data live on your own
server, and your agent builds them by asking.

### 1.1 The idea in one page

The Construct is a server-driven UI system: a server you own, the pod, decides what every screen of your apps looks
like and holds your data, while native clients on your phone and computer only draw what the pod sends. Your agent
(Claude) builds the screens by talking to the pod, so a new screen needs no app release.

> **Why it matters:** Every other part of the design follows from this split: the pod knows everything about your
> data, the clients know nothing about it, and the agent is how things get built.

#### The problem

Most personal software is someone else's app: its screens, its data model, its server. If you want a grocery list
that knows your recipes, a log of your car's maintenance, and your photo feed in one place, you either accept
several apps that don't talk to each other or you write and ship your own app, for every platform, and keep it
updated.

#### The answer

Split an app into three parts and give each one a single owner:

- **What the screens look like and what data they show** lives on a server you own, the **pod**, as data: JSON
  trees called **layouts**.
- **Drawing** happens on your devices, in one native renderer that knows how to draw any layout but nothing about
  any particular app.
- **Building** is done by your **agent**: Claude connects to the pod and writes layouts and data definitions for
  you, checked by the pod on every write.

So changing your grocery screen means the agent writes a new layout to the pod, and every device shows it a moment
later. No app store, no build, no release.

#### In one picture

```
 your agent (Claude)
        │ MCP
        ▼
 pod (your server) ◀──▶ the web,
        │               Pixelfed, …
        │ layouts + data,
        │ live events
        ▼
 engine + renderer on each device
 (desktop, Android, web, simulator)
```

### 1.2 The goals

The design serves six goals; when two pull against each other, this order is roughly how they are weighed.

#### Your data stays yours

Each user has their own pod: one SQLite database with their data, their layouts and their sign-ins to other
services. Tokens for those services are sealed at rest and never reach a device. A pod can be exported whole, and a
collection's data can be exported row by row.

#### Native and offline first

Screens are drawn natively by a Rust renderer (GPU or CPU), not in a web view. Everything a device has loaded is
cached, so the app opens and scrolls with no network. A tap changes the screen at once and is queued to be sent when
the network returns.

#### Most changes ship from the server alone

The core of the system is generic: it handles layouts, entities and actions in general, never a particular app. A
new screen or a new data source is server code and layout files, so it reaches users without a client release. Only
new **vocabulary** (a new kind of node the renderer must learn to draw) needs a new client build.

#### Your agent builds, only what you ask

A new pod is a blank home screen. Your agent adds only what you ask for, through the pod's MCP server. The pod
validates every write, keeps every version so any change can be undone, and draws previews so the agent can look at
what it made.

#### Old clients keep working

A phone that hasn't updated must still draw something sensible. Every piece of vocabulary is a named **feature**;
a client says which features it can draw, and the pod serves it the newest version of each layout that uses only
those.

#### Safe by default

The pod reaches other hosts only through a client that refuses private network addresses. Text that other people
wrote (web pages, shared layouts, tool output) is marked as data, never instructions, before an agent sees it.
Anything shared with another person is reviewed first.

### 1.3 What it is not

Knowing what was left out makes the rest easier to read: there are no apps at runtime, no code in layouts, and no
web view.

#### No apps, only layouts

There is no app store and no "installing an app". A pod has **layouts**, starting with `home`, which the clients
open first; home embeds or opens every other layout. Built-in screens (Pixelfed, Events, Settings, the inbox) are
**core layouts** the server ships, which your layouts reference or copy (decisions `layouts-not-apps`,
`blueprints-and-home`).

#### No logic in layouts

A layout says what to draw and what each tap sends, nothing more. It binds fields of entities (`$item.title`), shows
or hides things on simple conditions, and attaches **intents** ("change this field and send it to this endpoint").
Anything that needs real logic lives in a source on the pod, in Rust.

#### No web view

The renderer is Rust from layout to pixels. The web client is the same renderer compiled to WebAssembly and drawn
into a canvas; it is not HTML.

## Part 2 · The big pieces

A handful of big pieces make up the system: the pod on its host, the sources and jacks inside it, the construct
of layouts, the engine and renderer on each device, and the agent that builds.

### 2.1 The map

The system is a server half and a device half joined by HTTP, plus an agent that talks to the server over MCP. The
server half is the pod and the abilities it is plugged into; the device half is one engine and one renderer shared
by every client.

#### The whole system

```
 agent (Claude)    websites, Pixelfed,
      │ MCP        models, runners
      ▼                  ▲
┌─ server ───────────────┼───────┐
│ host: pods, tokens, scheduler  │
│ ┌─ pod ──────────────────────┐ │
│ │ the construct · sessions · │ │
│ │ notifications              │ │
│ │ sources: collections,      │ │
│ │ pixelfed, events, agents,  │ │
│ │ taste, shell, monitor,     │ │
│ │ dates                      │ │
│ │ jacks: model, web,         │ │
│ │ web_search, mcp, accounts  │ │
│ └────────────────────────────┘ │
└──────▲─────────────────┬───────┘
       │ HTTP: construct,│ SSE:
       │ objects, data,  │ construct,
       │ actions         ▼ data, …
┌─ each device ──────────────────┐
│ engine: cache, outbox,         │
│ sign-in, event stream          │
├────────────────────────────────┤
│ renderer: layout → pixels      │
├────────────────────────────────┤
│ desktop · Android · web ·      │
│ simulator · previews           │
└────────────────────────────────┘
```

#### The pieces in one line each

| Piece | Crates | Its job |
|---|---|---|
| **Host** | `pod-host`, `server` | One server process holding many pods: which token opens which pod, temporary pods, waking pods that have work due, what pods share |
| **Pod** | `pod-core` | One user's server: their layouts, data, sessions, sign-ins and notifications, and the HTTP and MCP endpoints |
| **Sources** | `source-*` | Everything specific to one kind of data: Collections, Pixelfed, Events, Agents, … The pod knows them only through one trait, `Source` |
| **Jacks and modules** | `pod_core::jacks`, `web-extract`, `agent` | The abilities a pod is plugged into (a model, the web, search, accounts), and the two modules every web request and model call go through |
| **The construct** | `construct-elements`, `protocol` | The pod's layouts and components as content-addressed elements, and the vocabulary they are written in |
| **Engine** | `client-core` | The device's brain: a local cache, an outbox of changes, sign-in, and the live event stream |
| **Renderer and clients** | `construct-render`, `construct-web`, `construct-sim` | Draws any layout; the desktop, Android and web apps, the headless simulator and the pod's previews are all this one renderer |
| **Agent** | (outside) | Claude, connected to the pod's MCP server, builds layouts and collections |

### 2.2 Following one screen

Opening the app shows how the pieces cooperate: the engine asks the pod for the root of its construct, fetches the
elements it lacks, loads the data they bind, and hands both to the renderer, which draws them.

#### Step by step

1. **The device opens a pod.** The user typed a pod code once; the engine traded it for a pod session
   (`POST /v1/pods/open`) and keeps that session in the platform's secure store.
2. **It asks for the root.** `GET /v1/construct` answers the hash of `home` as this device can draw it, given the
   features it sent in `X-Construct-Features`.
3. **It fetches what it lacks.** `POST /v1/objects {have, want}` answers the elements (layouts, components) home
   reaches that the device doesn't already hold. Elements are immutable and named by hash, so anything already
   cached is never fetched again.
4. **It loads the data.** `GET /v1/construct/data/<element>` answers a **data bundle**: the entities the layout
   binds, keyed by URI, and the queries (ordered lists of URIs) its lists draw. The pod asks each source for its
   feeds.
5. **It draws.** The renderer lays the layout out, binds each `$item.field` to the cached entities, and paints.
6. **It stays live.** `GET /v1/construct/events` stays open: a `construct` event when any layout changes, a `data`
   event when data changes, a `notification` event when something arrives.

> **Try it:** Run `cargo run -p construct-sim -- --workspace pixelfed_sample_no_login --out /tmp/sim --script "shot start"`
> and open `/tmp/sim`: that is steps 4 and 5 drawn from a fixture instead of a pod.

#### What each piece knew

The renderer knew layouts and entities, never "Pixelfed". The engine knew URLs, hashes and JSON, never what a post
is. The pod knew which source fills each feed; only the source knew how to talk to the outside service.

### 2.3 Following one tap

Liking a photo shows the other direction: the renderer turns a tap into an intent, the engine applies it at once and
queues it, the pod routes it to a source, and the source talks to the outside service.

#### Step by step

1. **The tap.** The layout's heart button carries an intent:
   ```json
   {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri", "field": "is_liked",
    "value_expr": "!entity.is_liked", "endpoint": "/v1/pixelfed/posts/like", "payload": {"id": "$item.id"}}
   ```
   The renderer binds `$item` and hands the intent to the engine.
2. **Optimistic change.** The engine flips `is_liked` in its cache and writes the request to its **outbox** in the
   same database transaction. The screen redraws: the heart is red before any network call.
3. **Sending.** A background worker sends outbox rows oldest first. A 2xx removes the row; a 4xx removes it and
   rolls the change back; a 5xx, a timeout or no network keeps it and retries with backoff. A row the pod has kept
   answering with an error for 10 minutes is given up (rolled back like a 4xx) so the rows behind it are sent, and
   the device says which (decision `failing-saves-give-up`).
4. **The pod.** The route `/v1/pixelfed/posts/like` belongs to the Pixelfed source, which the pod mounted. The
   source calls Pixelfed's API with the token it keeps sealed in the pod.
5. **Back to every device.** The source tells the pod its data changed; the pod sends a `data` event to every open
   stream, and other devices reload just that data.

> **Try it:** In the simulator, `click Like; assert-text Unlike; shot liked` runs this whole path against a fake pod.

### 2.4 The seams between pieces

The pieces meet at a handful of interfaces, and each one is narrow on purpose: knowing these seams is most of
knowing the architecture.

#### The interfaces

| Seam | Between | What crosses it |
|---|---|---|
| **The `Source` trait** (`pod_core::Source`) | pod ↔ each source | A manifest of feeds, data paths and actions; feed data; routes; tables; notifications; jacks; core layouts and components |
| **The construct API** (HTTP) | engine ↔ pod | `GET /v1/construct` (the root), `POST /v1/objects` (elements by hash), `GET /v1/construct/data/<el>` (a data bundle), source routes for actions |
| **The event stream** (SSE) | pod → engine | `construct`, `data`, `notification` and `notifications_read` events |
| **Features** (`X-Construct-Features`) | engine → pod | The vocabulary a device can draw, so the pod serves layouts it can draw |
| **The layout vocabulary** (`protocol`) | pod, engine, renderer | Node types (`core://Text`, `core://List`, …), intents (`action.*`), bindings, device data |
| **MCP** (`POST /v1/mcp`) | agent ↔ pod | Tools to read sources, write layouts and collections, preview, share |
| **`Loader` and `Platform`** (`construct-render`) | renderer ↔ its host | How the renderer gets layouts and data, and what it asks of the operating system |
| **Jacks** (`pod_core::jacks`) | features ↔ abilities | Whether the pod may use a model, the web, search or an account, and their settings |

#### Why narrow seams

Each seam is a promise that lets one side change without the other. A source can change everything about how it
talks to Pixelfed without touching the pod. The pod can change how it stores layouts without touching the engine,
as long as the construct API holds. A renderer can learn new tricks without the pod knowing, as long as it says so in
its features.

### 2.5 The rules everything follows

Three rules keep the pieces apart; most of the codebase's shape follows from them, and CONTRIBUTING.md lists them
as rules to keep.

#### The core knows no source

The engine, the renderer and `pod-core` handle layouts, entities and intents in general (decisions D34, D35, D40).
Everything specific to Pixelfed is in `crates/source-pixelfed`, and so on; a test (`the_core_names_no_source`)
fails if the core names a source. That is what lets product changes ship without client releases.

#### Plumbing and porcelain

The vocabulary has two layers (decision `plumbing-and-porcelain`). **Plumbing** is what the renderer draws: nodes,
intents and device data, each a named feature, and adding to it needs a client build. **Porcelain** is
**components**: pieces of layout defined as data in terms of plumbing, expanded before drawing, so adding or
changing one needs no client build.

#### History is kept, not rewritten

Core layouts and server components are append-only numbered files (`NNNN.json`): a deployed file is never edited,
and the server refuses to start if one changed. The pod keeps every version of every layout behind its name.
Database schemas change only by migrations that work on existing pods. Decisions are recorded, never rewritten.

## Part 3 · The pod and its host

The pod is one user's server; the host runs many pods in one process. This part opens both.

### 3.1 The pod's job

A pod is everything one user has on the server: their layouts, their data, their sessions and sign-ins, and their
notifications, all in one SQLite database. Its job is to serve layouts and data to that user's devices and agent,
and to be the only place that talks to the outside world on their behalf.

> **Why it matters:** Because the pod holds the sign-ins and does the fetching, devices never hold another
> service's token and never reach another host directly.

#### What a pod holds

Each pod is `pods/<id>/server.db`:

- **The construct**: every layout and component element, and the names that point at them with their history.
- **Sessions**: the pod sessions of devices and of the web client, and MCP credentials for agents.
- **Sign-ins**: tokens for upstream services (a Pixelfed account, an agent runner), sealed with the server's key.
- **Source data**: each source's own tables (`<source>_*`): a collection's rows, Pixelfed's cursors, the Agents
  mirror.
- **Notifications** and the devices registered for push.
- **Jacks**: which abilities are plugged in, with sealed settings.

#### What a pod serves

- **To devices**: the construct API and event stream, the data bundles, the sources' action routes, uploads,
  notifications.
- **To the agent**: the MCP server at `/v1/mcp`.
- **To nobody else**: a caller without a pod token sees only an "Open your pod" screen.

### 3.2 The host and the server binary

The host (`crates/pod-host`) is what lets one server process serve many pods: it maps tokens to pods, makes
temporary pods, wakes pods that have background work due, and holds what pods share. The `server` crate is the
binary that puts the host and the sources together.

#### host.db

The host's own database lists the pods and what crosses between them:

- `pods`: each pod's id, name, kind (`admin` or `temporary`), expiry, and `next_wake_at`.
- `credentials`: the SHA-256 of each pod token (`cpod_…`), MCP credential and web session. Tokens themselves are
  never stored.
- The **shelf**: handles, publications and their objects (what pods share with each other).
- `public_copies` and `calendar_feeds`: public web links and `.ics` feeds.
- OAuth clients and codes, for agents that connect with OAuth.

#### What the host does

- **Routing**: a request's bearer token names its pod; the host opens that pod lazily on its first request.
- **Temporary pods**: `POST /v1/pods/temporary` makes a 24-hour pod for trying a server, deleted afterwards, with
  risky jacks locked off.
- **The scheduler**: every 30 seconds it opens pods whose `next_wake_at` is due (notification polls, collection
  fillers, reminders). A pod polls upstreams only while it has a device seen in the last 30 days, so idle pods cost
  nothing.
- **OAuth for agents**, connected apps, public media under `/v1/p/<pod>/…`, the web client at `/web` (also loaded
  by the owner's own sites, decision `site-origins`), and public links at `/s/…` and `/<handle>/<slug>`.

#### The server binary

`crates/server` compiles in the sources (`server::adapters()`: Pixelfed, Events, the shell layouts, Collections,
Monitor, Agents, Dates) and runs `server serve`. Its other subcommands manage pods: `server pods create`, `rotate`,
`delete`, `mcp` (an MCP credential), `monitoring on`, `gaps`, and `server backup <dir>`. Configuration is
`CONSTRUCT_*` environment variables ([server.md](server.md)).

> **Try it:** Make a temporary pod on staging (`curl -fsS -X POST https://the-construct-pod.fly.dev/v1/pods/temporary`)
> and read its construct with `curl -H "Authorization: Bearer <token>" https://the-construct-pod.fly.dev/v1/construct`.
> Delete it afterwards with `DELETE /v1/pods/temporary`.

### 3.3 Inside pod-core

`pod-core` is the generic pod: about thirty modules, none of which knows any particular source. Grouping them by
job makes it easy to find your way.

#### The modules by job

| Job | Modules |
|---|---|
| Sources | `adapter` (the `Source` trait), `source` (manifests, matching paths and query keys), `static_app` (a source that is only layouts) |
| The construct | `construct` and `construct_*` (store, write, seal, events, HTTP, share, adopted, trace), `components`, `porcelain`, `layouts`, `validate`, `hints` |
| Devices | `session` (pod sessions, sign-ins), `notify`, `notify_store`, `push` (Web Push), `devices`, `uploads`, `image_proxy` |
| Agents | `mcp` (the pod's MCP server), `preview` (drawing layouts for the agent), `mcp_servers` (the pod as an MCP client) |
| Abilities | `jacks`, `net` (the outbound HTTP client), `secrets` (sealing) |
| Sharing outward | `public_copy` (public links), `ical` (calendar feeds) |
| Plumbing | `store` and `migrations` (SQLite and named migration steps), `tz`, `metrics`, `disk` |

#### The router

`pod_core::app` builds one axum router: the core routes (`/v1/construct…`, `/v1/objects`, `/v1/notifications…`,
`/v1/devices…`, `/v1/uploads`, `/v1/jacks…`, `/v1/mcp`, `/v1/auth…`) plus each source's own router under
`/v1/<source>/…`. Every handler gets the same state, `Pod` (an `Arc<AppState>`), which also gives a source its store,
its catalog of sources, per-pod values, and `publish_data` to say its data changed.

### 3.4 Background work

Pods are opened lazily, so work that can't wait for a request is driven by the host's scheduler, and a source says
when it needs the pod to stay open.

#### Polls

A source that pulls from an upstream implements `Source::poll_interval` and `poll_notifications`. Pixelfed polls
its notifications every two minutes; Collections uses the same poll to run scheduled fills and send due reminders;
Monitor checks its health every five. After a poll the pod records when it next needs waking in `host.db`
(`next_wake_at`). Failed polls back off from a minute to an hour.

#### Resuming and staying open

`Source::resume` restarts background jobs when a pod opens (an upload that was still posting). `Source::keeps_open`
asks the host to keep the pod open from server start: the Agents source does, to follow its runner's event stream
without waiting for a request.

### 3.5 Security by construction

The pod is the one place secrets and outside text meet, so the defaults are strict and most of them are enforced in
one spot rather than left to each feature.

#### Secrets

Pod tokens are stored as hashes. Upstream tokens and jack secrets are sealed at rest with the server's key
(`CONSTRUCT_SECRETS_KEY`) and never sent to a device. A credential issued to an agent reaches only `/v1/mcp`, and
only at the access its owner gave it: read, edit, or outward (sharing publicly, starting agent runs, reaching new
hosts, deleting). Every tool is checked against it before it runs, and rows the agent writes below outward count as
the agent's, not the owner's (decision `mcp-access-levels`). A credential for an unattended job can carry a
**grant** as well: its exact tools, the collections whose rows it may write, where it may delete, and screens it may
never change. The pod enforces it inside every write transaction (decision `mcp-grants`).

#### Outbound requests

Every request a pod makes to a host a user chose goes through `pod_core::net`'s outbound client, which resolves
names itself and refuses private and internal addresses; a lint (`clippy.toml`) forbids building another HTTP client.
Web pages are fetched through the web module, which adds per-host spacing, daily caps and backoff (Part 6).

#### Outside text is data

Text other people wrote (a web page, a shared layout, an MCP server's tool description) reaches the agent wrapped as
`{"untrusted": {"from", "content"}}`, and the pod's instructions say it is data, never instructions (decision
`untrusted-tool-content`). Sharing anything outward (a publication, a public link, a calendar feed) answers a
review of exactly what would go out, and nothing goes until it is confirmed.

## Part 4 · Layouts and the construct

A layout is a screen written as data. This part covers the vocabulary layouts are written in, the components built
on it, and the construct: how a pod stores layouts so devices fetch only what changed.

### 4.1 A layout is data

A layout is a JSON tree of generic nodes (`core://Column`, `core://Text`, `core://List`, …) with intents attached to
the parts you can tap. It binds to data by URI and field, and contains no code.

#### A small layout

```json
{"type": "core://Page", "id": "c-groceries1", "title": "Groceries",
 "root": {"type": "core://Column", "padding_h_dp": 20, "children": [
   {"type": "core://Text", "style": "Title", "value_expr": "Groceries"},
   {"type": "core://List", "query_key": "collection:groceries",
    "where": {"field": "deleted", "equals": "false"},
    "item_template": {"type": "core://Row", "children": [
      {"type": "core://Switch", "checked": "$item.done",
       "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "done", "value_expr": "!entity.done",
                    "endpoint": "/v1/collections/groceries/update",
                    "payload": {"id": "$item.id", "done": "$value"}}},
      {"type": "core://Text", "value_expr": "$item.name"}]}}]}}
```

The list draws every entity of the query `collection:groceries`; each row binds `$item` to one entity; the switch's
intent flips `done` at once and sends the change.

#### Pages and screens

A layout's root is a `core://Page`: any node, with nothing drawn around it by the app, so the layout draws its own
header and Back. A page can declare **screens**, opened by route: a tap with `target_uri:
"collections://groceries/r1"` opens the screen whose `route` is `collections://groceries/{id}`, and `$route.id` is
`r1` inside it. Older layouts may be a `core://Workspace`, which is porcelain the pod expands into a page.

> **Try it:** Ask your agent to `describe_vocabulary`, or read `crates/protocol/src/ast.rs`: every node type is one
> enum variant.

### 4.2 Plumbing: the vocabulary

Plumbing is what the renderer can draw: about thirty node types, a dozen intents, a small binding language and a set
of device data. It lives in `crates/protocol`, and each piece is a named feature.

#### Nodes

| Family | Nodes |
|---|---|
| Structure | `Page`, `Layout` (another layout, embedded or as a sheet or panel), `Scroll`, `Sticky`, `Dock`, `Drawer`, `AppFrame` |
| Arrangement | `Column`, `Row`, `Box` (a surface with padding, colour, border, corners), `Container`, `Spacer`, `Divider` |
| Content | `Text` (with styles, dates, counts and Markdown), `Image`, `Badge`, `Chip`, `Progress`, `Bars` (a chart) |
| Data | `List` (a query's entities through a template, filtered, grouped, in columns or a stack), `Carousel`, `Conditional` |
| Controls | `Button`, `IconButton`, `Tappable`, `Gesture`, `SwipeCard`, `Switch`, `TextField`, `ChoiceChips`, `MediaPicker` |

All are `core://<Name>`. A node type a client doesn't know is skipped and kept, so old versions still parse.

#### Intents

`action.navigation.open` (a screen, or another layout pushed, as a sheet, panel or overlay), `.back`, `.pop_to`,
`.pop_to_root`; `action.mutation.optimistic` and `action.mutation.create` (change or add data at once, sent through
the outbox); `action.local.set` (view state on this device only); `action.form.clear` (empty form boxes after an
Add, typing going on); `action.sequence`, `action.confirm`, `action.link.open`, `action.data.load`; `action.device`
(something only the device does: sign in, sign out, mark notifications read, leave the pod).

#### Bindings and device data

`$item.<field>` reads the current row, `${<uri>#<field>}` any entity, `$route.<name>` a screen's route,
`$form.<field>` a text field's value, `$nonce` a fresh id per tap. Mutations compute the new value with a tiny
expression language (`!entity.done`, `entity.count + 1`, `a ? b : c`). **Device data** is what the device knows
itself, with no pod request: `local://connection` (online, pending changes), `local://inbox`, `local://theme` (the
pod's colours), `local://device` (window size), `local://nav` (where the layout is drawn).

### 4.3 Porcelain: components

A component is a reusable piece of layout with parameters, defined as data and expanded into plumbing before it is
drawn, so a whole design system can change with no client release.

#### Using one

```json
{"type": "core://Use", "ref": "@core/card", "args": {"spacing": 6}, "children": [
  {"type": "core://Text", "style": "Label", "value_expr": "$item.title"}]}
```

Names say where a component comes from:

- `@core/<name>`: shipped with the server, in `crates/pod-core/components/<name>/NNNN.json` or a source's own
  `components/` folder. Every core layout is one too (`@core/events`), and so is each of its parts
  (`@core/pixelfed/screen:post`).
- `@me/<name>`: the pod's own, defined by the agent (`define_component`) or copied from a core one
  (`copy_component`).

Older layouts spell these `core:<name>` and `pod:<name>`; a write still accepts them.

#### Defining one

A definition is `{doc, params, children?, template, examples}`. In the template, `$arg.<name>` is an argument,
`core://Children` is where the caller's children go (or a named slot), `core://IfArg` and `core://Match` choose a
branch, and `core://Each` repeats a piece per entry of a list argument. Every example must expand to valid plumbing,
or the write is refused. `crates/construct-elements` does the expanding, for the pod and the engine alike.

### 4.4 The construct

The construct is how a pod stores its layouts: every layout and component is an immutable **element** named by the
hash of its JSON, **names** point at elements and keep history, and `home`'s hash stands for the whole tree. A device
is up to date exactly when its root hash matches the pod's.

> **Why it matters:** Content addressing makes sync cheap and exact: a device fetches only the elements it hasn't
> seen, and an edit moves only the names it touched.

#### Elements, names and references

- An **element** is `e1-` and the base32 SHA-256 of its canonical JSON (keys sorted, no whitespace). It never
  changes.
- A **name** (`home`, `c-…` for the pod's layouts, `@me/…`, `@core/…`) points at an element and keeps every element
  it pointed at before: that is the undo history (`restore_version`).
- A **reference** sits at one of three sites: a `core://Use` (a component, expanded in place), a `core://Layout` (an
  embed) and an `action.navigation.open` (a link). Written as a name, it is **sealed** to `{el, name, pin}`. An
  unpinned reference follows its name: when the name moves, the pod seals every element that follows it again, up to
  home.

#### Edges and reach

Embeds and links are **edges**, keyed by their `as` or their name. From home, the edges say which layouts a user can
reach and how (MCP `list_layouts` answers each layout's `reached` path). Writes that would make a layout reach
itself are refused, so the graph only goes forward; Back and `pop_to` go the other way.

#### How a device syncs

The engine asks for the root as a **manifest** of names and hashes, fetches the objects it lacks with `POST
/v1/objects`, keeps them in its store (a restart with the same root fetches nothing), and expands components itself
with `construct-elements`. A layout home doesn't reach (one a notification opens) is fetched by name with `GET
/v1/construct/names/<name>`.

> **Try it:** Ask your agent for `get_layout home`: its `history` is the list of elements the name `home` has
> pointed at, newest first.

### 4.5 Features: keeping old clients working

Every piece of vocabulary is a named feature, and the pod serves each client the newest version of each name that it
can fully draw. That is how a layout can use something new without breaking a phone that hasn't updated.

#### How it works

- Each feature is a file, `crates/protocol/features/<name>.md` (about a hundred), and `protocol::required_features`
  works out which features a node tree uses.
- A client sends every feature it draws in `X-Construct-Features`.
- When the pod **seals** a root for that client, it picks, for every name, the newest entry of its history that uses
  nothing the client lacks and nothing removed.

#### Adding and removing vocabulary

Adding a node type or field needs a client build: the type in `protocol`, a feature file, an arm in
`required_features`, drawing it in `construct-render`, and tests (`crates/protocol/features/README.md`). Removing one
never edits layouts: its file moves to `features/removed/`, and only elements that use it are retired.

### 4.6 Home, core layouts and the agent's layouts

Every pod's interface is what `home` reaches. Home starts as a blank page; the agent adds layouts that reference or
copy core layouts, and home embeds or opens them.

#### Core layouts

Core layouts are the screens the server ships: `pixelfed`, `events`, `agents`, `taste` (with `taste_item`, a piece as a sheet), `server_health`, `shell` (the blank
home), `settings`, `inbox`, and the `checklist` kit. Each is a folder of append-only files in its source's crate,
`crates/source-*/layouts/<id>/NNNN.json`; when a pod opens, each `@core/<id>` name moves to this build's newest
file. A pod's layout can use a core layout whole by reference (`{"type": "core://Use", "ref": "@core/events"}`), which
then follows every deploy, or copy it, which doesn't.

#### The pod's layouts

The agent's layouts are names `c-` plus ten characters, each with a title, an icon and a root node (at most 50
layouts, 256 KiB each). Home embeds a layout in place (`core://Layout` `Inline`), in a card, or as a sheet or panel
while a condition holds, and opens others (`action.navigation.open` with a layout: pushed, a sheet, a panel, an
overlay from any edge). **Home owns the feeds**: it lists the data feeds the pod's layouts may bind, and every write
is validated against what those feeds' sources declare.

#### Change what the user sees

Because a copy doesn't follow its core layout, fixing a core layout may not reach a user who copied it. MCP
`list_layouts` marks each layout's `copy_of` and `reached` (decision `trace-from-home`), so a change goes to the
element home actually reaches.

### 4.7 Sharing between people

A pod can share what it built three ways: a **publication** another pod adopts and binds to its own data, a **public
link** anyone can open in a browser, and a **calendar feed** for calendar apps. Each answers a review first.

#### Publications and adoption

`publish` copies a name's closure (everything it embeds, links and uses) to the host's **shelf**, turning each
collection it reads into an **input** with the fields it needs. Another pod `adopt`s the share link, binding each
input to one of its own collections. An adopted element reaches only the data it is bound to, and anything risky it
would do (start agent runs, plug jacks, delete, sign out) waits for the user's yes (decisions `sharing`,
`adoption-capabilities`).

#### Public links

`share_publicly` makes a read-only copy of a layout, the layouts it opens, and only the Collections rows they show,
served as a web page at `/s/pub-…` or `/<handle>/<slug>` and drawn by the web client, also on the owner's own site
(decision `site-origins`). Buttons that write are taken out. The copy is refreshed when its rows change; new rows wait for a new review unless the link says to publish them
(decisions `public-web-links`, `public-links-share-the-closure`).

## Part 5 · Data sources

A source is a module of the pod that knows one kind of data. This part explains the `Source` interface, the data it
serves, and each source in the repository.

### 5.1 What a source is

A source implements one trait, `pod_core::Source` (`crates/pod-core/src/adapter.rs`), and through it declares
everything the pod and layouts may use: its feeds, screen data and actions, its routes and tables, its notification
channels, its jacks, and the core layouts and components it ships.

> **Why it matters:** The trait is the only way the pod meets a source, so a new kind of data is a new crate that
> implements it, with no change to the core.

#### The manifest

`Source::manifest` answers a `SourceManifest`, the data source's contract with layouts:

- **feeds**: named data a layout can bind through home (`["collections", "groceries"]`), each with the query keys
  it fills;
- **data paths**: screen data a layout's screens load (`/v1/pixelfed/screens/post?id=`);
- **actions**: endpoints intents may call (`/v1/pixelfed/posts/like`);
- **entities**: the URI schemes and fields it serves, documented for the agent.

Layouts may only bind and call what a manifest declares; the pod checks every write (decision
`custom-apps-and-data-sources`).

#### The rest of the trait

| Group | Methods |
|---|---|
| Identity and data | `name` (also its table and route prefix), `manifest`, `feed` (answers a feed's data), `has_feed` |
| Routes and storage | `router`, `public_paths`, `migrations` (its `<name>_*` tables), `on_open` |
| Shipping layouts | `core_layouts`, `layout_files`, `component_files`, `element_catalog` |
| Sign-in | `sign_ins`, `sign_in_start`, `sign_in_account`, `share_sign_in` |
| Notifications | `notification_channels`, `poll_interval`, `poll_notifications`, `notification_store` |
| Background | `resume`, `keeps_open` |
| Jacks | `jacks`, `jack_status`, `unplug_jack` |
| The agent | `mcp_tools`, `mcp_call`, `mcp_instructions`, `check_custom_layout` |
| Sharing | `public_data`, `calendar_events`, `share_inputs`, `adopted_scope`, `data_sets` |

Almost every method has a default, so a small source implements only `name`, `manifest`, `feed`, `core_layouts` and
`layout_files`.

> **Try it:** Ask your agent for `list_data_sources`: every source's manifest, as layouts see it.

### 5.2 Entities, feeds and the data bundle

All data travels in one shape: **entities** keyed by URI, and **queries**, ordered lists of URIs. A source fills
them; the engine caches them; a layout's lists and bindings read them.

#### The bundle

```json
{"entities": {"collections://groceries/r1": {"id": "r1", "name": "Milk", "done": false}},
 "queries":  {"collection:groceries": ["collections://groceries/r1"]}}
```

Entities are shared across screens: two lists showing the same post show the same cached entity, so a like on one
shows on both. A feed's data, a screen's data path and a create's answer all have this shape (`protocol::DataBundle`).

#### Data events

When a source's data changes it calls `publish_data` on the pod, which sends a `data` event on every open stream,
usually naming what changed (a collection's rows, URI prefixes, query keys). The engine reloads only cached data that
matches, what's on screen first (decision `scoped-data-events`).

### 5.3 Collections

Collections (`crates/source-collections`) is the source for data the user keeps themselves: the agent declares a
collection's typed fields, and the pod stores, validates, computes and serves the rows. It is the source most
layouts are built on.

#### Fields and rows

A collection has typed fields: `text`, `number`, `bool`, `date`, `datetime`, `choice`, `ref` (a row of another
collection), `quantity` ("1 1/2 cups"). Rows are `collections://<c>/<id>` in the query `collection:<c>`, with
routes to create, update, delete and export them. Fields have permanent ids, so renames keep an alias, removals are
restorable for 30 days, and a write from an offline device made under an older definition still lands.

#### What else a collection can do

- **Computed fields**: `expr` (a small expression), `lookup` (a value from another collection, kept as editable
  rules), `count` and `rollup` (over rows that point here), `model` (a model's answer, in the background). Lookups
  and counts are live: a change to the rows they read recomputes what it affects.
- **Views**: small derived lists (`collection:<c>:view:<v>`), filtered, dated ("upcoming") and limited.
- **Actions**: insert-from-query, update-all and delete-all actions a button can run (add a recipe to the grocery
  list).
- **Fillers**: keep rows current from the web: `fetch` (a URL, an extractor, a field map: no model per fill),
  `agent` (a model with search researches the rows), `range` (one row per day of a moving window), `mcp` (a remote
  MCP server's tool).
- **Alerts and reminders**: notify once when a row matches, or when a date field is reached.

> **Try it:** Ask your agent to "keep a log of my car's oil changes and show it on home": it will declare a
> collection, write a layout for it and add the layout to home.

#### Notifications live here

Every pod's notifications are rows of its `notifications` collection (decision `notifications-in-a-collection`), so
a layout lists them like any data and the agent can edit them. Collections provides the pod's notification store.

### 5.4 Pixelfed

The Pixelfed source (`crates/source-pixelfed`) is a full client of a Pixelfed photo-sharing server, and the reference
for a source that talks to an outside service: its own sign-in, routes, tables, polls and core layout.

#### What it declares

- Entities `pixelfed://posts/…`, `users/…`, `media/…`; feed `home` (query `timeline:home`).
- Screen data: a post, its comments. Actions: like, bookmark, comment, create a post, retry or discard one.
- An OAuth sign-in to any instance (`/v1/auth/pixelfed/…`), and the `pixelfed` account jack.
- Notification channels (likes, comments, mentions, follows), polled every two minutes.
- The `pixelfed` core layout and its components; a sample layout that needs no sign-in for the simulator.

#### Posting

Posting photos crosses every piece: the device stages photos, the engine uploads them (`POST /v1/uploads`) and
queues the create, the pod claims the uploads, and the source posts them to Pixelfed, resuming after a restart
([posting.md](posting.md)).

### 5.5 Events, Agents, Taste and the small sources

The other sources show the range of what a source can be: one built entirely on Collections, one that mirrors an
outside runner, one that keeps a page of an outside service's feed, and three that are mostly layouts.

#### Events

Events (`crates/source-events`) has no data of its own: it is the system collection `events`, each website the user
watches is a fetch filler, and each rule an alert. It ships the `events` core layout and the `matches` notification
channel.

#### Agents

Agents (`crates/source-agents`) mirrors an external **agent runner** that runs Claude Code on cloud sandboxes, over
the runner's protocol (CAP v3: a snapshot, a resumable event stream, actions). The pod owns the work (Tasks rows,
schedules) and notifications; the runner owns the runs and their outcomes. It keeps its pod open to follow the
stream ([agents.md](agents.md)).

Two of the pod's schedules are **Anticipate**: every morning one reads the day ahead and one walks the owner's
journeys through their real screens. Writes in those walks are dry runs. A fix is staged as an edit group, walked
staged, and applied only when the server finds it safe (nothing lost, nothing new done) and unprotected; the owner
undoes or redoes it in one tap, and the device swaps the screen at once from both versions the manifest lists
([anticipate.md](anticipate.md)).

#### Taste

Taste (`crates/source-taste`) shows the feed of the owner's own **taste service**, which ranks clothes from real
stores to find the edges of what they like. The pod signs in to it with a URL and token like the agents runner,
keeps the feed it was served (the service answers a new page every time it is asked, and a layout's feed is fetched
again after every data event) and deals it as a deck of swipe cards, passing likes and skips on and fetching the
next page itself when few are left; the item, search, taste and map screens are read from the service when they
are shown. The owner's likes and dislikes are the pod's: every request names the pod's history, the service keeps a
copy of it, and a service that lacks the version named is sent it whole (decision `taste-history-in-the-pod`). The map is a scatter of the whole catalog (`core://Scatter`, decision `scatter-and-tap-paging`): tap a
dot to open its piece, drag a box to list the pieces in it; a tile grid stays for clients without it
([taste.md](taste.md)).

#### Shell, Monitor and Dates

- **Shell** (`crates/source-shell`): layouts only. `shell`, the blank home; `settings`; and `inbox`.
- **Monitor** (`crates/source-monitor`): the server's health counters and checks (feed `overview`, for pods the
  operator turned it on for), the user's devices with their builds (feed `devices`) and the pod's own recent
  failed requests (feed `errors`: route pattern, status, error code, device; kept in memory per pod by
  `pod_core::recent_errors`, decision `recent-server-errors`), with the `server_health` core layout.
- **Dates** (`crates/source-dates`): the days around today in the pod's time zone (`dates:today`,
  `dates:this_week`, `dates:next_7`, …), so a layout that lists days needs no collection of dates. It tells devices
  to reload at the pod's midnight.

### 5.6 Notifications across sources

Notifications are a shared service of the pod: any source hands it a ready-made notification on a channel it
declared, and the pod stores it, delivers it to every device, and works out which layout should open it.

#### The path

```
upstream
   │ poll
   ▼
source ── a notification:
   │      channel, title,
   │      body, target_uri
   ▼
pod: stored once, in the
notifications collection
   ├─▶ `notification` event
   │   on open streams
   └─▶ Web Push to devices
          │
          ▼
device: inbox, badge, OS
notification ─ tap ─▶ its
layout, then its screen
```

#### Who knows what

Only the source knows where a notification came from. The pod, the engine and the clients see a channel ("Likes"),
title and body text, a `target_uri`, and the layout to present it in, which the pod picks: the first of the pod's
layouts with a screen whose route matches the target ([notifications.md](notifications.md)).

## Part 6 · Jacks, the web module and the agent module

Some abilities are shared by many features: calling a model, fetching a web page, searching, calling an MCP server,
using an account. Jacks say which of them a pod is plugged into; two modules make every web request and model call
go one way.

### 6.1 What a jack is

A jack is an ability a pod can be plugged into or unplugged from, like a socket on a synthesizer: a switch (on or
off), a config (a provider and a sealed key), or an account (a sign-in a source declares). Features check the jack
before they use the ability (decision `jacks`).

> **Why it matters:** One place decides whether a pod may spend model calls or touch the web, so a host can lock
> abilities off for a temporary pod and a user can turn them off in Settings.

#### The interface

`pod_core::jacks` defines `JackSpec {id, title, summary, kind}`, with `JackKind::Switch {default_on}`,
`Config {fields}` and `Account {how}`. State is a row of the pod's `jacks` table; a host locks one with
`jack.<id> = locked` in the pod's settings. Code asks `is_plugged(pod, id)` (or `config(store, id)` for a config
jack's values, secrets opened). Claude plugs jacks with MCP (`list_jacks`, `plug_jack`, `unplug_jack`); people do in
Settings → Jacks.

#### The jacks today

| Jack | Kind | Enables |
|---|---|---|
| `model` | switch, on | Computed fields that ask a model, fillers that read pages or research, picks from search results |
| `web` | switch, on | Fetching websites: fillers, Events' sites, previews |
| `web_search` | config: `brave` + key, `searxng` + URL, or `duckduckgo` | `query_field` fillers and the search tool of agent fills |
| `mcp` | switch, on | Calling tools of the remote MCP servers the pod is connected to |
| `pixelfed` | account | The Pixelfed source |
| `agent_runner` | account | The Agents source |
| `taste` | account | The Taste source |

> **Try it:** Ask your agent for `list_jacks` to see what your pod is plugged into.

### 6.2 The web module

Every request to a website goes through `crates/web-extract`: fetching, reading structured data out of pages, the
per-host policy, search providers, and the prompts used when a model reads web text (decision
`web-and-agent-modules`).

#### Its parts

- `fetch`: a capped, conditional GET that skips unchanged pages.
- `hosts::Hosts`: the host policy: spacing between requests, a daily cap per site, and a shared backoff after a site
  blocks the pod.
- Extractors (`items::extract`): `events` (iCalendar and schema.org events), `json`, `embedded-json`, `jsonld`,
  `rss`, `ics`, `selectors` (CSS), with a small path language to map items to fields.
- `render::Browser`: a headless Chromium for pages that only fill in with JavaScript.
- `search`: the providers behind the `web_search` jack.
- `tasks` and `tools`: model tasks over web content (extract rows, research a row, pick from results), built for the
  agent module.

### 6.3 The agent module

Every model call goes through `crates/agent` (`construct-agent`): it checks the `model` jack, counts the call against
daily budgets, picks the model for the task's tier, retries, and runs the tool loop.

#### The interface

A feature builds a `Task::new(feature, tier, prompt)` with optional system text, a JSON schema for the answer, tools
and a finishing tool, and calls `Agent::run(store, task)`. It gets an `Output {answer, finished, turns, …}` or an
`AgentError` (`Unplugged`, `Budget`, `NoModel`, `Refused`, `CutOff`, `Failed`).

#### Inside a run

1. Refuse if the pod's `model` jack is unplugged.
2. Admit the call against the budgets: per feature, per pod (calls and tokens a day), and host-wide.
3. Pick the model: `Cheap` for computed fields, `Capable` for reading pages and research.
4. Send it, retrying rate limits, server errors and timeouts with backoff.
5. Run tools the model asks for (web fetch, search) until the finishing tool answers.

The server has one model key, `CONSTRUCT_INFER_API_KEY`, used by every model feature.

### 6.4 The pod as an MCP client

Besides being an MCP server for the agent, a pod can be a client of other MCP servers (the `mcp` jack, decision
`mcp-jack`), so a collection can be filled from another service's tools.

#### How it works

`mcp_connect {name, url}` connects a server: at once if it needs nothing, with a sealed token, or through an OAuth
sign-in the user opens in a browser. The pod lists its tools and calls only those the server marks read-only (others
only when the user puts them on the server's allow list). A collection filler of kind `mcp` calls one on a schedule
and maps its answer to rows. What a server answers is untrusted text.

## Part 7 · The device side

On each device, one engine and one renderer do everything; each client is a thin shell of platform code around
them.

### 7.1 The engine

The engine (`crates/client-core`, `ConstructEngine`) is the device's brain: it holds a local cache of everything
loaded, an outbox of changes waiting to be sent, the pod session and upstream sign-ins, and the live event stream.
The renderer only ever sees its snapshots.

> **Why it matters:** Offline use, instant taps and live updates are all the engine's work; a client gets them by
> using it, not by writing them again.

#### Its interface

- **In**: `dispatch(intent_json)` (what a tap does), `open_layout(id)`, `refresh`, `app_foregrounded`, sign-in
  redirects, staged attachments, device and push registration.
- **Out**: `subscribe(callback)` delivers immutable `StateSnapshot`s: the layout and every layout it reaches, entities
  and queries, connectivity, pending count, sign-in state, notifications.
- **Platform traits** a client implements: `SecureStore` (the keychain or keystore) and `UrlOpener` (the browser),
  plus a notification listener.

#### Its parts

- **The cache** (`cache.db`, SQLite): entities, data loads, construct objects, form values, notifications, so the
  last screen shows at once on launch and offline.
- **The outbox** (`action_outbox`): optimistic changes and their requests, written in one transaction, sent oldest
  first, rolled back on a refusal, retried on failure; one the pod keeps failing for 10 minutes is given up and
  rolled back, and `local://connection` says when changes are stuck and which were given up. A refetch re-applies
  changes still pending, so nothing flickers.
- **The event stream**: `/v1/construct/events`, reconnecting with backoff and catching up on reconnect.
- **Sign-in**: OAuth 2 with PKCE run by the engine itself, through a loopback port on desktop and an App Link on
  Android. Upstream tokens stay on the pod; the device keeps only its pod session.

### 7.2 The renderer

The renderer (`crates/construct-render`) turns a plumbing layout and its data into pixels, in Rust, on the GPU or the
CPU. It is the only renderer: every client and the pod's previews use it.

#### The pipeline

```
layout + entities + form,
route, theme
   │ layout: taffy flexbox,
   │ text by cosmic-text
   ▼
Scene: rounded quads, text
runs, a semantic tree
   │
   ├─▶ cpu: a distance function
   │   per pixel → PNG
   └─▶ gpu: wgpu + glyphon →
       window, surface, canvas
```

#### The session

`Session` (in `interact`) holds what is open: the root layout (home), one back stack of presented layouts and
screens (pushed, sheets, panels, overlays, each animated), form values, scroll offsets and focus. It takes input
(`tap`, `type_text`, `scroll_by`, gestures) and answers a scene. A new version of a layout replaces its tree where it
is open, keeping open screens and scroll positions.

#### Its two interfaces

- **`Loader`**: how a session gets layouts and data and sends intents. `PodEngine` implements it over the engine; the
  web client over `fetch`; the simulator over fixtures or a live pod.
- **`Platform`**: what it asks of the operating system: the keyboard, safe-area insets, opening links, the clipboard,
  picking photos, dark mode, notification permission. The desktop and Android apps implement it.

> **Try it:** `scripts/sim_scenarios.sh /tmp/sim` draws the scenarios CI screenshots, through this same pipeline.

### 7.3 The clients

Each client is the renderer plus a thin layer for its platform; none has screens of its own beyond a bundled sign-in
page, because Settings and the inbox are layouts too.

#### The list

| Client | Where | Its platform layer |
|---|---|---|
| **Desktop** | `construct-viewer` (`--features viewer`); `scripts/macos_app.sh` makes the Mac app | A winit window, the loopback OAuth redirect, the login keychain |
| **Android** | a NativeActivity APK (`scripts/android_native.sh`, `android/`) | A small Java bridge that only forwards to Rust; the keystore; App Link sign-in; push through UnifiedPush |
| **Web** | `crates/construct-web`, served at `/web` with `CONSTRUCT_WEB=1` | WebAssembly in a canvas (WebGPU or WebGL2, else the CPU); a session cookie; no engine, its own loader |
| **Simulator** | `crates/construct-sim` | Headless, drawn on the CPU to PNGs from a script, against fixtures or a live pod |
| **Previews** | `pod_core::preview` | The pod draws a layout for the agent's `render_layout_preview` and taps it for `dispatch_virtual_interaction` |

#### Screens of every size

One layout serves every window: the pod never sends a different layout per size. A layout uses
`${local://device#size_class}` (`compact`, `medium`, `expanded`), `max_width_dp` and grid columns to use the room.
A layout that uses none of them was written for a phone: the renderer draws it at most 480 points wide (centred on a
screen of its own, against the pane before it when embedded or in a panel), so it stays phone-sized in a browser
or beside a sidebar.

## Part 8 · Building with an agent

The agent is how anything gets built: Claude connects to the pod's MCP server and reads, writes and looks at layouts
and collections. This part covers that server and how an agent is taught to use it well.

### 8.1 The pod's MCP server

Each pod serves MCP at `POST /v1/mcp`. A Claude app connects to it as a custom connector (OAuth, then the user types
their pod code), Claude Code with a credential from `server pods mcp`. The credential reaches only that pod's MCP
endpoint.

#### The tools by job

| Job | Tools |
|---|---|
| Look around | `list_layouts`, `get_layout`, `find_elements`, `list_data_sources`, `sample_data`, `describe_vocabulary`, `guide` |
| Build screens | `create_layout`, `update_layout` (whole or JSON Patch), `validate_layout`, `define_component`, `copy_component`, `set_theme`, `restore_version` |
| Look at them | `render_layout_preview`, `dispatch_virtual_interaction`, `expand`, `walk_journey` (a whole journey replayed, writes as dry runs) |
| Change safely | `stage_edit_group`, `apply_edit_group`, `revert_edit_group`, `restore_checkpoint`, `usage_report`, `usage_paths` |
| Keep data | `define_collection`, `apply_change`, `edit_rows`, `add_row`, `describe_collection`, fillers (`collections_preview`, `collections_refresh`, `collections_status`), `undo_edit` |
| Abilities | `list_jacks`, `plug_jack`, `mcp_connect`, `mcp_servers` |
| Share | `publish`, `adopt`, `share_publicly`, `set_calendar_feed` |

Sources add their own (`agents_schedules`, `agents_link_tasks`). After the tool list changes, a Claude connector
needs "Refresh tools list".

#### What every write gets

Validation (against home's feeds and the sources' manifests, with a JSON pointer to each error), history (every
version kept, `restore_version`), `warnings` (a hand-written piece a component already draws, fixed colours that won't
follow dark mode), and live delivery to every device.

> **Try it:** Ask your agent to `render_layout_preview` your home: it sees the screen as your phone draws it.

### 8.2 How an agent is taught

The pod teaches the agent as it goes: short instructions with a build procedure, a guide per topic, a catalog of what
exists, and warnings on every write. The aim is that the agent reuses what exists and changes what the user sees.

#### The procedure

The MCP instructions (under 2,000 characters) give the procedure: see what exists (`list_layouts`,
`find_elements`); use a core layout whole when one fits; otherwise compose components; make a component of anything
written twice; look at the result with a preview; say what was missing with `note_gap`, which the server's operator
reads. `guide {topic}` holds the detail per topic (navigation, pages, components, theme, actions, jacks, sharing,
and one per source).

#### Trace from home

A request about "my groceries screen" is about the element home reaches there, which may be the user's own copy of a
core layout. `reached` and `copy_of` on every layout make that visible (decision `trace-from-home`).

## Part 9 · Working on the codebase

This part is for changing the code: where things are, how they are tested, and the rules that keep the design
intact.

### 9.1 The repository

The repository is one Rust workspace: the wire types, the pod and its host, the sources, the two shared modules,
the engine, the renderer and its clients, and the test tools.

#### The crates

| Crate | What it is |
|---|---|
| `protocol` | Wire types: `UiNode`, `ActionIntent`, `DataBundle`; `features/`, the named vocabulary |
| `construct-elements` | Elements, hashes, references, component expansion, materializing; shared by pod and engine |
| `pod-core` | The generic pod; `components/` holds the server components |
| `pod-host` | Many pods in one server: `host.db`, tokens, temporary pods, scheduler, shelf, public links |
| `server` | The binary and the `server pods` admin commands |
| `source-collections`, `source-pixelfed`, `source-events`, `source-agents`, `source-taste`, `source-monitor`, `source-dates`, `source-shell` | The sources; core layouts in `layouts/<id>/NNNN.json`, components in `components/` |
| `web-extract`, `agent` | The web module and the agent module |
| `client-core` | The engine |
| `construct-render` | The renderer, the desktop and Android apps, the pod's previews |
| `construct-web`, `construct-sim` | The web client and the headless simulator |
| `e2e`, `fake-pixelfed/` | The end-to-end suite and a fake Pixelfed to run it against |
| `design-lessons` | Reads this document into the course's JSON |

### 9.2 Testing

Every crate has unit tests and one integration test binary; the end-to-end suite runs the real pod and engine
together; the simulator checks screens as PNGs. There are no emulator or device tests, so a change that needs a
device says so.

#### The layers

- **Unit and integration tests**: `cargo test --workspace`; each crate's integration tests are one binary,
  `tests/integration/main.rs`, with a module per file.
- **End to end** (`crates/e2e`): the pod and the engine in one process against the fake Pixelfed (or a real one),
  through a proxy that can be cut to test offline behaviour.
- **The simulator** (`construct-sim`): scripts like `click Like; assert-text Unlike; shot after` drive the real
  renderer and write PNGs; CI runs the scenarios and pull requests embed the screenshots.
- **Checks**: `cargo fmt`, `clippy -D warnings`, and scripts for decisions, test data (always made up) and old
  names.

### 9.3 Changing things safely

A few rules protect the design, and most mistakes they prevent are ones a first change would make: editing a deployed
layout file, teaching the core about a source, or resetting a database.

#### The rules

1. **The core knows no source**: changes to a core layout are layout files plus source code.
2. **Layout and component files are append-only**: add the next `NNNN.json`; never edit a deployed one.
3. **Vocabulary is named features**: a new node type, intent or field needs a feature file and a client build.
4. **Migrations, not resets**: staging upgrades in place.
5. **Keep UI state across layout updates**: open screens and scroll positions survive a new version.
6. **Record decisions** in `docs/decisions/<topic>.md`, in the same pull request, and keep the guides current.

#### Where to go next

[CONTRIBUTING.md](../CONTRIBUTING.md) for setup and checks, [layouts.md](layouts.md) to write layouts,
[testing.md](testing.md) and [simulator.md](simulator.md) to check a change, [server.md](server.md) to run it.

## Keeping this current

This file is the source of the course. `crates/design-lessons` reads it into
[design-lessons.json](design-lessons.json), and a test fails when the JSON doesn't match the file, so a change here
is a change to both:

```bash
UPDATE_DESIGN_LESSONS=1 cargo test -p design-lessons   # after editing this file
```

The course on the pod is a set of Collections (`construct_tracks`, `construct_lessons`, `construct_cards`,
`construct_glossary`), each filled by a fetch filler that reads the JSON from `main` once a day and when the course is
opened. Rows keep their ids across fills, so a reader's progress stays. So the course follows `main` by itself:
merge a change here and the course shows it within a day.

The shape the generator reads (see `crates/design-lessons/src/lib.rs`): `## Part <n> · <title>` starts a part,
`### <n>.<m> <title>` a lesson (numbered without gaps), `#### <heading>` a card; the first paragraph of a part or
lesson is its summary; `> **Why it matters:**` and `> **Try it:**` quotes become the lesson's callouts; `##` sections
that aren't parts, like this one, are for readers of the file only. Keep a card under about a phone screen of text.

When a change to the system changes something described here, update this file in the same pull request, as with
any guide.

## Glossary

- **Action**: an endpoint a source declares that intents may call, such as `/v1/collections/<c>/update`.
- **Adopt**: copy another pod's publication into this pod, bound to this pod's own data.
- **Agent**: the AI that builds a pod's layouts (Claude, over MCP). Not to be confused with the Agents source.
- **Agent module**: `crates/agent`, which every model call goes through.
- **Agent runner**: an outside service that runs Claude Code on sandboxes; the Agents source follows it.
- **Collection**: a typed table the agent declares, stored and served by the Collections source.
- **Component**: a reusable piece of layout with parameters, expanded into plumbing before drawing; `@core/<name>`
  ships with the server, `@me/<name>` is the pod's own.
- **Construct**: a pod's layouts and components as content-addressed elements and names, with `home` as the root.
- **Core layout**: a layout the server ships and updates with each deploy, named `@core/<id>` (once called a
  blueprint, and before that an app).
- **Data bundle**: entities by URI plus queries (ordered lists of URIs), the shape all data travels in.
- **Device data**: what a device knows itself and layouts read with no pod request, under `local://`.
- **Edge**: an embed or a link from one layout to another, keyed by its name or `as`.
- **Element**: an immutable layout or component, named by the hash of its JSON (`e1-…`).
- **Engine**: `client-core`, the device's cache, outbox, sign-in and event stream.
- **Entity**: one record a layout can bind, keyed by URI, such as `pixelfed://posts/101`.
- **Feature**: a named piece of vocabulary; clients say which they draw, and the pod serves what they can.
- **Feed**: named data a source offers layouts, listed on home, such as `["collections", "groceries"]`.
- **Filler**: a collection's recipe for keeping its rows current from the web, a model or an MCP server.
- **Home**: the pod's root layout, the one every client opens; it reaches every other layout.
- **Host**: the server process that holds many pods (`pod-host`).
- **Intent**: what a tap does, written as data, such as `action.mutation.optimistic`.
- **Jack**: an ability a pod can be plugged into: a switch, a config or an account.
- **Layout**: a screen written as a JSON tree of nodes; a pod's own are named `home` and `c-…`.
- **Manifest**: a source's declaration of its feeds, data paths, actions and entities.
- **MCP**: the Model Context Protocol; the pod serves it to the agent and can call other servers with it.
- **Name**: what points at an element and keeps its history, such as `home` or `@me/card`.
- **Outbox**: the engine's queue of changes waiting to be sent to the pod.
- **Plumbing**: the vocabulary the renderer draws: nodes, intents and device data.
- **Pod**: one user's server: their layouts, data, sessions and sign-ins in one database.
- **Pod code**: the secret (`cpod_…`) that opens a pod; a device trades it for a pod session.
- **Porcelain**: components, defined as data over plumbing and expanded before drawing.
- **Public link**: a read-only web page copied from a layout and the rows it shows.
- **Publication**: what a pod shares with other pods on the server, on the host's shelf.
- **Query**: an ordered list of entity URIs under a key, such as `collection:groceries`.
- **Renderer**: `construct-render`, which draws layouts on every client.
- **Screen**: a part of a page layout opened by route, such as `collections://events/{id}`.
- **Seal**: fix a reference to the element its name points at now, for a client's features.
- **Shelf**: the host's store of what pods publish.
- **Source**: a module of the pod that knows one kind of data, implementing `pod_core::Source` (once called an app
  adapter).
- **Web module**: `crates/web-extract`, which every request to a website goes through.
