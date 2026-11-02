# The Construct

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

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
server (D33). The AI layout agent from M7 is no longer routed: its code stays in `server::agent`, with its tests,
so it can come back later.

Quick links: [Server runtime configuration](#server-runtime-configuration),
[Adding a new layout version](#adding-a-new-layout-version), [The menu](#the-menu-all-surfaces),
[Try it on your phone with a real Pixelfed instance](#try-it-on-your-phone-with-a-real-pixelfed-instance),
[Milestone check recipes](#milestone-check-recipes).

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord (+ JSON Schema
                     registry behind the `schema` feature)
crates/server        Axum server ("the pod"): GET /v1/workspaces/{pixelfed_home,pixelfed_demo,home}, persistent
                     counters (SQLite), Pixelfed adapter (auth, feed normalization, actions, media proxy),
                     layouts as code (one linear version history per app), read-only apps API, SSE push;
                     the layout agent module (rules / Anthropic providers) is kept but not routed
crates/client-core   ConstructEngine: fetches the workspace, holds state, applies optimistic mutations (SQLite cache +
                     outbox sync worker), OAuth2 PKCE sign-in (pod session in a secure store), read-only apps
                     API, SSE layout subscriber
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
crates/fake-pixelfed fake Pixelfed (Mastodon API + OAuth PKCE, admin/fault controls), the primary e2e upstream;
                     single binary with embedded fixtures, Dockerfile for a sidecar image
crates/e2e           end-to-end suite: pod + engine in-process against the fake (default) or a real Pixelfed
layouts/             layouts as code: <app>/NNNN.json ({"note", "layout"}), embedded in the pod, append-only
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

Terminal 1: start the server on 127.0.0.1:8080 (set `CONSTRUCT_ADDR` to use another address).

```bash
cargo run -p server
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq .
```

Server details:

- State lives in SQLite at `$CONSTRUCT_DATA_DIR/server.db`; `CONSTRUCT_DATA_DIR` defaults to `~/.construct-server`
  (created if missing). The path is logged at startup. Delete the file to start from a zero count.
- `GET /v1/workspaces/home` returns the counter fixture with `core://counters/home`.count set to the stored count.
- `POST /v1/counters/home/{increment,decrement,reset}` (optional JSON body, ignored) returns
  `{"ok":true,"count":N}`. Decrementing at 0 is refused with 422 `{"error":"count_below_zero","count":0}` and
  changes nothing. Unknown counters/ops are 404. Every mutation is logged at info with the resulting count.
- Test hook, only when started with `CONSTRUCT_TEST_HOOKS=1` (otherwise 404): `POST /v1/test/fault` with
  `{"status":503,"count":3}` makes the next 3 POSTs (other than `/v1/test/*`) return that status (400-599)
  without being applied; `"count":0` clears it. Each injected fault is logged.

Layouts, apps and the push channel (DECISIONS D32). Each app (= workspace) has one linear, append-only layout
history. Its versions come from the files `layouts/<app>/NNNN.json`, which are embedded in the binary and appended
at startup (see [Adding a new layout version](#adding-a-new-layout-version)). Entities are not versioned. The
signed-out `pixelfed_home` screen is not versioned. Apps: `pixelfed_home` "Pixelfed", plus the dev apps
`pixelfed_demo` "Pixelfed demo" and `home` "Counter". `GET /v1/apps` lists the dev apps only with
`CONSTRUCT_SHOW_DEV_APPS=1`; their other endpoints always work.

- `GET /v1/workspaces/{id}` serves the app's latest layout with `X-Construct-Layout-Version: <n>`. The header is
  absent on the signed-out `pixelfed_home` screen and on its 401.
- `GET /v1/apps` -> `{"apps":[{id, title, icon, latest_version, updated_at}]}`.
- `GET /v1/apps/{id}/versions[?limit=1..100 (default 20)&before=N]` -> `{app_id, latest_version, versions:[{version,
  created_at, source, note?}], next_before?}`, newest first. `next_before` is present iff older versions exist;
  pass it as `before`. `source` is `file` for layout files. Versions migrated from the old catalog keep `seed`,
  `agent`, `rollback`, `restore`, `manual` or `switch`, with a note.
- `GET /v1/apps/{id}/versions/{n}` -> `{app_id, version, created_at, source, note?, layout}`.
- All public (no session needed). Errors: 400 `invalid_workspace_id` (ids are 1..=64 characters of
  `[A-Za-z0-9_-]`), 404 `app_not_found` / `version_not_found`, 400 `invalid_request` (bad `limit` / `before` /
  version).
- `GET /v1/workspaces/{id}/events` (SSE): the first message is the current version, then one per new version of
  that app (`event: layout` / `data: {"version":N,"workspace_id":"home"}`), plus a `: keepalive` comment every
  15 s. New versions normally arrive with a deploy (the restart ends the stream, and the reconnect's first message
  carries the new number). No session is needed; for `pixelfed_home`, a bearer that is not a valid session gets 401
  like the workspace GET.
- Nothing changes a layout over HTTP. The agent, rollback, workspace versions and `/v1/layouts` routes are gone
  (404).

```bash
CONSTRUCT_SHOW_DEV_APPS=1 cargo run -p server
curl -s http://127.0.0.1:8080/v1/apps | jq '.apps[] | {id, latest_version}'
curl -s http://127.0.0.1:8080/v1/apps/pixelfed_home/versions | jq '.versions[] | {version, source, note}'
# -> {"version":1,"source":"file","note":"Classic feed"} on a fresh database
curl -s http://127.0.0.1:8080/v1/apps/pixelfed_home/versions/1 | jq .layout.id     # "pixelfed_home"
curl -sN http://127.0.0.1:8080/v1/workspaces/home/events    # event: layout, data: {"version":1,"workspace_id":"home"}
```

```bash
CONSTRUCT_TEST_HOOKS=1 cargo run -p server
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
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080       # or: CONSTRUCT_BASE_URL=... ./gradlew ...
./gradlew :desktopApp:run -Dconstruct.baseUrl=http://127.0.0.1:8080 -Dconstruct.workspace=home   # the counter
```

All clients default to the deployed staging pod, `https://the-construct-pod.fly.dev`, and to the Pixelfed app
(`pixelfed_home`; signed out, it shows "Sign in with Pixelfed"). For the local server started above, pass its URL
explicitly (`CONSTRUCT_BASE_URL=http://127.0.0.1:8080`) or pick **Local** in the menu's Server section. With
`-Dconstruct.workspace=home`, a "The Construct" window opens with the counter layout served by the server.
Clicking **Increment** or **Reset** prints the intent to stdout and the count updates:

```
[desktop] dispatch: {"intent":"action.mutation.optimistic",...}
[ConstructEngine] dispatch: {...}
[ConstructEngine] core://counters/home#count = 1
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
- `construct.workspace` / `CONSTRUCT_WORKSPACE`: default `pixelfed_home`. Use `home` for the counter and
  `pixelfed_demo` for the one-post demo. Other apps can also be opened from the menu.

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
including on the next launch. Each accepted row is deleted. A row the server rejects with a 4xx (e.g.
decrementing below 0) is deleted and its field rolled back, with later queued clicks re-applied on top. A 5xx,
408, 429 or network error keeps the row and retries it with exponential backoff (2 s, 4 s, ... capped at 60 s). A
re-fetched workspace has the still-pending clicks re-applied, so they never visibly disappear. The engine logs
each attempt, e.g. `[ConstructEngine] outbox #3 POST http://127.0.0.1:8080/v1/counters/home/increment -> 200,
deleted (0 pending)`.

## Server runtime configuration

Everything the pod (`cargo run -p server`) reads from the environment. Empty values count as unset. The chosen
Pixelfed instance, the database path and each app's served layout version (plus any layout file deployed as a new
version) are logged at startup. `GET /healthz` answers 200 `ok`
(no auth; the engine's 2 s reconnect probe uses it).

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_ADDR` | `127.0.0.1:8080` | Listen address. Use `0.0.0.0:8080` to accept LAN connections. |
| `CONSTRUCT_DATA_DIR` | `~/.construct-server` | Directory of `server.db` (counters, Pixelfed app/account/sessions, media keys, layout versions); created if missing. |
| `CONSTRUCT_TEST_HOOKS` | unset | `1` enables `POST /v1/test/fault` (fault injection; otherwise 404). |
| `CONSTRUCT_SHOW_DEV_APPS` | unset | `1`: `GET /v1/apps` also lists the dev apps `pixelfed_demo` and `home` (they work either way; this only makes them show up in the clients' menu). |
| `PIXELFED_INSTANCE_URL` | `http://127.0.0.1:8081` (the fake) | Pixelfed instance for pod->upstream calls, and the only instance accepted in auth calls. `http://localhost:8090` for the local real Pixelfed, `https://<instance>` for a public one. |
| `PIXELFED_PUBLIC_URL` | = instance URL | Base for browser-facing links (the authorize page), e.g. `http://10.0.2.2:8081` for the emulator while the pod calls `127.0.0.1:8081`. Also accepted in auth calls. |
| `PIXELFED_MEDIA_HOSTS` | unset | Comma-separated `host`, `host:port` or `scheme://host[:port]`: extra origins the media proxy may fetch (CDN/S3) besides the instance and public URLs. Media elsewhere become placeholders. |
| `PIXELFED_EXTRA_REDIRECTS` | unset | Comma-separated exact redirect URIs accepted by `/v1/auth/pixelfed/app`, besides `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback` and `construct://oauth`. |
| `PIXELFED_OWNER_ACCOUNT_ID` | unset | The only upstream account id allowed to sign in (replaces a pinned owner). Unset: the first account to sign in is pinned; others get 403 `account_not_owner`. |
| `PIXELFED_ALLOW_ACCOUNT_CHANGE` | unset | `1`/`true`: any account may sign in and replace the stored one (no pinning). |
| `CONSTRUCT_SESSION_TTL_DAYS` | `90` | Pod session lifetime in days (1..=36500; other values warn and use 90). |
| `RUST_LOG` | `info` | `tracing` filter, e.g. `RUST_LOG=server=debug,info`. |

The layout agent is not routed (D32), so `CONSTRUCT_AGENT_OPEN`, `CONSTRUCT_LLM_PROVIDER`, `CONSTRUCT_LLM_MODEL`,
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
| `FAKE_PIXELFED_ADMIN` | loopback only | `1` serves `/_admin` on any address, `0` never; unset, only when bound to loopback. |
| `RUST_LOG` | `info` | `tracing` filter. |

## Try it on your phone with a real Pixelfed instance

Nothing has to be registered by hand: the pod registers its own OAuth app on the instance (`POST /api/v1/apps`,
redirect `construct://oauth`) on the first sign-in. The pod keeps the upstream token; the phone only gets a pod
session (DECISIONS D17, D19).

1. Start the pod for the instance (any account on it; the first one to sign in becomes the pod's owner):

   ```bash
   PIXELFED_INSTANCE_URL=https://<instance> CONSTRUCT_DATA_DIR=~/.construct-pixelfed cargo run -p server
   # add PIXELFED_MEDIA_HOSTS=<cdn host> if the instance serves media from a CDN/S3 host (else placeholders)
   ```

2. Make the pod reachable from the phone. Simplest is USB: `adb reverse tcp:8080 tcp:8080` and base URL
   `http://localhost:8080`. A LAN address (`CONSTRUCT_ADDR=0.0.0.0:8080`, `http://<mac-ip>:8080`) or an HTTPS URL also
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

   (A release build has no extras: pick **Local USB** in the menu's Server section instead.)

4. Tap **Sign in with Pixelfed**: the browser opens the instance's authorize page; log in and approve; the
   `construct://oauth` redirect returns to the app, which shows your home timeline (the menu's Account section says
   "Signed in as @you"). Like and bookmark are applied at once and go through the outbox to the instance (queued while
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

## Adding a new layout version

The only way to change what the app looks like is a new layout file plus a deploy (DECISIONS D32):

1. Copy the newest file of the app, e.g. `layouts/pixelfed_home/0001.json` to `layouts/pixelfed_home/0002.json`
   (4 digits, the next number, no gaps), and edit it. The format is `{"note": "short changelog line", "layout":
   <UiNode>}`. The note is what the menu's version history shows. The layout root is the app's
   `core://Workspace`, and its id is the app id.
2. Add one line to `layouts::FILES` in `crates/server/src/layouts.rs`: `layout_file!("pixelfed_home", "0002"),`.
3. Run `cargo test` (`-p server` is enough). It validates every file (structure, size, slots, aspect ratios,
   endpoints) and fails if a file in `layouts/` is not listed.
4. Push to `main`. CI runs the checks and deploys the pod to staging. On start, the pod appends the file as the
   app's next version (logged as `layout file deployed as a new version`). Connected clients reconnect their SSE
   stream, see the newer version and refetch. Clients that follow the latest version show it live. A client pinned
   to an older version keeps it, and its menu says "Newer version available".

Files are **append-only**. Never edit, rename or delete a deployed file: the pod refuses to start and names the
file (e.g. "layouts/pixelfed_home/0002.json changed after it was deployed as version 7; layout files are
append-only, add 0003.json instead"). A gap in the numbering, an invalid layout or a wrong root id also stops the
start. Reformatting a file without changing its layout or note is fine. To undo a change, add a new file with the
old content.

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
- **Schema versions:** the engine sends `X-Construct-Schema: 2`. The pod serves each client the newest version of
  an app whose vocabulary it can draw, so an older app (no header = 1) keeps getting the last version without
  screens or the new nodes. Using any of the above makes a layout version need schema 2.

`layouts/pixelfed_home/0002.json` uses all of this for comments: the feed's comment button (already
`pixelfed://posts/$item.id/comments`) opens the `comments` screen, which loads
`/v1/pixelfed/screens/comments?post_id=$route.id` into a `core://List` and posts new comments with a
`core://TextField` and `action.mutation.create`.

## The menu (all surfaces)

Desktop, Android and the simulator share one Compose UI (DECISIONS D33). The screen shows only the layout, with a
small **menu button** at the top left (content description "Menu"). The button gets a badge dot when you are
offline, when actions wait to be sent, or when the version you picked has a newer one. The button opens a drawer:

- **Apps:** `Pixelfed` (plus `Pixelfed demo` and `Counter` when the pod runs with `CONSTRUCT_SHOW_DEV_APPS=1`).
  Tap a row to open that app. The history button opens the **version picker**: "Latest", then every version
  newest first ("vN", age, note), with "Load older". Picking a version shows that layout with your current data.
  This happens only on this device: nothing changes on the pod, and the next launch shows the latest again. While
  you view an older version and a newer one is published, the drawer shows "Newer version available" with
  **Show latest**. Picking "Latest" follows new versions again. When the version you picked is on an older page,
  a checked "Pinned: vN" row sits under "Latest" until "Load older" lists it.
- **Connection:** Online / Offline and "N pending" ("Connecting…" until the engine first reports; no offline badge
  then). Offline, the Apps list from the last load stays without an error, and it reloads once back online.
- **Account:** "Signed in as @you" with **Sign out**, or **Sign in**.
- **Server:** Staging, Local emulator (`http://10.0.2.2:8080`), Local USB (`http://127.0.0.1:8080`) or a custom
  URL on Android; Staging, Local (`http://127.0.0.1:8080`) or a custom `http(s)://host[:port]` on desktop. On
  Android a custom URL must be `https://` unless it is `localhost`, `127.0.0.1` or `10.0.2.2` ("Use https:// for
  remote servers"), since images can only load over cleartext from those hosts. This is in every build, release
  included. Both switch in place: the new engine is started first, and the choice is saved only once it started.
  Each server keeps its own cache and outbox.

Back (Android) closes the innermost level (custom URL field, picker, drawer). Test tags: `menu-button`,
`menu-badge`, `menu-drawer`, `menu-close`, `menu-back`, `app-<id>`, `app-history-<id>`, `app-error`, `apps-error`,
`menu-newer`, `menu-newer-latest`, `menu-connection`, `menu-account`, `menu-sign-in`, `menu-sign-out`,
`menu-server`, `server-<key>`, `server-custom`, `server-custom-url`, `server-custom-hint`, `server-custom-use`,
`version-picker`, `version-latest`, `version-pinned`, `version-<n>`, `versions-more`, `versions-error`,
`version-error`.

## Milestone check recipes

DESIGN.md M5 (offline click lands in the outbox, flushed on reconnect), with the counter workspace:

```bash
cargo run -p server                                                  # terminal 1
cd kmp-app && CONSTRUCT_BASE_URL=http://127.0.0.1:8080 CONSTRUCT_WORKSPACE=home ./gradlew :desktopApp:run   # terminal 2
# stop the server with Ctrl-C in terminal 1: the menu button gets a badge; the menu says Offline
# click Increment 3 times: the count updates at once and the menu shows "3 pending"
sqlite3 ~/.construct/cache.db "SELECT id, endpoint, status, retry_count FROM action_outbox;"   # 3 PENDING rows
cargo run -p server                                                  # terminal 1 again: within ~2 s flushed, rows deleted
sqlite3 ~/.construct/cache.db "SELECT count(*) FROM action_outbox;"     # 0
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq '.entities["core://counters/home"].count'
```

A new layout version reaches an open app without a restart (the M7 push, now driven by a deploy):

```bash
cargo run -p server                                                  # terminal 1
cd kmp-app && CONSTRUCT_BASE_URL=http://127.0.0.1:8080 CONSTRUCT_WORKSPACE=home ./gradlew :desktopApp:run   # terminal 2
# add layouts/home/0002.json + its layouts::FILES line (see "Adding a new layout version"), then restart terminal 1:
# the app reconnects its event stream and shows v2 within a few seconds; the menu's Counter history lists it
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
is Mastodon's:

- `GET /api/v1/statuses/:id/context`: `{"ancestors": [...], "descendants": [...]}` — the parent chain root first,
  and every reply below depth first (siblings oldest first); thread them by `in_reply_to_id`. On a reblog, the
  reblogged post's thread.
- `POST /api/v1/statuses` (form or JSON) with `status` and `in_reply_to_id`: adds a comment by the signed-in
  account and bumps the parent's `replies_count`. Optional `visibility`, `spoiler_text`, `sensitive`; the
  `Idempotency-Key` header replays the first result. 422 for blank text, more than 500 characters or no
  `in_reply_to_id` (top-level posts need media on Pixelfed, which the fake doesn't take); 404 for an unknown parent.
- `GET /api/v1/statuses/:id`; `DELETE /api/v1/statuses/:id` (own statuses only, else 403; returns it with `text`).
- Comments are statuses, so `favourite` / `bookmark` work on them too. `POST /_admin/reset` restores the threads.

```bash
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/statuses/781254316723458049/context \
  | jq '.descendants[] | {id, in_reply_to_id, who: .account.username, content_text}'
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

The server (`server::pixelfed`, docs/DECISIONS.md D17) adapts one Pixelfed instance: it owns the upstream
access token, normalizes the home timeline into Construct entities and serves the `pixelfed_home` workspace. The
client only holds a pod session token and sends `Authorization: Bearer <session>`.

Configuration (`PIXELFED_*`, `CONSTRUCT_SESSION_TTL_DAYS`): see
[Server runtime configuration](#server-runtime-configuration).

Endpoints:

- `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}` -> `{client_id, scopes, authorize_endpoint}`
  (registers an app upstream once per redirect_uri; a cached app is re-checked with a `client_credentials` grant
  and re-registered if the instance no longer knows it). Another instance -> 400 `instance_not_allowed`.
  `redirect_uri` must be `http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback`, `construct://oauth`
  or a `PIXELFED_EXTRA_REDIRECTS` entry, else 400 `redirect_uri_not_allowed`.
- `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}` -> `{session_token,
  account:{id, username, display_name, avatar_url}}`. Upstream rejection -> 400 `oauth_exchange_failed`. An
  account other than the pinned owner -> 403 `account_not_owner` (the stored account is untouched).
- `POST /v1/auth/logout` (bearer) -> `{ok:true}`; deletes the session (401 if it is not valid).
- `GET /v1/workspaces/pixelfed_home`: with a valid session, the home feed bundle (`queries["timeline:home"]` +
  posts/users/media entities, PostCard layout with like/bookmark optimistic mutations). Without a bearer: 200 with a
  signed-out bundle (a "Sign in with Pixelfed" `action.auth.login` button). With a bearer that no longer works
  (unknown session, or the upstream rejected the stored token): 401 with the same signed-out bundle as the body and
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
  upstream `GET /api/v1/statuses/{id}/context`. A boost's id gives the boosted post's thread. Unknown post 404
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
  another app). A custom URI (Android: `construct://oauth`): the app forwards the redirect to `handleAuthRedirect`.
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
- Signing out (or a 401) removes the cached private feed (`pixelfed://` entities, the feed workspace row) from
  `cache.db`; `logout()` also drops queued Pixelfed actions, a 401 keeps them for the next sign-in. The DB runs
  with `secure_delete=ON` and the WAL is truncated after the purge, so the rows don't linger in the file's bytes.
- UI: the menu's Account section shows "Signed in as @username" with **Sign out** (`ConstructController.logout()`),
  "Signing in…" while pending, then "Loading your feed…" from the code exchange until the first signed-in fetch
  lands. It shows "Sign-in failed: <message>" on error, otherwise "Not signed in" with **Sign in** (the screen's own
  sign-in button works too). Messages are short and human ("This pod belongs to a different Pixelfed account.",
  "Can't reach your pod.", ...); the URL/status/body detail is in the `[ConstructEngine]` log only.

Per platform (DECISIONS D21):

- **Desktop** (`desktopApp` Main.kt): no store (Keychain), loopback redirect, and the default browser via
  `java.awt.Desktop.browse` (else `open <url>`). Clicking "Sign in with Pixelfed" opens the browser; after approving,
  the browser lands on the "Signed in" page and the window shows the feed.
- **Android**: `construct://oauth` intent filter on `MainActivity` (`launchMode="singleTask"`, so the redirect returns to
  the same activity and engine through `onNewIntent`); the session is encrypted with an AndroidKeyStore AES/GCM key
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
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
  -Pscript="shot start; click Increment; click Increment; assert-text Count: 2; shot after; offline; shot offline"

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
  -Pscript="shot signed-out; click Sign in with Pixelfed; open-menu; wait-text 20000 @constructdemo; close-menu; wait-text 20000 Alice Lumen; wait-images 20000; shot feed; click Like; shot liked"
# live menu against a local pod started with CONSTRUCT_SHOW_DEV_APPS=1
./gradlew -PskipRustBuild :desktopApp:simulate -PbaseUrl=http://127.0.0.1:8080 \
  -Pscript="open-menu; open-history Pixelfed; select-version 1; open-menu; open-app Counter; assert-text Count"
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
locally and queued (the menu shows "N pending"), and `online` flushes them FIFO to an in-memory fake
server. That server rejects a `.../decrement` that would take the count below 0; the fake then rolls the
field back and re-applies later queued rows (D8). It also serves a fake apps catalog (`FakeAppCatalog`, as with
`CONSTRUCT_SHOW_DEV_APPS=1`): Pixelfed v1..v3, Pixelfed demo v1..v2, Counter v1..v2. The newest version of each is
the fixture's own layout, and `publish-version` adds more. Opening another app switches to that app's fixture.
**Live mode** runs the real Rust engine through JNA against
the server. By default it uses a fresh temp storage dir, so it neither reads nor writes `~/.construct`; pass
`-Pstorage=<dir>` to keep a persistent one. If the workspace
does not load within `timeoutMs`, it logs a warning and still runs the script (shots show Loading/Offline).
`offline`/`online` are not supported in live mode; a live-mode script containing them is rejected before any
step runs. **Sign-in in live mode:** the engine gets an in-memory session store (every run starts signed out; the
Keychain is never touched) and a "browser" that GETs the authorize URL with `auto=1`; when the answer is a redirect to
the engine's loopback `redirect_uri` (the fake upstream does that, with or without `FAKE_PIXELFED_AUTO_APPROVE`), it
follows it, so `click Sign in with Pixelfed; open-menu; wait-text 20000 @constructdemo` signs in (the account is shown in the menu). A real Pixelfed answers with its
login page instead: the simulator prints the authorize URL and the menu's Account section shows "no browser available"
(opening the URL in a real browser and logging in still completes the sign-in while the script waits). For a real
instance pass `-PpixelfedLogin=<login>:<password>`: the "browser" then does what a person would (`PixelfedLoginBrowser`,
own cookie jar): `GET /login` for the CSRF `_token`, `POST /login` (Pixelfed logs in by e-mail; a login without `@` is
sent as `<login>@example.test`, the dev seed's domain), `GET` the authorize URL, `POST` the consent page's approve
form (`_token`, `state`, `client_id`, `auth_token`; the deny form is the one with `_method=DELETE`) and follows the
302 to the loopback listener. Failures (wrong password, 429 throttling) are logged with the URL. After the user
appears (`open-menu; wait-text 30000 @constructadmin; close-menu`), wait for feed text too (e.g. `wait-text 20000 Alice
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
| `wait-text <ms> <text>` | Like `assert-text` with a custom timeout (at most 600000), e.g. `wait-text 15000 Count: 3` while a live sync worker retries. |
| `wait-images [ms]` | Waits until no image is loading (default 10000 ms, at most 600000). Works in both modes. |
| `swipe-left <target>` / `swipe-right <target>` | Touch-swipes the selected node; `swipe-left` on a carousel shows the next page. |
| `scroll-to <target>` | Scrolls the feed until the selected node is visible (e.g. `scroll-to Kept my distance`). |
| `type <target> <text>` | Replaces the text of the selected text field with `<text>` (clear, then type), e.g. `type tag:server-custom-url http://127.0.0.1:8080`. The target is `tag:<testTag>`, a single word, or `"quoted text"` (may contain spaces); the text is the rest of the command. |
| `offline` / `online` | Fixture mode only: flips the engine's connectivity. |
| `publish-version <note>` | Fixture mode only: the fake pod publishes a new newest version of the app on screen (its layout with a "What's new: <note>" banner); followers see it at once. |
| `screen <WxH or preset>` | Restarts the renderer at the new size. Engine state carries over; UI state such as scroll position does not. |
| `back` | A system Back press (closes the innermost menu level); fails if nothing handles it. |
| `open-menu` | Opens the drawer (clicks `menu-button`) and waits for the apps list (or its error). |
| `close-menu` | Closes the drawer (leaving the version picker first). |
| `open-app <id or title>` | Opens an app from the drawer (`pixelfed_home` or `Pixelfed`) and waits until it has loaded. |
| `open-history <id or title>` | Opens that app's version picker (opening the drawer first if needed). |
| `select-version <n or latest>` | In the picker, shows version `n` (`2` or `v2`; presses "Load older" as needed) or follows the latest again; waits for the drawer to close. |
| `set-server <preset or URL>` | Picks `staging`, `emulator`, `usb` or a custom `http(s)://host[:port]` in the Server section. The simulator records the choice and shows it in the menu, but keeps its engine. |

The menu commands are thin wrappers over clicks on the menu's test tags (`click tag:app-history-home` works too,
see [The menu](#the-menu-all-surfaces)). Each one waits until idle and until every apps call has finished (they run
on a background thread), and fails with a hint when its control is missing (e.g. `select-version 2` without
`open-history`: "no version picker on screen (use open-history first)"). Examples (fixture mode). The menu commands also
work in live mode; start the pod with `CONSTRUCT_SHOW_DEV_APPS=1` to list the dev apps. `publish-version`,
`offline` and `online` are fixture-only.

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
open-menu; open-app Counter; click Increment; assert-text Count: 1; shot counter
open-menu; set-server http://127.0.0.1:8080; assert-text 127.0.0.1; close-menu
SCRIPT
./gradlew -PskipRustBuild :desktopApp:simulate -PscriptFile=/tmp/menu.txt
./gradlew -PskipRustBuild :desktopApp:simulate -Pscreen=desktop -PscriptFile=/tmp/menu.txt
# offline: the badge and the Connection section
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home \
  -Pscript="offline; click Increment; open-menu; assert-text Offline; assert-text 1 pending; shot offline"
```

A `<target>` is either `tag:<testTag>` (an exact test tag, else the first tag containing it) or text: an exact text
match, then an exact content description (clickable nodes preferred for both), then the same as substrings; the
first match wins. Pixelfed widgets carry content descriptions `Like`/`Unlike`, `Bookmark`/`Remove bookmark`,
`Comment`, the image alt text, and `Page n of N` on the carousel dots (plus an `n/N` badge text), and test tags
`carousel:<post uri>`, `carousel-dots:<post uri>`, `like:<post uri>`, `bookmark:<post uri>`, `comment:<post uri>`,
e.g. `click tag:like:pixelfed://posts/781254316723458049` or `swipe-left tag:carousel:` (the first carousel).
The menu's test tags are listed in [The menu](#the-menu-all-surfaces).

**Screens (D34).** Screens carry the tags `screen:<id>`, `screen-back`, `screen-loading`, `screen-error` and
`screen-retry`; generic nodes carry `textfield:<field>`, `textfield-send:<field>` and `list-empty`. In fixture
mode the fake engine answers data paths from `fixtures/pixelfed_screen_data.json`, and sends creates through its
fake outbox (`offline` keeps them "Sending…" until `online`). That file and the feed fixture's layout are generated
from the pod: `UPDATE_FIXTURES=1 cargo test -p server --test fixtures` (the test fails when they are stale).

```bash
./gradlew -Dorg.gradle.java.home="$JAVA_HOME" -PskipRustBuild :desktopApp:simulate -Pscreen=phone -Pout=/tmp/sim \
  -Pscript='click tag:comment:pixelfed://posts/781254316723458049; wait-text 3000 Which lens?; shot comments;
    type tag:textfield:comment Golden light!; click tag:textfield-send:comment; wait-text 3000 just now; shot sent'
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
`--es baseUrl http://10.0.2.2:8080`, or pick **Local emulator** in the menu's Server section:

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
