# The Construct — notes for Claude

Server-driven UI: a Rust "pod" serves app layouts (JSON `UiNode` trees) plus normalized entities; native clients
(macOS desktop, Android, headless simulator) render them with shared Compose code and a shared Rust engine.
Apps today: Pixelfed (`pixelfed_home`), Recipes (`recipes`) and Events (`watcher`, once the Watcher). Read `README.md` for details and `docs/DECISIONS.md` (D1–D52) and
`docs/decisions/` (newer ones, one file each) before changing architecture; open work is in GitHub issues; `docs/NOTIFICATIONS.md` is the notifications design (D52). `DESIGN.md` is the original design doc
(product renamed from "Project Canvas"; AI layout editing was built, then shelved — see D32/D33).

## Repo map
- `crates/protocol` — wire types (`UiNode`, `ActionIntent`, `WorkspaceSpec`, `EntityRecord`).
- `crates/pod-core` — the generic pod (Axum + SQLite): workspaces, read-only app versions + SSE, sessions and
  sign-ins, store. Knows apps only through `AppAdapter` (`adapter.rs`, D40). `validate.rs` checks layout files.
- `crates/app-pixelfed` — the Pixelfed adapter (OAuth, feed, likes, comments, media proxy, `pixelfed_*` tables) and
  the `pixelfed_demo` dev app (a `StaticAdapter`).
- `crates/app-recipes` — the recipes adapter (`recipes_*` tables, the recipe import migration, D41).
- `crates/app-collections` — Collections: data custom apps declare over MCP (`collections_*` tables, change sets,
  row edits; decision `collections`), fillers that keep rows current from the web and alerts (decision
  `collection-fillers`).
- `crates/app-watcher` — the Events app (app id `watcher`): the system collection `events`, one filler per watched
  website, alerts for rules; moves the old Watcher's `watcher_*` data in once (decision `collection-fillers`).
- `crates/web-extract` — fetching pages and reading structured data out of them (shared by fillers).
- `crates/pod-host` — one server, many pods (D46): `host.db`, pod tokens, temporary pods, routing by bearer token.
- `crates/server` — the server binary: `pod-host` over pods built from `pod-core` + the compile-time adapters
  (`server::adapters`); `server pods …` is the admin CLI.
- `crates/client-core` — engine: SQLite cache, outbox sync, PKCE sign-in, SSE subscriber.
- `crates/client-ffi` — UniFFI bridge (Kotlin bindings live in `kmp-app/shared/src/jvmCommonMain`).
- `fake-pixelfed/` — fake upstream (primary test target), a test tool kept outside `crates/`: the crate, the
  fixtures it serves, its Dockerfile and `fly.toml`; deployed by its own workflow. `crates/e2e` — end-to-end suite.
- `layouts/<app>/NNNN.json` — layout versions as code (see below).
- `kmp-app/` — Gradle: `:shared` (commonMain UI), `:desktopApp` (JVM + simulator), `:androidApp`.
- `deploy/` (Fly.io), `.github/workflows/ci.yml`, `scripts/`, `fixtures/`, `assets/icon/`.

## Commands
```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo run -p server                      # 127.0.0.1:8080; on loopback, callers without a pod token use the "dev" pod
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

### Testing on staging
Staging needs a pod token (D46). Make a temporary one (24 h, deleted afterwards; no secrets needed):
`curl -fsS -X POST https://the-construct-pod.fly.dev/v1/pods/temporary | jq -r .session_token`, then send it as
`Authorization: Bearer <token>`; in the simulator click "Try a temporary pod". Never put pod codes in git or files.

## Deploys and CI
- Every PR/push: clippy + Rust tests, JVM tests, simulator screenshots (artifact).
- **Push to `main` auto-deploys staging** (https://the-construct-pod.fly.dev) when the Rust job passes. The fake
  Pixelfed deploys separately (`.github/workflows/fake-pixelfed.yml`) when `fake-pixelfed/` or `Cargo.*` change.
  Keep `main` green; server schema changes need a migration that works on staging's existing DB.
- Prefer branches + PRs for non-trivial work; check `gh run list` after pushing.
- Every PR body must follow `.github/pull_request_template.md` and state (1) **New or removed feature**: no,
  or which (new `crates/protocol/features/` files, or ones moved to `features/removed/`) and (2) **Client update needed**: yes/no (does the desktop/Android app need
  a new build, or does the change ship through the pod/layout files alone?).
- PRs that change the interface (layouts, shared UI, new node types) must embed screenshots of the changed
  screens, taken with the headless simulator (`:desktopApp:simulate`), before/after where it helps. Images can't
  be uploaded through the API: push the PNGs to a side branch `screenshots/<pr-branch>` (never merged) and embed
  them with `https://github.com/aidanwolter3/the-construct/blob/screenshots/<pr-branch>/<file>.png?raw=true`
  (the repo is private, so `raw.githubusercontent.com` links don't render).
- Keep sessions to one task: file follow-up work as a GitHub issue with the `/ticket` skill
  (`.claude/skills/ticket`); each issue starts with a link that opens a new session on it.

## Layouts as code (only way to publish a new layout version)
Add `layouts/<app>/NNNN.json` (`{"note": "...", "layout": {...}}`, next number, no gaps; the features it uses are
worked out by the pod, decision `named-vocabulary`), run `cargo test`, push. There is no file list to edit (each adapter's
`build.rs` embeds the directory). Each file is a whole snapshot: if another PR took your number (add/add
conflict), merge both layouts into the next number. Never edit or renumber a deployed file — the server refuses to start. Vocabulary is removed by
moving its feature file to `crates/protocol/features/removed/` (decision `named-vocabulary`), never by editing layout files. Clients are read-only for
layout state.

## Conventions
- The engine (client-core), the shared UI and `pod-core` must not know any app (D34/D35/D40): app changes ship as
  layout files (screens go in `Workspace.screens`) plus adapter code (`crates/app-*`). Only new generic vocabulary (node types, intents) needs a
  client build. All vocabulary is named layout features: one file `crates/protocol/features/<name>.md` plus
  an arm in `protocol::required_features` (see that folder's README). There is no schema number
  (docs/decisions/named-vocabulary.md). After changing a pixelfed_home layout or the comments
  data, regenerate the simulator fixtures: `UPDATE_FIXTURES=1 cargo test -p server --test fixtures`.
- Names: product "The Construct"; Kotlin package `org.theconstruct`; env vars `CONSTRUCT_*`; data dirs
  `~/.construct*`; OAuth redirect `construct://oauth`. Server env vars: README "Server runtime configuration".
- Clients default to staging; override with `CONSTRUCT_BASE_URL` / `-Dconstruct.baseUrl` (desktop),
  `-PconstructBaseUrl` or the in-app server picker (Android).
- Shared UI goes in `kmp-app/shared/src/commonMain`; keep Compose keys stable (state must survive layout updates).
- Record architecture decisions as new files `docs/decisions/<topic-name>.md`, not numbered and not in
  `docs/DECISIONS.md` (closed at D52; numbers made parallel PRs conflict). Rules and template:
  `docs/decisions/README.md`. Don't rewrite old ones; mark superseded. CI runs `scripts/check_decisions.sh`.
- Read files with the Read tool — never `cat`, `head`, `tail`, `sed` or `awk` to read files (developer's rule).
- No model names in commit messages or docs prose.
- Commit messages follow standard git style: an imperative subject line of at most 70 characters, a blank line,
  then a body explaining what and why, with every line wrapped at 70 characters or fewer
  (trailers such as `Co-Authored-By` and long URLs are exempt).
