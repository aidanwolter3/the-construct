# The Construct

**Native apps whose screens come from your own server, built by your agent.**

The Construct is a server-driven UI system. A server you own, the **pod**, holds your data and your sign-ins and
decides what every screen looks like. Native clients on your phone and computer draw those screens with one Rust
renderer (no web view) and keep working offline. A new pod is a blank home screen: you ask your agent (Claude,
connected to the pod over MCP) for what you want, a grocery list that knows your recipes, a car maintenance log, your
photo feed, and it builds just that into your pod. Every device shows it a moment later, with no app release.

**New here? Take the course:** [Inside The Construct](https://the-construct-pod.fly.dev/aidan/design) walks through
the whole design, from the goals down to each piece, in short lessons. It is drawn by The Construct itself from
[docs/design.md](docs/design.md), which holds the same lessons as one document.

## Goals

- **Your data stays yours.** Each user has their own pod: one SQLite database with their data, layouts and
  sign-ins. Tokens for other services are sealed on the pod and never reach a device.
- **Native and offline first.** Screens are drawn natively. Everything loaded is cached on the device; a tap
  changes the screen at once and is sent when the network returns.
- **Most changes ship from the server.** Screens are data (JSON **layouts**) and the clients know no particular
  app, so a new screen or data source reaches users without a client build.
- **Your agent builds, only what you ask.** The pod validates every write, keeps every version, and draws previews
  for the agent to check its work.
- **Old clients keep working.** Each client says which vocabulary it can draw, and the pod serves it the newest
  version of each layout it can.

## The pieces

```
 agent (Claude) ── MCP ──▶ pod ◀──▶ websites, Pixelfed, a model API, …
                            │  layouts + data, live events
                            ▼
              engine + renderer on each device
              (desktop, Android, web, headless simulator)
```

- **The pod and its host** (`crates/pod-core`, `crates/pod-host`, `crates/server`): one server process hosts many
  pods, one per user. A pod serves its **construct** (its layouts and components, stored by content hash) and its
  data to devices, and an MCP server to the agent.
- **Sources** (`crates/source-*`): each kind of data is a source behind one trait: **Collections** (typed data the
  agent declares, kept current from the web by fillers), **Pixelfed**, **Events**, **Agents**, and a few small ones.
- **Jacks** and two shared modules: the abilities a pod is plugged into (a model, the web, search, MCP servers,
  accounts), with every web request going through the web module (`crates/web-extract`) and every model call
  through the agent module (`crates/agent`).
- **The engine** (`crates/client-core`): the device's cache, outbox of pending changes, sign-in and event stream.
- **The renderer** (`crates/construct-render`): draws any layout, on the GPU or CPU; the desktop and Android apps,
  the web client (`crates/construct-web`), the simulator (`crates/construct-sim`) and the pod's previews all use it.

Read [docs/design.md](docs/design.md) for how they fit and the interfaces between them.

## Quick start

You need Rust; for the Android app also an Android SDK with an NDK. Details are in
[CONTRIBUTING.md](CONTRIBUTING.md#setup).

1. Run the server with fixed local codes, so the app and Claude Code keep working across restarts
   ([docs/server.md](docs/server.md#fixed-codes-for-local-testing)):

   ```bash
   cargo run -p server -- serve --pod-code cpod_localtest123 --mcp-key cmcp_localtest123   # 127.0.0.1:8080
   cargo run -p fake-pixelfed               # optional: a fake Pixelfed upstream on 127.0.0.1:8081
   ```

2. Open the desktop app against it and type the pod code `cpod_localtest123`. You see the blank home: "The
   Construct", "Ask your agent to build anything", and Notifications and Settings buttons.

   ```bash
   CONSTRUCT_POD_URL=http://127.0.0.1:8080 cargo run --release -p construct-render --features viewer --bin construct-viewer
   ```

3. Connect Claude Code to the pod and ask for a first screen, such as "keep my recipes in a collection and show them
   on home":

   ```bash
   claude mcp add --transport http construct-local http://127.0.0.1:8080/v1/mcp \
     --header "Authorization: Bearer cmcp_localtest123"
   ```

   Claude reads the pod's core layouts and sources, declares the collection, writes the layout and puts it on home;
   the app shows it at once. The Claude apps can connect to a deployed server as a custom connector instead
   ([docs/building.md](docs/building.md#building-with-claude)).

Without `CONSTRUCT_POD_URL`, clients use the public staging server, where **Try a temporary pod** gives you a
24-hour pod to explore.

To draw layouts headlessly to PNGs (no window, no device), use the simulator:

```bash
cargo run -p construct-sim -- --workspace pixelfed_sample_no_login \
  --script "shot feed; click Like; assert-text Unlike; shot liked" --out /tmp/sim
```

## Documentation

| Guide | |
|---|---|
| [Design](docs/design.md) | The whole system from the top down: goals, the pieces, their interfaces (also [as a course](https://the-construct-pod.fly.dev/aidan/design)) |
| [Writing layouts](docs/layouts.md) | Layout files and the vocabulary: nodes, intents, bindings, device data, components |
| [Layouts, Collections and sharing](docs/building.md) | Core layouts and home, building with Claude (MCP), Collections and fillers, jacks, sharing |
| [HTTP API](docs/api.md) | The pod's core routes and event stream |
| [Running the server](docs/server.md) | Configuration, pods, fixed local codes, monitoring, deploying |
| [Clients](docs/clients.md) and [rendering](docs/rendering.md) | Desktop, Android and web; the Rust renderer |
| [Simulator](docs/simulator.md) and [testing](docs/testing.md) | Headless screenshots and UI scripts; the test suites |
| [Pixelfed](docs/pixelfed.md), [posting photos](docs/posting.md), [notifications](docs/notifications.md), [Agents](docs/agents.md) | Feature guides |
| [Decisions](docs/DECISIONS.md) and [newer decisions](docs/decisions/) | Why things are the way they are |

## Status

The Construct is under active development. The server and the clients work and run on a staging server; the Android
app and the Mac app come from GitHub releases (`.github/workflows/release.yml`), not a store. The API and the layout
format may change.

## Contributing

Issues and feedback are welcome. Outside code contributions aren't accepted until a license is chosen.
[CONTRIBUTING.md](CONTRIBUTING.md) covers setup, the checks to run, and the few rules that keep the architecture
intact.

## License

No license has been chosen yet, so all rights are reserved. You're welcome to read the code and open issues, but
please ask before reusing it.
