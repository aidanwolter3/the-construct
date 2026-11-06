# The Construct

**Native apps whose screens come from your own server.** A Rust server, the *pod*, holds your data and sign-ins and
decides what every screen looks like. Desktop and Android clients draw those screens natively with one shared Compose
UI and one shared Rust engine, and keep working offline. A new pod starts as a blank home screen; your agent (Claude,
through the pod's MCP endpoint) builds the rest into it, only what you ask for, with no client release.

- **Your layouts, built by your agent.** There are no apps at runtime. A pod's screens are its own layouts, starting
  with `home`, and the agent writes them from blueprints, components and your data. Every write is validated and
  appears live on your devices; any version can be restored.
- **Server-driven, but native.** Layouts are JSON trees of generic nodes (`core://List`, `core://Carousel`,
  `core://TextField`, …) rendered with Compose, not a web view.
- **Device data and commands.** Layouts read what the device knows (`local://connection`, the inbox, the pod's
  layouts, the theme) and ask the device to act (`action.device`: sign in, hide a layout, leave the pod), so even
  Settings and the inbox are layouts.
- **Offline first.** Everything is cached in SQLite on the device. Taps apply at once and queue in an outbox that
  syncs when the network returns.
- **Your pod, your data.** Each user gets a pod: one SQLite database with their sessions, upstream tokens (sealed at
  rest) and data. Upstream tokens never reach the device.

## What a pod offers

| | |
|---|---|
| **Blueprints** | Built-in layouts the server ships and keeps current, for the agent to reference or copy: **Pixelfed** (a [Pixelfed](https://pixelfed.org) client with feed and card views, likes, bookmarks, threaded comments, photo posting and notifications, for any instance), **Recipes** (a recipe collection with search and categories), **Events** (upcoming events from websites you watch, with alerts when one matches), **Settings** and the **inbox**. A new pod's `home` is the blank `shell` blueprint. |
| **Collections** | Typed data the agent declares (a todo list, a maintenance log), with computed fields, reminders, and **fillers** that keep rows current from the web (iCalendar feeds, schema.org data, or a model reading a page). |
| **Components** | Reusable pieces of layout defined as data and expanded by the pod: the server's (`core:card`, `core:section_header`, `core:back_button`, every blueprint and its parts) and the pod's own (`pod:…`, defined by the agent). Changing one needs no client build. |
| **Theme** | The pod's colours (`${local://theme#accent}` and other tokens), set by the agent. |

## How it works

```
 Pixelfed, web pages ◀──▶ pod (Rust, Axum, SQLite) ── layouts + entities, events ──▶ engine (Rust) ──▶ Compose UI
                          adapters: pixelfed,                                         cache, outbox,     desktop,
 Claude (MCP) ──────────▶ recipes, collections, …                                     sign-in, SSE       Android
```

Clients open the pod's `home` layout. Home embeds other layouts (`core://Layout`: in place, in a card, or as a sheet
or panel while a condition holds) and presents them (`action.navigation.open {layout}`: pushed, a sheet or a side
panel), all on one engine that shares their data; system Back pops them. Home owns the pod's feeds, and every layout
may bind any of them.

The vocabulary has two layers. **Plumbing** is what the app draws: nodes, intents and device data, each a named
feature. **Porcelain** is components, expanded by the pod before serving, so the app only ever sees plumbing. A layout
rooted at a `core://Page` draws its own header and Back; a `core://Workspace` is porcelain over the same nodes.

The engine caches layouts and entities, applies actions optimistically and keeps an event stream open, so new layout
versions, data changes and notifications arrive live. A goal of the design is that old clients keep working: a client
says which features it can draw, and the pod serves it the newest version of each layout that uses only those. Read
[docs/architecture.md](docs/architecture.md) for the full picture.

## Quick start

You need Rust and, for the clients, JDK 21. Details for macOS and Linux are in [CONTRIBUTING.md](CONTRIBUTING.md#setup).

1. Run the server with fixed local codes, so the app and Claude Code keep working across restarts
   ([docs/server.md](docs/server.md#fixed-codes-for-local-testing)):

   ```bash
   cargo run -p server -- serve --pod-code cpod_localtest123 --mcp-key cmcp_localtest123   # 127.0.0.1:8080
   cargo run -p fake-pixelfed               # optional: a fake Pixelfed upstream on 127.0.0.1:8081
   ```

2. Open the desktop app against it and type the pod code `cpod_localtest123`. You see the blank home: "The
   Construct", "Ask your agent to build anything", and Notifications and Settings buttons.

   ```bash
   cd app && ./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080
   ```

   On Linux, build the engine first and skip the macOS build step: `cargo build -p client-ffi --release`, then add
   `-PskipRustBuild`.

3. Connect Claude Code to the pod and ask for a first layout, such as "show my recipes on home":

   ```bash
   claude mcp add --transport http construct-local http://127.0.0.1:8080/v1/mcp \
     --header "Authorization: Bearer cmcp_localtest123"
   ```

   Claude reads the pod's blueprints and sources, creates the layout and puts it on home; the app shows it at once.
   The Claude apps can connect to a deployed server as a custom connector instead
   ([docs/custom-apps.md](docs/custom-apps.md#building-apps-with-claude)).

Without `-Dconstruct.baseUrl`, clients use the public staging server, where **Try a temporary pod** gives you a
24-hour pod to explore.

To render the shared UI headlessly to PNGs (no window, no device), use the simulator:

```bash
cargo build -p client-ffi --release
cd app && ./gradlew -PskipRustBuild :desktopApp:simulate \
  -Pworkspace=pixelfed_sample_no_login -Pscript="shot feed; click Like; assert-text Unlike; shot liked" -Pout=/tmp/sim
```

## Documentation

| Guide | |
|---|---|
| [Architecture](docs/architecture.md) | Pods, adapters, layouts, the engine and the clients |
| [Writing layouts](docs/layouts.md) | Publishing a layout version; nodes, intents, device data and components |
| [Running the server](docs/server.md) | Configuration, pods, fixed local codes, monitoring, deploying |
| [HTTP API](docs/api.md) | The pod's core routes |
| [Custom apps, Collections and Events](docs/custom-apps.md) | Blueprints and home, the pod's layouts, Claude (MCP), Collections, fillers and the Events app |
| [Clients](docs/clients.md) | Desktop and Android: building, options, home and the layouts it shows, sign-in |
| [Simulator](docs/simulator.md) | Headless screenshots and UI scripts |
| [Testing](docs/testing.md) | Test suites, the fake Pixelfed, a local real Pixelfed |
| [Pixelfed](docs/pixelfed.md), [Posting photos](docs/posting.md), [Notifications](docs/notifications.md) | Feature guides |
| [Decisions](docs/DECISIONS.md) and [newer decisions](docs/decisions/) | Why things are the way they are |

## Repository layout

```
crates/protocol         wire types: UiNode, ActionIntent, bundles; features/, the named vocabulary
crates/pod-core         the generic pod: layouts and their versions, components, sessions, sign-ins, uploads,
                          notifications, MCP; components/, the server's components
crates/pod-host         one server, many pods: host.db, pod tokens, temporary pods, scheduler
crates/server           the server binary and the `server pods` admin CLI
crates/app-*            adapters: pixelfed, recipes, collections, events, monitor, shell (home, Settings, the
                          inbox); blueprint versions in layouts/<id>/NNNN.json
crates/web-extract      fetching pages and reading structured data out of them, for fillers
crates/client-core      the engine: SQLite cache, outbox, PKCE sign-in, event stream
crates/client-ffi       the UniFFI bridge to Kotlin
crates/e2e              end-to-end suite
fake-pixelfed/          a fake Pixelfed upstream for tests and staging
app/                    Gradle: :shared (Compose UI), :desktopApp (and the simulator), :androidApp
deploy/, dev/, scripts/, assets/icon/
```

## Status

The Construct is under active development. The server and both clients work and run on a staging server; the
Android app is not in a store. The API and the layout format may change.

## Contributing

Issues and feedback are welcome. Outside code contributions aren't accepted until a license is chosen.
[CONTRIBUTING.md](CONTRIBUTING.md) covers setup, the checks to run, and the few rules that keep the architecture
intact.

## License

No license has been chosen yet, so all rights are reserved. You're welcome to read the code and open issues, but
please ask before reusing it.
