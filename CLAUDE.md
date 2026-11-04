# The Construct — notes for Claude

Server-driven UI: a Rust pod serves app layouts (JSON `UiNode` trees) and entities; desktop, Android and a headless
simulator draw them with shared Compose code over a shared Rust engine. Apps: Pixelfed (`pixelfed_home`), Recipes
(`recipes`) and Events (`watcher`), plus users' custom apps and Collections.

**Read [CONTRIBUTING.md](CONTRIBUTING.md) first: its rules and checks apply to you.** Then, as the task needs:
[docs/architecture.md](docs/architecture.md) (read before changing architecture), [docs/layouts.md](docs/layouts.md)
(layout files and vocabulary), the other guides in [docs/](docs/), and the decisions
([docs/DECISIONS.md](docs/DECISIONS.md) D1–D52, [docs/decisions/](docs/decisions/) newer). Open work is in GitHub
issues. `docs/history/` is archived; don't treat it as current.

## Where things are

- `crates/protocol` wire types; `crates/pod-core` the generic pod (knows apps only through `AppAdapter`,
  `adapter.rs`; `validate.rs` checks layout files); `crates/pod-host` many pods per server; `crates/server` the
  binary and `server pods` CLI.
- `crates/app-pixelfed`, `app-recipes`, `app-collections`, `app-watcher` (Events), `app-monitor`: the adapters.
  `crates/web-extract`: page fetching and extraction for fillers.
- `crates/client-core` the engine; `crates/client-ffi` UniFFI (Kotlin bindings in
  `kmp-app/shared/src/jvmCommonMain`); `kmp-app/shared/src/commonMain` the shared UI.
- `fake-pixelfed/` the fake upstream (primary test target); `crates/e2e` the end-to-end suite.
- `layouts/<app>/NNNN.json` layout versions; `crates/protocol/features/` the named vocabulary.
- Each crate's integration tests are one binary, `tests/integration/main.rs`: add a test file as a `mod` there
  ([docs/testing.md](docs/testing.md)).

## Commands

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo run -p server                      # 127.0.0.1:8080; callers without a pod token use the "dev" pod
cargo run -p fake-pixelfed               # 127.0.0.1:8081
scripts/e2e.sh                           # e2e suite against the fake binary

# JVM (from kmp-app; any JDK 21 in JAVA_HOME or on PATH). The engine library first: cargo build -p client-ffi --release
./gradlew -PskipRustBuild :shared:allTests :desktopApp:test

# Headless simulator: the real shared UI to PNGs. Read the PNGs to check UI changes.
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo \
  -Pscreen=phone -Pout=/tmp/sim -Pscript="shot start; click Like; assert-text Unlike; shot after"
```

Script commands and flags: [docs/simulator.md](docs/simulator.md). `-PbaseUrl=http://127.0.0.1:8080` runs live
against a local pod.

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

The warm-up (~2-3 min) runs in the background; builds started meanwhile wait on Gradle's cache lock.
`~/.construct-cloud-warmup.status` reads `running`, `done` or `failed` (log: `~/.construct-cloud-warmup.log`). If
there is no setup script or the warm-up failed, run `CONSTRUCT_SETUP_WARM=foreground bash scripts/cloud_setup.sh`
once.

**Staging** needs a pod token: make a temporary one with
`curl -fsS -X POST https://the-construct-pod.fly.dev/v1/pods/temporary | jq -r .session_token` and send it as
`Authorization: Bearer <token>`; in the simulator, click "Try a temporary pod". Never put pod codes in git or files.

## Pull requests

- Check `gh run list` after pushing; pushes to `main` deploy staging.
- The PR body follows `.github/pull_request_template.md` and states **New or removed feature** and **Client update
  needed** (see CONTRIBUTING.md).
- Interface changes embed simulator screenshots. Images can't be uploaded through the API: push the PNGs to a side
  branch `screenshots/<pr-branch>` (never merged) and embed them with
  `https://raw.githubusercontent.com/aidanwolter3/the-construct/screenshots/<pr-branch>/<file>.png`.
- Keep sessions to one task: file follow-up work as a GitHub issue with the `/ticket` skill (`.claude/skills/ticket`).

## House rules

- Read files with the Read tool, never `cat`, `head`, `tail`, `sed` or `awk` (the developer's rule).
- No model names in commit messages or docs prose.
- Keep docs current: a change to behaviour, configuration or vocabulary updates the guide in `docs/` that covers
  it, in the same PR.
