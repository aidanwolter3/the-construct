# The Construct — notes for Claude

Server-driven UI: a Rust "pod" serves app layouts (JSON `UiNode` trees) plus normalized entities; native clients
(macOS desktop, Android, headless simulator) render them with shared Compose code and a shared Rust engine.
Only app today: Pixelfed (`pixelfed_home`). Read `README.md` for details and `docs/DECISIONS.md` (D1–D39) before
changing architecture; `docs/PROGRESS.md` has history and next steps. `DESIGN.md` is the original design doc
(product renamed from "Project Canvas"; AI layout editing was built, then shelved — see D32/D33).

## Repo map
- `crates/protocol` — wire types (`UiNode`, `ActionIntent`, `WorkspaceSpec`, `EntityRecord`).
- `crates/server` — the pod (Axum + SQLite): workspaces, read-only app versions + SSE, Pixelfed adapter
  (OAuth, feed, likes, media proxy). `validate.rs` checks layout files.
- `crates/client-core` — engine: SQLite cache, outbox sync, PKCE sign-in, SSE subscriber.
- `crates/client-ffi` — UniFFI bridge (Kotlin bindings live in `kmp-app/shared/src/jvmCommonMain`).
- `crates/fake-pixelfed` — fake upstream (primary test target); `crates/e2e` — end-to-end suite.
- `layouts/<app>/NNNN.json` — layout versions as code (see below).
- `kmp-app/` — Gradle: `:shared` (commonMain UI), `:desktopApp` (JVM + simulator), `:androidApp`.
- `deploy/` (Fly.io), `.github/workflows/ci.yml`, `scripts/`, `fixtures/`, `assets/icon/`.

## Commands
```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo run -p server                      # pod on 127.0.0.1:8080 (fake Pixelfed expected on :8081)
cargo run -p fake-pixelfed               # fake upstream on 127.0.0.1:8081
scripts/e2e.sh                           # e2e suite against the fake binary

# JVM / Kotlin (from kmp-app). On macOS gradle.properties pins Android Studio's JDK; elsewhere override it:
cargo build -p client-ffi --release      # Linux: target/release/libclient_ffi.so (macOS: scripts/build_ffi.sh)
cd kmp-app && ./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :shared:allTests :desktopApp:test

# Headless simulator: real shared UI -> PNGs (no window). Read the PNGs to check UI changes.
./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo \
  -Pscreen=phone -Pout=/tmp/sim -Pscript="shot start; click Like; assert-text Unlike; shot after"
```
Simulator script commands (shot, click, assert-text, wait-text, open-menu, open-history, select-version,
publish-version, set-server, offline, …) are documented in README "Simulation mode". Fixture mode needs no
server; `-PbaseUrl=http://127.0.0.1:8080` runs live against a local pod.

Android/emulator/device checks and the macOS window need the developer's Mac (`scripts/android_smoke.sh`,
`./gradlew :androidApp:installDebug`); cloud sessions can't run them — say so in your summary when a change
needs a device check.

### Cloud sessions
`scripts/cloud_setup.sh` installs the toolchains and warms the caches (client-ffi build + every Gradle
dependency), so the JVM test command above works without Maven Central rate limits (HTTP 429, issue #4). Gradle
also tries Google's Maven Central mirror first (`kmp-app/settings.gradle.kts`). The environment's setup script is
stored in Claude Code settings, not in the repo: on claude.ai/code open the environment menu in the session's
title bar → Edit → **Setup script**, and paste this (the setup script starts one directory above the checkout; if
it can't find one it clones a copy just to warm the caches):
```bash
#!/bin/bash
set -uo pipefail
for d in "$PWD/the-construct" "$PWD" /home/user/the-construct; do
  [ -f "$d/scripts/cloud_setup.sh" ] && exec bash "$d/scripts/cloud_setup.sh"
done
tmp=$(mktemp -d) && git clone --depth 1 https://github.com/aidanwolter3/the-construct "$tmp/the-construct" \
  && exec bash "$tmp/the-construct/scripts/cloud_setup.sh"
```
The warm-up (~2-3 min) runs in the background so sessions start immediately; builds started meanwhile wait on
Gradle's cache lock. `~/.construct-cloud-warmup.status` reads `running`, `done` or `failed` (log:
`~/.construct-cloud-warmup.log`). If the environment has no setup script or the warm-up failed, run
`CONSTRUCT_SETUP_WARM=foreground bash scripts/cloud_setup.sh` once.

## Deploys and CI
- Every PR/push: clippy + Rust tests, JVM tests, simulator screenshots (artifact).
- **Push to `main` auto-deploys staging** (https://the-construct-pod.fly.dev + fake Pixelfed) when the Rust job
  passes. Keep `main` green; server schema changes need a migration that works on staging's existing DB.
- Prefer branches + PRs for non-trivial work; check `gh run list` after pushing.
- Keep sessions to one task: file follow-up work as a GitHub issue with the `/ticket` skill
  (`.claude/skills/ticket`); each issue starts with a link that opens a new session on it.

## Layouts as code (only way to publish a new layout version)
Add `layouts/<app>/NNNN.json` (`{"note": "...", "schema": N, "layout": {...}}`, next number, no gaps; `schema` =
the current `protocol::SCHEMA_VERSION`), add one
`layout_file!("<app>", "NNNN")` line to `layouts::FILES` in `crates/server/src/layouts.rs`, run `cargo test`,
push. Never edit or renumber a deployed file (its `schema` included) — the server refuses to start. A breaking
schema change retires old versions via `protocol::RETIRED`, not by editing files (D36). Clients are read-only for
layout state.

## Conventions
- The engine (client-core) and the shared UI must not know any app (D34/D35): app changes ship as layout files
  (screens go in `Workspace.screens`) plus pod code. Only new generic vocabulary (node types, intents) needs a
  client build, and it bumps `protocol::SCHEMA_VERSION`. After changing a pixelfed_home layout or the comments
  data, regenerate the simulator fixtures: `UPDATE_FIXTURES=1 cargo test -p server --test fixtures`.
- Names: product "The Construct"; Kotlin package `org.theconstruct`; env vars `CONSTRUCT_*`; data dirs
  `~/.construct*`; OAuth redirect `construct://oauth`. Server env vars: README "Server runtime configuration".
- Clients default to staging; override with `CONSTRUCT_BASE_URL` / `-Dconstruct.baseUrl` (desktop),
  `-PconstructBaseUrl` or the in-app server picker (Android).
- Shared UI goes in `kmp-app/shared/src/commonMain`; keep Compose keys stable (state must survive layout updates).
- Record architecture decisions as new `docs/DECISIONS.md` entries (don't rewrite old ones; mark superseded).
- Read files with the Read tool — never `cat`, `head`, `tail`, `sed` or `awk` to read files (developer's rule).
- No model names in commit messages or docs prose.
