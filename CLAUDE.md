# The Construct — notes for Claude

Server-driven UI: a Rust pod serves app layouts (JSON `UiNode` trees) and entities; desktop, Android and a headless
simulator draw them with shared Compose code over a shared Rust engine. Built-in layouts are blueprints, not listed:
Pixelfed (`pixelfed`), Recipes (`recipes`), Events (`events`), the blank home (`shell`), Settings (`settings`) and the
inbox (`inbox`). There are no apps at runtime: a pod's layouts are the user's own (from blueprints or Collections),
starting with `home`, which clients open first and which embeds and presents the others on one engine
(decision `layouts-not-apps`).

**Read [CONTRIBUTING.md](CONTRIBUTING.md) first: its rules and checks apply to you.** Then, as the task needs:
[docs/architecture.md](docs/architecture.md) (read before changing architecture), [docs/layouts.md](docs/layouts.md)
(layout files and vocabulary), the other guides in [docs/](docs/), and the decisions
([docs/DECISIONS.md](docs/DECISIONS.md) D1–D52, [docs/decisions/](docs/decisions/) newer). Open work is in GitHub
issues. `docs/history/` is archived; don't treat it as current.

## Where things are

- `crates/protocol` wire types; `crates/pod-core` the generic pod (knows apps only through `AppAdapter`,
  `adapter.rs`; `validate.rs` checks layout files); `crates/pod-host` many pods per server; `crates/server` the
  binary and `server pods` CLI.
- `crates/app-pixelfed`, `app-recipes`, `app-collections`, `app-events` (Events), `app-monitor`, `app-shell` (the home blueprint): the adapters.
  `crates/web-extract`: page fetching and extraction for fillers.
- `crates/client-core` the engine; `crates/client-ffi` UniFFI (Kotlin bindings in
  `app/shared/src/jvmCommonMain`); `app/shared/src/commonMain` the shared UI.
- `fake-pixelfed/` the fake upstream (primary test target); `crates/e2e` the end-to-end suite.
- `crates/app-*/layouts/<app>/NNNN.json` layout versions; `crates/protocol/features/` the named vocabulary (plumbing);
  `crates/pod-core/components/<name>/NNNN.json` the server components (porcelain, decision `plumbing-and-porcelain`).
- Each crate's integration tests are one binary, `tests/integration/main.rs`: add a test file as a `mod` there
  ([docs/testing.md](docs/testing.md)).
- Data files live next to the code that reads them (decision `data-next-to-its-code`): test data in
  `crates/*/tests/data/`, simulator data in `app/desktopApp/src/sim/resources/`; anything else ships.
  Test data is made up: invented people and places and `.example` domains, never real personal data.

## Commands

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo run -p server                      # 127.0.0.1:8080; callers without a pod token use the "dev" pod
cargo run -p fake-pixelfed               # 127.0.0.1:8081
scripts/e2e.sh                           # e2e suite against the fake binary

# JVM (from app; any JDK 21 in JAVA_HOME or on PATH). The engine library first: cargo build -p client-ffi --release
./gradlew -PskipRustBuild :shared:allTests :desktopApp:test

# Headless simulator: the real shared UI to PNGs. Read the PNGs to check UI changes.
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_sample_no_login \
  -Pscreen=phone -Pout=/tmp/sim -Pscript="shot start; click Like; assert-text Unlike; shot after"
```

Script commands and flags: [docs/simulator.md](docs/simulator.md). `-PbaseUrl=http://127.0.0.1:8080` runs live
against a local pod. Easy to get wrong: `wait-text <ms> <text>` takes the timeout first, and a command's argument
is the rest of the entry as written, so `click "Sign in"` looks for the quote marks too (only `type` and `pick`
read quotes).

Cloud sessions can't run the Android app on a phone or open the macOS window; say so in your summary when a change
needs a device check.

## Cloud sessions

`scripts/cloud_setup.sh` installs the toolchains and warms the caches (client-ffi build and every Gradle
dependency), so the JVM tests work without Maven Central rate limits (HTTP 429, issue #4). The environment's setup
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

The warm-up (~3-5 min) runs in the background; builds started meanwhile wait on Gradle's cache lock. Before the
JVM tests or the simulator, run `scripts/await_warmup.sh`: it waits for the warm-up, restarts it if its process
died (the status file can say `running` long after), and fails with the log's location if it failed. If there is
no setup script, run `CONSTRUCT_SETUP_WARM=foreground bash scripts/cloud_setup.sh` once.

What a cloud session can't do, so don't spend time trying:
- **Re-run a CI job**: the API answers 403. Fix the cause (or the flaky test) and push; merging `main` in also
  starts a fresh run.
- **Delete remote branches**: merged PR branches are deleted by GitHub on merge.
- **Run the Android app or open the macOS window**: say which device checks are left in your summary.

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

**Staging** needs a pod token: make a temporary one with
`curl -fsS -X POST https://the-construct-pod.fly.dev/v1/pods/temporary | jq -r .session_token` and send it as
`Authorization: Bearer <token>`; in the simulator, click "Try a temporary pod". Never put pod codes in git or files.
Staging's image has no shell (distroless): `fly ssh console -C` can't run `rm` or scripts there. Use the
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
- Sessions started for tickets (`ticket` skill, "Starting sessions") each own their PR through to merged; the
  session that started them tracks them but doesn't watch their PRs.
- Pushes to `main` deploy staging.
- The PR body follows `.github/pull_request_template.md` and states **New or removed feature** and **Client update
  needed** (see CONTRIBUTING.md).
- Interface changes embed simulator screenshots. Images can't be uploaded through the API: push the PNGs to a side
  branch `screenshots/<pr-branch>` (never merged) and embed them with
  `https://raw.githubusercontent.com/aidanwolter3/the-construct/screenshots/<pr-branch>/<file>.png`.
- Keep sessions to one task: file follow-up work as a GitHub issue with the `/ticket` skill (`.claude/skills/ticket`).

## House rules

- No model names in commit messages or docs prose.
- Keep docs current: a change to behaviour, configuration or vocabulary updates the guide in `docs/` that covers
  it, in the same PR.
