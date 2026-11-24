# The Construct — notes for Claude

Server-driven UI: a Rust pod serves app layouts (JSON `UiNode` trees) and entities; desktop, Android and a headless
simulator draw them with one Rust renderer (`construct-render`) over one Rust engine (decision `rust-clients`). Built-in layouts are core layouts, not listed:
Pixelfed (`pixelfed`), Events (`events`), Agents (`agents`), Taste (`taste`, with its piece sheet `taste_item`), Server health (`server_health`), the blank home (`shell`), Settings
(`settings`) and the inbox (`inbox`). There are no apps at runtime: a pod's layouts are the user's own (from core layouts or Collections),
starting with `home`, which clients open first and which embeds and presents the others on one engine
(decision `layouts-not-apps`).

**Read [CONTRIBUTING.md](CONTRIBUTING.md) first: its rules and checks apply to you.** Then, as the task needs:
[docs/design.md](docs/design.md) (the whole system breadth first: read before changing architecture),
[docs/layouts.md](docs/layouts.md) (layout files and vocabulary), the other guides in [docs/](docs/), and the decisions
([docs/DECISIONS.md](docs/DECISIONS.md) D1–D52, [docs/decisions/](docs/decisions/) newer). Open work is in GitHub
issues. `docs/history/` is archived; don't treat it as current.

## Where things are

- `crates/protocol` wire types; `crates/pod-core` the generic pod (knows sources only through `Source`,
  `adapter.rs`; `validate.rs` checks layout files); `crates/pod-host` many pods per server; `crates/server` the
  binary and `server pods` CLI.
- `crates/source-pixelfed`, `source-collections`, `source-events` (Events), `source-monitor`,
  `source-agents` (Agents: an external agent runner, [docs/agents.md](docs/agents.md)), `source-taste` (Taste: a
  feed of clothes from the owner's taste service, [docs/taste.md](docs/taste.md)), `source-dates` (the days
  around today), `source-shell` (the home core layout): the sources.
  `crates/web-extract`: the web module (page fetching and extraction, host policy, search, web prompts);
  `crates/agent` (`construct-agent`): the agent module every model call goes through; `pod_core::jacks`: the
  abilities a pod is plugged into (decisions `jacks`, `web-and-agent-modules`).
- `crates/client-core` the engine (cache, outbox, sign-in, event stream), a library of the clients.
- `crates/construct-elements`: the content-addressed construct's elements (hashes, references, component
  expansion, materializing), shared by the pod (`pod_core::construct`) and the engine (`client_core::construct`);
  it is the only layout system: a pod still on the old one is converted when it opens (decisions
  `content-addressed-construct`, `one-layout-system`).
- `crates/construct-render` the renderer and the clients (taffy, wgpu, a CPU rasterizer): the pod's MCP layout
  previews, `construct-viewer` (desktop; `scripts/macos_app.sh` bundles the Mac app) and the Android app
  (NativeActivity; `scripts/android_native.sh` builds and installs it, `android/` its manifest, resources and Java
  bridge, which only forwards to Rust: decision `android-native-bridge`) ([docs/rendering.md](docs/rendering.md),
  [docs/clients.md](docs/clients.md)); `crates/construct-web` the web client (that renderer in a browser, served at
  `/web` only with `CONSTRUCT_WEB=1`; `scripts/web_build.sh` builds it); `crates/construct-sim` the headless simulator over it
  ([docs/simulator.md](docs/simulator.md)).
- `fake-pixelfed/` the fake upstream (primary test target); `crates/e2e` the end-to-end suite.
- `docs/design.md` the design as a course; `crates/design-lessons` reads it into `docs/design-lessons.json`, which
  the developer's pod fetches from `main` to show it as the public course "Inside The Construct" (decision
  `design-doc-and-lessons`). After editing design.md run `UPDATE_DESIGN_LESSONS=1 cargo test -p design-lessons`.
- `crates/source-*/layouts/<layout>/NNNN.json` layout versions; `crates/protocol/features/` the named vocabulary
  (plumbing); `crates/pod-core/components/<name>/NNNN.json` the server components (porcelain, decision
  `plumbing-and-porcelain`); a source's own in `crates/source-*/components/` (decision `adapter-components`).
- Each crate's integration tests are one binary, `tests/integration/main.rs`: add a test file as a `mod` there
  ([docs/testing.md](docs/testing.md)).
- Data files live next to the code that reads them (decision `data-next-to-its-code`): test data in
  `crates/*/tests/data/`, simulator data in `crates/construct-sim/data/`; anything else ships.
  Test data is made up: invented people and places and `.example` domains, never real personal data.

## Commands

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo run -p server                      # 127.0.0.1:8080; callers without a pod token use the "dev" pod
cargo run -p fake-pixelfed               # 127.0.0.1:8081
scripts/e2e.sh                           # e2e suite against the fake binary

cargo run --release -p construct-render --features viewer --bin construct-viewer   # the desktop app

# Headless simulator: the Rust renderer to PNGs (CPU, no window). Read the PNGs to check UI changes.
cargo run -p construct-sim -- --workspace pixelfed_sample_no_login --screen phone --out /tmp/sim \
  --script "shot start; click Like; assert-text Unlike; shot after"
scripts/sim_scenarios.sh /tmp/sim        # the scenarios CI screenshots
```

Script commands and flags: [docs/simulator.md](docs/simulator.md). `--base-url http://127.0.0.1:8080` runs live
against a local pod (token in `CONSTRUCT_POD_TOKEN`). Easy to get wrong: `wait-text <ms> <text>` takes the timeout first, and a command's argument
is the rest of the entry as written, so `click "Sign in"` looks for the quote marks too (only `type` and `pick`
read quotes).

Cloud sessions can't run the Android app on a phone or open the macOS window; say so in your summary when a change
needs a device check.

## Cloud sessions

`scripts/cloud_setup.sh` installs the toolchains and warms the caches (every crate fetched, the simulator built,
cargo-nextest installed). The environment's setup
script is stored in Claude Code settings, not in the repo: on claude.ai/code open the environment menu in the
session's title bar → Edit → **Setup script**, and paste this (it starts one directory above the checkout; if it
can't find one it clones a copy just to warm the caches):

```bash
#!/bin/bash
set -uo pipefail
for d in "$PWD/the-construct" "$PWD" /home/user/the-construct; do
  [ -f "$d/scripts/cloud_setup.sh" ] && exec bash "$d/scripts/cloud_setup.sh"
done
tmp=$(mktemp -d) && git clone --depth 1 https://github.com/aidanwolter3/the-construct "$tmp/the-construct" \
  && exec bash "$tmp/the-construct/scripts/cloud_setup.sh"
```

The warm-up runs in the background; builds started meanwhile wait on cargo's lock. To wait for it, run
`scripts/await_warmup.sh`: it waits for the warm-up, restarts it if its process died (the status file can say
`running` long after), and fails with the log's location if it failed. If there is
no setup script, run `CONSTRUCT_SETUP_WARM=foreground bash scripts/cloud_setup.sh` once.

What a cloud session can't do, so don't spend time trying:
- **Re-run a CI job**: the API answers 403. Fix the cause (or the flaky test) and push; merging `main` in also
  starts a fresh run.
- **Delete remote branches**: merged PR branches are deleted by GitHub on merge.
- **Run the Android app or open the macOS window**: say which device checks are left in your summary. There is no
  Android SDK or NDK in the cloud: CI's `android` job builds the APK, and `release.yml` the release APK and the Mac
  app (on PRs that change their scripts).

Habits that avoid known traps:
- **Disk** is a fixed allowance of about 30 GB; incremental builds are off in cloud sessions (the setup script
  sets it) and dev builds carry little debug info (`Cargo.toml`). If `df` gets low, `rm -rf target/debug` and
  rebuild. Don't make a second checkout or a second target directory.
- **Stop processes by pid** (`kill $pid`, with the pid from `$!`), never `pkill -f`: the pattern matches the
  session's own shell and kills it (exit 144).
- **Wait with Monitor or PR events**, not `sleep` (blocked in the foreground).
- **`git fetch origin main`** before branching from, merging or comparing with main: the clone's `origin/main`
  is as old as the container.
- Rust is pinned in `rust-toolchain.toml` (CI uses the same file), so local clippy matches CI. Bump it in its own
  PR.

**Plugins**: the repo is also a Claude Code plugin marketplace (`.claude-plugin/marketplace.json`, plugins in
`plugins/`). `.claude/settings.json` enables them for sessions in this repo, cloud ones included; `afk` gives
`/afk` (keep working without the developer, then summarize). The marketplace is pinned to a commit (`ref` in
`.claude/settings.json`, a full SHA; Claude Code 2.1.324 or later), so a merge to `main` doesn't change the plugins
any session or Mac loads (issue #452). To ship a plugin change: merge it, then in a second PR set `ref` to that
merge commit's full SHA (`git rev-parse origin/main`); both PRs need the developer's review. On a Mac, install once
for every project with `/plugin marketplace add aidanwolter3/the-construct#<that SHA>` and
`/plugin install afk@the-construct`; after a bump, remove and re-add the marketplace with the new SHA.

**Staging** needs a pod token: make a temporary one with
`curl -fsS -X POST -H "X-Construct-Client: claude-session $CLAUDE_CODE_SESSION_ID" https://the-construct-pod.fly.dev/v1/pods/temporary | jq -r .session_token`
and send it as `Authorization: Bearer <token>`; in the simulator, click "Try a temporary pod". Staging holds 50
temporary pods: when done, delete yours with `curl -fsS -X DELETE -H "Authorization: Bearer <token>"
https://the-construct-pod.fly.dev/v1/pods/temporary`. Never put pod codes in git or files.
Staging's image is Debian slim (it carries Chromium for rendered fillers), but manage pods with the
`server pods …` subcommands ([docs/server.md](docs/server.md)).

**Secrets** are set on staging by the developer with `flyctl secrets set` (list in [docs/server.md](docs/server.md)).
There is one Anthropic key, `CONSTRUCT_INFER_API_KEY`, already set: every model feature (computed fields, `llm`
extraction, app building) uses it. Don't add a new key variable or ask for another secret when an existing one
fits; if a feature truly needs a new secret, say so before building it.

**Pod MCP tools**: after a change adds or renames tools of the pod's MCP server, the Claude connector keeps its old
tool list. Tell the developer to use "Refresh tools list" in Claude's connector settings (or remove and re-add the
connector); a new session started after that sees the new tools.

## Pull requests

- **"Open a PR" means ship it**: use the `ship` skill (`.claude/skills/ship`): sync with main, run the checks,
  open the PR, watch CI, fix failures, squash-merge when green and confirm the staging deploy. Stop before merging
  only when the developer says so ("don't merge", "draft", "I'll review"), or the PR isn't this session's.
- **Ask before a change that needs a client build** (a new node type, intent or field, or an engine store change):
  say so before starting, and offer a ticket instead if it wasn't part of the request. Every PR's summary says
  plainly whether a new desktop or Android build is needed.
- **Protected paths** (`.github/CODEOWNERS`: workflows, `deploy/`, plugins, `.claude/`, this file, the signing and
  cloud setup scripts) need the developer's approving review before they merge; the `ship` skill opens, greens and
  waits on such a PR instead of merging it (decision `protected-paths-and-environments`).
- Sessions started for tickets (`ticket` skill, "Starting sessions") each own their PR through to merged; the
  session that started them tracks them but doesn't watch their PRs.
- Pushes to `main` deploy staging.
- The PR body follows `.github/pull_request_template.md` and states **New or removed feature** and **Client update
  needed** (see CONTRIBUTING.md).
- Interface changes embed simulator screenshots. Images can't be uploaded through the API: push the PNGs to a side
  branch `screenshots/<pr-branch>` (never merged) and embed them with
  `https://raw.githubusercontent.com/aidanwolter3/the-construct/screenshots/<pr-branch>/<file>.png`.
- **One feature, one PR**, even a large one: don't split a feature into phases or a series of PRs, and don't
  ticket parts of it for later. Its decision file and doc updates go in that same PR, not a design PR before it
  or a docs PR after it. Split only when the developer asks.
- Keep sessions to one task: file unrelated follow-up work as a GitHub issue with the `/ticket` skill
  (`.claude/skills/ticket`).

## House rules

- **Change what the user actually sees.** A request about "my notifications" or "the shopping screen" is about
  the element their pod's home reaches there, which may be the user's own copy of a core layout (copies don't
  follow core layouts). Before changing a core layout, look at what real pods draw (on staging, MCP `list_layouts`:
  `reached` and `copy_of`, decision `trace-from-home`), and say which pods' copies the change won't reach; a fix
  for one user's screen may be an edit of their copy instead (or as well). Agents without the repo get the same
  rule from the pod's MCP instructions.

- No model names in commit messages or docs prose.
- Keep docs current: a change to behaviour, configuration or vocabulary updates the guide in `docs/` that covers
  it, in the same PR; a change to a piece, an interface or a name that `docs/design.md` describes updates it too
  (and regenerates `docs/design-lessons.json`).
