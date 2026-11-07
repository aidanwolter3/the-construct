# Documentation

Start with [architecture.md](architecture.md), then the guide for what you're doing.

| Guide | Covers |
|---|---|
| [architecture.md](architecture.md) | How the pieces fit: pods, adapters, layouts, the engine, the clients |
| [layouts.md](layouts.md) | Publishing a layout version; bindings, nodes, intents and features |
| [server.md](server.md) | Running and configuring the server, pods and their admin CLI, monitoring, deploying |
| [api.md](api.md) | The pod's core HTTP API and event stream |
| [custom-apps.md](custom-apps.md) | Blueprints and home, the pod's layouts, building them with Claude (MCP), Collections, fillers and alerts, the Events app |
| [clients.md](clients.md) | Building and running the desktop and Android apps; home and the layouts it shows; sign-in |
| [rendering.md](rendering.md) | The Rust rendering engine: layout, scenes, GPU and CPU rasterizers, agent previews, the native prototypes |
| [simulator.md](simulator.md) | The headless simulator: flags, script commands, test tags |
| [testing.md](testing.md) | Test suites, the end-to-end suite, the fake Pixelfed, a local real Pixelfed |
| [pixelfed.md](pixelfed.md) | The Pixelfed app: sign-in, instances, its endpoints |
| [posting.md](posting.md) | How photo posting works across client, engine, pod and adapter |
| [notifications.md](notifications.md) | How notifications travel from an upstream to a device |
| [agents.md](agents.md) | The Agents source: Claude Code sessions from an agent runner (CAP v1), its blueprint, notifications and Tasks wiring |

**Decisions.** [DECISIONS.md](DECISIONS.md) holds D1–D52, numbered and closed to new entries. Newer decisions are
one file each in [decisions/](decisions/) ([how to write one](decisions/README.md)). They record why, at the time;
the guides above describe how things work now.

**History.** [history/](history/) keeps documents that described plans rather than the current system: the
[original design](history/original-design.md) the project started from, and the
[notifications build plan](history/notifications-plan.md).
