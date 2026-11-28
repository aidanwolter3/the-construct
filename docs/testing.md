# Testing

```bash
cargo test --workspace                       # Rust, including the end-to-end suite against an in-process fake Pixelfed
scripts/sim_scenarios.sh /tmp/sim            # the simulator's scenarios: UI checks, PNGs to look at
scripts/android_native.sh                    # Android build check (needs an Android SDK and NDK; installs only if a
                                             # device is connected)
```

CI's Rust job runs in a container image, `.github/ci/Dockerfile` (Debian, as uid 1001), that has everything it
needs (the pinned Rust, cargo-nextest, lavapipe, the simulator's fonts), so a run installs nothing from package
mirrors. Only main's CI runs build and push it to `ghcr.io/aidanwolter3/the-construct-ci`, when that file or
`rust-toolchain.toml` changes, and run it by the digest they recorded (decision `ci-container-image`); a pull request
that changes it builds it without pushing, and its tests run in main's image until it merges. To try a change
locally, build it from a directory holding only `rust-toolchain.toml`. CI runs `scripts/check_decisions.sh`, `scripts/check_test_data.sh` (no test data in the pod binary, decision
`data-next-to-its-code`), clippy with warnings denied, the Rust tests (with
[cargo-nextest](https://nexte.st), which runs tests from different binaries in parallel, plus `cargo test --doc`,
which nextest skips), a dependency advisory and sources check (`cargo-deny`; a failure also blocks the staging deploy), the simulator's scenarios
(`scripts/sim_scenarios.sh`, screenshots uploaded as the `simulator` artifact), and a build of the Android APK
(`scripts/android_native.sh`, uploaded as the `android-apk` artifact), and the web client's build and browser
end-to-end tests (`scripts/web_e2e.sh` in Chromium; on a failure its screenshots and logs are uploaded as the `web-e2e`
artifact). The Android and web builds run with
`RUSTFLAGS=-D warnings`, so a warning only those targets see (code behind a `target_os` or `target_arch` the Linux
clippy skips) fails CI too; local builds and the release workflow leave warnings as warnings. The release workflow (`release.yml`) also
builds the release APK and the Mac app on pull requests that change their build scripts.

A crate's integration tests are one test binary, `tests/integration/main.rs`, with one module per file: add a test
file as a `mod` there, not as a new `tests/*.rs` (linking one binary per file was most of CI's build time). Pick a
file's tests with a filter, e.g. `cargo test -p server --test integration fixtures::`. The tests of a crate share a
process under `cargo test`, so don't assume process-wide state (such as the server's metrics counters) starts empty.

Keep the tests fast (decision `ci-speed`): most of a server test's time used to be opening pods, each building this
build's core layouts again. A process now builds them once (`construct::core::load_shared`) and dev builds optimize
dependencies (`Cargo.toml`), so a test that opens several pods costs about one. nextest runs each test in a process
of its own, so the processes of one nextest run also share them through a file: the first test of a binary to open
a pod writes the built core under the system's temporary directory (`construct-core-cache/<NEXTEST_RUN_ID>/`, about
2 MB per test binary, runs older than a day removed), and the run's later tests read it instead of building it.
`cargo test` and servers never write it. A new test that is slow on its own (nextest reports it as `SLOW` past 60 s)
is worth a look before it merges.

Test what a change touches while working on it (`cargo nextest run -p <crate>`, or a filter) and the whole workspace
once before pushing. Measured in a 4-core cloud session (incremental compilation off, as `scripts/cloud_setup.sh`
sets it): a cold build of the tests takes about 4.5 minutes; after it, touching a crate few others use
(`source-dates`) rebuilds the tests in about 12 s, `pod-core` in about 65 s and `protocol`, which most crates use, in
about 85 s; the whole suite then takes about 100 s under nextest and about 110 s under `cargo test` (which runs one
test binary at a time, so its slowest binaries set its time). `cargo test` also stops at the first failing binary
unless given `--no-fail-fast`.

The Rust rendering engine (`crates/construct-render`) has golden frames: CPU frames are compared with
`tests/data/goldens/`, and GPU frames with the same files when a wgpu adapter exists (CI's image has lavapipe,
`mesa-vulkan-drivers`, and CI sets `CONSTRUCT_REQUIRE_GPU=1`). See [rendering.md](rendering.md#tests).

Rendered fillers' tests (`web-extract`'s and `source-collections`' `render::`, `source-taste`'s
`fit::a_store_that_refuses_a_plain_fetch_is_read_in_the_browser`, decision `rendered-fetches`) drive a real headless
Chromium against local pages. They use the browser `CONSTRUCT_BROWSER` names, else the first of `chromium`,
`chromium-browser`, `headless_shell`, `chrome-headless-shell` and `google-chrome` on `PATH` (CI's image has Debian's
`chromium`, cloud sessions a `google-chrome` wrapper around Playwright's), and skip without one
(`CONSTRUCT_BROWSER=off` skips them too; on Ubuntu, `npx playwright install chromium` downloads one without the snap).
Run as root, a server that finds a browser loads the browser's network fence (decision `browser-network-fence`). That
nftables rule belongs to the machine, not the process, and stays after the server exits: later browsers that aren't that
server's then fail with `ERR_PROXY_CONNECTION_FAILED`, and the render's error names the leftover table. Tests that
start the `server` binary set `CONSTRUCT_BROWSER=off` so they don't leave it behind; if one is left over, remove it with
`nft delete table inet construct_browser`.

There are no on-device tests: UI is checked with the [headless simulator](simulator.md), which draws with the same
renderer as the apps. The Android build checks that every Java class the Rust code names in a JNI signature exists;
other Android-only runtime failures (JNI calls, permissions, push with the app closed) show only when the app runs
on a phone, and the macOS window only on a Mac.

## End-to-end suite

`crates/e2e` runs the real pod and the real engine in-process against a Pixelfed upstream. The engine reaches the pod
through a TCP proxy the tests can switch off (offline). The same suite (`tests/integration/suite.rs`) runs against either
upstream: sign-in with PKCE and a scripted browser, the feed, a like, an offline like flushed on reconnect, a bookmark,
sign-out.

```bash
cargo test -p e2e                       # the in-process fake (part of cargo test --workspace)
scripts/e2e.sh                          # against the fake-pixelfed binary on a free port
scripts/e2e.sh --faults                 # plus the fault tests
scripts/e2e.sh --upstream real          # against a real Pixelfed (default http://localhost:8090, user alice)
```

`scripts/e2e.sh` runs its tests one at a time: they share one upstream, and each resets it and signs in.

- **Fake** (`E2E_UPSTREAM=fake`, default): `E2E_FAKE_URL=<url>` uses an already running fake and resets it first.
- **Real** (`E2E_UPSTREAM=real`): `PIXELFED_INSTANCE_URL`, `E2E_PIXELFED_LOGIN` / `E2E_PIXELFED_PASSWORD` (default
  `alice` / `alice-dev-pass`) and `E2E_PIXELFED_TOKEN` to check upstream state.
- **Fault tests** (`tests/integration/upstream_faults.rs`): upstream 503 → pod 502 → client retries; 429 → retried, never rolled
  back; an expired upstream token → the pod refreshes transparently; latency past the 10 s timeouts → retried; a
  reset with and without `keep_auth`.

## Fake Pixelfed

`fake-pixelfed/` is a Mastodon-compatible stand-in for a Pixelfed instance and the main end-to-end upstream (D16): no
account, no containers, deterministic fixtures, controllable faults. It is a test tool, not part of the product, so
it lives outside `crates/` with everything it needs (decision
[`fake-pixelfed-own-directory`](decisions/fake-pixelfed-own-directory.md)). The pod's tests link it as a library.

```bash
FAKE_PIXELFED_TOKEN=dev cargo run -p fake-pixelfed      # 127.0.0.1:8081
curl -s -H 'Authorization: Bearer dev' http://127.0.0.1:8081/api/v1/timelines/home | jq '.[].id'
```

| Variable | Default | Meaning |
|---|---|---|
| `FAKE_PIXELFED_ADDR` | `127.0.0.1:8081` | Listen address; with `PORT` set and this unset, `0.0.0.0:$PORT`. |
| `FAKE_PIXELFED_PUBLIC_URL` | `http://<bound addr>` | Replaces `{{BASE}}` in the fixtures, so media URLs point at it (`http://10.0.2.2:8081` for the emulator). |
| `FAKE_PIXELFED_AUTO_APPROVE` | unset | `1`: the authorize page redirects back at once (same as `auto=1`). |
| `FAKE_PIXELFED_TOKEN` | unset | A pre-seeded valid access token. |
| `FAKE_PIXELFED_FIXTURES` | embedded | A fixture directory to load instead (e.g. `fake-pixelfed/fixtures` while editing). |
| `FAKE_PIXELFED_THREADS` | `pixelfed` | `mastodon`: `/context` returns whole threads instead of one level each way. |
| `FAKE_PIXELFED_ADMIN` | loopback only | `1` serves `/_admin` on any address, `0` never. |
| `FAKE_PIXELFED_MAX_UPLOAD_MIB` | `20` | Largest media upload (413 past it); staging uses `4`. |

**What it serves.** OAuth app registration, authorization code with PKCE (`S256` only, single-use codes), tokens with
refresh, `verify_credentials`, the home timeline with paging (five posts: a carousel, a boost, a landscape image, a
text-only post and a content-warning post), favourite and bookmark, statuses and threaded comments
(`fixtures/comments.json`), notifications, and media under `/storage/<file>`. Media are small PNGs with a digit or
letter, so screenshots show which image is visible.

**Uploads** (`POST /api/v{1,2}/media`, then posts with `media_ids`) are kept in memory and served at
`/storage/uploads/<id>.<ext>`: at most 200 of them and 64 MiB in all, the oldest dropped first (a post whose photo
was dropped shows a 404 image). Only JPEG, PNG and WebP are taken, and every response carries
`X-Content-Type-Options: nosniff`, so the staging fake can't be used to serve pages or scripts.

**Admin endpoints** (test controls; in-memory state resets on restart):

- `POST /_admin/reset[?keep_auth=1]`: favourites, bookmarks, comments and counts back to the fixtures; faults,
  request log and grants cleared; apps and tokens dropped unless `keep_auth=1`.
- `GET /_admin/state[?tail=50]`: counts, created comment ids, grants, favourited and bookmarked ids, active faults and
  the last requests.
- `POST /_admin/faults {"rules": [...]}` appends fault rules; `DELETE /_admin/faults` clears them.
- `POST /_admin/notifications` injects a notification.
- In-process only: `set_reply_counts_deferred(true)` leaves a parent's `replies_count` alone on a new comment until
  `run_reply_count_jobs()`, like Pixelfed's queued job; `/context` lists no replies while the count is 0.

```bash
curl -s -X POST http://127.0.0.1:8081/_admin/faults -H 'content-type: application/json' -d '{"rules":[
  {"path_prefix":"/api/v1/statuses","kind":"status","status":503,"times":2},
  {"kind":"rate_limit","retry_after":2,"times":1},
  {"path_prefix":"/api/v1/timelines","method":"GET","kind":"latency","ms":1500,"times":3},
  {"kind":"expire_tokens"}]}'
```

A rule matches requests whose path starts with `path_prefix` (default: every non-admin path) and, if given, whose
`method` matches, for the next `times` matches. Kinds: `status` (401 is shaped like an expired token, 429 adds
`Retry-After`), `rate_limit`, `latency` (delay, then handle normally) and `expire_tokens` (every access token stops
working at once; refresh tokens stay valid). A single request can carry `X-Fake-Fault: 503` or `latency=500,503`.

**Docker and staging.** `docker build -f fake-pixelfed/Dockerfile -t construct-fake-pixelfed .` builds a ~50 MB
image. Staging's fake runs at `https://the-construct-fake-pixelfed.fly.dev` (`fake-pixelfed/fly.toml`), deployed by
`.github/workflows/fake-pixelfed.yml` when `fake-pixelfed/` or `Cargo.*` change.

## A local real Pixelfed (optional)

For re-recording `crates/source-pixelfed/tests/data/recorded/` and as a contract test, `dev/pixelfed/` runs a real Pixelfed (v0.15.2,
digest-pinned) with MariaDB and Redis in Docker (D20). It is loopback-only (`127.0.0.1:8090`), federation and
registration are off, and every secret in `dev/pixelfed/pixelfed.env` is a committed dev-only dummy.

```bash
scripts/pixelfed_dev.sh up       # start the container runtime if needed, compose up, first-run init
scripts/pixelfed_dev.sh seed     # users and posts; tokens in dev/pixelfed/.seed/ (gitignored)
scripts/pixelfed_dev.sh record   # re-record the home timelines in crates/source-pixelfed/tests/data/recorded/
scripts/pixelfed_dev.sh status   # | logs [svc] | down (keeps data) | reset (deletes all data)
```

Seeded users: `constructadmin` / `construct-dev-pass` (admin), `alice` / `alice-dev-pass`, `bob` / `bob-dev-pass`;
Pixelfed logs in by e-mail (`alice@example.test`). Point a pod at it with
`PIXELFED_INSTANCE_URL=http://localhost:8090` (use `localhost`: Pixelfed's web routes answer only for that host). The
script expects [Colima](https://github.com/abiosoft/colima) and the docker CLI with the compose plugin on `PATH`; on
macOS they can be installed as release binaries in `~/.local/bin` without Homebrew.

Ways the real instance differs from the fake, all handled by the source: OAuth routes are throttled to 10 requests a
minute; the home timeline hides boosts unless `include_reblogs=true` and hides text-only posts; JSON shapes differ in
small ways (covered by `test_pixelfed_recorded_timeline_normalization`); an unfavourite's count can lag; and Passport
rejects a token whose user id equals the OAuth client's id, so the dev stack starts client ids at 1000.

## Newcomer test

Does a new user, with an empty pod and their own agent, get a working screen without anyone landing code, and how
much does their agent rebuild of what the server already ships? (decision `newcomer-journeys`)

```bash
scripts/newcomer.sh /tmp/newcomer                     # every persona, against staging
scripts/newcomer.sh /tmp/newcomer --only groceries    # one (a persona that starts from another brings it along)
scripts/newcomer.sh /tmp/newcomer --base-url http://127.0.0.1:8080   # a local server started with CONSTRUCT_TEMP_PODS=1
```

It needs Claude Code (`claude`, signed in) on the path. For each persona in `crates/newcomer/personas/`, it makes a
temporary pod and runs Claude Code headless in an empty directory with only that pod's MCP tools (no shell, files
or web). A simulated user sends the persona's wishes in order and answers the agent's questions from the persona's
"If asked". Then it checks the pod, never the agent:

- the persona's `sim:` script, live on the pod at phone size (`construct-sim --base-url`);
- every name on its `uses:` line (what the server already ships for the ask) is referenced, not copied or rebuilt;
- home reaches every layout the agent made;
- a reviewer reads the screenshots and the final layouts and answers each `judge:` line yes or no.

Results are `pass`, `fail`, `flagged` (the reviewer said no; a fail when it happens two days running), `stuck`,
`broken` (the server or the model failed) and `skipped` (a `fresh` persona: its wishes need the model or the web,
which temporary pods lock). Time to working, refused calls, writes the pod's reuse hints flagged and the time they
took, hints left on the final layouts and `note_gap` entries are recorded, never failed on.

It writes `report.md`, `report.json` and a folder per persona: `transcript.jsonl` (the agent's stream),
`shots/`, `layouts.json` and `review.json`. Pod tokens stay in memory, and the pods are deleted when it ends.
Staging makes at most 5 temporary pods per client address an hour, so keep a run to four personas with their own
pods. A Claude scheduled task runs it daily in a fresh cloud session (which clones `main` read-only) and adds a row
per persona to the owner's "Newcomer runs" collection.

A persona file:

```markdown
---
id: groceries
tier: temporary          # temporary | fresh
max_turns: 6             # user messages before it counts as stuck
max_budget_usd: 5        # the agent's spending cap for the persona
---
## Who I am
## What I say
1. The first message, in the person's own words; never a tool, layout or collection name.
2. The next, sent when the agent is done with the first.
## If asked
- Facts the simulated user answers from.
## Done when
sim: shot home; click Groceries; type title Oat milk; click tag:send:title; wait-text 10000 Oat milk
uses: @core/checklist
judge:
- A yes-or-no question about the screenshots.
```

`starts_from: <id>` runs a persona on another's pod after it, as a later visit.

## Manual checks

The offline outbox, end to end on the desktop app (the state directory is `~/Library/Application Support/The
Construct` on macOS, `~/.config/the-construct` on Linux):

```bash
cargo run -p server -- serve --pod-code cpod_localtest123                               # terminal 1
CONSTRUCT_POD_URL=http://127.0.0.1:8080 cargo run --release -p construct-render --features viewer --bin construct-viewer
# open the pod with cpod_localtest123 and have a layout with a feed on home (ask the agent, or the MCP tools)
# stop the server: Settings says Offline
# click the heart and the bookmark: both change at once and Settings shows "Offline · 2 pending"
sqlite3 "$HOME/.config/the-construct"/engine/*/cache.db "SELECT id, endpoint, status FROM action_outbox;"
# start the server again: within a few seconds the rows are sent and deleted
```

A new layout version reaching an open app: with the app open on a local server, add the next
`crates/source-pixelfed/layouts/pixelfed_sample_no_login/NNNN.json` and restart the server. The app reconnects its event
stream and shows the new version within seconds.
