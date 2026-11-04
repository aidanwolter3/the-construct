# The Construct

**Native apps whose screens come from your own server.** A Rust server, the *pod*, holds your data and sign-ins and
decides what every screen looks like. Desktop and Android clients draw those screens natively with one shared Compose
UI and one shared Rust engine, and keep working offline. New screens and whole new apps ship from the server, with
no client release.

- **Server-driven, but native.** Layouts are JSON trees of generic nodes (`core://List`, `core://Carousel`,
  `core://TextField`, …) rendered with Compose, not a web view.
- **Offline first.** Everything is cached in SQLite on the device. Taps apply at once and queue in an outbox that
  syncs when the network returns.
- **Your pod, your data.** Each user gets a pod: one SQLite database with their sessions, upstream tokens (sealed at
  rest) and app data. Upstream tokens never reach the device.
- **Apps are adapters.** The core knows no app. Pixelfed, Recipes and Events are adapters plus layout files, and
  users can build their own apps over the pod's data, by hand or with Claude through the pod's MCP endpoint.

## Apps

| App | What it does |
|---|---|
| **Pixelfed** | A [Pixelfed](https://pixelfed.org) client: feed and card views, carousels, likes, bookmarks, threaded comments, photo posting and notifications. Signs in to any instance. |
| **Recipes** | A recipe collection with search, categories and recipe cards. |
| **Events** | Upcoming events from websites you watch (iCalendar feeds or schema.org events), with alerts when one matches. |
| **Custom apps** | Your own layouts over the pod's sources and **Collections** (typed data such as a todo list or a maintenance log), with computed fields, reminders and fillers that keep rows current from the web. |

## How it works

```
 Pixelfed, web pages ◀──▶ pod (Rust, Axum, SQLite) ── layouts + entities, events ──▶ engine (Rust) ──▶ Compose UI
                          adapters: pixelfed,                                         cache, outbox,     desktop,
                          recipes, collections, …                                     sign-in, SSE       Android
```

The pod serves each app as a layout (a `core://Workspace` with screens) plus normalized entities. The engine caches
both, applies actions optimistically and keeps an event stream open, so new layout versions and notifications arrive
live. Layout versions are files in [`layouts/`](layouts/) and every version stays servable: a client is always sent
the newest one it can draw. Read [docs/architecture.md](docs/architecture.md) for the full picture.

## Quick start

You need Rust (stable) and, for the clients, JDK 21. Details for macOS and Linux are in
[CONTRIBUTING.md](CONTRIBUTING.md#setup).

```bash
cargo run -p fake-pixelfed               # a fake Pixelfed upstream on 127.0.0.1:8081
cargo run -p server                      # the server on 127.0.0.1:8080, with a local "dev" pod
```

Render the shared UI headlessly to PNGs (no window, no device):

```bash
cargo build -p client-ffi --release
cd kmp-app && ./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :desktopApp:simulate \
  -Pworkspace=pixelfed_demo -Pscript="shot feed; click Like; assert-text Unlike; shot liked" -Pout=/tmp/sim
```

(`kmp-app/gradle.properties` points Gradle at Android Studio's JDK on macOS; `-Dorg.gradle.java.home` overrides it
with any JDK 21.)

Run the desktop app against the local server (macOS):

```bash
cd kmp-app && ./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080
```

Without `-Dconstruct.baseUrl`, clients use the public staging server, where **Try a temporary pod** gives you a
24-hour pod to explore.

## Documentation

| Guide | |
|---|---|
| [Architecture](docs/architecture.md) | Pods, adapters, layouts, the engine and the clients |
| [Writing layouts](docs/layouts.md) | Publishing a layout version, and the node and intent reference |
| [Running the server](docs/server.md) | Configuration, pods, monitoring, deploying |
| [HTTP API](docs/api.md) | The pod's core routes |
| [Custom apps](docs/custom-apps.md) | Custom apps, Claude (MCP), Collections, fillers and the Events app |
| [Clients](docs/clients.md) | Desktop and Android: building, options, the app menu, sign-in |
| [Simulator](docs/simulator.md) | Headless screenshots and UI scripts |
| [Testing](docs/testing.md) | Test suites, the fake Pixelfed, a local real Pixelfed |
| [Pixelfed](docs/pixelfed.md), [Posting photos](docs/posting.md), [Notifications](docs/notifications.md) | Feature guides |
| [Decisions](docs/DECISIONS.md) and [newer decisions](docs/decisions/) | Why things are the way they are |

## Repository layout

```
crates/protocol         wire types: UiNode, ActionIntent, bundles, layout features
crates/pod-core         the generic pod: apps, layout versions, sessions, sign-ins, uploads, notifications, MCP
crates/pod-host         one server, many pods: host.db, pod tokens, temporary pods, scheduler
crates/server           the server binary and the `server pods` admin CLI
crates/app-*            adapters: pixelfed, recipes, collections, watcher (Events), monitor
crates/web-extract      fetching pages and reading structured data out of them, for fillers
crates/client-core      the engine: SQLite cache, outbox, PKCE sign-in, event stream
crates/client-ffi       the UniFFI bridge to Kotlin
crates/e2e              end-to-end suite
fake-pixelfed/          a fake Pixelfed upstream for tests and staging
kmp-app/                Gradle: :shared (Compose UI), :desktopApp (and the simulator), :androidApp
layouts/<app>/          layout versions as code
deploy/, dev/, scripts/, fixtures/, assets/icon/
```

## Status

The Construct is under active development. The server, both clients and the apps above work and run on a staging
server; the Android app is not in a store. The API and layout format may still change, with one promise: published
layout versions keep working for the clients that drew them.

## Contributing

Issues and feedback are welcome. Outside code contributions aren't accepted until a license is chosen.
[CONTRIBUTING.md](CONTRIBUTING.md) covers setup, the checks to run, and the few rules that keep the architecture
intact.

## License

No license has been chosen yet, so all rights are reserved. You're welcome to read the code and open issues, but
please ask before reusing it.
