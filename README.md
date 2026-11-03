# The Construct

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md) (D1–D52) and [docs/decisions/](docs/decisions/) (newer), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

> Renamed from Project Canvas (D31). There is no migration: the desktop cache moved from `~/.canvas` to
> `~/.construct`, the local server's data dir default from `~/.canvas-server` to `~/.construct-server`, env vars
> from `CANVAS_*` to `CONSTRUCT_*`, and the Android app is a new package (`org.theconstruct.android`; uninstall
> `org.genui.canvas.android`). Old sessions and caches are simply not read; sign in again.

Milestones 1-7 are implemented: the protocol crate, the Axum server, the macOS desktop app, the Android app
(see [Android](#android)), the SQLite cache + outbox (M5), the Pixelfed feed with OAuth2 PKCE sign-in (M6), and
versioned layouts with SSE push (M7). A fake Pixelfed is the primary end-to-end upstream, and a headless
simulation mode renders the shared UI to PNGs (see Simulation mode).

The pod serves **one app, Pixelfed** (workspace `pixelfed_home`), at its latest layout version. Layouts are code:
new versions are files in [`layouts/`](layouts/) that ship with a deploy (DECISIONS D32). Clients can't change
layouts. They show the layout plus a small hamburger menu with the app's version history, connection, account and
server (D33). The AI layout agent from M7 was removed (D37); it can come back from git history.

Quick links: [Server runtime configuration](#server-runtime-configuration),
[Adding a new layout version](#adding-a-new-layout-version), [The menu](#the-menu-all-surfaces),
[Try it on your phone with a real Pixelfed instance](#try-it-on-your-phone-with-a-real-pixelfed-instance),
[Milestone check recipes](#milestone-check-recipes).

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord; generic nodes
                     only (D37)
crates/pod-core      the generic pod (Axum + SQLite, D40): GET /v1/workspaces/:id, sessions and sign-ins, layouts
                     as code (one linear version history per app, checked by `validate`), read-only apps API, SSE
                     push; knows apps only through the `AppAdapter` trait (and `StaticAdapter` for layout-only apps)
crates/app-pixelfed  the Pixelfed adapter: pixelfed_home (auth, feed normalization, actions, comments, media proxy,
                     its tables) and the pixelfed_demo dev app
crates/app-recipes   the recipes adapter (D41): the family recipe collection in recipes_* tables, imported by
                     migration from data/import-0001.json (made by scripts/recipes_from_pdf.py)
crates/app-collections  Collections: data custom apps declare over MCP (collections_* tables)
crates/app-watcher   the watcher (issue #58): polls websites for events (ics, JSON-LD, LLM extractors), matches
                     rules, notifies; the watcher app
crates/server        "the pod" binary: pod-core serving the compile-time adapters (`server::adapters`)
crates/client-core   ConstructEngine: fetches the workspace, holds state, applies optimistic mutations (SQLite cache +
                     outbox sync worker), OAuth2 PKCE sign-in (pod session in a secure store), read-only apps
                     API, SSE layout subscriber
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
crates/fake-pixelfed fake Pixelfed (Mastodon API + OAuth PKCE, admin/fault controls), the primary e2e upstream;
                     single binary with embedded fixtures, Dockerfile for a sidecar image
crates/e2e           end-to-end suite: pod + engine in-process against the fake (default) or a real Pixelfed
layouts/             layouts as code: <app>/NNNN.json ({"note", "schema", "layout"}), embedded in the pod, append-only
fixtures/            pixelfed_home_bundle.json (DESIGN.md §2.2C; the pixelfed_demo app),
                     pixelfed_home_feed_bundle.json (simulator), counter_home_bundle.json (test-only: a minimal
                     stateful workspace for engine and UI tests; no pod serves it); pixelfed/ (Mastodon API JSON + media served by
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

Terminal 1: start the server on 127.0.0.1:8080 (set `CONSTRUCT_ADDR` to use another address).

```bash
cargo run -p server
curl -s http://127.0.0.1:8080/v1/workspaces/pixelfed_demo | jq .
```

Server details:

- One server hosts many pods, one per user (D46, see [Pods](#pods)). Each pod is a SQLite file,
  `$CONSTRUCT_DATA_DIR/pods/<pod id>/server.db`, listed in `$CONSTRUCT_DATA_DIR/host.db`; `CONSTRUCT_DATA_DIR`
  defaults to `~/.construct-server` (created if missing). Bound to loopback (the default), the server serves
  callers without a pod token from its `dev` pod, so the commands here work as before; `CONSTRUCT_OPEN_POD=off`
  turns that off.
- Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
  `X-Frame-Options: DENY` and `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` unless the route
  sets its own (uploads keep their `sandbox` CSP); behind Fly (`FLY_APP_NAME`) also `Strict-Transport-Security`.
- `GET /v1/workspaces/pixelfed_demo` returns the static one-post demo bundle; its like and bookmark actions post to
  `POST /v1/demo/ack`, a stateless 200.
- Test hook, only when started with `CONSTRUCT_TEST_HOOKS=1` (otherwise 404): `POST /v1/test/fault` with
  `{"status":503,"count":3}` makes the next 3 POSTs (other than `/v1/test/*`) return that status (400-599)
  without being applied; `"count":0` clears it. Each injected fault is logged.

Layouts, apps and the push channel (DECISIONS D32). Each app (= workspace) has one linear, append-only layout
history. Its versions come from the files `layouts/<app>/NNNN.json`, which are embedded in the binary and appended
at startup (see [Adding a new layout version](#adding-a-new-layout-version)). Entities are not versioned. The
signed-out `pixelfed_home` screen is not versioned. Apps: `pixelfed_home` "Pixelfed", plus the dev app
`pixelfed_demo` "Pixelfed demo". `GET /v1/apps` lists the dev app only with `CONSTRUCT_SHOW_DEV_APPS=1`; its other
endpoints always work. (The `home` "Counter" app was removed, D39.)

- `GET /v1/workspaces/{id}` serves the app's latest layout with `X-Construct-Layout-Version: <n>`. The header is
  absent on the signed-out `pixelfed_home` screen and on its 401.
- `GET /v1/apps` -> `{"apps":[{id, title, icon, latest_version, latest_compatible_version, updated_at}]}`.
  `latest_compatible_version` is the newest version the asking client (`X-Construct-Schema`) can draw, i.e. what
  opening the app serves it (null if none fits); see "Schema versions" below.
- `GET /v1/apps/{id}/versions[?limit=1..100 (default 20)&before=N]` -> `{app_id, latest_version,
  latest_compatible_version, versions:[{version, created_at, source, note?}], next_before?}`, newest first. `next_before` is present iff older versions exist;
  pass it as `before`. `source` is `file` for layout files. Versions migrated from the old catalog keep `seed`,
  `agent`, `rollback`, `restore`, `manual` or `switch`, with a note.
- `GET /v1/apps/{id}/versions/{n}` -> `{app_id, version, created_at, source, note?, layout}`.
- All public (no session needed). Errors: 400 `invalid_workspace_id` (ids are 1..=64 characters of
  `[A-Za-z0-9_-]`), 404 `app_not_found` / `version_not_found`, 400 `invalid_request` (bad `limit` / `before` /
  version).
- `GET /v1/workspaces/{id}/events` (SSE): the first message is the current version, then one each time a new
  version of that app changes it (`event: layout` / `data: {"version":N,"workspace_id":"pixelfed_demo"}`), plus a
  `: keepalive` comment every 15 s. With `X-Construct-Schema` the version is the newest one that client can draw
  (what a workspace GET serves it), so a version it can't draw sends nothing; without the header it is the app's
  latest (D38). New versions normally arrive with a deploy (the restart ends the stream, and the reconnect's first message
  carries the new number). No session is needed; for `pixelfed_home`, a bearer that is not a valid session gets 401
  like the workspace GET. A stream opened with a session ends once the session is revoked (logout, `server pods
  rotate`, expiry): it is re-checked before each private event and every 15 s. One session token may hold 8
  streams open; another gets 429 `{"error": "too_many_streams", "limit": 8}` (decision
  [`sse-session-revocation`](docs/decisions/sse-session-revocation.md)).
- Nothing changes a layout over HTTP. The agent, rollback, workspace versions and `/v1/layouts` routes are gone
  (404).

```bash
CONSTRUCT_SHOW_DEV_APPS=1 cargo run -p server
curl -s http://127.0.0.1:8080/v1/apps | jq '.apps[] | {id, latest_version}'
curl -s http://127.0.0.1:8080/v1/apps/pixelfed_home/versions | jq '.versions[] | {version, note, schema, compatible}'
# -> {"version":1,"note":"Classic feed","schema":1,"compatible":true} on a fresh database (no header = schema 1,
#    so version 2, written for schema 2, says "compatible": false, "incompatible": "update_required")
curl -s http://127.0.0.1:8080/v1/apps/pixelfed_home/versions/1 | jq .layout.id     # "pixelfed_home"
curl -sN http://127.0.0.1:8080/v1/workspaces/pixelfed_demo/events   # event: layout, data: {"version":2,"workspace_id":"pixelfed_demo"}
```

```bash
CONSTRUCT_TEST_HOOKS=1 cargo run -p server
curl -s -X POST http://127.0.0.1:8080/v1/demo/ack                         # {"ok":true}
curl -s -X POST -H 'content-type: application/json' \
  -d '{"status":503,"count":2}' http://127.0.0.1:8080/v1/test/fault       # next 2 POSTs -> 503
```

Terminal 2: build the FFI library and bindings, then run the desktop app.

```bash
scripts/build_ffi.sh            # optional: :shared:buildRustFfi runs it before the desktop compile
                                # (dylib built to target/aarch64-apple-darwin/release, copied to target/release)
cd kmp-app
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
./gradlew :desktopApp:run                                              # the staging pod (default)
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080       # or: CONSTRUCT_BASE_URL=... ./gradlew ...
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080 -Dconstruct.workspace=pixelfed_demo   # the demo
```

All clients default to the deployed staging pod, `https://the-construct-pod.fly.dev`, and to the Pixelfed app
(`pixelfed_home`; signed out, it shows "Sign in with Pixelfed"). For the local server started above, pass its URL
explicitly (`CONSTRUCT_BASE_URL=http://127.0.0.1:8080`) or pick **Local** in Settings' server picker. With
`-Dconstruct.workspace=pixelfed_demo`, a "The Construct" window opens with the one-post demo served by the server
(no sign-in). Clicking the heart prints the intent to stdout and the post's `is_liked` and `like_count` update:

```
[desktop] dispatch: {"intent":"action.mutation.optimistic",...}
[ConstructEngine] dispatch: {...}
```

Options for the desktop app (set as `-D` JVM properties or environment variables):

- `construct.baseUrl` / `CONSTRUCT_BASE_URL`: the server at launch. Precedence: `-Dconstruct.baseUrl`, then
  `CONSTRUCT_BASE_URL`, then the menu's saved choice (`java.util.prefs`, node `org/theconstruct/desktop`, key
  `baseUrl`), then the staging pod `https://the-construct-pod.fly.dev`. Local server: `http://127.0.0.1:8080`. An
  override only decides the launch server; picking a server in the menu still switches the running app (and saves
  the choice for the next launch without an override). Any server but staging gets its own storage,
  `~/.construct/servers/<url>`; `~/.construct` itself now belongs to staging. If you ran older builds against a
  local pod, its cache and pending outbox there would be shown on, and flushed to, staging: delete
  `~/.construct/cache.db` once.
- `construct.workspace` / `CONSTRUCT_WORKSPACE`: default `pixelfed_home`. Use `pixelfed_demo` for the one-post
  demo. Other apps can also be opened from the menu.

Separately, `-PskipRustBuild` is a Gradle property (`./gradlew -PskipRustBuild :desktopApp:run`) that skips
the cargo build step.

The engine keeps a SQLite cache at `~/.construct/cache.db` (tables `entities`, `active_workspace`,
`action_outbox`; see docs/DECISIONS.md D13). Every fetched workspace (layout + entities) is written there and
loaded on startup, so the last workspace shows when the server is unreachable; the fetch is retried every
2 seconds until the server responds. Each click updates the entity and queues a PENDING `action_outbox` row in
one transaction (`sqlite3 ~/.construct/cache.db "SELECT * FROM action_outbox;"`). The old
`workspace_<id>.json` cache files are ignored.

Queued rows are sent by a background sync worker (docs/DECISIONS.md D14), oldest first and one at a time,
as `POST <base_url><endpoint>`. Clicks made while offline (or with the app closed before they were sent) stay
in the queue and the menu shows "N pending" (and a badge on the menu button); they are flushed as soon as the server is reachable again,
including on the next launch. Each accepted row is deleted. A row the server rejects with a 4xx (e.g. a like
on a deleted post) is deleted and its field rolled back, with later queued clicks re-applied on top. A 5xx,
408, 429 or network error keeps the row and retries it with exponential backoff (2 s, 4 s, ... capped at 60 s). A
re-fetched workspace has the still-pending clicks re-applied, so they never visibly disappear. The engine logs
each attempt, e.g. `[ConstructEngine] outbox #3 POST http://127.0.0.1:8080/v1/demo/ack -> 200,
deleted (0 pending)`.

## Server runtime configuration

Everything the server (`cargo run -p server`) reads from the environment. Empty values count as unset. The chosen
Pixelfed instance and the host's settings are logged at startup; each pod logs its layout sync when it is first
opened. `GET /healthz` answers 200 `ok` (no auth; the engine's 2 s reconnect probe uses it).

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_ADDR` | `127.0.0.1:8080` | Listen address. Use `0.0.0.0:8080` to accept LAN connections. |
| `CONSTRUCT_DATA_DIR` | `~/.construct-server` | `host.db` (pods, token hashes) and `pods/<id>/server.db` (each pod's apps, accounts, sessions, layout versions); created if missing. |
| `CONSTRUCT_OPEN_POD` | `dev` on a loopback address, else unset | Callers without a pod token use this pod (created if missing). Development only; `off` disables it on loopback too. The server refuses to start with it on a non-loopback address (unless `CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK=1`) and always behind Fly (`FLY_APP_NAME`). Only requests whose `Host` is `localhost`, `*.localhost` or an IP address use it, so a DNS-rebinding web page can't (403 `host_not_allowed`); pod tokens work with any `Host`. |
| `CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK` | unset | `1`: allow `CONSTRUCT_OPEN_POD` on a non-loopback address. Everyone who can reach the server gets the whole pod and its sign-ins; only on a network you trust, never on a public server. Ignored behind Fly. |
| `CONSTRUCT_TEMP_PODS` | unset | `1`: anyone may create a temporary pod (`POST /v1/pods/temporary`, the "Try a temporary pod" button). Staging sets it. |
| `CONSTRUCT_TEMP_POD_HOURS` | `24` | Temporary pod lifetime (1..=720); the sweeper deletes it and its files afterwards. |
| `CONSTRUCT_TEMP_POD_MAX` | `50` | Live temporary pods at most (then 503 `temporary_pods_full`). |
| `CONSTRUCT_TEST_HOOKS` | unset | `1` enables `POST /v1/test/fault` (fault injection; otherwise 404). |
| `CONSTRUCT_SHOW_DEV_APPS` | unset | `1`: `GET /v1/apps` also lists the dev app `pixelfed_demo` (it works either way; this only makes it show up in the clients' menu). |
| `CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_MODEL` | unset | Both set: Collections answers `model`-computed fields (decision `collection-computed-fields`) by asking this model over the Messages API. Unset: those fields keep their default (`expr` fields need nothing). |
| `CONSTRUCT_INFER_URL` | `https://api.anthropic.com/v1/messages` | The Messages API endpoint for `model`-computed fields. |
| `PIXELFED_INSTANCE_URL` | `http://127.0.0.1:8081` (the fake) | The default Pixelfed instance: the "Sign in with Pixelfed" button uses it. `http://localhost:8090` for the local real Pixelfed, `https://<instance>` for a public one. |
| `PIXELFED_PUBLIC_URL` | = instance URL | Base for the default instance's browser-facing links (the authorize page), e.g. `http://10.0.2.2:8081` for the emulator while the pod calls `127.0.0.1:8081`. Also accepted in auth calls. |
| `PIXELFED_ALLOWED_INSTANCES` | unset | Other instances users may sign in to (D45): comma-separated base URLs (trusted like the default, may be private), and/or `*` for any public `https://` instance typed on the sign-in screen (only public IP addresses are contacted for those, and their media may come from any public HTTPS host). Set: the signed-out screen adds a server box. Unset: only the default instance. |
| `PIXELFED_MEDIA_HOSTS` | unset | Comma-separated `host`, `host:port` or `scheme://host[:port]`: extra origins the media proxy may fetch (CDN/S3) besides the configured instances. Media elsewhere become placeholders (except public HTTPS media under `*`). |
| `PIXELFED_EXTRA_REDIRECTS` | unset | Comma-separated exact redirect URIs accepted by `/v1/auth/pixelfed/app`, besides `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback` and `construct://oauth`. A pod that serves the Android App Link lists `https://<its host>/oauth/android` here (staging does). |
| `CONSTRUCT_ANDROID_CERT_SHA256` | unset | Comma-separated SHA-256 fingerprints (`AA:BB:…` or 64 hex digits) of the Android signing certificates whose builds may open this server's `https://<host>/oauth/android` sign-in redirect (App Links; decision [`android-app-link-sign-in-redirect`](docs/decisions/android-app-link-sign-in-redirect.md)). Served at `GET /.well-known/assetlinks.json` (no auth); unset: 404 there, and Android 12+ builds keep `construct://oauth`. List the release key and any debug keys that should verify. Not secret (every APK carries its certificate). |
| `CONSTRUCT_ANDROID_PACKAGE` | `org.theconstruct.android` | The Android package `assetlinks.json` names, and the one the `/oauth/android` fallback page opens. |
| `PIXELFED_OWNER_ACCOUNT_ID` | unset | The only upstream account id allowed to sign in to the default instance (replaces a pinned owner). Unset, and on every other instance: the first account to sign in is pinned; others get 403 `account_not_owner`. |
| `PIXELFED_ALLOW_ACCOUNT_CHANGE` | unset | `1`/`true`: on the default and listed instances any account may sign in and replace the stored one (no pinning). Instances typed under `*` always keep their first account. |
| `CONSTRUCT_SESSION_TTL_DAYS` | `90` | Lifetime of sessions other than pod tokens, in days (1..=36500; other values warn and use 90). Pod tokens don't expire. |
| `CONSTRUCT_VAPID_PRIVATE_KEY` | generated | Web Push signing key for notifications (D52): base64url of a 32-byte P-256 private key. Unset: the server generates one on first start and keeps it in `host.db` (devices subscribed with its public key keep working across restarts). Staging/production should set it as a secret so a lost volume doesn't orphan subscriptions. |
| `CONSTRUCT_SECRETS_KEY` | unset | Seals upstream credentials (Pixelfed access/refresh tokens, OAuth client secrets) in the pod databases with AES-256-GCM: base64 of 32 random bytes (`openssl rand -base64 32`). Comma-separated: key rotation (the first seals, all open; values are re-sealed under the first when a pod opens). Unset: stored in plaintext, with a startup warning (an error on Fly). Setting it later seals existing rows when each pod opens. A sealed row whose key is missing or wrong reads as signed out (logged); the right key restores it. Malformed: the server refuses to start. Staging/production must set it as a secret (decision [`upstream-credentials-sealed-at-rest`](docs/decisions/upstream-credentials-sealed-at-rest.md)). The data dir and pod dirs are 0700 and the databases 0600 either way. |
| `CONSTRUCT_VAPID_SUBJECT` | the repo URL | Contact (`mailto:` or `https:`) that push services may use to reach the operator. |
| `CONSTRUCT_PUSH` | on | `off`: no Web Push (notifications still reach the inbox and open apps). |
| `CONSTRUCT_UPLOAD_MAX_BYTES` | `20971520` (20 MB) | Largest file a client may upload to `POST /v1/uploads` (photos for a post, D49); at least 1024. |
| `CONSTRUCT_POD_UPLOAD_QUOTA_BYTES` | `524288000` (500 MiB) | Most bytes all of an admin pod's uploads may hold together (then 507 `upload_quota_exceeded`, retried by clients). |
| `CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES` | `26214400` (25 MiB) | The same for a temporary pod. |
| `CONSTRUCT_MIN_FREE_DISK_BYTES` | `104857600` (100 MiB) | Free space kept on the data volume: below it uploads get 507 `low_disk_space` and new temporary pods 503 `low_disk_space`, so the volume never fills and every pod's other writes keep working. |
| `CONSTRUCT_PUBLIC_URL` | from each request's `Host` | The base URL clients reach the server at, for the OAuth metadata Claude reads (decision `mcp-connector`), e.g. `https://the-construct-pod.fly.dev`. Unset: the request's `Host`, `https` behind Fly or with `X-Forwarded-Proto: https`. |
| `CONSTRUCT_WATCHER_ANTHROPIC_API_KEY` | unset | Claude API key for the watcher's `llm` extractor (decision [`watcher`](docs/decisions/watcher.md)). Unset: `llm` is off (`auto` stops at `ics`/`jsonld`, and an `llm` target reports it in its health). Shared by every pod on the server; the model runs only when a watched page changed. |
| `CONSTRUCT_WATCHER_LLM_MODEL` | `claude-opus-5-5` | The model of the `llm` extractor. |
| `CONSTRUCT_WATCHER_LLM_URL` | `https://api.anthropic.com` | Base URL of the Messages API (tests point it at a fake). |
| `CONSTRUCT_WATCHER_PRIVATE_HOSTS` | unset | `1`: watched websites may be on private addresses (local development). Otherwise only public addresses are fetched. |
| `RUST_LOG` | `info` | `tracing` filter, e.g. `RUST_LOG=server=debug,info`. |

The layout agent was removed (D37), so `CONSTRUCT_AGENT_OPEN`, `CONSTRUCT_LLM_PROVIDER`, `CONSTRUCT_LLM_MODEL`,
`CONSTRUCT_LLM_FALLBACKS`, `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` have no effect on the pod.
`deploy/fly/pod.toml` still sets `CONSTRUCT_LLM_PROVIDER`; that is harmless.

<a id="fake-pixelfed-environment"></a>`cargo run -p fake-pixelfed`:

| Variable | Default | Meaning |
|---|---|---|
| `FAKE_PIXELFED_ADDR` | `127.0.0.1:8081` | Listen address. If unset and `PORT` is set (most PaaS), `0.0.0.0:$PORT`. |
| `FAKE_PIXELFED_PUBLIC_URL` | `http://<bound addr>` (`0.0.0.0` shown as `127.0.0.1`) | Replaces every `{{BASE}}` in the fixture JSON, so emitted media/profile/status URLs point at it. `http://10.0.2.2:8081` when the emulator must load them directly. |
| `FAKE_PIXELFED_AUTO_APPROVE` | unset | `1`: `GET /oauth/authorize` redirects straight back with a code (same as `auto=1` in the query); otherwise an HTML page with an **Authorize** button. |
| `FAKE_PIXELFED_TOKEN` | unset | A pre-seeded valid access token, to skip OAuth in QA (survives resets). |
| `FAKE_PIXELFED_FIXTURES` | embedded | Fixture directory to load instead of the embedded copies (e.g. `fixtures/pixelfed` while editing). |
| `FAKE_PIXELFED_THREADS` | `pixelfed` | `mastodon`: `GET /api/v1/statuses/:id/context` returns the whole thread (Mastodon) instead of one level each way (Pixelfed). |
| `FAKE_PIXELFED_ADMIN` | loopback only | `1` serves `/_admin` on any address, `0` never; unset, only when bound to loopback. |
| `RUST_LOG` | `info` | `tracing` filter. |

## Pods

One server hosts many pods, one per user (D46). Every pod has all the apps; each pod's data is its own SQLite
file. A request names its pod by its bearer: a **pod token** (`cpod_…`, 256 random bits). The server stores only its
SHA-256; the code is shown once when it is made. Without a valid token a caller gets only `/healthz`, the "Open your
pod" screen and the two calls below; images load without a bearer from `/v1/p/<pod id>/…` (only adapters' public
paths, e.g. the Pixelfed media proxy).

- **In the app:** the "Open your pod" screen takes the pod code (or, where enabled, **Try a temporary pod**). Any
  number of devices can use the same code. Signing in to an app (Pixelfed) happens inside the pod, so every device
  of the pod sees it; **Sign out** signs the pod out of the app and keeps the pod open; Settings' **Leave pod**
  forgets the code on this device.
- **API:** `POST /v1/pods/open {"token": "cpod_…"}` -> `{"session_token"}` (401 `pod_not_found`; 10 tries a minute
  per client address; behind Fly the address is `Fly-Client-IP`, elsewhere the connection's peer). `POST /v1/pods/temporary` -> `{"session_token", "expires_at", "pod"}` (404 when disabled, 429, 503
  `temporary_pods_full`). Then send the token as `Authorization: Bearer` on every call.
- **Admin** (on the server; on Fly `fly ssh console -a the-construct-pod -C "/server pods …"`):

  ```bash
  server pods create <name> [--import-recipes]   # prints the pod code once; --import-recipes: the family collection
  server pods adopt <name> <path/to/server.db>   # a pre-D46 single-pod database becomes a pod (copied, checked first)
  server pods remove-adopted [dir]               # delete the renamed originals adopt left behind (default: data dir)
  server pods rotate <name>                      # new code; the old one stops working, app sign-ins end
  server pods delete <name>                      # stops now; files deleted within 5 minutes
  server pods notify-test <name>                 # a test notification: inbox + Web Push to the pod's devices
  server pods mcp <name> [label]                 # a credential for Claude Code (reaches only /v1/mcp); printed once
  server pods revoke-mcp <name>                  # disconnect Claude (every MCP credential of the pod)
  server pods list
  ```

  Pod codes never go into git or config files; keep yours in a password manager.
- **Temporary pods** (`CONSTRUCT_TEMP_PODS=1`, staging): every app, but Pixelfed only on the default instance (the
  fake on staging; no typed-in servers, so a visitor can't make the server contact hosts of their choosing), no family
  recipes, deleted with everything in them after `CONSTRUCT_TEMP_POD_HOURS`. At most 5 per client address an hour
  (IPv6 per /64; 10 per /48) and `CONSTRUCT_TEMP_POD_MAX` live, with a smaller upload quota
  (`CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES`). Use an admin pod to try a real Pixelfed server. Testing staging
  from a script:

  ```bash
  pod=https://the-construct-pod.fly.dev
  token=$(curl -fsS -X POST $pod/v1/pods/temporary | jq -r .session_token)
  curl -fsS -H "Authorization: Bearer $token" $pod/v1/workspaces/recipes | jq '.queries'
  ```

  In the simulator: `-Pscript="click Try a temporary pod; wait-text 20000 Sign in with Pixelfed; …"`.
- **Moving the pre-D46 staging data into a pod** (once, after the D46 deploy): take a volume snapshot
  (`fly volumes snapshots create <volume id>`), then
  `fly ssh console -a the-construct-pod -C "/server pods adopt <name> /data/server.db"` and keep the printed code.
  Install the app build with the "Open your pod" screen and enter the code: the recipes and everything else are there.
  Then delete the renamed original (it still holds the old sessions and app tokens):
  `fly ssh console -a the-construct-pod -C "/server pods remove-adopted"`. The image has no shell, so `rm` doesn't
  exist there (not in `fly ssh sftp shell` either).

## Custom apps

A pod's user can have apps of their own next to the built-in ones (decision `custom-apps-and-data-sources`, issue
#45): any layout the node vocabulary can draw, over the feeds, screen data and actions the pod's data sources
declare (each adapter's `SourceManifest`). They are served like built-ins (workspace, SSE, versions, schema
negotiation) and marked `"kind": "custom"` in `GET /v1/apps`. Every write is validated against the app's sources
(errors name a JSON pointer and what is allowed) and appended to the app's history.

```bash
pod=http://127.0.0.1:8080; token=cpod_…   # the pod token
curl -fsS -H "Authorization: Bearer $token" -H 'content-type: application/json' $pod/v1/custom-apps -d '{
  "title": "Kitchen", "icon": "recipe", "feeds": [["recipes", "all"], ["pixelfed", "home"]],
  "note": "Recipes next to my feed", "layout": {"type": "core://Workspace", "title": "Kitchen", "slots": […]}}'
```

- `GET /v1/custom-apps/{id}` (feeds, latest layout), `PUT …/{id}/layout {layout, note?, base_version?}` (409
  `stale_base`), `PATCH …/{id} {title?, icon?, feeds?}`, `POST …/{id}/restore {version}`, `DELETE …/{id}` (soft;
  purged after 30 days; open streams get `event: deleted`), `POST /v1/custom-apps/validate {feeds, layout}` (dry run),
  `PUT /v1/apps/{id}/hidden {hidden}` (any app; hidden apps are listed only with `GET /v1/apps?all=1`).
- Writes need the pod token's session; any valid session may delete and hide. The pod sets the layout's root `id`.
- A custom app's root also gets `core://sources/<source>` entities `{title, signed_in, error?}`: a signed-out or
  failing source shows no data, and the layout can say so (`${core://sources/pixelfed#signed_in}`). Sign-ins start
  from Settings; custom layouts can't hold sign-in buttons.
- Caps per pod: 50 apps, 500 versions per app (the oldest are pruned), 256 KiB per layout, 60 writes a minute.
- **Built-in parts** (decision `layout-parts`): `{"type": "core://Ref", "app": "pixelfed_home", "part":
  "screen:post"}` reuses a built-in's screen (`screen:<id>`), slot content (`slot:<id>`) or declared part (a layout
  file's `"parts"`); the pod expands it when serving and keeps it current (a deploy appends a `ref_update` version).
  A copy says `"copied_from": {"app", "version"}`; `GET /v1/apps` then reports `updates_from` when that built-in
  moves on, until `POST /v1/custom-apps/{id}/dismiss-update {version}`.

### Claude (MCP)

Each pod has an MCP endpoint, `POST /v1/mcp` (decision `mcp-connector`), so Claude can build and edit your custom
apps. Its tools: `list_apps`, `list_data_sources`, `describe_vocabulary`, `get_layout`, `sample_data`,
`validate_layout`, `create_app`, `update_layout` (a layout or a JSON Patch, with `base_version`), `restore_version`,
`rename_app`, `delete_app`, `set_hidden`, `list_parts`, `rebase_context`, `dismiss_update`. Writes go through the same validation as the API above and show in the
version history as written by Claude.

- **Claude (web, desktop, mobile):** add a custom connector with the URL `https://<server>/v1/mcp`. Claude finds the
  server's OAuth endpoints, registers itself and opens a page where you type your pod code. It then holds a
  credential that reaches only this pod's `/v1/mcp`: no app routes, no sign-ins, no pod settings.
- **Claude Code:** `server pods mcp <name>` prints a credential, then
  `claude mcp add --transport http construct https://<server>/v1/mcp --header "Authorization: Bearer cmcp_…"`
  (OAuth works too).
- **Disconnect:** `server pods revoke-mcp <name>`. Rotating the pod code (`server pods rotate`) keeps Claude
  connected.

### Collections (data a custom app keeps)

A custom app that needs data no source holds, such as a todo list, keeps it in **Collections** (decision
`collections`, issue #54, `crates/app-collections`): one built-in source that stores any collection Claude declares.
Claude never writes a data source; it declares typed fields (`text`, `number`, `bool`, `date`, `choice`; optional, with
a default) and the pod stores, validates and serves the rows. There is no logic: no computed fields or triggers.

| A layout uses (for collection `<c>`) | |
|---|---|
| feed `["collections", "<c>"]` | rows `collections://<c>/<id>` under `collection:<c>`, with every field by name plus `id`, `deleted` (`false`), `created_at`, `updated_at` |
| `GET /v1/collections/<c>/rows` | the same, as a screen's data path |
| `POST /v1/collections/<c>/create {<field>: …, client_id?}` | `action.mutation.create` (a repeated `client_id` answers the same row) |
| `POST /v1/collections/<c>/update {id, <field>: …}` | `action.mutation.optimistic` (an `id` of `pending-<nonce>` finds a row whose create was just sent) |
| `POST /v1/collections/<c>/delete {id}` | a mutation of `deleted` to `true`; hide it at once with the List's `where: {field: "deleted", equals: "false"}` |
| `GET /v1/collections/<c>/export` | everything, deleted rows and kept-aside values included |

- **MCP tools:** `list_collections`, `define_collection`, `describe_collection` (with layout snippets),
  `apply_change`, `restore_collection_version`, `edit_rows`, `undo_edit`, `export_collection`.
- **Change sets:** `apply_change` sends the collection change and the app layouts that go with it. Every custom app
  using Collections is checked against the new definition; if one would break, nothing applies and the error names
  the app and the binding. Fields have permanent ids: a rename keeps the old name as an alias, a removal is soft
  (hidden, data kept 30 days, `restore_field`), a type change converts row by row and keeps what doesn't convert
  aside (a later change back puts it back).
- **Lenient writes:** a write made under an older definition (an offline todo) still lands: old names resolve through
  aliases, removed fields are ignored, only a wrong type is refused.
- **Row edits by Claude** (imports, backfills; Collections only): `edit_rows` first answers a preview (count and
  sample) and a confirm token, which Claude shows the user before applying; `undo_edit` reverts it as one step.
- **Caps per pod** (temporary pods too): 20 collections, 40 fields each, 5,000 rows and 4 MiB of values in all, 500
  rows per edit, 4,000 characters per text value.

## Watcher (events from websites)

The **Watcher** app (decision `watcher`, issue #58, `crates/app-watcher`) checks websites for events and sends a
notification when a new one matches a rule ("Improv Boulder lists a show with Budget Cuts"). A website (*target*) and
a rule are rows of config, added in the app or by Claude over MCP; no deploy per site.

- **Extractors:** `ics` (calendar feeds), `jsonld` (schema.org `Event` markup, which most event sites embed), `llm`
  (Claude reads the page text; needs `CONSTRUCT_WATCHER_ANTHROPIC_API_KEY`), `auto` (the default: the first of those
  that applies). `details: true` fetches each new event's own page once for its full description (lineups).
- **Checks:** conditional `GET` (`ETag`, `Last-Modified`) and a body hash: an unchanged page costs no extraction. Back
  off on errors, honour `Retry-After`. Each target shows its health (`4 upcoming events · checked 5 min ago`,
  failing, no events found). Checks run from the notification poller (while a device is registered) and when the app
  opens with a target overdue. Temporary pods don't fetch.
- **Rules:** any of / all of / none of phrases, venue, days, a window in days, which sites. Each new match is one
  notification (`watcher.matches`), opening the event in the app.

| A layout uses | |
|---|---|
| feed `["watcher", "matches"]` | upcoming matched events `watcher://events/<id>` under `watcher:matches` |
| feed `["watcher", "targets"]` | sites `watcher://targets/<id>` (with `health`, `health_text`, `failing`) under `watcher:targets`; rules `watcher://rules/<id>` under `watcher:rules` |
| `GET /v1/watcher/screens/event?id=…` / `…/screens/target?id=…` | an event; a site and its events (`watcher:target:<id>:events`) |
| `POST /v1/watcher/targets/create {url, title?, extractor?, interval_minutes?, hint?, details?, utc_offset?, client_id?}` | add a site |
| `POST /v1/watcher/targets/update {id, …, archived?}` · `POST /v1/watcher/targets/refresh {id}` | change or archive a site; check it now |
| `POST /v1/watcher/rules/create {title?, keywords, exclude?, venue?, days?, within_days?, targets?, client_id?}` · `…/rules/update {id, …}` | add, change or archive a rule |
| `POST /v1/watcher/events/seen {id, seen}` | mark an event's matches seen |

- **MCP tools:** `watcher_status`, `watcher_preview` (fetch and extract now, nothing saved), `watcher_set_target`,
  `watcher_set_rule`, `watcher_refresh`.
- **Caps per pod:** 50 sites, 100 rules; events are kept 30 days after they are over.
- **Try it locally:** `CONSTRUCT_WATCHER_PRIVATE_HOSTS=1 cargo run -p server`, serve a page with an `.ics` or JSON-LD
  events, add it under "Sites & rules", and add a rule.

## Notifications

Apps send notifications through the pod (D52, design in `docs/NOTIFICATIONS.md`). Only an app's adapter knows where
they come from; the pod, the engine and the clients see an app id, a channel the adapter declares ("Likes"), text the
adapter wrote, and a navigation target in that app's own layout routes.

- **Sources:** an adapter polls its upstream (`AppAdapter::poll_notifications`, every `poll_interval`) or reports
  from its own routes (`AppState::notify`). The host's scheduler checks every 30 s and opens only pods that are due
  (`host.db` `pods.next_wake_at`). A pod polls only while it has a registered device (one seen in the last 30 days),
  so idle and temporary pods cost no upstream calls. Failed polls back off (1 min doubling to 1 h); a revoked
  sign-in stops polls until someone signs in again. Pixelfed polls `GET /api/v1/notifications` every 2 minutes; the
  first poll stores the recent backlog as already read.
- **Delivery:** each notification is stored once (`UNIQUE(app_id, dedupe_key)`, the newest 500 for 30 days), sent as
  an `event: notification` on every open events stream of the pod (callers with a session only), and sent by Web Push
  (RFC 8291 encryption, VAPID) to registered devices that want its channel and have no stream open.
- **API** (pod session required; `X-Construct-Device: <client-made id>` on calls and the events stream):

  | Call | Answer |
  |---|---|
  | `GET /v1/notifications?limit=0..100&before=<id>&app=<id>` | `{notifications, unread_total, unread_by_app, next_before?}`, newest first |
  | `POST /v1/notifications/read {"ids": [...]}` or `{"all": true, "app"?: id}` | `{unread_total, unread_by_app}`; also an `event: notifications_read` |
  | `GET /v1/notifications/channels` | `{"channels": [{app_id, id, title, description, default_enabled, importance, enabled}]}` (`enabled` for the calling device) |
  | `PUT /v1/devices/{id} {"platform", "name"?, "push"?: {endpoint, p256dh, auth} \| null}` | registers the device under the caller's session; `push` sets or (null) removes its Web Push subscription |
  | `PUT /v1/devices/{id}/channels {"<app>.<channel>": bool}` | the device's channel choices |
  | `DELETE /v1/devices/{id}` | forget the device (the engine calls it on sign-out) |
  | `GET /v1/push/key` | `{"vapid_public_key"}` (404 `push_unavailable` with `CONSTRUCT_PUSH=off`) |

  Push endpoints must be `https://` on the default port (443) of a public host name (any local URL with
  `CONSTRUCT_TEST_HOOKS=1`, for test receivers); deliveries resolve only public addresses and follow no redirects. A
  port rule rather than a domain allowlist keeps self-hosted UnifiedPush distributors working. Endpoints stored
  before this rule are dropped on their next delivery. 404/410 from the push service drops the subscription, as do 5
  failures in a row. At most 20 devices and 60 deliveries a minute per pod.
- **Try it:** `server pods notify-test <name>` (or, against the fake upstream, inject one with its admin API; see
  "Fake Pixelfed upstream").

## Try it on your phone with a real Pixelfed instance

Nothing has to be registered by hand: the pod registers its own OAuth app on the instance (`POST /api/v1/apps`,
redirect `construct://oauth`, or on staging the App Link `https://the-construct-pod.fly.dev/oauth/android`) on the
first sign-in. The pod keeps the upstream token; the phone only gets a pod
session (DECISIONS D17, D19).

**On staging:** open your pod (enter your pod code, or tap **Try a temporary pod**; see [Pods](#pods)). Staging
runs with `PIXELFED_ALLOWED_INSTANCES=*` (D45): in the Pixelfed app type your server (e.g. `pixelfed.social`) in the
box under "Or sign in to another Pixelfed server", tap **Sign in to this server**, log in and approve in the
browser. "Sign in with Pixelfed" still goes to the fake. Your upstream token lives only in your pod's database.

**On your own server:**

1. Start the pod for the instance (any account on it; the first one to sign in becomes the pod's owner):

   ```bash
   PIXELFED_INSTANCE_URL=https://<instance> CONSTRUCT_DATA_DIR=~/.construct-pixelfed cargo run -p server
   # add PIXELFED_MEDIA_HOSTS=<cdn host> if the instance serves media from a CDN/S3 host (else placeholders)
   ```

2. Make the pod reachable from the phone. Simplest is USB: `adb reverse tcp:8080 tcp:8080` and base URL
   `http://localhost:8080`. A LAN address (`CONSTRUCT_ADDR=0.0.0.0:8080`, `http://<mac-ip>:8080`; not loopback, so create a pod with `server pods create` and enter its code; the open `dev` pod is refused there, since anyone on the LAN would get it and its sign-ins) or an HTTPS URL also
   works for the engine, which talks through Rust and is not restricted by Android's network security config; but
   the app allows cleartext to `localhost`, `127.0.0.1` and `10.0.2.2` only, and only for Java/Kotlin traffic, and images
   load through Coil/OkHttp from the pod's media proxy, so over plain-HTTP LAN the feed images fail. Use `adb
   reverse` or HTTPS. The authorize page itself is the instance's own `https://` URL, opened in the phone's browser.

3. Build and install the debug APK, then launch it on the feed workspace (extras are honoured by debug builds
   only):

   ```bash
   cd kmp-app && export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
   ./gradlew :androidApp:installDebug
   adb shell am start -S -n org.theconstruct.android/.MainActivity --es baseUrl http://localhost:8080 --es workspace pixelfed_home
   ```

   (A release build has no extras: pick **Local USB** in Settings' server picker instead.)

4. Tap **Sign in with Pixelfed**: the browser opens the instance's authorize page; log in and approve; the
   `construct://oauth` redirect (a local pod has no App Link) returns to the app, which shows your home timeline (Settings lists
   "Pixelfed · @you · <instance>"). Like and bookmark are applied at once and go through the outbox to the instance (queued while
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
- To offer more instances than the default, set `PIXELFED_ALLOWED_INSTANCES` (a list, or `*`); the signed-out screen
  then has a server box. A typed value without a scheme means `https://`.

## Adding a new layout version

The only way to change what the app looks like is a new layout file plus a deploy (DECISIONS D32):

1. Copy the newest file of the app, e.g. `layouts/pixelfed_home/0001.json` to `layouts/pixelfed_home/0002.json`
   (4 digits, the next number, no gaps), and edit it. The format is `{"note": "short changelog line", "schema": N,
   "layout": <UiNode>}`. The note is what the menu's version history shows. `schema` is the client schema the
   layout is written for: normally the current `protocol::SCHEMA_VERSION` (the pod refuses a value below what the
   layout's vocabulary needs, or above the current one). The layout features it uses are worked out by the pod,
   not declared (docs/decisions/layout-features.md). The layout root is the app's `core://Workspace`, and its id
   is the app id. If another PR adds the same number first (git reports an add/add conflict), merge both changes
   into the next number: each file is a whole snapshot of the layout.
2. There is no list to edit: the adapter's `build.rs` embeds every `layouts/<app>/*.json`.
3. Run `cargo test` (`-p app-pixelfed` is enough). It validates every file (structure, size, slots, aspect ratios,
   endpoints).
4. Push to `main`. CI runs the checks and deploys the pod to staging. On start, the pod appends the file as the
   app's next version (logged as `layout file deployed as a new version`). Connected clients reconnect their SSE
   stream, see the newer version and refetch. Clients that follow the latest version show it live. A client pinned
   to an older version keeps it, and its menu says "Newer version available".

Files are **append-only**. Never edit, rename or delete a deployed file: the pod refuses to start and names the
file (e.g. "layouts/pixelfed_home/0002.json changed after it was deployed as version 7; layout files are
append-only, add 0003.json instead"). A gap in the numbering, an invalid layout or a wrong root id also stops the
start. Reformatting a file without changing its layout or note is fine. To undo a change, add a new file with the
old content. A file's `schema` is append-only too (it is recorded separately from the content hash, since it was
added to the first files after they deployed).

### Screens, generic nodes and actions (D34)

App changes ship from the pod: a layout file can add screens, lists, text fields and new actions without a new
client build. The engine and the clients know no app.

- **Screens:** `core://Workspace` takes an optional `screens` list. Each screen is `{"id", "route", "title",
  "data"?, "content", "footer"?}`. `route` is a URI pattern such as `pixelfed://posts/{id}/comments`; an
  `action.navigation.open` whose `target_uri` matches it opens the screen (Push / Modal stack it, Replace swaps the
  top one), and Back (the arrow, Android Back, `action.navigation.back`) closes it. Every `$route.<name>` in the
  screen is replaced by the captured value. `data` is a pod path the client loads when the screen opens; it answers
  `{"entities": {...}, "queries": {...}}`. `content` scrolls; `footer` stays at the bottom.
- **Nodes:** `core://List` (`query_key`, `item_template`, optional `indent: {field, step_dp, max?}` and
  `empty_text`; `$item.<field>` binds each entity), `core://Image` (`url` literal or `$item.<field>`, `shape`
  Square/Rounded/Circle, `size_dp` or `aspect_ratio`, `content_description`) and `core://TextField` (`field`,
  `placeholder`, `max_length`, `on_submit`). The text field's value is `$form.<field>` for intents on the same
  screen. Submitting dispatches `on_submit` with it, then clears the field. Nothing is sent while a referenced
  field is blank.
- **Actions:** `action.data.load {path}` loads and caches a data path. `action.mutation.create {query_key,
  entity_uri, entity, endpoint, payload, refresh_workspace?}` lists `entity` in the query at once, sends `payload`
  through the offline outbox, then reloads the paths that list the query (and the workspace with
  `refresh_workspace`); a 4xx removes the entity. `$nonce` is a fresh value per dispatch, for temporary URIs and
  idempotency keys.
- **Building blocks (schema 3, D37):** `core://Row`, `core://Column`, `core://Spacer`, `core://Divider`,
  `core://Conditional` (`value` binding, `content`), `core://Carousel` (`items` binding, `item_template`,
  `aspect_ratio` or `aspect_field` with `min_aspect`/`max_aspect`, `tag`), `core://IconButton` (`icon` from
  `protocol::ICONS`, `on_click`, `active` binding with `active_icon`/`active_tint`/`active_content_description`,
  `tag`), `core://Chip` (`tone` Neutral/Warning) and `core://Badge`. There are no app-specific nodes: the Pixelfed
  post card is a `core://Column` in the layout file.
- **Tappable (schema 4, D42):** `core://Tappable` (`on_click`, `content` any node, optional `content_description`
  for screen readers and bindable `tag`) makes its content tappable: a tap anywhere on it dispatches `on_click` with
  `$item` bound. A button inside keeps its own tap. The recipes list rows (`layouts/recipes/0002.json`) are
  tappable titles.
- **List search and groups (schema 5, D43):** `core://List` takes an optional `filter` (`value`: `$form.<field>` or
  literal text, `fields`, optional `no_match_text`): while the value is non-blank, only items where every word of it
  appears in one of `fields` are drawn (case and accents ignored), filtered on the client as you type, offline too.
  A `core://TextField` without `on_submit` edits that form value (one line, with a Clear button). `group_by`
  (`field`, `header` node bound to the group's first drawn item) draws a header wherever `item[field]` changes, so
  headers stay right while a filter hides items. `layouts/recipes/0003.json` puts a search box over the recipes
  list (the adapter's `search_text`: title, category, ingredient names).
- **Surfaces and choices (schema 6, D44):** `core://Box` puts any node on a surface (`background`,
  `content_color`, `border_color` as `#RRGGBB` or a binding such as `$item.color`; `corner_dp`, paddings, optional
  `width_dp`/`height_dp` and `align`). `core://ChoiceChips` (`field`, `options` of `{label, value}`) is a chip row
  editing a form value; a List's `where` (`field`, `equals`) then shows only exact matches. Group headers get
  `$item.group_count`. `core://Column` and `core://Row` take `align`; `Display` is a large serif text style; `flame`
  and `timer` are icons. `layouts/recipes/0004.json` uses them for colour tiles, category chips and the recipe
  card.
- **Grids and switches (schema 8, D48):** a `core://List` with `columns` (2..6) lays its items out in rows of equal
  cells (`spacing_dp` between items both ways, `padding_h_dp` at the sides; a group header starts a new row).
  `core://Conditional` reads a form value (`"value": "$form.view"`, "" while unset) and takes an optional `equals`:
  the content shows only when the value, as text, equals it. With `core://ChoiceChips` editing that form value, a
  layout switches between views with no new state. `layouts/pixelfed_home/0004.json` puts a List/Cards switch on
  the feed: two-column cards (cover photo, author, like count) that open a post screen
  (`pixelfed://posts/{id}`, data from `/v1/pixelfed/screens/post?id=`).
- **Remembered choices (schema 10, decision `remembered-form-values`):** `"remember": true` on a
  `core://ChoiceChips` makes the device keep its value (engine `cache.db`, table `form_values`, per app and form:
  `""` for the app's layout, else the screen id) and fill it in before the layout or screen first draws. Only a
  value that is still one of the options comes back. Never sent to the pod; kept at sign-out.
  `layouts/pixelfed_home/0006.json` remembers the List/Cards switch.
- **Sticky (schema 11, [sticky-nodes](docs/decisions/sticky-nodes.md)):** `core://Sticky` (`content` any node)
  scrolls with the page until it reaches the top of the scrolling view, then stays pinned there while the rest of
  its parent scrolls under it (like CSS `position: sticky`), and scrolls away with the end of its parent. In the
  workspace it pins just below the menu and bell buttons; in a screen, right under the bar. It may not sit inside a
  List or Carousel template or another Sticky. `layouts/recipes/0005.json` pins the search box and category chips.
- **Scroll to one item (schema 12, decision [`list-focus`](docs/decisions/list-focus.md)):** a `core://List` takes
  an optional `focus: {uri, background?, content_color?}`. The item whose entity URI is `uri` (usually built from a
  route parameter, e.g. `pixelfed://comments/$route.comment`) is scrolled into view once, the first time the list
  draws it, and drawn on `background` (`#RRGGBB`). One-column lists only. `layouts/pixelfed_home/0007.json` uses it
  for the `comment_thread` screen (`pixelfed://posts/{id}/comments/{comment}`), which a comment notification opens.
- **Schema versions (D36, D38):** the engine sends `X-Construct-Schema: 12` (workspace fetches, data loads, the
  apps API and the events stream). Every version records the schema it was written for (`"schema"` in its file). The pod serves each client
  the newest version it can draw, so an older app (no header = 1) keeps getting the last version without screens
  or the new nodes. Screens and List/Image/TextField need `"schema": 2`; the building blocks need 3; `core://Tappable` needs 4; a List `filter` or `group_by` needs 5; the schema-6 vocabulary above needs 6; `action.auth.session` needs 7; List `columns`/`spacing_dp`/`padding_h_dp` and a form-based or `equals` Conditional need 8; the photo-posting vocabulary (D49-D51) needs 9; a remembered `core://ChoiceChips` needs 10; `core://Sticky` needs 11; a List `focus` needs 12. `GET /v1/apps/{id}/versions[/{n}]` adds `schema`,
  `compatible` and, when false, `incompatible` (`update_required`: written for a newer app; `retired`: written for
  a schema this app no longer draws) per version; the menu greys those rows out and won't pin them. "Latest" in the
  menu is always a version this app can draw; when the newest needs a newer app, the menu adds "Update the app to
  see vN" instead of "Newer version available".
- **Layout features (additive vocabulary after schema 12):** new nodes and attributes no longer bump
  `SCHEMA_VERSION`. Each is a named feature, one file `crates/protocol/features/<name>.md`, mapped from the
  vocabulary in `protocol::required_features` (docs/decisions/layout-features.md). Clients send the features they
  draw in `X-Construct-Features`; the pod serves a version only to clients that have every feature it uses (else
  `update_required`) and lists them as `features` in `GET /v1/apps/{id}/versions[/{n}]`.
- **Progress (feature `progress`):** `core://Progress` (`size_dp` optional, default 16) is an indeterminate
  circular spinner in a quiet theme colour, with no children or intents. Put it in a `core://Conditional` to show
  work in progress, e.g. a group header that spins while its `section` is still `"Unsorted"`.
- **A breaking schema change:** bump `protocol::SCHEMA_VERSION` and add `(new schema, oldest layout schema it still
  draws)` to `protocol::RETIRED`. Older versions then show as incompatible to new clients and are never served to
  them; the old files are not edited. `RETIRED` is `[(3, 3)]`: schema 3 removed the `pixelfed://` widgets (D37),
  so schema-3 clients draw only schema-3 versions (`pixelfed_home/0003`, `pixelfed_demo/0002`). Schema 4 (D42) was
  additive (no `RETIRED` entry): schema-4 clients still draw schema-3 versions, and schema-3 clients keep getting
  the newest schema-3 version (e.g. `recipes/0001`). Schema 5 (D43) and 6 (D44) were additive too (`recipes/0002` for
  schema-4 clients, `recipes/0003` for schema 5).

`layouts/pixelfed_home/0003.json` builds the feed from these nodes. It (like 0002) uses the rest for comments: the feed's comment button (already
`pixelfed://posts/$item.id/comments`) opens the `comments` screen, which loads
`/v1/pixelfed/screens/comments?post_id=$route.id` into a `core://List` and posts new comments with a
`core://TextField` and `action.mutation.create`. From `0007` the comments screen also shows its post above the
comments (the same data call returns it as `post:<id>`; given a comment id, the pod walks up to the post), and each
comment has a like button (the posts' `/v1/pixelfed/posts/like` endpoint works on comments).

## The menu (all surfaces)

Desktop, Android and the simulator share one Compose UI (DECISIONS D33). The screen shows only the layout, with a
small **menu button** at the top left (content description "Menu"). The button gets a badge dot when you are
offline, when actions wait to be sent, or when the version you picked has a newer one. The button opens a drawer:

- **Apps:** `Pixelfed` (plus `Pixelfed demo` when the pod runs with `CONSTRUCT_SHOW_DEV_APPS=1`).
  Tap a row to open that app. The history button opens the **version picker**: "Latest", then every version
  newest first ("vN", age, note), with "Load older". Picking a version shows that layout with your current data.
  This happens only on this device: nothing changes on the pod, and the next launch shows the latest again. While
  you view an older version and a newer one is published, the drawer shows "Newer version available" with
  **Show latest**. Picking "Latest" follows new versions again. When the version you picked is on an older page,
  a checked "Pinned: vN" row sits under "Latest" until "Load older" lists it.
  Custom apps (README "Custom apps") are marked **Custom** (also in the version picker). Each row's **⋮** offers
  **Hide from the app list** (any app, on every device; "Show hidden apps (N)" lists them again) and, for custom
  apps, **Delete…** with a confirmation. When the app on screen is deleted (here or elsewhere), the app opens the
  default app and says "This app was deleted.".
- **Settings** (at the bottom; decision [settings-page-and-sign-ins](docs/decisions/settings-page-and-sign-ins.md)),
  with the connection status beside it: Online / Offline and "N pending" ("Connecting…" until the engine first
  reports; no offline badge then). Offline, the Apps list from the last load stays without an error, and it reloads
  once back online. Settings opens a page with:
  - **Your pod:** the connection (with the last error), the server picker (below), and "Pod open on this device"
    with **Leave pod**, which forgets the pod code on this device.
  - **Sign-ins:** every sign-in on the pod (`GET /v1/auth/sign-ins`), e.g. "Pixelfed · @you · pixelfed.social".
    A row opens its detail: server, username, the apps that use it, and **Sign out of Pixelfed** (the pod stays
    open). With none: "Not signed in", or "Signing in…" / "Sign-in failed: …". Every sign-in the pod offers and
    the session lacks gets a **Sign in to Pixelfed** row (with a server box where other servers are allowed); a pod
    too old to list them falls back to the screen's **Sign in** button. A pod that can't list sign-ins (an older
    one) still shows "Signed in as @you" with **Sign out**.
  - **Notifications:** the per-app notification channels (D52).
- **Server** (in Settings): Staging, Local emulator (`http://10.0.2.2:8080`), Local USB (`http://127.0.0.1:8080`) or a custom
  URL on Android; Staging, Local (`http://127.0.0.1:8080`) or a custom `http(s)://host[:port]` on desktop. On
  Android a custom URL must be `https://` unless it is `localhost`, `127.0.0.1` or `10.0.2.2` ("Use https:// for
  remote servers"), since images can only load over cleartext from those hosts. This is in every build, release
  included. Both switch in place: the new engine is started first, and the choice is saved only once it started.
  Each server keeps its own cache and outbox.

Back (Android) closes the innermost level (custom URL field, notification settings, a sign-in's detail, Settings,
picker, drawer). Test tags: `menu-button`,
`menu-badge`, `menu-drawer`, `menu-close`, `menu-back`, `app-<id>`, `app-history-<id>`, `app-error`, `apps-error`,
`app-more-<id>`, `app-hide-<id>`, `app-delete-<id>`, `app-custom-<id>`, `apps-show-hidden`, `app-action-error`,
`delete-app-dialog`, `delete-app-confirm`, `delete-app-cancel`, `app-gone`, `notice`, `notice-dismiss`,
`provider-<id>`, `provider-server-<id>`, `provider-server-go-<id>`, `app-updates-<id>`, `app-updates-dismiss-<id>`,
`menu-newer`, `menu-newer-latest`, `menu-settings`, `settings`, `menu-connection`, `menu-account`, `sign-in-<id>`,
`sign-in-detail`, `sign-ins-error`, `menu-sign-in`, `menu-sign-out`, `menu-leave-pod`, `menu-notifications`,
`menu-server`, `server-<key>`, `server-custom`, `server-custom-url`, `server-custom-hint`, `server-custom-use`,
`version-picker`, `version-latest`, `version-pinned`, `version-<n>`, `versions-more`, `versions-error`,
`version-error`.

## Milestone check recipes

DESIGN.md M5 (offline click lands in the outbox, flushed on reconnect), with the one-post demo (no sign-in):

```bash
cargo run -p server                                                  # terminal 1
cd kmp-app && CONSTRUCT_BASE_URL=http://127.0.0.1:8080 CONSTRUCT_WORKSPACE=pixelfed_demo ./gradlew :desktopApp:run   # terminal 2
# stop the server with Ctrl-C in terminal 1: the menu button gets a badge; the menu says Offline
# click the heart, then the bookmark: both update at once and the menu shows "2 pending"
sqlite3 ~/.construct/cache.db "SELECT id, endpoint, status, retry_count FROM action_outbox;"   # 2 PENDING rows
cargo run -p server                                                  # terminal 1 again: within ~2 s flushed, rows deleted
sqlite3 ~/.construct/cache.db "SELECT count(*) FROM action_outbox;"     # 0
```

A new layout version reaches an open app without a restart (the M7 push, now driven by a deploy):

```bash
cargo run -p server                                                  # terminal 1
cd kmp-app && CONSTRUCT_BASE_URL=http://127.0.0.1:8080 CONSTRUCT_WORKSPACE=pixelfed_demo ./gradlew :desktopApp:run   # terminal 2
# add layouts/pixelfed_demo/0003.json + its DEMO_FILES line (see "Adding a new layout version"), then restart
# terminal 1: the app reconnects its event stream and shows v3 within a few seconds; the menu's Pixelfed demo history
# lists it
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

**Comments.** `fixtures/pixelfed/comments.json` seeds threaded comments on every timeline post (as many direct
comments as each post's `replies_count`, plus nested replies; authors are the timeline accounts and the signed-in
`constructdemo`). Each entry is compact (`id`, `in_reply_to_id`, `account` username, `created_at`, plain `text`,
optional `favourites_count`) and is expanded into a full status; parents must come before their replies. The API
is Mastodon's, with Pixelfed's threads:

- `GET /api/v1/statuses/:id/context`: `{"ancestors": [...], "descendants": [...]}`. Like Pixelfed, one level each
  way: the direct parent only, and the direct replies only (oldest first, at most 100, none while `replies_count` is
  0), so the replies to a comment come from that comment's own `context` (D47). With
  `FAKE_PIXELFED_THREADS=mastodon`, Mastodon's whole thread instead: the parent chain root first, and every reply
  below depth first. On a reblog, the reblogged post's thread.
- `POST /api/v1/statuses` (form or JSON) with `status` and `in_reply_to_id`: adds a comment by the signed-in
  account and bumps the parent's `replies_count`. Optional `visibility`, `spoiler_text`, `sensitive`; the
  `Idempotency-Key` header replays the first result. 422 for blank text, more than 500 characters or no
  `in_reply_to_id` (top-level posts need media on Pixelfed, which the fake doesn't take); 404 for an unknown parent.
- `GET /api/v1/statuses/:id`; `DELETE /api/v1/statuses/:id` (own statuses only, else 403; returns it with `text`).
- Comments are statuses, so `favourite` / `bookmark` work on them too. `POST /_admin/reset` restores the threads.

```bash
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/statuses/781254316723458049/context \
  | jq '.descendants[] | {id, in_reply_to_id, who: .account.username, content_text, replies_count}'
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/statuses/781300000000000101/context \
  | jq '.descendants[] | {id, in_reply_to_id, content_text}'     # the replies to a comment
curl -s -H 'Authorization: Bearer dev' -d status='Lovely light!' -d in_reply_to_id=781254316723458049 \
  http://127.0.0.1:8081/api/v1/statuses | jq '{id, content}'
```

**Sidecar / Docker.** One self-contained binary (release: 3.4 MB, 2.1 MB stripped with thin LTO; ~3.5 MB RSS
idle, ~5 MB after a few hundred requests; the Docker container ~1 MB). Image: `rust:1-slim-bookworm` builder ->
`gcr.io/distroless/cc-debian12:nonroot`, 51 MB.

```bash
docker build -f crates/fake-pixelfed/Dockerfile -t construct-fake-pixelfed .      # from the repo root
docker run --rm -p 127.0.0.1:8081:8081 -e FAKE_PIXELFED_ADMIN=1 \
  -e FAKE_PIXELFED_PUBLIC_URL=http://127.0.0.1:8081 construct-fake-pixelfed
E2E_FAKE_URL=http://127.0.0.1:8081 cargo test -p e2e --test suite                # suite against it
```

Inside the container it binds `0.0.0.0:8081`, so the admin routes stay off unless `FAKE_PIXELFED_ADMIN=1`; it
stops on SIGTERM.

**Admin endpoints** (test controls; in-process the same as `AppState::reset`, `add_faults`, `clear_faults`,
`report`, `requests`, `favourited_ids`, `bookmarked_ids`, plus `reply_ids`, `created_ids` and `status`):

- `POST /_admin/reset[?keep_auth=1]`: favourites, bookmarks, comments and counts back to the fixtures; fault rules,
  request log and grant counters cleared; apps, codes and tokens dropped (as after an instance wipe) unless
  `keep_auth=1`.
- `GET /_admin/state[?tail=50]`: `counts` (apps, codes, access/refresh tokens, statuses, comments, requests),
  `created` (ids of comments posted through the API since the reset),
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

The Pixelfed adapter (`crates/app-pixelfed`, docs/DECISIONS.md D17, D40) adapts one Pixelfed instance: it owns
the upstream access token, normalizes the home timeline into Construct entities and supplies the `pixelfed_home`
workspace's data (the core adds the layout). The client only holds a pod session token and sends
`Authorization: Bearer <session>`. Sessions belong to the core and are shared by every app; signing in records the
`pixelfed` sign-in on the session, which `pixelfed_home` requires.

Configuration (`PIXELFED_*`, `CONSTRUCT_SESSION_TTL_DAYS`): see
[Server runtime configuration](#server-runtime-configuration).

Endpoints:

- `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}` -> `{client_id, scopes, authorize_endpoint}`
  (registers an app upstream once per redirect_uri; a cached app is re-checked with a `client_credentials` grant
  and re-registered if the instance no longer knows it). Another instance -> 400 `instance_not_allowed`.
  `redirect_uri` must be `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback`, `construct://oauth`
  or a `PIXELFED_EXTRA_REDIRECTS` entry (e.g. the Android App Link `https://<pod host>/oauth/android`), else 400
  `redirect_uri_not_allowed`.
- `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}` -> `{session_token,
  account:{id, username, display_name, avatar_url}}`. Upstream rejection -> 400 `oauth_exchange_failed`. An
  account other than the pinned owner -> 403 `account_not_owner` (the stored account is untouched).
  With a valid bearer, the sign-in is added to that session and its token is returned.
- `POST /v1/auth/logout` (bearer, a core route) -> `{ok:true}`; deletes the session and its sign-ins (401 if it is
  not valid).
- `GET /v1/workspaces/pixelfed_home`: with a valid session, the home feed bundle (`queries["timeline:home"]` +
  posts/users/media entities, PostCard layout with like/bookmark optimistic mutations). Without a bearer: 200 with a
  signed-out bundle (a "Sign in with Pixelfed" `action.auth.login` button). With a bearer that no longer works
  (unknown session, a session without the `pixelfed` sign-in, or the upstream rejected the stored token): 401 with the same signed-out bundle as the body and
  `X-Construct-Session: invalid`, so the client drops its stale session. Upstream unreachable -> 502. The upstream
  request is `GET /api/v1/timelines/home?limit=20&include_reblogs=true` (Pixelfed hides boosts without it).
- Upstream tokens: the pod stores the access token and, when the upstream issues them (Pixelfed does), the refresh
  token and expiry. On an upstream 401 (feed or action) it refreshes once (`grant_type=refresh_token`, serialized
  so concurrent requests share one refresh) and retries; if that fails, the feed signs out as above.
- `POST /v1/pixelfed/posts/like {id, liked}` and `POST /v1/pixelfed/posts/bookmark {id, bookmarked}` -> `{ok, id,
  is_liked, like_count, is_bookmarked}`. No session 401; upstream 4xx passed through; upstream 5xx/unreachable 502.
- `GET /v1/pixelfed/posts/{id}/comments` (bearer) -> `{post_id, comments: [{id, in_reply_to_id, depth, author:{id,
  username, display_name, avatar_url}, text, created_at, like_count, is_liked, is_own}]}`: the post's thread from
  upstream `GET /api/v1/statuses/{id}/context`, each comment followed by its replies (depth capped at 4). A boost's
  id gives the boosted post's thread. Unknown post 404 `post_not_found`; no session 401; upstream down 502.
  Clients don't use it yet (D34).
- `POST /v1/pixelfed/posts/comment {id, text, client_id?}` (bearer) -> `{ok, id, comment}`: posts `text` (1..500
  characters) as a reply to post or comment `id` (upstream `POST /api/v1/statuses` with `in_reply_to_id`).
  `client_id` (`[A-Za-z0-9_-]{8,64}`, a client nonce) becomes the upstream `Idempotency-Key`. Bad input 400;
  otherwise as for likes. Usable as an outbox endpoint.
- `GET /v1/pixelfed/screens/comments?post_id=<id>` (bearer): the comments screen's data, `{"entities", "queries"}`:
  one `pixelfed://comments/<id>` entity per comment (`id, post_id, in_reply_to_id, depth, author_name,
  author_handle, avatar_url, text, created_at, meta ("@bob · 3 h"), like_count, is_liked, is_own`) and
  `queries["comments:<post_id>"]` in thread order (each comment followed by its replies, depth capped at 4), from
  upstream `GET /api/v1/statuses/{id}/context`, plus that call for each comment that reports more replies than
  the pod has (Pixelfed lists only direct replies; at most 50 extra calls, 6 at a time; a failed one only loses
  its branch, D47). A boost's id gives the boosted post's thread. Unknown post 404
  `post_not_found`; no session 401; upstream down 502.
- `POST /v1/pixelfed/posts/comment {id, text, client_id?}` (bearer) -> `{ok, id, comment}`: posts `text` (1..500
  characters) as a reply to post or comment `id` (upstream `POST /api/v1/statuses` with `in_reply_to_id`).
  `client_id` (`[A-Za-z0-9_-]{8,64}`) becomes the upstream `Idempotency-Key`, so an outbox retry doesn't post
  twice. Bad input 400 (the client drops the comment); otherwise as for likes.
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
  another app). A custom URI (Android: the App Link `https://<pod host>/oauth/android` or `construct://oauth`, see
  below): the app forwards the redirect to `handleAuthRedirect`, which matches it by `state` whatever its form.
  The custom-scheme flow is also kept in the secure store (`pending_auth:<baseUrl>`, 10 minutes), so a redirect
  that cold-starts the app after its process was killed still signs in; one that matches nothing shows "Sign-in
  was interrupted; please try again".
- `secureStore = null`: macOS Keychain (service `org.theconstruct.client`, key `pod_session:<baseUrl>`), in-memory
  elsewhere. The session survives restarts; `logout()` deletes it on the pod and locally.
- `urlOpener = null` (or `open` returns false): `auth_json` says "no browser available" and the authorize URL is
  logged (`[ConstructEngine] no browser available; to sign in, open this URL ...`); opening it by hand still works.
  With the fake, append `&auto=1` to approve without clicking.
- A 401 from the pod signs the client out (a stale session on `pixelfed_home` gets one, see above). A like queued
  while the session was invalid stays pending (not rolled back) and is sent after the next sign-in.
- Signing out (or a 401) removes cached private data from `cache.db`: every entity no query references, every
  data load and the workspace row (D37). `logout()` also drops actions queued while signed in; a 401 keeps them for
  the next sign-in. The DB runs
  with `secure_delete=ON` and the WAL is truncated after the purge, so the rows don't linger in the file's bytes.
- UI: Settings' Sign-ins list shows "Pixelfed · @username · <instance>"; its detail page has **Sign out**
  (`ConstructController.logout()`). The list shows "Signing in…" while pending, then "Loading your feed…" from the code exchange until the first signed-in fetch
  lands. It shows "Sign-in failed: <message>" on error, otherwise "Not signed in" with **Sign in** (the screen's own
  sign-in button works too). Messages are short and human ("This pod belongs to a different Pixelfed account.",
  "Can't reach your pod.", ...); the URL/status/body detail is in the `[ConstructEngine]` log only.

Per platform (DECISIONS D21):

- **Desktop** (`desktopApp` Main.kt): no store (Keychain), loopback redirect, and the default browser via
  `java.awt.Desktop.browse` (else `open <url>`). Clicking "Sign in with Pixelfed" opens the browser; after approving,
  the browser lands on the "Signed in" page and the window shows the feed.
- **Android**: the redirect is a verified App Link when the server is the build's App Link host (default the staging
  pod; `-PconstructAppLinkHost`) and the device has verified it (Android 12+ is asked through
  `DomainVerificationManager`; 8-11 verify at install), else `construct://oauth` (decision
  [`android-app-link-sign-in-redirect`](docs/decisions/android-app-link-sign-in-redirect.md); the custom scheme can
  be claimed by any app). Both intent filters are on `MainActivity` (`launchMode="singleTask"`, so the redirect
  returns to the same activity and engine through `onNewIntent`). The App Link's filter has `autoVerify`: Android
  fetches `https://<host>/.well-known/assetlinks.json`, which must list the build's signing certificate
  (`CONSTRUCT_ANDROID_CERT_SHA256`). If the browser keeps the redirect anyway, the pod's `/oauth/android` page has an
  **Open The Construct** button that opens this package only (an `intent:` link carrying the query on
  `construct://oauth`). Check with `adb shell pm get-app-links org.theconstruct.android` (`verified`) and the log
  line `opening engine for workspace=… redirect=…`. The session is encrypted with an AndroidKeyStore AES/GCM key
  (`construct_session_key`) and kept in SharedPreferences `construct_secure`; the authorize page opens with `ACTION_VIEW`.
  For the emulator, start the pod with `PIXELFED_PUBLIC_URL=http://10.0.2.2:8081` (and the fake with
  `FAKE_PIXELFED_PUBLIC_URL=http://10.0.2.2:8081`) so the browser can reach the authorize page. If the browser is not
  usable (e.g. Chrome's first-run screen), deliver the redirect by hand:
  `adb shell am start -a android.intent.action.VIEW -d '"construct://oauth?code=...&state=..."'`.
- **Simulator** (live mode): in-memory store and an auto-approving "browser" for the fake, or a scripted Pixelfed
  web login for a real instance with `-PpixelfedLogin=<user>:<password>` (see Simulation mode).

### Live layout updates and the apps API (client)

client-core side of D24 (push) and D33 (apps):

- The engine remembers the served layout version (`X-Construct-Layout-Version`). While online, it keeps
  `GET /v1/workspaces/<id>/events` open on a `construct-engine-events` thread. A `layout` event with a newer
  version refetches the workspace, and queued optimistic changes are re-applied on top; equal or older versions are
  ignored. After errors the stream reconnects with a 1 s -> 30 s backoff (and after 45 s of silence). It closes when
  the engine is dropped, and a line or event over 64 KB drops the connection and reconnects. Log lines:
  `[ConstructEngine] events: connected to ...`, `events: layout v3 pushed (have v2); refetching`.
- Every snapshot carries `layout_json` = `{"app_id": "<workspace>", "version": n}` (`ConstructState.activeLayout`
  in Kotlin), or `{}` when the screen is unversioned (the signed-out `pixelfed_home` screen) or nothing has loaded.
- Read-only apps API (blocking; on Kotlin, call it on `Dispatchers.IO`, never the UI thread). Each call returns the
  pod's JSON body and sends no bearer (the endpoints are public):

| Kotlin (`ConstructEngineBridge`) | Rust (`ConstructEngine` / `EngineHandle`) | Pod |
|---|---|---|
| `listApps()` | `list_apps()` | `GET /v1/apps` |
| `appVersions(appId, before?, limit?)` | `app_versions(app_id, before, limit)` | `GET /v1/apps/:id/versions` |
| `appVersion(appId, n)` | `app_version(app_id, n)` | `GET /v1/apps/:id/versions/:n` (with `layout`) |

- Failures throw `LayoutApiException` (Kotlin; `kind` Offline / Unauthorized / NotFound / Server) with a readable
  message, e.g. "That app doesn't exist.", "That version doesn't exist." or "Can't reach your pod. Check your
  connection and try again.". Bridges without the API (other fakes) throw "Apps aren't available here.".
- The agent prompt (`action.agent.prompt`), `agent_json` and the layout catalog calls are removed. A layout that
  still contains an unknown intent (e.g. an old AI-edited version in the history) renders, and that action does
  nothing (`ActionIntent::Unknown`).

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

Seeded data (idempotent: existing users/posts are kept, a fresh token is minted per run): `constructadmin` /
`construct-dev-pass` (admin), `alice` / `alice-dev-pass`, `bob` / `bob-dev-pass`; tokens in
`dev/pixelfed/.seed/{admin,alice,bob}_token` (gitignored). constructadmin follows alice; alice posts a 3-image
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
  `constructadmin@example.test`, `alice@example.test`, `bob@example.test`). A client the user already approved is
  redirected straight back without the consent page.

End to end with the desktop simulator (pod on 8480 so it does not clash with a dev pod on 8080):

```bash
CONSTRUCT_ADDR=127.0.0.1:8480 CONSTRUCT_DATA_DIR=/tmp/construct-pf PIXELFED_INSTANCE_URL=http://localhost:8090 \
  cargo run -p server &
cd kmp-app && ./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home \
  -PbaseUrl=http://127.0.0.1:8480 -PpixelfedLogin=constructadmin:construct-dev-pass \
  -Pscript="shot signed-out; click Sign in with Pixelfed; wait-text 30000 @constructadmin; wait-text 20000 Alice Example boosted; wait-images 30000; shot feed"
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

A headless feedback loop: `:desktopApp:simulate` renders the real shared `ConstructApp` to an offscreen
virtual screen (no window), runs a small script against it and writes PNG screenshots. A run takes about
2 seconds. Run it from `kmp-app` with `JAVA_HOME` set as above:

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pscreen=phone \
  -Pscript="shot feed; click Like; assert-text Unlike; shot liked; offline; shot offline"

./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pscreen=desktop
./gradlew -PskipRustBuild :desktopApp:simulate \
  -Pscript="wait-images; shot feed; swipe-left tag:carousel:; assert-text Page 2 of 3; shot page2; click Like; assert-text 28; shot liked"
# the menu: history, an older version, a newly published one, back to latest (see "Menu commands" below)
./gradlew -PskipRustBuild :desktopApp:simulate \
  -Pscript="open-menu; shot menu; open-history Pixelfed; shot history; select-version 1; shot v1; publish-version Bigger photos; open-menu; assert-text Newer version available; shot newer; open-history Pixelfed; select-version latest; shot latest"
./gradlew -PskipRustBuild :desktopApp:simulate -PscriptFile=my_script.txt -Pdensity=1 -Pout=/tmp/shots
./gradlew -PskipRustBuild :desktopApp:simulate -PbaseUrl=http://127.0.0.1:8080 -Pscript="shot live"   # live mode
# live sign-in against the pod + fake upstream (see "Sign in (client)")
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_home -PbaseUrl=http://127.0.0.1:8080 \
  -Pscript="shot signed-out; click Sign in with Pixelfed; open-settings; wait-text 20000 @constructdemo; close-menu; wait-text 20000 Alice Lumen; wait-images 20000; shot feed; click Like; shot liked"
# live menu against a local pod started with CONSTRUCT_SHOW_DEV_APPS=1
./gradlew -PskipRustBuild :desktopApp:simulate -PbaseUrl=http://127.0.0.1:8080 \
  -Pscript="open-menu; open-history Pixelfed; select-version 1; open-menu; open-app pixelfed_demo; assert-text Sunrise over Boulder"
```

Flags (all optional):

| Flag | Default | Meaning |
|---|---|---|
| `-Pworkspace` | `pixelfed_home` | Workspace (app) id at start. In fixture mode it selects the default fixture. |
| `-Pscreen` | `phone` | `phone` (390x844 dp), `tablet` (820x1180), `desktop` (1280x800), or `WxH` in dp. |
| `-Pdensity` | per screen | Pixels per dp: phone 3, tablet 2, desktop 2, `WxH` 2. PNG size = dp x density. |
| `-Pscript` | `shot initial` | Script text (see below). |
| `-PscriptFile` | | Script read from a file. Use either this or `-Pscript`, not both. |
| `-Pmode` | `fixture` | `fixture` or `live`. Live mode is selected automatically when `-PbaseUrl` is given. |
| `-Pfixture` | per workspace | Bundle JSON in fixture mode. Defaults: `pixelfed_demo` uses `fixtures/pixelfed_home_bundle.json` (one post), `pixelfed_home` uses `fixtures/pixelfed_home_feed_bundle.json` (the fake upstream's five-post timeline, normalized as the pod does, with `queries`). Other workspaces need `-Pfixture`. |
| `-PbaseUrl` | `https://the-construct-pod.fly.dev` | Server for live mode (the staging pod unless given; local: `http://127.0.0.1:8080`). |
| `-PtimeoutMs` | `10000` | Live mode only: how long to wait for the workspace to load before the first step. |
| `-Pout` | `kmp-app/desktopApp/build/sim/<yyyyMMdd-HHmmss>` | Output directory. |
| `-PpixelfedLogin` | | Live mode only: `<e-mail or seed username>:<password>` for a real Pixelfed (below). |
| `-Pstorage` | fresh temp dir | Live mode only: engine storage dir (holds `cache.db`). It is kept after the run, so a second run with the same dir exercises restart persistence. The temp default is deleted on exit. |

Relative paths (`-Pfixture`, `-PscriptFile`, `-Pout`, `-Pstorage`) resolve against the **repo root**, the task's working
directory, so `-Pfixture=fixtures/pixelfed_home_bundle.json` works.

**Fixture mode** (the default) needs no server, and no Rust build when run with `-PskipRustBuild` (without
it, the desktop compile triggers `:shared:buildRustFfi`, i.e. cargo). A fake in-memory engine
(`FakeEngineBridge`) loads the bundle, applies `OptimisticMutate` clicks with the D4 expression subset, and
toggles connectivity for `offline`/`online`. It models the outbox: clicks made while offline are applied
locally and queued (the menu shows "N pending"), and `online` flushes them FIFO to an in-memory fake
server. That server rejects a `.../decrement` that would take a `count` below 0 (the test-only
`counter_home_bundle.json`); the fake then rolls the field back and re-applies later queued rows (D8). It also
serves a fake apps catalog (`FakeAppCatalog`, as with `CONSTRUCT_SHOW_DEV_APPS=1`): Pixelfed v1..v3 and Pixelfed
demo v1..v2 (a `-Pfixture` bundle of another workspace is added as a one-version app). The newest version of each is
the fixture's own layout, and `publish-version` adds more. Opening another app switches to that app's fixture.
**Live mode** runs the real Rust engine through JNA against
the server. By default it uses a fresh temp storage dir, so it neither reads nor writes `~/.construct`; pass
`-Pstorage=<dir>` to keep a persistent one. If the workspace
does not load within `timeoutMs`, it logs a warning and still runs the script (shots show Loading/Offline).
`offline`/`online` are not supported in live mode; a live-mode script containing them is rejected before any
step runs. **Sign-in in live mode:** the engine gets an in-memory session store (every run starts signed out; the
Keychain is never touched) and a "browser" that GETs the authorize URL with `auto=1`; when the answer is a redirect to
the engine's loopback `redirect_uri` (the fake upstream does that, with or without `FAKE_PIXELFED_AUTO_APPROVE`), it
follows it, so `click Sign in with Pixelfed; open-settings; wait-text 20000 @constructdemo` signs in (the account is shown in Settings). A real Pixelfed answers with its
login page instead: the simulator prints the authorize URL and Settings' Sign-ins section shows "no browser available"
(opening the URL in a real browser and logging in still completes the sign-in while the script waits). For a real
instance pass `-PpixelfedLogin=<login>:<password>`: the "browser" then does what a person would (`PixelfedLoginBrowser`,
own cookie jar): `GET /login` for the CSRF `_token`, `POST /login` (Pixelfed logs in by e-mail; a login without `@` is
sent as `<login>@example.test`, the dev seed's domain), `GET` the authorize URL, `POST` the consent page's approve
form (`_token`, `state`, `client_id`, `auth_token`; the deny form is the one with `_method=DELETE`) and follows the
302 to the loopback listener. Failures (wrong password, 429 throttling) are logged with the URL. After the user
appears (`open-settings; wait-text 30000 @constructadmin; close-menu`), wait for feed text too (e.g. `wait-text 20000 Alice
Example boosted`): the account updates before the refetched feed arrives.

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
| `wait-text <ms> <text>` | Like `assert-text` with a custom timeout (at most 600000), e.g. `wait-text 15000 Alice Lumen` while a live feed loads. |
| `wait-images [ms]` | Waits until no image is loading (default 10000 ms, at most 600000). Works in both modes. |
| `swipe-left <target>` / `swipe-right <target>` | Touch-swipes the selected node; `swipe-left` on a carousel shows the next page. |
| `scroll-to <target>` | Scrolls the feed until the selected node is visible (e.g. `scroll-to Kept my distance`). |
| `type <target> <text>` | Replaces the text of the selected text field with `<text>` (clear, then type), e.g. `type tag:server-custom-url http://127.0.0.1:8080`. The target is `tag:<testTag>`, a single word, or `"quoted text"` (may contain spaces); the text is the rest of the command. |
| `pick <path> [<path>…]` | Queues files the next photo picker (`core://MediaPicker`'s add tile) returns; relative paths resolve against the working directory, `"quoted paths"` may contain spaces. E.g. `click tag:new-post; pick fixtures/pixelfed/media/carousel-1.png; click tag:media-picker-add:photos; click Share`. In fixture mode a shared post shows "Posting…" until the next `wait` / `idle`. |
| `offline` / `online` | Fixture mode only: flips the engine's connectivity. |
| `publish-version <note>` | Fixture mode only: the fake pod publishes a new newest version of the app on screen (its layout with a "What's new: <note>" banner); followers see it at once. |
| `screen <WxH or preset>` | Restarts the renderer at the new size. Engine state carries over; UI state such as scroll position does not. |
| `back` | A system Back press (closes the innermost menu level); fails if nothing handles it. |
| `open-menu` | Opens the drawer (clicks `menu-button`) and waits for the apps list (or its error). |
| `close-menu` | Closes the drawer (leaving the version picker, Settings or a detail page first). |
| `open-settings` | Opens the drawer's Settings page (`menu-settings`) and waits for its list of sign-ins; reports how many are listed. |
| `open-app <id or title>` | Opens an app from the drawer (`pixelfed_home` or `Pixelfed`) and waits until it has loaded. |
| `open-history <id or title>` | Opens that app's version picker (opening the drawer first if needed). |
| `select-version <n or latest>` | In the picker, shows version `n` (`2` or `v2`; presses "Load older" as needed) or follows the latest again; waits for the drawer to close. |
| `set-server <preset or URL>` | Picks `staging`, `emulator`, `usb` or a custom `http(s)://host[:port]` in Settings' server picker (opening Settings first). The simulator records the choice and shows it in the menu, but keeps its engine. |
| `notify <app_id> <channel> <title>` | Fixture mode only: the fake pod sends a live notification (D52): the platform hook (`onNotification`) fires and the bell's unread count goes up. |
| `open-inbox` | Clicks the bell (`inbox-button`) and waits for the inbox (`inbox`) to load; reports the rows shown or its message. |
| `close-inbox` | Closes the inbox. |
| `click-notification <text>` | Clicks the inbox row containing `<text>` and waits for its app (opened if it is another one) and target screen; reports both. |
| `open-notification-settings` | Opens the notification settings (Settings' `menu-notifications` row) and waits for the channels. |
| `toggle-channel <app>.<channel>` | In the settings, flips a channel's switch (`channel:<app>.<channel>`, e.g. `pixelfed_home.likes`); reports `on`/`off`. |

The menu commands are thin wrappers over clicks on the menu's test tags (`click tag:app-history-pixelfed_demo` works too,
see [The menu](#the-menu-all-surfaces)). Each one waits until idle and until every apps call has finished (they run
on a background thread), and fails with a hint when its control is missing (e.g. `select-version 2` without
`open-history`: "no version picker on screen (use open-history first)"). Examples (fixture mode). The menu commands also
work in live mode; start the pod with `CONSTRUCT_SHOW_DEV_APPS=1` to list the dev apps. `publish-version`,
`notify`, `offline` and `online` are fixture-only.

```bash
cat > /tmp/menu.txt <<'SCRIPT'
# the drawer, the Pixelfed history, an older version, a newer one published meanwhile, back to latest
open-menu; shot menu
open-history Pixelfed; shot history
select-version 1; shot v1
publish-version Bigger photos
open-menu; assert-text Newer version available; shot newer
open-history Pixelfed; select-version latest; shot latest
# another app, then the server picker
open-menu; open-app pixelfed_demo; click Like; assert-text Unlike; shot demo
open-menu; set-server http://127.0.0.1:8080; assert-text 127.0.0.1; close-menu
SCRIPT
./gradlew -PskipRustBuild :desktopApp:simulate -PscriptFile=/tmp/menu.txt
./gradlew -PskipRustBuild :desktopApp:simulate -Pscreen=desktop -PscriptFile=/tmp/menu.txt
# offline: the badge and the Settings row
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo \
  -Pscript="offline; click Like; open-menu; assert-text Offline; assert-text 1 pending; shot offline"
```

A `<target>` is either `tag:<testTag>` (an exact test tag, else the first tag containing it) or text: an exact text
match, then an exact content description (clickable nodes preferred for both), then the same as substrings; the
first match wins. Pixelfed widgets carry content descriptions `Like`/`Unlike`, `Bookmark`/`Remove bookmark`,
`Comment`, the image alt text, and `Page n of N` on the carousel dots (plus an `n/N` badge text), and test tags
`carousel:<post uri>`, `carousel-dots:<post uri>`, `like:<post uri>`, `bookmark:<post uri>`, `comment:<post uri>`,
e.g. `click tag:like:pixelfed://posts/781254316723458049` or `swipe-left tag:carousel:` (the first carousel).
The menu's test tags are listed in [The menu](#the-menu-all-surfaces).

**Screens (D34).** Screens carry the tags `screen:<id>`, `screen-back`, `screen-loading`, `screen-error` and
`screen-retry` (`screen-loading` is the spinner in the title bar, so the content doesn't move when the load
ends); generic nodes carry `textfield:<field>`, `textfield-send:<field>` and `list-empty`. In fixture
mode the fake engine answers data paths from `fixtures/pixelfed_screen_data.json`, and sends creates through its
fake outbox (`offline` keeps them "Sending…" until `online`). That file and the feed fixture's layout are generated
from the pod: `UPDATE_FIXTURES=1 cargo test -p server --test fixtures` (the test fails when they are stale).

```bash
./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :desktopApp:simulate -Pscreen=phone -Pout=/tmp/sim \
  -Pscript='click tag:comment:pixelfed://posts/781254316723458049; wait-text 3000 Which lens?; shot comments;
    type tag:textfield:comment Golden light!; click tag:textfield-send:comment; wait-text 3000 just now; shot sent'
```

**Notifications (D52).** The bell (`inbox-button`, content description "Notifications") sits top-right with the
unread count (`inbox-badge`); the inbox (`inbox`) lists rows `notification-row:<id>` by day, with `inbox-mark-all`
and `inbox-close`; Settings' `menu-notifications` row opens `notification-settings` with `channel:<app>.<channel>` rows
(`channel-switch:<app>.<channel>`). In fixture mode the fake pod serves `fixtures/notifications.json` (wire-shaped,
except `age_seconds`, so times stay relative to now); its targets open the comments screen of `pixelfed_home` posts.
The inbox commands also work live (`-PbaseUrl`) against a pod's real notifications.

```bash
./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_demo -Pout=/tmp/sim \
  -Pscript='notify pixelfed_demo likes Bob liked your post; shot badge; open-inbox; shot inbox;
    click-notification commented on; shot comments; back; open-notification-settings; toggle-channel pixelfed_home.likes; shot settings'
```

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

`:androidApp` runs the same shared `ConstructApp` on Android, with the Rust engine cross-compiled to
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
- `-PandroidAbis=arm64-v8a,x86_64` (default): ABIs to build and package (`CONSTRUCT_ANDROID_ABIS` for the script,
  and the APK's `abiFilters`).
- `-PconstructBaseUrl=https://the-construct-pod.fly.dev` (default, the staging pod) and
  `-PconstructWorkspace=pixelfed_home` (default): the built-in defaults (`BuildConfig`). `-PconstructBaseUrl=http://10.0.2.2:8080` bakes in the local
  server as seen from the emulator.

Run on an emulator. The app talks to the staging pod unless told otherwise. For a local server, start it first
(`cargo run -p server`, on the host's 127.0.0.1:8080, which the emulator reaches as `10.0.2.2:8080`) and pass
`--es baseUrl http://10.0.2.2:8080`, or pick **Local emulator** in Settings' server picker:

```bash
export PATH="$HOME/Library/Android/sdk/platform-tools:$HOME/Library/Android/sdk/emulator:$PATH"
emulator -list-avds
emulator -avd Medium_Phone_API_36 -no-window -no-audio -no-boot-anim -no-snapshot-save &   # headless
adb wait-for-device
adb shell 'while [ "$(getprop sys.boot_completed)" != 1 ]; do sleep 1; done'

./gradlew :androidApp:installDebug
adb shell am start -n org.theconstruct.android/.MainActivity                                   # staging pod
adb shell am start -S -n org.theconstruct.android/.MainActivity --es workspace home           # -S restarts it
adb shell am start -S -n org.theconstruct.android/.MainActivity --es baseUrl http://10.0.2.2:8080   # local server
adb logcat -s ConstructEngine:V
adb exec-out screencap -p > screen.png
```

Intent extras `baseUrl` and `workspace` override the `BuildConfig` defaults (debug builds only; release builds
ignore them). In every build, release included, the menu's **Server** section switches between Staging, Local
emulator (`http://10.0.2.2:8080`), Local USB (`http://127.0.0.1:8080`), a custom `http(s)://host[:port]` and
"Build default" (when that is not a preset). The switch happens in place (a new engine on that server, same app,
then the old one is closed) and the choice is saved (SharedPreferences `server`, key `baseUrl`); if the new engine
can't be created, nothing changes and the Server section shows why. Precedence: the `baseUrl` extra (debug only,
and no longer once the menu has switched servers), then the saved choice, then `BuildConfig`. A server other than the build default gets its own storage dir (`filesDir/servers/<url>`), so cached
entities and pending outbox rows never cross servers. Other apps open from the menu's Apps section (with a pod that
lists them, `CONSTRUCT_SHOW_DEV_APPS=1`). `scripts/android_smoke.sh` passes `--es baseUrl http://10.0.2.2:8080` (its
`--base-url`). The extras are read when the engine is
created, so use `am start -S` (force-stop first) to switch workspace on a running app. Rotation keeps the
same engine. Logcat tag `ConstructEngine` carries both the Kotlin log lines (`starting base_url=...`,
`dispatch: ...`) and the Rust engine's stdout/stderr. The stdout/stderr redirect is process-wide, so the tag also
shows other native output from the app process (e.g. GL/EGL driver lines). The SQLite cache (`cache.db`) is in the app's
`filesDir`.

On a physical phone, use `adb reverse tcp:8080 tcp:8080` and `--es baseUrl http://127.0.0.1:8080`.

Cleartext caveat: `network_security_config.xml` allows cleartext only to `10.0.2.2`, `localhost` and
`127.0.0.1`, in all builds (the server picker ships in release, so a release build can reach a local pod; D33).
That config governs Java/Kotlin networking only (images). All engine traffic goes through Rust (`reqwest`), which
the config does not cover, so the engine will talk plain HTTP to any `baseUrl` it is given, including a custom URL
picked in the menu. Enforcing HTTPS in the engine for release builds is a follow-up.

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

Counts at the time of writing: `cargo test --workspace` 281, `:shared:allTests` 63, `:desktopApp:test` 63 (all
passing).

`assembleDebug` and the JVM host tests can't catch Android-only runtime failures (for example, Android's ICU regex
engine rejects patterns that `java.util.regex` accepts). Two checks run on a booted emulator or device:

```bash
# Launch check: installs the debug app, launches it per workspace (default pixelfed_home; --pixelfed adds
# pixelfed_demo and home), then fails on a FATAL EXCEPTION in logcat, a dead process, or a `uiautomator dump`
# missing the "Menu" button or the expected text ("Sign in with Pixelfed" or a Like button for pixelfed_home,
# "Sunrise over Boulder" for pixelfed_demo, "Count: <n>" for home). Writes <ws>.png, <ws>.xml and
# <ws>.logcat.txt to --out (default kmp-app/androidApp/build/smoke).
../scripts/android_smoke.sh --pixelfed                    # device and server already running
../scripts/android_smoke.sh --boot --server --pixelfed    # boots Medium_Phone_API_36 headless and starts the
                                                          # server, then stops both at the end
../scripts/android_smoke.sh --help                        # --workspaces, --base-url, --skip-install, --skip-rust, --wait, --timeout

# Instrumented tests (androidApp/src/androidTest): the shared interpolation regexes on the device, and the
# AndroidKeyStore session store round trip (set/get/delete, survives a new instance, encrypted at rest).
# Note: AGP uninstalls the app afterwards, so don't follow this with android_smoke.sh --skip-install.
./gradlew :androidApp:connectedDebugAndroidTest
```
