# Documentation

Start with [design.md](design.md), the whole system from the top down (also a course on a pod:
[Inside The Construct](https://the-construct-pod.fly.dev/aidan/design)), then the guide for what you're doing.

| Guide | Covers |
|---|---|
| [design.md](design.md) | The design breadth first: goals, the big pieces and their interfaces, then each piece; a glossary |
| [layouts.md](layouts.md) | Publishing a layout version; bindings, nodes, intents and features |
| [server.md](server.md) | Running and configuring the server, pods and their admin CLI, monitoring, deploying |
| [api.md](api.md) | The pod's core HTTP API and event stream |
| [building.md](building.md) | Core layouts and home, the pod's layouts, building them with Claude (MCP), sharing, Collections, fillers and alerts, jacks, Events |
| [clients.md](clients.md) | Building and running the desktop and Android apps; home and the layouts it shows; sign-in |
| [rendering.md](rendering.md) | The Rust rendering engine: layout, scenes, GPU and CPU rasterizers, agent previews, the native prototypes |
| [simulator.md](simulator.md) | The headless simulator (`construct-sim`): fixture and live modes, flags, script commands, targets |
| [testing.md](testing.md) | Test suites, the end-to-end suite, the fake Pixelfed, a local real Pixelfed |
| [pixelfed.md](pixelfed.md) | The Pixelfed source: sign-in, instances, its endpoints |
| [posting.md](posting.md) | How photo posting works across client, engine, pod and source |
| [notifications.md](notifications.md) | How notifications travel from an upstream to a device |
| [agents.md](agents.md) | The Agents source: runs from an agent runner (CAP v3), the pod's schedules, its core layout, notifications and Tasks wiring |

**Decisions.** [DECISIONS.md](DECISIONS.md) holds D1–D52, numbered and closed to new entries. Newer decisions are
one file each in [decisions/](decisions/) ([how to write one](decisions/README.md)). They record why, at the time;
the guides above describe how things work now.

**History.** [history/](history/) keeps documents that described plans rather than the current system: the
[original design](history/original-design.md) the project started from, and the
[notifications build plan](history/notifications-plan.md).
