# Project Canvas

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

Milestones 1-7 are implemented: the protocol crate, the Axum server, the macOS desktop app, the Android app
(see [Android](#android)), the SQLite cache + outbox (M5), the Pixelfed feed with OAuth2 PKCE sign-in (M6), and
the AI layout agent with versioned layouts, rollback and SSE push (M7). A fake Pixelfed is the primary end-to-end
upstream, and a headless simulation mode renders the shared UI to PNGs (see Simulation mode).

Quick links: [Server runtime configuration](#server-runtime-configuration),
[Try it on your phone with a real Pixelfed instance](#try-it-on-your-phone-with-a-real-pixelfed-instance),
[Try the AI layout agent](#try-the-ai-layout-agent), [Milestone check recipes](#milestone-check-recipes).

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord (+ JSON Schema
                     registry behind the `schema` feature)
crates/server        Axum server ("the pod"): GET /v1/workspaces/{home,pixelfed_demo,pixelfed_home}, persistent
                     counters (SQLite), Pixelfed adapter (auth, feed normalization, actions, media proxy),
                     versioned layouts + rollback + SSE push, layout agent (rules / Anthropic providers)
crates/client-core   CanvasEngine: fetches the workspace, holds state, applies optimistic mutations (SQLite cache +
                     outbox sync worker), OAuth2 PKCE sign-in (pod session in a secure store), agent prompts,
                     SSE layout subscriber
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
crates/fake-pixelfed fake Pixelfed (Mastodon API + OAuth PKCE, admin/fault controls), the primary e2e upstream;
                     single binary with embedded fixtures, Dockerfile for a sidecar image
crates/e2e           end-to-end suite: pod + engine in-process against the fake (default) or a real Pixelfed
fixtures/            counter_home_bundle.json, pixelfed_home_bundle.json (DESIGN.md §2.2C),
                     pixelfed_home_feed_bundle.json (simulator); pixelfed/ (Mastodon API JSON + media served by
                     fake-pixelfed, recorded/ real-instance responses, images/ seed JPEGs)
dev/pixelfed/        docker compose stack for the optional local real Pixelfed (dev-only secrets)
scripts/build_ffi.sh builds libclient_ffi.dylib and regenerates the Kotlin bindings
scripts/build_android.sh  cross-compiles libclient_ffi.so for Android (cargo-ndk)
scripts/e2e.sh       end-to-end suite against the fake binary or a real Pixelfed (End-to-end tests)
scripts/android_smoke.sh  installs and launches the Android app on an emulator, checks it renders (Tests)
scripts/pixelfed_dev.sh   up/seed/record/status/down/reset for the local real Pixelfed (Local Pixelfed instance)
kmp-app/             Gradle project: :shared (model + Compose renderer), :desktopApp (JVM, JNA),
                     :androidApp (Android, JNA aar)
```

## Prerequisites

- macOS on Apple Silicon, with Rust installed through rustup (target `aarch64-apple-darwin`; add it with
  `rustup target add aarch64-apple-darwin` if needed).
- JDK 21 (`:desktopApp` sets `jvmToolchain(21)`). This repo uses Android Studio's bundled JBR, which is set in
  `kmp-app/gradle.properties` (`org.gradle.java.home`). Set `JAVA_HOME` to match:

```bash
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
```

## Run the demo

Terminal 1: start the server on 127.0.0.1:8080 (set `CANVAS_ADDR` to use another address).

```bash
cargo run -p server
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq .
```

Server details:

- State lives in SQLite at `$CANVAS_DATA_DIR/server.db`; `CANVAS_DATA_DIR` defaults to `~/.canvas-server`
  (created if missing). The path is logged at startup. Delete the file to start from a zero count.
- `GET /v1/workspaces/home` returns the counter fixture with `core://counters/home`.count set to the stored count.
- `POST /v1/counters/home/{increment,decrement,reset}` (optional JSON body, ignored) returns
  `{"ok":true,"count":N}`. Decrementing at 0 is refused with 422 `{"error":"count_below_zero","count":0}` and
  changes nothing. Unknown counters/ops are 404. Every mutation is logged at info with the resulting count.
- Test hook, only when started with `CANVAS_TEST_HOOKS=1` (otherwise 404): `POST /v1/test/fault` with
  `{"status":503,"count":3}` makes the next 3 POSTs (other than `/v1/test/*`) return that status (400-599)
  without being applied; `"count":0` clears it. Each injected fault is logged.

Versioned layouts and push channel (M7, DECISIONS D22). The layouts of `home`, `pixelfed_demo` and the signed-in
`pixelfed_home` feed are versioned (append-only; version 1 is the built-in layout, stored on first use; entities
are not versioned). The signed-out `pixelfed_home` screen is not versioned.

- `GET /v1/workspaces/{id}` serves the workspace's ACTIVE layout (see the layout catalog below) and sets
  `X-Canvas-Layout-Version: <workspace revision>` plus `X-Canvas-Layout-Id` / `X-Canvas-Layout-Id-Version` (absent
  on the signed-out `pixelfed_home` screen and on its 401). The revision bumps on every visible change (an edit, a
  restore, a switch to another layout), so clients track one number per workspace.
- `GET /v1/workspaces/{id}/versions` -> `[{version, created_at, prompt, provider}]`, newest first (the active
  layout's history).
- `POST /v1/workspaces/{id}/rollback {"version": N}` -> appends a copy of version N of the active layout as a new
  version, pushes it, and returns `{version, rolled_back_to, workspace, revision, layout_id}`. Unknown version: 404
  `version_not_found`; unversioned workspace: 404 `workspace_not_found`; bad body: 400.
- Versions and rollback (and the agent endpoint) need a valid pod session (`Authorization: Bearer <session>`,
  else 401 with a `detail` saying so) unless the server runs with `CANVAS_AGENT_OPEN=1` (dev/demo only; logged as a
  warning at startup).
- Workspace ids are 1..=64 characters of `[A-Za-z0-9_-]`; anything else gets 400 `invalid_workspace_id` (with the
  id truncated in the echo) on the workspace GET, versions, rollback, events and the agent endpoint.
- `GET /v1/workspaces/{id}/events` (SSE): first message is the current revision, then one message per new revision
  of that workspace, `event: layout` / `data: {"layout_id":"home.counter","layout_version":1,"version":N,
  "workspace_id":"home"}` (`version` = the revision), and a `: keepalive` comment
  every 15 s. No session needed (events carry only a version number; clients refetch the workspace, which is
  authorized as usual); for `pixelfed_home` a bearer that is not a valid session gets 401 like the workspace GET.

```bash
CANVAS_AGENT_OPEN=1 cargo run -p server
curl -sN http://127.0.0.1:8080/v1/workspaces/home/events &                  # event: layout, version 1
curl -s -X POST -H 'content-type: application/json' -d '{"version":1}' \
  http://127.0.0.1:8080/v1/workspaces/home/rollback | jq .version         # 2 (and the stream gets version 2)
curl -s http://127.0.0.1:8080/v1/workspaces/home/versions | jq .
```

Layout catalog (DECISIONS D26). Each versioned workspace has several layouts (`<workspace>.<name>`), each with its
own append-only history, and one active layout. Built-ins: `pixelfed_home.*` and `pixelfed_demo.*`: `classic`
(default), `editorial`, `compact`, `minimal`; `home.counter` (default), `home.big_counter`. Specs are stored once
per distinct content (SHA-256, zstd), so a restore costs a ~0.2 KB row and a distinct edit ~1.2 KB.

- `GET /v1/layouts[?workspace_id=<ws>]` -> `{"layouts":[{id, workspace_id, title, description, preview_hint,
  builtin, current_version, created_at, updated_at, active, active_version?}], "workspaces":[{workspace_id,
  revision, layout_id, layout_version}]}` (filtered: also `workspace_id`, `workspace_revision`).
- `GET /v1/layouts/{id}`; `GET /v1/layouts/{id}/versions[?limit=N&before=V]` -> `{"versions":[{version,
  created_at, source, prompt?, provider?, hash, bytes}], "next_before"?}` newest first (`source`: `seed`, `agent`,
  `rollback`, `restore`, `manual`); `GET /v1/layouts/{id}/versions/{n}` -> metadata plus `layout` (the root node).
- `POST /v1/layouts/{id}/activate` `{}` or `{"version": n}` (restores n into the layout first) -> `{workspace_id,
  revision, layout_id, layout_version, unchanged}`; pushed on the SSE channel.
- `POST /v1/layouts/{id}/restore {"version": n}` -> appends a copy as a new version (undoable itself; content
  equal to the newest -> `"unchanged": true`) -> `{layout_id, workspace_id, version, restored_from, unchanged,
  active, revision?}`; pushed when the layout is active.
- Reads follow the workspace GET (`pixelfed_home` layouts need a session; the unfiltered list hides them from
  anonymous callers); activate/restore need a session unless `CANVAS_AGENT_OPEN=1`. Errors: 400
  `invalid_layout_id` / `invalid_request`, 404 `layout_not_found` / `version_not_found`.

```bash
CANVAS_AGENT_OPEN=1 cargo run -p server
curl -s 'http://127.0.0.1:8080/v1/layouts?workspace_id=pixelfed_demo' | jq '.layouts[] | {id, title, active}'
curl -s -X POST -H 'content-type: application/json' -d '{}' \
  http://127.0.0.1:8080/v1/layouts/pixelfed_demo.editorial/activate
# -> {"layout_id":"pixelfed_demo.editorial","layout_version":1,"revision":2,"unchanged":false,"workspace_id":"pixelfed_demo"}
curl -s http://127.0.0.1:8080/v1/layouts/pixelfed_demo.classic/versions | jq '.versions[] | {version, source, prompt}'
curl -s -X POST -H 'content-type: application/json' -d '{"version":1}' \
  http://127.0.0.1:8080/v1/layouts/pixelfed_demo.classic/restore
# -> {"active":false,"layout_id":"pixelfed_demo.classic","restored_from":1,"unchanged":true,"version":1,...}
curl -s http://127.0.0.1:8080/v1/layouts/home.big_counter/versions/1 | jq .layout
```

Layout agent (M7, DECISIONS D25). `POST /v1/agent/modify_layout {"prompt": "...", "workspace_id": "pixelfed_home"}`
(`workspace_id` optional, default `pixelfed_home`; any versioned workspace) turns the prompt into an RFC 6902 patch
of the latest layout, validates the result (must deserialize as a `UiNode`, root still the same `core://Workspace`
with 1..=50 slots, optimistic-mutation endpoints start with `/v1/`, at most 256 KB), commits it as a new version
and pushes it on the SSE channel. Same auth as versions/rollback. It edits the signed-in feed layout, so it works
without a Pixelfed session.

- 200: a `WorkspaceSpec` body `{version, workspace, entities: {}, layout_version, provider, model, patch}` (plus
  `explanation` from the LLM, and `"unchanged": true` with no new version when the patch is empty).
- 422 `invalid_patch` (`detail`, `patch`, `provider`) when the patch does not apply or the result is invalid;
  422 `unsupported_prompt` (`supported`: example prompts) from the rules provider; 422 `use_rollback` for "reset
  layout"; 502 `provider_error`; 503 `provider_not_configured`; 400 bad body; 404 unknown workspace. Nothing is
  committed on any error. Every prompt is logged with provider, model, op count and outcome.
- Providers, chosen at startup (logged): `CANVAS_LLM_PROVIDER` = `auto` (default: `anthropic` if
  `ANTHROPIC_API_KEY` is set, else `rules`) | `anthropic` | `rules`. `anthropic` without a key: every prompt
  answers 503 (the server still starts).
  - `rules`: deterministic and offline. Phrases (case-insensitive, composable in one prompt, applied in the order
    written, idempotent): "hide (post) captions", "show captions", "media at the (very) top" / "photos first",
    "move the action bar above the photo", "add a weather widget (at the top)" (a placeholder slot: a text in a
    `core://Container` with 16 dp padding; no weather component exists), "reset layout" (-> 422 pointing at rollback), and "remove the action bar", which is
    rejected on purpose (422 `invalid_patch`) to demonstrate validation.
  - `anthropic`: the Messages API with structured outputs. `ANTHROPIC_API_KEY`; `ANTHROPIC_BASE_URL` (default
    `https://api.anthropic.com`); `CANVAS_LLM_MODEL` (model id; default in `agent::anthropic::DEFAULT_MODEL`);
    `CANVAS_LLM_FALLBACKS=0` turns off the server-side refusal fallback. 120 s timeout, up to 2 retries on
    429/5xx/overloaded.

```bash
CANVAS_AGENT_OPEN=1 CANVAS_ADDR=127.0.0.1:8681 cargo run -p server
curl -X POST http://127.0.0.1:8681/v1/agent/modify_layout -H "Content-Type: application/json" \
  -d '{"prompt": "Hide post captions and put media at the very top"}'
# -> 200 {"layout_version":2,"provider":"rules","patch":[3 replace ops],"workspace":{...},...}
curl -s http://127.0.0.1:8681/v1/workspaces/pixelfed_home/versions | jq .
curl -s -X POST -H 'content-type: application/json' -d '{"version":1}' \
  http://127.0.0.1:8681/v1/workspaces/pixelfed_home/rollback | jq .version   # 3
```

```bash
CANVAS_TEST_HOOKS=1 cargo run -p server
curl -s -X POST http://127.0.0.1:8080/v1/counters/home/increment          # {"count":1,"ok":true}
curl -s -X POST -H 'content-type: application/json' \
  -d '{"status":503,"count":2}' http://127.0.0.1:8080/v1/test/fault       # next 2 mutations -> 503
```

Terminal 2: build the FFI library and bindings, then run the desktop app.

```bash
scripts/build_ffi.sh            # optional: :shared:buildRustFfi runs it before the desktop compile
                                # (dylib built to target/aarch64-apple-darwin/release, copied to target/release)
cd kmp-app
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
./gradlew :desktopApp:run                                              # the staging pod (default)
./gradlew :desktopApp:run -Dcanvas.baseUrl=http://127.0.0.1:8080       # or: CANVAS_BASE_URL=... ./gradlew ...
```

All clients default to the deployed staging pod, `https://the-construct-pod.fly.dev`; for the local server
started above, pass its URL explicitly (`CANVAS_BASE_URL=http://127.0.0.1:8080`). A "Project Canvas" window opens with the counter layout served by the server. Clicking
**Increment** or **Reset** prints the intent to stdout and the count updates:

```
[desktop] dispatch: {"intent":"action.mutation.optimistic",...}
[CanvasEngine] dispatch: {...}
[CanvasEngine] core://counters/home#count = 1
```

Options for the desktop app (set as `-D` JVM properties or environment variables):

- `canvas.baseUrl` / `CANVAS_BASE_URL` (default `https://the-construct-pod.fly.dev`, the staging pod; local server:
  `http://127.0.0.1:8080`)
- `canvas.workspace` / `CANVAS_WORKSPACE` (default `home`; `pixelfed_demo` shows the Pixelfed feed fixture)

Separately, `-PskipRustBuild` is a Gradle property (`./gradlew -PskipRustBuild :desktopApp:run`) that skips
the cargo build step.

The engine keeps a SQLite cache at `~/.canvas/cache.db` (tables `entities`, `active_workspace`,
`action_outbox`; see docs/DECISIONS.md D13). Every fetched workspace (layout + entities) is written there and
loaded on startup, so the last workspace shows when the server is unreachable; the fetch is retried every
2 seconds until the server responds. Each click updates the entity and queues a PENDING `action_outbox` row in
one transaction (`sqlite3 ~/.canvas/cache.db "SELECT * FROM action_outbox;"`). The old
`workspace_<id>.json` cache files are ignored.

Queued rows are sent by a background sync worker (docs/DECISIONS.md D14), oldest first and one at a time,
as `POST <base_url><endpoint>`. Clicks made while offline (or with the app closed before they were sent) stay
in the queue and the UI shows "N pending"; they are flushed as soon as the server is reachable again,
including on the next launch. Each accepted row is deleted. A row the server rejects with a 4xx (e.g.
decrementing below 0) is deleted and its field rolled back, with later queued clicks re-applied on top. A 5xx,
408, 429 or network error keeps the row and retries it with exponential backoff (2 s, 4 s, ... capped at 60 s). A
re-fetched workspace has the still-pending clicks re-applied, so they never visibly disappear. The engine logs
each attempt, e.g. `[CanvasEngine] outbox #3 POST http://127.0.0.1:8080/v1/counters/home/increment -> 200,
deleted (0 pending)`.

## Server runtime configuration

Everything the pod (`cargo run -p server`) reads from the environment. Empty values count as unset. The chosen
Pixelfed instance, database path and layout agent provider are logged at startup. `GET /healthz` answers 200 `ok`
(no auth; the engine's 2 s reconnect probe uses it).

| Variable | Default | Meaning |
|---|---|---|
| `CANVAS_ADDR` | `127.0.0.1:8080` | Listen address. Use `0.0.0.0:8080` to accept LAN connections. |
| `CANVAS_DATA_DIR` | `~/.canvas-server` | Directory of `server.db` (counters, Pixelfed app/account/sessions, media keys, layout versions); created if missing. |
| `CANVAS_TEST_HOOKS` | unset | `1` enables `POST /v1/test/fault` (fault injection; otherwise 404). |
| `PIXELFED_INSTANCE_URL` | `http://127.0.0.1:8081` (the fake) | Pixelfed instance for pod->upstream calls, and the only instance accepted in auth calls. `http://localhost:8090` for the local real Pixelfed, `https://<instance>` for a public one. |
| `PIXELFED_PUBLIC_URL` | = instance URL | Base for browser-facing links (the authorize page), e.g. `http://10.0.2.2:8081` for the emulator while the pod calls `127.0.0.1:8081`. Also accepted in auth calls. |
| `PIXELFED_MEDIA_HOSTS` | unset | Comma-separated `host`, `host:port` or `scheme://host[:port]`: extra origins the media proxy may fetch (CDN/S3) besides the instance and public URLs. Media elsewhere become placeholders. |
| `PIXELFED_EXTRA_REDIRECTS` | unset | Comma-separated exact redirect URIs accepted by `/v1/auth/pixelfed/app`, besides `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback` and `canvas://oauth`. |
| `PIXELFED_OWNER_ACCOUNT_ID` | unset | The only upstream account id allowed to sign in (replaces a pinned owner). Unset: the first account to sign in is pinned; others get 403 `account_not_owner`. |
| `PIXELFED_ALLOW_ACCOUNT_CHANGE` | unset | `1`/`true`: any account may sign in and replace the stored one (no pinning). |
| `CANVAS_SESSION_TTL_DAYS` | `90` | Pod session lifetime in days (1..=36500; other values warn and use 90). |
| `CANVAS_AGENT_OPEN` | unset | `1`: layout versions, rollback, the agent endpoint, layout activate/restore and `pixelfed_home` catalog reads need no pod session. Local demos only (warned at startup). |
| `CANVAS_LLM_PROVIDER` | `auto` | Layout agent provider: `auto` (`anthropic` if `ANTHROPIC_API_KEY` is set, else `rules`), `anthropic`, `rules`. `anthropic` without a key: the server starts and prompts answer 503. |
| `ANTHROPIC_API_KEY` | unset | Enables the Anthropic provider (never logged). |
| `CANVAS_LLM_MODEL` | `claude-opus-5-5` | Model id for the Anthropic provider (`agent::anthropic::DEFAULT_MODEL`). |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | Messages API base URL (proxy/gateway). |
| `CANVAS_LLM_FALLBACKS` | on | `0`/`false` turns off the server-side refusal fallback of the Anthropic request. |
| `RUST_LOG` | `info` | `tracing` filter, e.g. `RUST_LOG=server=debug,info`. |

<a id="fake-pixelfed-environment"></a>`cargo run -p fake-pixelfed`:

| Variable | Default | Meaning |
|---|---|---|
| `FAKE_PIXELFED_ADDR` | `127.0.0.1:8081` | Listen address. If unset and `PORT` is set (most PaaS), `0.0.0.0:$PORT`. |
| `FAKE_PIXELFED_PUBLIC_URL` | `http://<bound addr>` (`0.0.0.0` shown as `127.0.0.1`) | Replaces every `{{BASE}}` in the fixture JSON, so emitted media/profile/status URLs point at it. `http://10.0.2.2:8081` when the emulator must load them directly. |
| `FAKE_PIXELFED_AUTO_APPROVE` | unset | `1`: `GET /oauth/authorize` redirects straight back with a code (same as `auto=1` in the query); otherwise an HTML page with an **Authorize** button. |
| `FAKE_PIXELFED_TOKEN` | unset | A pre-seeded valid access token, to skip OAuth in QA (survives resets). |
| `FAKE_PIXELFED_FIXTURES` | embedded | Fixture directory to load instead of the embedded copies (e.g. `fixtures/pixelfed` while editing). |
| `FAKE_PIXELFED_ADMIN` | loopback only | `1` serves `/_admin` on any address, `0` never; unset, only when bound to loopback. |
| `RUST_LOG` | `info` | `tracing` filter. |

## Try it on your phone with a real Pixelfed instance

Nothing has to be registered by hand: the pod registers its own OAuth app on the instance (`POST /api/v1/apps`,
redirect `canvas://oauth`) on the first sign-in. The pod keeps the upstream token; the phone only gets a pod
session (DECISIONS D17, D19).

1. Start the pod for the instance (any account on it; the first one to sign in becomes the pod's owner):

   ```bash
   PIXELFED_INSTANCE_URL=https://<instance> CANVAS_DATA_DIR=~/.canvas-pixelfed cargo run -p server
   # add PIXELFED_MEDIA_HOSTS=<cdn host> if the instance serves media from a CDN/S3 host (else placeholders)
   ```

2. Make the pod reachable from the phone. Simplest is USB: `adb reverse tcp:8080 tcp:8080` and base URL
   `http://localhost:8080`. A LAN address (`CANVAS_ADDR=0.0.0.0:8080`, `http://<mac-ip>:8080`) or an HTTPS URL also
   works for the engine, which talks through Rust and is not restricted by Android's network security config; but
   debug builds allow cleartext to `localhost`, `127.0.0.1` and `10.0.2.2` only for Java/Kotlin traffic, and images
   load through Coil/OkHttp from the pod's media proxy, so over plain-HTTP LAN the feed images fail. Use `adb
   reverse` or HTTPS. The authorize page itself is the instance's own `https://` URL, opened in the phone's browser.

3. Build and install the debug APK, then launch it on the feed workspace (extras are honoured by debug builds
   only):

   ```bash
   cd kmp-app && export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
   ./gradlew :androidApp:installDebug
   adb shell am start -S -n org.genui.canvas.android/.MainActivity --es baseUrl http://localhost:8080 --es workspace pixelfed_home
   ```

4. Tap **Sign in with Pixelfed**: the browser opens the instance's authorize page; log in and approve; the
   `canvas://oauth` redirect returns to the app, which shows "Loading your feed…" and then `@you` and your home
   timeline. Like and bookmark are applied at once and go through the outbox to the instance (queued while
   offline, flushed on reconnect). **Sign out** deletes the session and the cached private feed.

Notes:

- Pixelfed throttles `/oauth/*` to 10 requests/minute per IP, and each sign-in uses two `/oauth/token` requests;
  repeated attempts can get 429 for a minute.
- A different account than the pinned owner gets "This pod belongs to a different Pixelfed account." Delete
  `server.db` (or set `PIXELFED_OWNER_ACCOUNT_ID` / `PIXELFED_ALLOW_ACCOUNT_CHANGE=1`) to change owners.
- Passport quirk (D20): Passport 13 rejects a token whose user id equals the (first-party, confidential) OAuth
  client's id. Instances with few clients and users can hit this; the local dev stack works around it by starting
  client ids at 1000. A public instance normally has far more clients than the colliding user ids.
- Verified against the local real Pixelfed 0.15.2 (`PIXELFED_INSTANCE_URL=http://localhost:8090` plus
  `adb reverse tcp:8090 tcp:8090`, see "Local Pixelfed instance") on the emulator; not yet against a public
  instance.

## Try the AI layout agent

The agent turns a prompt into a JSON Patch of the current layout, validates it, stores it as a new layout version
and pushes it to every connected client over SSE (D22, D25).

- **Offline, no key:** the default `rules` provider understands fixed phrases (combinable): "hide post captions",
  "show captions", "put media at the very top" / "photos first", "move the action bar above the photo", "add a
  weather widget"; "remove the action bar" is rejected on purpose to show validation.
- **With the Anthropic provider:** set `ANTHROPIC_API_KEY` (optionally `CANVAS_LLM_MODEL`, default `claude-opus-5-5`) and restart
  the pod; the startup log names the provider and model. Free-form prompts then work. (Not yet run against the live
  API: no key was available during development.)
- **In the app:** tap the sparkle button (bottom right, "Edit layout with AI"), type a prompt or pick a chip, and
  tap **Send**. The panel shows "Layout updated (vN, <provider>)" and the feed changes in place; other clients on the
  same pod update through the push channel. The prompt needs a signed-in session (or `CANVAS_AGENT_OPEN=1`).
- **Versions and rollback** (`CANVAS_AGENT_OPEN=1` is for local demos only; otherwise add
  `-H "Authorization: Bearer <session>"`):

  ```bash
  CANVAS_AGENT_OPEN=1 cargo run -p server
  curl -s -X POST http://127.0.0.1:8080/v1/agent/modify_layout -H 'content-type: application/json' \
    -d '{"prompt": "Add a weather widget at the top"}' | jq '{layout_version, provider, patch}'
  curl -s http://127.0.0.1:8080/v1/workspaces/pixelfed_home/versions | jq .
  curl -s -X POST -H 'content-type: application/json' -d '{"version":1}' \
    http://127.0.0.1:8080/v1/workspaces/pixelfed_home/rollback | jq '{version, rolled_back_to}'
  ```

- **Other layouts and going back in time** (D26): the agent edits the workspace's active layout. Switch layouts or
  restore any earlier version of one; both are pushed to connected clients:

  ```bash
  curl -s http://127.0.0.1:8080/v1/layouts?workspace_id=pixelfed_home | jq '.layouts[] | {id, title, active, current_version}'
  curl -s -X POST -H 'content-type: application/json' -d '{}' \
    http://127.0.0.1:8080/v1/layouts/pixelfed_home.minimal/activate | jq .
  curl -s -X POST -H 'content-type: application/json' -d '{"version":2}' \
    http://127.0.0.1:8080/v1/layouts/pixelfed_home.classic/restore | jq .
  ```

## Milestone check recipes

DESIGN.md M5 (offline click lands in the outbox, flushed on reconnect), with the counter workspace:

```bash
cargo run -p server                                                  # terminal 1
cd kmp-app && CANVAS_BASE_URL=http://127.0.0.1:8080 ./gradlew :desktopApp:run   # terminal 2: Count: N, Online
# stop the server with Ctrl-C in terminal 1: the status line goes Offline
# click Increment 3 times: the count updates at once and the status line shows "3 pending"
sqlite3 ~/.canvas/cache.db "SELECT id, endpoint, status, retry_count FROM action_outbox;"   # 3 PENDING rows
cargo run -p server                                                  # terminal 1 again: within ~2 s flushed, rows deleted
sqlite3 ~/.canvas/cache.db "SELECT count(*) FROM action_outbox;"     # 0
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq '.entities["core://counters/home"].count'
```

DESIGN.md M7 (prompt -> valid patched `WorkspaceSpec`; the open app recomposes without a restart):

```bash
CANVAS_AGENT_OPEN=1 cargo run -p server
curl -s -X POST http://127.0.0.1:8080/v1/agent/modify_layout -H "Content-Type: application/json" \
  -d '{"prompt": "Hide post captions and put media at the very top"}' | jq '{layout_version, provider, patch}'
# a desktop app or simulator showing pixelfed_home on this pod switches to the new layout within a second
```

## Fake Pixelfed upstream

`crates/fake-pixelfed` is a Mastodon-compatible stand-in for a Pixelfed instance and the **primary end-to-end
target** (docs/DECISIONS.md D16): no account, no containers, deterministic fixtures, and controllable faults. The
real Pixelfed below is optional (recording fixtures, contract test). It serves `fixtures/pixelfed/` (compiled into
the binary) with in-memory state that resets on restart or on `POST /_admin/reset`.

```bash
cargo run -p fake-pixelfed                       # listens on 127.0.0.1:8081
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/timelines/home | jq '.[].id'
                                                 # (started with FAKE_PIXELFED_TOKEN=dev)
```

Its environment variables are listed in [Server runtime configuration](#fake-pixelfed-environment).

**Sidecar / Docker.** One self-contained binary (release: 3.4 MB, 2.1 MB stripped with thin LTO; ~3.5 MB RSS
idle, ~5 MB after a few hundred requests; the Docker container ~1 MB). Image: `rust:1-slim-bookworm` builder ->
`gcr.io/distroless/cc-debian12:nonroot`, 51 MB.

```bash
docker build -f crates/fake-pixelfed/Dockerfile -t canvas-fake-pixelfed .      # from the repo root
docker run --rm -p 127.0.0.1:8081:8081 -e FAKE_PIXELFED_ADMIN=1 \
  -e FAKE_PIXELFED_PUBLIC_URL=http://127.0.0.1:8081 canvas-fake-pixelfed
E2E_FAKE_URL=http://127.0.0.1:8081 cargo test -p e2e --test suite                # suite against it
```

Inside the container it binds `0.0.0.0:8081`, so the admin routes stay off unless `FAKE_PIXELFED_ADMIN=1`; it
stops on SIGTERM.

**Admin endpoints** (test controls; in-process the same as `AppState::reset`, `add_faults`, `clear_faults`,
`report`, `requests`, `favourited_ids`, `bookmarked_ids`):

- `POST /_admin/reset[?keep_auth=1]`: favourites, bookmarks and counts back to the fixtures; fault rules, request
  log and grant counters cleared; apps, codes and tokens dropped (as after an instance wipe) unless `keep_auth=1`.
- `GET /_admin/state[?tail=50]`: `counts` (apps, codes, access/refresh tokens, statuses, requests),
  `grants` (authorization_code, refresh_token, client_credentials), `favourited` / `bookmarked` ids, active
  `faults`, and the last `tail` `requests` (`method`, `path` with query, `status`, `ms`, `fault`; 499 = the client
  hung up first, e.g. on its timeout). Admin calls are not logged.
- `POST /_admin/faults {"rules":[...]}` appends rules, returns the active ones; `DELETE /_admin/faults` clears.

```bash
curl -s -X POST http://127.0.0.1:8081/_admin/faults -H 'content-type: application/json' -d '{"rules":[
  {"path_prefix":"/api/v1/statuses","kind":"status","status":503,"times":2},
  {"kind":"rate_limit","retry_after":2,"times":1},
  {"path_prefix":"/api/v1/timelines","method":"GET","kind":"latency","ms":1500,"times":3},
  {"kind":"expire_tokens"}]}'
```

A rule matches requests whose path starts with `path_prefix` (default: every non-admin path) and, if given,
whose `method` matches, for the next `times` matches (default: until cleared). Kinds: `status` (401 is shaped like
an expired token: `{"error":"The access token is invalid"}` + `WWW-Authenticate: Bearer`; 429 adds `Retry-After:
1`; others `{"error":"<reason>"}`), `rate_limit` (429 `{"error":"Too Many Attempts."}` + `Retry-After:
<retry_after>`), `latency` (delay `ms`, then handle normally; adds to a status rule), `expire_tokens` (applied at
once, not stored: every access token stops working, refresh tokens stay valid). For a single request, send
`X-Fake-Fault: 401|429|500|503|<status>|latency=<ms>` (comma-separated, e.g. `latency=500,503`); the pod does not
forward it, so through the pod use the rules.

Endpoints: `POST /api/v1/apps` and `POST /oauth/token` (form-urlencoded or JSON bodies), `GET|POST
/oauth/authorize` (authorization code + PKCE; only `S256` accepted, the verifier is checked, codes are single-use
and expire after 10 minutes), `GET /api/v1/accounts/verify_credentials`, `GET /api/v1/timelines/home` (`limit`,
`max_id`, `Link: rel="next"`), `POST /api/v1/statuses/{id}/{favourite,unfavourite,bookmark,unbookmark}`
(idempotent, returns the updated status; a reblog id acts on the reblogged status; unknown id -> 404
`{"error":"Record not found"}`) and media under `/storage/<file>`. The `/api/v1` endpoints other than apps need
`Authorization: Bearer <token>`, else 401 `{"error":"The access token is invalid"}`. Each request, app
registration, code/token issuance and favourite/bookmark change is logged at info.

The timeline has five entries: a 3-image carousel, a reblog of a single-image post, a landscape single image
(already favourited, no alt text), a text-only post, and a content-warning (sensitive) post with a portrait
image. Media are small generated PNGs with a big digit (1-6) or avatar letter so screenshots show which image is
visible.

## End-to-end tests

`crates/e2e` runs the real pod (`server::app`, in-memory store) and the client-core engine in-process against a
Pixelfed upstream; the engine reaches the pod through a TCP proxy the tests can switch off (offline). The same
suite (`tests/suite.rs`) runs against either upstream: sign-in via PKCE with a scripted browser, the feed, a like,
an offline like flushed on reconnect, a bookmark, sign-out. It leaves the upstream as it found it.

```bash
cargo test -p e2e                       # in-process fake (part of cargo test --workspace; suite < 1 s)
scripts/e2e.sh                          # same suite against the fake-pixelfed binary on a free port
scripts/e2e.sh --faults                 # ... plus the fault tests below
scripts/e2e.sh --upstream real          # a real Pixelfed (default http://localhost:8090, user alice)
```

- **Fake** (default, `E2E_UPSTREAM=fake`): the browser GETs the authorize URL with `auto=1`; upstream state is read
  from `/_admin/state`. `E2E_FAKE_URL=<url>` uses an already running fake (binary or Docker image, admin on) and
  resets it first.
- **Real** (`E2E_UPSTREAM=real`, optional contract test, e.g. a scheduled CI job on a throwaway instance):
  `PIXELFED_INSTANCE_URL` (default `http://localhost:8090`), the browser scripts Pixelfed's web login (by e-mail;
  a bare login gets `@example.test`) and consent form as `E2E_PIXELFED_LOGIN` / `E2E_PIXELFED_PASSWORD` (default
  `alice` / `alice-dev-pass`), and upstream state is checked with `E2E_PIXELFED_TOKEN` (default
  `dev/pixelfed/.seed/<login>_token`). Each run registers one OAuth app on the instance (the pod is in-memory).
- **Fault tests** (`tests/upstream_faults.rs`, always the in-process fake): upstream 503 x2 -> pod 502 -> the client
  retries with backoff -> flushed; upstream 429 -> pod 429 -> retried, never rolled back; expired upstream token ->
  the pod refreshes transparently (action and feed); latency past the 10 s timeouts -> retried (this one takes
  ~11 s); reset with and without `keep_auth` through a live session (a full reset signs the client out; a new
  sign-in re-registers the app).

## Pixelfed (pod adapter)

The server (`server::pixelfed`, docs/DECISIONS.md D17) adapts one Pixelfed instance: it owns the upstream
access token, normalizes the home timeline into Canvas entities and serves the `pixelfed_home` workspace. The
client only holds a pod session token and sends `Authorization: Bearer <session>`.

Configuration (`PIXELFED_*`, `CANVAS_SESSION_TTL_DAYS`): see
[Server runtime configuration](#server-runtime-configuration).

Endpoints:

- `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}` -> `{client_id, scopes, authorize_endpoint}`
  (registers an app upstream once per redirect_uri; a cached app is re-checked with a `client_credentials` grant
  and re-registered if the instance no longer knows it). Another instance -> 400 `instance_not_allowed`.
  `redirect_uri` must be `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback`, `canvas://oauth`
  or a `PIXELFED_EXTRA_REDIRECTS` entry, else 400 `redirect_uri_not_allowed`.
- `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}` -> `{session_token,
  account:{id, username, display_name, avatar_url}}`. Upstream rejection -> 400 `oauth_exchange_failed`. An
  account other than the pinned owner -> 403 `account_not_owner` (the stored account is untouched).
- `POST /v1/auth/logout` (bearer) -> `{ok:true}`; deletes the session (401 if it is not valid).
- `GET /v1/workspaces/pixelfed_home`: with a valid session, the home feed bundle (`queries["timeline:home"]` +
  posts/users/media entities, PostCard layout with like/bookmark optimistic mutations). Without a bearer: 200 with a
  signed-out bundle (a "Sign in with Pixelfed" `action.auth.login` button). With a bearer that no longer works
  (unknown session, or the upstream rejected the stored token): 401 with the same signed-out bundle as the body and
  `X-Canvas-Session: invalid`, so the client drops its stale session. Upstream unreachable -> 502. The upstream
  request is `GET /api/v1/timelines/home?limit=20&include_reblogs=true` (Pixelfed hides boosts without it).
- Upstream tokens: the pod stores the access token and, when the upstream issues them (Pixelfed does), the refresh
  token and expiry. On an upstream 401 (feed or action) it refreshes once (`grant_type=refresh_token`, serialized
  so concurrent requests share one refresh) and retries; if that fails, the feed signs out as above.
- `POST /v1/pixelfed/posts/like {id, liked}` and `POST /v1/pixelfed/posts/bookmark {id, bookmarked}` -> `{ok, id,
  is_liked, like_count, is_bookmarked}`. No session 401; upstream 4xx passed through; upstream 5xx/unreachable 502.
- `GET /v1/pixelfed/media/{key}`: unauthenticated media/avatar proxy. Every image URL in a bundle on an allowed
  origin (instance, public URL, `PIXELFED_MEDIA_HOSTS`) is rewritten to
  `http://<request Host>/v1/pixelfed/media/<key>`; other image URLs become `null`. Unknown keys are 404, a key for
  a disallowed origin 403; redirects are only followed to allowed origins; bodies over 25 MB are refused (502).
- `POST /v1/demo/ack`: stateless 200 used by the static `pixelfed_demo` fixture's like button.

Try it with curl against the fake (auto-approve skips the browser):

```bash
FAKE_PIXELFED_ADDR=127.0.0.1:8081 FAKE_PIXELFED_AUTO_APPROVE=1 cargo run -p fake-pixelfed &
cargo run -p server &
curl -s -X POST -H 'content-type: application/json' http://127.0.0.1:8080/v1/auth/pixelfed/app \
  -d '{"instance_url":"http://127.0.0.1:8081","redirect_uri":"http://127.0.0.1:9/callback"}'
# GET <authorize_endpoint>?response_type=code&client_id=..&redirect_uri=..&scope=read+write&state=s
#   &code_challenge=<S256(verifier)>&code_challenge_method=S256  -> 302 to redirect_uri?code=..
curl -s -X POST -H 'content-type: application/json' http://127.0.0.1:8080/v1/auth/pixelfed/token \
  -d '{"instance_url":"http://127.0.0.1:8081","code":"..","code_verifier":"..","redirect_uri":"http://127.0.0.1:9/callback"}'
curl -s -H "Authorization: Bearer $SESSION" http://127.0.0.1:8080/v1/workspaces/pixelfed_home | jq .queries
```

### Sign in (client)

The engine runs the browser half of OAuth2 PKCE itself (docs/DECISIONS.md D19): a `core://Button` with
`{"intent":"action.auth.login","instance_url":..}` (the signed-out bundle's button) starts it on a background
thread; `auth_json` in every snapshot goes `pending` -> `signed_in` (username, display name, avatar) or `error`.

- FFI: `EngineHandle.newWithOptions(storagePath, baseUrl, workspaceId, secureStore?, urlOpener?, redirectUri?)`,
  `handleAuthRedirect(url): Boolean`, `logout()`. `SecureStore` (get/set/delete) and `UrlOpener` (open -> Boolean)
  are Kotlin callback interfaces; exceptions in them are contained.
- `redirectUri = null`: loopback (desktop). The engine listens once on `http://127.0.0.1:47823/callback` for 5
  minutes (a fixed port so the pod reuses one upstream OAuth app; if 47823 is busy, a random port, which registers
  another app). A custom URI (Android: `canvas://oauth`): the app forwards the redirect to `handleAuthRedirect`.
  The custom-scheme flow is also kept in the secure store (`pending_auth:<baseUrl>`, 10 minutes), so a redirect
  that cold-starts the app after its process was killed still signs in; one that matches nothing shows "Sign-in
  was interrupted; please try again".
- `secureStore = null`: macOS Keychain (service `dev.canvas.client`, key `pod_session:<baseUrl>`), in-memory
  elsewhere. The session survives restarts; `logout()` deletes it on the pod and locally.
- `urlOpener = null` (or `open` returns false): `auth_json` says "no browser available" and the authorize URL is
  logged (`[CanvasEngine] no browser available; to sign in, open this URL ...`); opening it by hand still works.
  With the fake, append `&auto=1` to approve without clicking.
- A 401 from the pod signs the client out (a stale session on `pixelfed_home` gets one, see above). A like queued
  while the session was invalid stays pending (not rolled back) and is sent after the next sign-in.
- Signing out (or a 401) removes the cached private feed (`pixelfed://` entities, the feed workspace row) from
  `cache.db`; `logout()` also drops queued Pixelfed actions, a 401 keeps them for the next sign-in. The DB runs
  with `secure_delete=ON` and the WAL is truncated after the purge, so the rows don't linger in the file's bytes.
- UI: while signed in the status line shows `@username` and a **Sign out** button (`CanvasController.logout()`);
  "Signing in…" while pending, then "Loading your feed…" from the code exchange until the first signed-in fetch
  lands (only then `@username`; a second "Sign in" tap meanwhile is ignored); "Sign-in failed: <message>" on
  error, wrapped to at most 2 lines. Messages are short and human ("This pod belongs to a different Pixelfed
  account.", "Can't reach your pod.", ...); the URL/status/body detail is in the `[CanvasEngine]` log only.

Per platform (DECISIONS D21):

- **Desktop** (`desktopApp` Main.kt): no store (Keychain), loopback redirect, and the default browser via
  `java.awt.Desktop.browse` (else `open <url>`). Clicking "Sign in with Pixelfed" opens the browser; after approving,
  the browser lands on the "Signed in" page and the window shows the feed.
- **Android**: `canvas://oauth` intent filter on `MainActivity` (`launchMode="singleTask"`, so the redirect returns to
  the same activity and engine through `onNewIntent`); the session is encrypted with an AndroidKeyStore AES/GCM key
  (`canvas_session_key`) and kept in SharedPreferences `canvas_secure`; the authorize page opens with `ACTION_VIEW`.
  For the emulator, start the pod with `PIXELFED_PUBLIC_URL=http://10.0.2.2:8081` (and the fake with
  `FAKE_PIXELFED_PUBLIC_URL=http://10.0.2.2:8081`) so the browser can reach the authorize page. If the browser is not
  usable (e.g. Chrome's first-run screen), deliver the redirect by hand:
  `adb shell am start -a android.intent.action.VIEW -d '"canvas://oauth?code=...&state=..."'`.
- **Simulator** (live mode): in-memory store and an auto-approving "browser" for the fake, or a scripted Pixelfed
  web login for a real instance with `-PpixelfedLogin=<user>:<password>` (see Simulation mode).

### Layout agent and live layout updates (client)

client-core side of M7 (docs/DECISIONS.md D24):

- `{"intent":"action.agent.prompt","prompt_text":".."}` posts the prompt to `POST /v1/agent/modify_layout` (with
  the engine's workspace id and the bearer when signed in) on a background thread. `agent_json` in every snapshot:
  `{"state":"idle"}` -> `running` -> `done` (`message` "Layout updated (vN, rules)", or "No change needed (vN)" with
  `unchanged: true`; `provider`, `layout_version`; the returned layout is shown at once, entities and queued actions
  untouched) or `error` with a readable message: "I can't do that yet. Try: <3 examples>" (`unsupported_prompt`),
  "That change would break the layout: <detail>" (`invalid_patch`), "The layout changed meanwhile; please try
  again." (409), "The AI layout provider isn't configured on this pod.", "The AI provider failed; please try
  again.", "Sign in to change the layout." (401), or "offline: try again when online". A newer layout version
  arriving from elsewhere (SSE refetch) resets a finished status to idle, so the panel never shows a stale result.
- The engine remembers the served layout version (`X-Canvas-Layout-Version`) and, while online, keeps
  `GET /v1/workspaces/<id>/events` open on a `canvas-engine-events` thread. A `layout` event with a newer version
  refetches the workspace (queued optimistic changes are re-applied on top); equal or older versions are ignored.
  The stream reconnects after errors with a 1 s -> 30 s backoff (and after 45 s of silence), and closes when the
  engine is dropped; a line or event over 64 KB drops the connection and reconnects. Log lines: `[CanvasEngine] events: connected to ...`, `events: layout v3 pushed (have v2);
  refetching`, `agent: applied layout v3 (rules)`.
- FFI: `FfiStateSnapshot.agentJson` -> `EngineSnapshot.agentJson`.

### Layout catalog (client)

client-core / FFI / bridge API over the pod's `/v1/layouts` endpoints (docs/DECISIONS.md D26, D27), for a layout
browser. All calls are blocking: on Kotlin call them on `Dispatchers.IO`, never the UI thread.

| Kotlin (`CanvasEngineBridge`) | Rust (`CanvasEngine` / `EngineHandle`) | Pod |
|---|---|---|
| `listLayouts()` | `list_layouts()` | `GET /v1/layouts?workspace_id=<engine's>` |
| `layoutVersions(id, before?, limit?)` | `layout_versions(id, before, limit)` | `GET /v1/layouts/:id/versions` |
| `layoutVersion(id, n)` | `layout_version(id, n)` | `GET /v1/layouts/:id/versions/:n` (with `layout`) |
| `activateLayout(id, version?)` | `activate_layout(id, version)` | `POST /v1/layouts/:id/activate` |
| `restoreLayoutVersion(id, n)` | `restore_layout_version(id, n)` | `POST /v1/layouts/:id/restore` |

- Each returns the pod's JSON body. Failures throw `LayoutApiException` (Kotlin; `kind` Offline / Unauthorized /
  NotFound / Server) with a readable message, e.g. "Sign in to change layouts." (writes need a session unless the
  pod runs with `CANVAS_AGENT_OPEN=1`) or "Can't reach your pod. Check your connection and try again.". A 401 with
  a stale session signs out like any other request.
- An activate / restore that changes the screen refetches the workspace before returning, so the UI shows the new
  layout immediately (the SSE echo is a no-op).
- Every snapshot carries `layout_json` = `{"layout_id","layout_version","revision"}` (`CanvasState.activeLayout`
  in Kotlin) to mark the active layout and version; `{}` when unknown.
- Simulation mode: `FakeEngineBridge` serves a small in-memory catalog (`home`: counter, big_counter;
  `pixelfed_demo`: classic with 3 versions, editorial, compact).

## Local Pixelfed instance (real, optional)

Optional: the fake above is the primary e2e target. Use the real instance to re-record
`fixtures/pixelfed/recorded/` and as a contract test (`scripts/e2e.sh --upstream real`).


A real Pixelfed (v0.15.2, `ghcr.io/pixelfed/pixelfed`, digest-pinned) + MariaDB 11.4 + Redis 7.4 in Docker, for
end-to-end tests against the genuine API. It is loopback-only (`127.0.0.1:8090`), federation is off, open
registration is off, and every secret in `dev/pixelfed/pixelfed.env` is a committed dev-only dummy. It never talks
to a public instance. docs/DECISIONS.md D20.

```bash
scripts/pixelfed_dev.sh up       # start colima if needed, compose up, wait healthy, first-run init
scripts/pixelfed_dev.sh seed     # users + posts; prints the admin token (also dev/pixelfed/.seed/admin_token)
scripts/pixelfed_dev.sh record   # re-record fixtures/pixelfed/recorded/*.json with the admin token
scripts/pixelfed_dev.sh status   # | logs [svc] | down (keeps data) | reset (deletes all data)
```

Seeded data (idempotent: existing users/posts are kept, a fresh token is minted per run): `canvasadmin` /
`canvas-dev-pass` (admin), `alice` / `alice-dev-pass`, `bob` / `bob-dev-pass`; tokens in
`dev/pixelfed/.seed/{admin,alice,bob}_token` (gitignored). canvasadmin follows alice; alice posts a 3-image
carousel, a portrait single image without alt text, a landscape image with alt text and a sensitive post with a CW;
bob posts one image that alice boosts. Images come from `fixtures/pixelfed/images/` (numbered, coloured JPEGs).

Point the pod at it with `PIXELFED_INSTANCE_URL=http://localhost:8090` (use `localhost`, not `127.0.0.1`: see
below). Things the real instance does differently from the fake:

- Web routes (`/login`, `/oauth/authorize`, `/oauth/token`) only answer for Host `localhost` (Laravel domain
  routing on `APP_DOMAIN`, port ignored); with Host `127.0.0.1` or `10.0.2.2` they are 404. `/api/*` and
  `/storage/*` answer on any Host. For the Android emulator use `adb reverse tcp:8090 tcp:8090` and
  `localhost:8090`.
- `/oauth/authorize` and `/oauth/token` are throttled to 10 requests/minute per IP (429). Each pod sign-in uses
  two `/oauth/token` requests (the cached-app `client_credentials` check and the code exchange).
- The home timeline hides boosts unless `include_reblogs=true`, hides text-only posts, and emits absolute `Link`
  headers built from `APP_URL` that drop `include_reblogs`.
- Media/avatar URLs are absolute `http://localhost:8090/storage/...`.
- JSON differences the normalizer tolerates (covered by `test_pixelfed_recorded_timeline_normalization` over
  `fixtures/pixelfed/recorded/`): `content` without a `<p>` wrapper, empty alt text `""` (-> null), small integer
  media ids, integer `meta.original.aspect` (`1`), no `meta.small`, millisecond `created_at`; a boost's inner status
  is Pixelfed's extended entity (`pf_type`, `liked_by`, `reply_count`, media `mime`), and `_pe` accounts lack
  `avatar_static`. The token response has `created_at` as an ISO string, plus `expires_in` (one year) and
  `refresh_token` (rotated on refresh), and no `scope`.
- An unfavourite answer's `favourites_count` can lag (the count is updated by a queue job). The client ignores
  action response bodies (D14), so the optimistic count stays right; a refetch within a second or two may show the
  old count.
- The authorize page needs a logged-in Pixelfed web session (login is by **e-mail**:
  `canvasadmin@example.test`, `alice@example.test`, `bob@example.test`). A client the user already approved is
  redirected straight back without the consent page.

End to end with the desktop simulator (pod on 8480 so it does not clash with a dev pod on 8080):

```bash
CANVAS_ADDR=127.0.0.1:8480 CANVAS_DATA_DIR=/tmp/canvas-pf PIXELFED_INSTANCE_URL=http://localhost:8090 \
  cargo run -p server &
cd kmp-app && ./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home \
  -PbaseUrl=http://127.0.0.1:8480 -PpixelfedLogin=canvasadmin:canvas-dev-pass \
  -Pscript="shot signed-out; click Sign in with Pixelfed; wait-text 30000 @canvasadmin; wait-text 20000 Alice Example boosted; wait-images 30000; shot feed"
```

Upstream quirks handled by the script: the image ships without `storage/app/public` (default avatar/header are
installed from `fixtures/pixelfed/images/`); OAuth client ids start at 1000 because Passport 13 rejects a token
whose user id equals its (first-party, confidential) client id; a user's first `POST /api/v1/media` can answer
`200 []` (retried). Upstream's Horizon and scheduler containers are replaced by one `queue:work` worker (~75 MB
instead of >1 GB).

### Container runtime (no Homebrew, no sudo)

Colima + Lima + docker CLI + compose plugin as release binaries in `~/.local/bin` (Lima's guest agents and
templates in `~/.local/share/lima`, the compose plugin in `~/.docker/cli-plugins/`). Versions used: Colima
v0.10.3, Lima v2.2.1, docker CLI 29.9.2 (static), Compose v5.6.1.

```bash
mkdir -p ~/.local/bin ~/.docker/cli-plugins && cd "$(mktemp -d)"
curl -fLO https://github.com/abiosoft/colima/releases/download/v0.10.3/colima-Darwin-arm64
curl -fLO https://github.com/abiosoft/colima/releases/download/v0.10.3/colima-Darwin-arm64.sha256sum
curl -fLO https://github.com/lima-vm/lima/releases/download/v2.2.1/lima-2.2.1-Darwin-arm64.tar.gz
curl -fLO https://github.com/lima-vm/lima/releases/download/v2.2.1/SHA256SUMS
curl -fLO https://github.com/docker/compose/releases/download/v5.6.1/docker-compose-darwin-aarch64
curl -fLO https://github.com/docker/compose/releases/download/v5.6.1/docker-compose-darwin-aarch64.sha256
curl -fLO https://download.docker.com/mac/static/stable/aarch64/docker-29.9.2.tgz
shasum -a 256 -c colima-Darwin-arm64.sha256sum
grep ' lima-2.2.1-Darwin-arm64.tar.gz$' SHA256SUMS | shasum -a 256 -c
shasum -a 256 -c docker-compose-darwin-aarch64.sha256
# docker-29.9.2.tgz: download.docker.com publishes no checksum; expected sha256
# ea8a2ce5ac31f9f5f3fb711a7a485e1e46cc7a69798a629a70306f6e6af25b7a (as downloaded 2026-11-01)
tar xzf lima-2.2.1-Darwin-arm64.tar.gz -C ~/.local          # bin/limactl, bin/lima, share/lima/...
install -m 755 colima-Darwin-arm64 ~/.local/bin/colima
tar xzf docker-29.9.2.tgz && install -m 755 docker/docker ~/.local/bin/docker
install -m 755 docker-compose-darwin-aarch64 ~/.docker/cli-plugins/docker-compose
export PATH="$HOME/.local/bin:$PATH"
colima start --cpu 2 --memory 4 --disk 15 --vm-type vz --mount-type virtiofs
```

Footprint: the four containers use ~290 MB RAM idle; images 2.6 GB (pixelfed 2.04 GB, mariadb 481 MB, redis
59 MB); `~/.colima` ~4 GB on the host (sparse VM disks).

Uninstall: `scripts/pixelfed_dev.sh reset; colima stop; colima delete -f; rm -rf ~/.colima ~/.lima
~/.local/share/lima ~/.local/share/doc/lima; rm -f ~/.local/bin/{colima,limactl,lima,docker,*.lima}
~/.docker/cli-plugins/docker-compose`, and drop the `colima` context from `~/.docker/config.json` (or
`docker context rm colima` before removing the CLI).

## Simulation mode

A headless feedback loop: `:desktopApp:simulate` renders the real shared `CanvasApp` to an offscreen
virtual screen (no window), runs a small script against it and writes PNG screenshots. A run takes about
2 seconds. Run it from `kmp-app` with `JAVA_HOME` set as above:

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
  -Pscript="shot start; click Increment; click Increment; assert-text Count: 2; shot after; offline; shot offline"

./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pscreen=desktop
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home \
  -Pscript="wait-images; shot feed; swipe-left tag:carousel:; assert-text Page 2 of 3; shot page2; click Like; assert-text 28; shot liked"
# layout agent (fixture mode: the fake engine's rules subset; live mode: the pod's agent endpoint)
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home \
  -Pscript="click tag:agent-fab; type tag:agent-prompt Hide post captions and put media at the very top; click tag:agent-send; wait-text 2000 Layout updated; shot after"
./gradlew -PskipRustBuild :desktopApp:simulate -PscriptFile=my_script.txt -Pdensity=1 -Pout=/tmp/shots
./gradlew -PskipRustBuild :desktopApp:simulate -PbaseUrl=http://127.0.0.1:8080 -Pscript="shot live"   # live mode
# live sign-in against the pod + fake upstream (see "Sign in (client)")
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home -PbaseUrl=http://127.0.0.1:8080 \
  -Pscript="shot signed-out; click Sign in with Pixelfed; wait-text 20000 @canvasdemo; wait-images 20000; shot feed; click Like; shot liked"
```

Flags (all optional):

| Flag | Default | Meaning |
|---|---|---|
| `-Pworkspace` | `home` | Workspace id. In fixture mode it selects the default fixture. |
| `-Pscreen` | `phone` | `phone` (390x844 dp), `tablet` (820x1180), `desktop` (1280x800), or `WxH` in dp. |
| `-Pdensity` | per screen | Pixels per dp: phone 3, tablet 2, desktop 2, `WxH` 2. PNG size = dp x density. |
| `-Pscript` | `shot initial` | Script text (see below). |
| `-PscriptFile` | | Script read from a file. Use either this or `-Pscript`, not both. |
| `-Pmode` | `fixture` | `fixture` or `live`. Live mode is selected automatically when `-PbaseUrl` is given. |
| `-Pfixture` | per workspace | Bundle JSON in fixture mode. Defaults: `home` uses `fixtures/counter_home_bundle.json`, `pixelfed_demo` uses `fixtures/pixelfed_home_bundle.json` (one post), `pixelfed_home` uses `fixtures/pixelfed_home_feed_bundle.json` (the fake upstream's five-post timeline, normalized as the pod does, with `queries`). Other workspaces need `-Pfixture`. |
| `-PbaseUrl` | `https://the-construct-pod.fly.dev` | Server for live mode (the staging pod unless given; local: `http://127.0.0.1:8080`). |
| `-PtimeoutMs` | `10000` | Live mode only: how long to wait for the workspace to load before the first step. |
| `-Pout` | `kmp-app/desktopApp/build/sim/<yyyyMMdd-HHmmss>` | Output directory. |
| `-PpixelfedLogin` | | Live mode only: `<e-mail or seed username>:<password>` for a real Pixelfed (below). |
| `-Pstorage` | fresh temp dir | Live mode only: engine storage dir (holds `cache.db`). It is kept after the run, so a second run with the same dir exercises restart persistence. The temp default is deleted on exit. |

Relative paths (`-Pfixture`, `-PscriptFile`, `-Pout`, `-Pstorage`) resolve against the **repo root**, the task's working
directory, so `-Pfixture=fixtures/counter_home_bundle.json` works.

**Fixture mode** (the default) needs no server, and no Rust build when run with `-PskipRustBuild` (without
it, the desktop compile triggers `:shared:buildRustFfi`, i.e. cargo). A fake in-memory engine
(`FakeEngineBridge`) loads the bundle, applies `OptimisticMutate` clicks with the D4 expression subset, and
toggles connectivity for `offline`/`online`. It models the outbox: clicks made while offline are applied
locally and queued (the status line shows "N pending"), and `online` flushes them FIFO to an in-memory fake
server. That server rejects a `.../decrement` that would take the count below 0; the fake then rolls the
field back and re-applies later queued rows (D8). The fake also answers the layout agent (`PromptAgent`) with a small
built-in rules subset (D23): "hide (post) captions", "media at the (very) top" / "photos first" and "add a weather
widget", composable in one prompt; each success bumps the layout version and the panel shows
"Layout updated (vN, fake-rules)" ("No change needed (vN)" when the layout is already in that state); anything
else shows "I can't do that yet. Try: ...", and so does nothing while
offline (error "offline: ..."). **Live mode** runs the real Rust engine through JNA against
the server. By default it uses a fresh temp storage dir, so it neither reads nor writes `~/.canvas`; pass
`-Pstorage=<dir>` to keep a persistent one. If the workspace
does not load within `timeoutMs`, it logs a warning and still runs the script (shots show Loading/Offline).
`offline`/`online` are not supported in live mode; a live-mode script containing them is rejected before any
step runs. **Sign-in in live mode:** the engine gets an in-memory session store (every run starts signed out; the
Keychain is never touched) and a "browser" that GETs the authorize URL with `auto=1`; when the answer is a redirect to
the engine's loopback `redirect_uri` (the fake upstream does that, with or without `FAKE_PIXELFED_AUTO_APPROVE`), it
follows it, so `click Sign in with Pixelfed; wait-text 20000 @canvasdemo` signs in. A real Pixelfed answers with its
login page instead: the simulator prints the authorize URL and the status line shows "no browser available"
(opening the URL in a real browser and logging in still completes the sign-in while the script waits). For a real
instance pass `-PpixelfedLogin=<login>:<password>`: the "browser" then does what a person would (`PixelfedLoginBrowser`,
own cookie jar): `GET /login` for the CSRF `_token`, `POST /login` (Pixelfed logs in by e-mail; a login without `@` is
sent as `<login>@example.test`, the dev seed's domain), `GET` the authorize URL, `POST` the consent page's approve
form (`_token`, `state`, `client_id`, `auth_token`; the deny form is the one with `_method=DELETE`) and follows the
302 to the loopback listener. Failures (wrong password, 429 throttling) are logged with the URL. After the user
appears (`wait-text 30000 @canvasadmin`), wait for feed text too (e.g. `wait-text 20000 Alice Example boosted`): the
status line updates before the refetched feed arrives.

**Images** (Coil, DECISIONS D18) are deterministic. In fixture mode the network is never used: any image URL whose
path ends in `/storage/<file>` or `/v1/pixelfed/media/<file>` is served from `fixtures/pixelfed/media/<file>` (the
`pixelfed_home` fixture points at `http://127.0.0.1:8081/storage/...`, the fake upstream's URLs); every other URL
fails at once and the widget shows its error state (e.g. `pixelfed_demo`'s pixelfed.social URL). In live mode
images load over HTTP from wherever the URLs point (the pod's media proxy). Every `shot` first waits up to 5 s for
on-screen images to finish loading (and their crossfade to end); `wait-images` waits explicitly and fails on
timeout.

Script commands are separated by `;` or newlines. Each command is trimmed, blank entries are skipped, and
entries starting with `#` are comments. An argument cannot contain `;`, and since `;` separates commands, a
`#` comment ends at the next `;` (the rest of the line is parsed as commands).

| Command | Effect |
|---|---|
| `shot <name>` | Writes `<NN>-<name>.png`, where NN is the step number. |
| `click <target>` | Clicks the node `<target>` selects (below), at its center. A node whose center is off screen (or clipped by its parent) is first scrolled into view (`performScrollTo`, step message "scrolled into view"); fails if no node matches or it is still not on screen. |
| `wait <ms>` | Sleeps, advances the UI clock, then waits for idle. At most 600000 (10 minutes). |
| `idle` | Waits until the UI is idle. |
| `assert-text <text>` | Fails unless some node's text or content description contains `<text>`. Retries for up to 2 s. |
| `assert-no-text <text>` | The inverse of `assert-text`. |
| `wait-text <ms> <text>` | Like `assert-text` with a custom timeout (at most 600000), e.g. `wait-text 15000 Count: 3` while a live sync worker retries. |
| `wait-images [ms]` | Waits until no image is loading (default 10000 ms, at most 600000). Works in both modes. |
| `swipe-left <target>` / `swipe-right <target>` | Touch-swipes the selected node; `swipe-left` on a carousel shows the next page. |
| `scroll-to <target>` | Scrolls the feed until the selected node is visible (e.g. `scroll-to Kept my distance`). |
| `type <target> <text>` | Replaces the text of the selected text field with `<text>` (clear, then type), e.g. `type tag:agent-prompt Hide post captions`. The target is `tag:<testTag>`, a single word, or `"quoted text"` (may contain spaces); the text is the rest of the command. |
| `offline` / `online` | Fixture mode only: flips the engine's connectivity. |
| `screen <WxH or preset>` | Restarts the renderer at the new size. Engine state carries over; UI state such as scroll position does not. |

A `<target>` is either `tag:<testTag>` (an exact test tag, else the first tag containing it) or text: an exact text
match, then an exact content description (clickable nodes preferred for both), then the same as substrings; the
first match wins. Pixelfed widgets carry content descriptions `Like`/`Unlike`, `Bookmark`/`Remove bookmark`,
`Comment`, the image alt text, and `Page n of N` on the carousel dots (plus an `n/N` badge text), and test tags
`carousel:<post uri>`, `carousel-dots:<post uri>`, `like:<post uri>`, `bookmark:<post uri>`, `comment:<post uri>`,
e.g. `click tag:like:pixelfed://posts/781254316723458049` or `swipe-left tag:carousel:` (the first carousel).
The layout agent's controls (drawn over every workspace once it has loaded): the FAB `agent-fab` (content
description `Edit layout with AI`), and in its panel the field `agent-prompt`, the chips `Hide captions`,
`Media at the very top`, `Add a weather widget` (they fill the field), `agent-send` ("Send"), `agent-status`
("Applying...", "Layout updated (v3, rules)", "No change needed (v3)" or the error) and `agent-close` ("Close").
On Android, system Back closes the open panel (the typed text is kept) instead of leaving the app.

The output directory holds the PNGs, `steps.log` (human-readable) and `steps.json` (one entry per step:
`index`, `command`, `arg`, `status` (`ok`/`fail`), `message`, `file`, `elapsed_ms`). Its absolute path is
printed at the end. The run stops at the first failing step; the logs are still written, and the screen at that
point is saved (best effort) as `<NN>-FAILED.png`, recorded as that step's `file` and printed with the error.
The simulator's exit codes: 0 for success, 1 for a failed step, 2 for bad flags, a bad script or an unusable
`-Pout` directory. Unknown commands, bad arguments and `offline`/`online` in live mode are rejected before any
step runs. `./gradlew` itself exits 1 for any simulator failure; the simulator's own code appears in Gradle's
message `Process ... finished with non-zero exit value N`. An unrecognized `-P` property (e.g. a typo such as
`-Pscren`) is ignored, with a warning.

## Android

`:androidApp` runs the same shared `CanvasApp` on Android, with the Rust engine cross-compiled to
`libclient_ffi.so` (DECISIONS D9, D11).

Prerequisites (once): an Android SDK with an NDK (Android Studio SDK Manager; the SDK path comes from
`kmp-app/local.properties` `sdk.dir=...` or `ANDROID_HOME`), plus

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android x86_64-linux-android
```

Build (from `kmp-app`, `JAVA_HOME` set as above). `assembleDebug` runs `:androidApp:buildRustAndroid`, i.e.
`scripts/build_android.sh`, first; running the script by hand is optional:

```bash
../scripts/build_android.sh          # optional; writes androidApp/src/main/jniLibs/{arm64-v8a,x86_64}/libclient_ffi.so
./gradlew :androidApp:assembleDebug  # -> androidApp/build/outputs/apk/debug/androidApp-debug.apk
```

Gradle properties for `:androidApp`:

- `-PskipRustBuild`: don't run cargo; package the `.so` files already in `jniLibs`.
- `-PandroidAbis=arm64-v8a,x86_64` (default): ABIs to build and package (`CANVAS_ANDROID_ABIS` for the script,
  and the APK's `abiFilters`).
- `-PcanvasBaseUrl=https://the-construct-pod.fly.dev` (default, the staging pod) and `-PcanvasWorkspace=home`
  (default): the built-in defaults (`BuildConfig`). `-PcanvasBaseUrl=http://10.0.2.2:8080` bakes in the local
  server as seen from the emulator.

Run on an emulator. The app talks to the staging pod unless told otherwise. For a local server, start it first
(`cargo run -p server`, on the host's 127.0.0.1:8080, which the emulator reaches as `10.0.2.2:8080`) and pass
`--es baseUrl http://10.0.2.2:8080`, or pick it in the app:

```bash
export PATH="$HOME/Library/Android/sdk/platform-tools:$HOME/Library/Android/sdk/emulator:$PATH"
emulator -list-avds
emulator -avd Medium_Phone_API_36 -no-window -no-audio -no-boot-anim -no-snapshot-save &   # headless
adb wait-for-device
adb shell 'while [ "$(getprop sys.boot_completed)" != 1 ]; do sleep 1; done'

./gradlew :androidApp:installDebug
adb shell am start -n org.genui.canvas.android/.MainActivity                                   # staging pod
adb shell am start -S -n org.genui.canvas.android/.MainActivity --es workspace pixelfed_demo   # -S restarts it
adb shell am start -S -n org.genui.canvas.android/.MainActivity --es baseUrl http://10.0.2.2:8080   # local server
adb logcat -s CanvasEngine:V
adb exec-out screencap -p > screen.png
```

Intent extras `baseUrl` and `workspace` override the `BuildConfig` defaults (debug builds only; release builds
ignore them). Debug builds also show a small "server: ..." chip at the bottom left: it switches between Staging,
Local (emulator, `http://10.0.2.2:8080`), Local (USB, `http://127.0.0.1:8080`), a custom URL and the build default.
The choice is saved (SharedPreferences) and the activity restarts with a new engine; the `baseUrl` extra still
wins over it. A server other than the build default gets its own storage dir (`filesDir/servers/<url>`), so cached
entities and pending outbox rows never cross servers. `scripts/android_smoke.sh` passes `--es baseUrl http://10.0.2.2:8080` (its `--base-url`). They are read when the engine is
created, so use `am start -S` (force-stop first) to switch workspace on a running app. Rotation keeps the
same engine. Logcat tag `CanvasEngine` carries both the Kotlin log lines (`starting base_url=...`,
`dispatch: ...`) and the Rust engine's stdout/stderr. The stdout/stderr redirect is process-wide, so the tag also
shows other native output from the app process (e.g. GL/EGL driver lines). The SQLite cache (`cache.db`) is in the app's
`filesDir`.

On a physical phone, use `adb reverse tcp:8080 tcp:8080` and `--es baseUrl http://127.0.0.1:8080`.

Cleartext caveat: the debug `network_security_config.xml` (cleartext only to `10.0.2.2`, `localhost`,
`127.0.0.1`) governs Java/Kotlin networking only. All engine traffic goes through Rust (`reqwest`), which that
config does not cover, so the engine will talk plain HTTP to any `baseUrl` it is given, in debug and release.
The real controls are the base URL itself (`BuildConfig`, and intent extras that only debug builds honour).
Enforcing HTTPS in the engine for release builds is a follow-up.

## App icon

"The construct": a bright white space with a grey perspective grid floor and one cube standing on it. Every
asset comes from one geometry definition in `scripts/gen_icons.py`; regenerate with `scripts/gen_icons.sh`
(python3 with Pillow and numpy; `iconutil` on macOS for the `.icns`). Outputs: `assets/icon/` (SVG masters,
`png/construct-{16..1024}.png`, `construct.icns`, `construct.ico`), the Android adaptive icon
(`res/drawable/ic_launcher_{background,foreground,monochrome}.xml`, `mipmap-*`; the monochrome layer gives
Android 13+ themed icons) and the desktop window/dock icon (`desktopApp/src/main/resources/icon/`). Don't
edit the generated files by hand. Set `ICON_PREVIEW_DIR=<dir>` to also write previews of the adaptive layers.

## Tests

```bash
cargo test --workspace                # includes the e2e suite against the in-process fake (End-to-end tests)
scripts/e2e.sh --upstream real        # optional: the same suite against the local real Pixelfed
cd kmp-app && ./gradlew :shared:allTests :desktopApp:test
./gradlew :androidApp:assembleDebug   # Android build check only: it doesn't launch anything
```

`assembleDebug` and the JVM host tests can't catch Android-only runtime failures (for example, Android's ICU regex
engine rejects patterns that `java.util.regex` accepts). Two checks run on a booted emulator or device:

```bash
# Launch check: installs the debug app, launches it per workspace, then fails on a FATAL EXCEPTION in logcat,
# a dead process, or missing text in a `uiautomator dump` ("Count: <n>" and "Online" for home, "Sunrise over
# Boulder" for pixelfed_demo). Writes <ws>.png, <ws>.xml and <ws>.logcat.txt to --out
# (default kmp-app/androidApp/build/smoke).
../scripts/android_smoke.sh --pixelfed                    # device and server already running
../scripts/android_smoke.sh --boot --server --pixelfed    # boots Medium_Phone_API_36 headless and starts the
                                                          # server, then stops both at the end
../scripts/android_smoke.sh --help                        # --workspaces, --base-url, --skip-install, --skip-rust, --wait, --timeout

# Instrumented tests (androidApp/src/androidTest): the shared interpolation regexes on the device, and the
# AndroidKeyStore session store round trip (set/get/delete, survives a new instance, encrypted at rest).
# Note: AGP uninstalls the app afterwards, so don't follow this with android_smoke.sh --skip-install.
./gradlew :androidApp:connectedDebugAndroidTest
```
