# A daily newcomer test: an agent with only a new pod's MCP tools builds what a scripted person asks for

Date: 2026-11-28. Related: [element-roles-and-reuse](element-roles-and-reuse.md),
[journeys-and-dry-runs](journeys-and-dry-runs.md), [rust-simulator](rust-simulator.md),
[blueprints-and-home](blueprints-and-home.md), [jacks](jacks.md).

The developer's own pod was built over many sessions, and its agent leans on what that pod already holds. Nothing
showed whether a new user, with an empty pod and their own agent, gets a working screen without anyone landing code,
or how much of what the server already ships their agent rebuilds by hand. Rebuilding wastes the newcomer's time;
it is the cost reuse exists to save.

Considered:

- **A test in CI** (GitHub Actions on a schedule). Its steps are fixed and it has the repository, but the agent and
  the reviewer need a model key added as a GitHub secret, billed per token, and a workflow is a protected path.
- **The owner's pod's own schedules** (Agents, run by the Foreman). Results would land in the pod, but a staging
  outage would take the scheduler down with what it tests, and the Foreman expects each run to open a pull request.
- **A Claude scheduled task**: a fresh cloud session each day. It has no repository attached, but the repository is
  public, so it clones `main` read-only and runs the script; no new secret. Taken.

Decision:

- **Personas** (`crates/newcomer/personas/<id>.md`): a person's wishes in their own words (`## What I say`, sent in
  order), facts for their answers (`## If asked`), and `## Done when`: a `sim:` script, a `uses:` list of what the
  server already ships for the ask, and `judge:` lines. Front matter: `tier` (`temporary`, or `fresh` for wishes that
  need the model or the web, which temporary pods lock), `max_turns`, `max_budget_usd`, `starts_from` (a later visit
  to another persona's pod). A persona never names a tool, layout or collection.
- **The run** (`scripts/newcomer.sh`, binary `newcomer`): per persona a new temporary pod (`X-Construct-Client:
  newcomer-test session <id>`, deleted when done). The agent is Claude Code run headless (`claude -p`) in an empty
  directory outside any checkout, with `--tools ""`, `--strict-mcp-config` and no setting sources, so the pod's MCP
  tools are all it has and no code can land. Each next message continues its conversation (`--resume`). Between
  turns a simulated user (`claude -p`, no tools) answers a question the agent is waiting on from "If asked", else
  sends the next wish.
- **Checks ask the pod, never the agent.** The simulator runs the `sim:` script live on the pod at phone size; the
  pod's layouts and own components are read back (`list_layouts`, `get_layout`, `get_component`); a reviewer
  (`claude -p` with only `Read`) answers each `judge:` line from the screenshots and layouts, never the transcript.
- **Results**: `pass`; `fail` (the `sim:` script fails, a `uses:` piece isn't referenced or was copied, or home
  doesn't reach a layout the agent made); `flagged` (the reviewer said no to a line: a fail only two days running,
  as a model's judgement varies); `stuck` (turns or budget used up, or the agent's turn ended in an error); `broken`
  (the server, its pods or the model failed: neither pass nor fail); `skipped` (a fresh persona, until a run makes
  admin pods).
- **Rebuilt work and time are measured, not failed on** (except a `uses:` piece): reuse hints the pod still gives
  for the final layouts (`validate_layout`) and copies of core layouts; the writes whose answers carried a reuse
  hint and the time they took; refused calls; `note_gap` entries (what the server should ship next); and time to
  working (the agent's time when the checks pass).
- **Reports**: `report.json` and `report.md`, and per persona its transcript, screenshots, final layouts and the
  reviewer's answers. The daily session adds a row per persona to the owner's "Newcomer runs" collection.

Not done: the fresh tier (a new admin pod per run needs `server pods create` on staging, so a Fly token in the
session's environment); more than four temporary pods a run (staging makes 5 per client address an hour); a
persona's walk with `walk_journey` (its writes would be dry runs; the simulator's are real, on a pod that is
deleted).
