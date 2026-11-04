# Testing

```bash
cargo test --workspace                       # Rust, including the end-to-end suite against an in-process fake Pixelfed
cd app && ./gradlew -PskipRustBuild :shared:allTests :desktopApp:test   # shared UI and desktop (needs the engine library)
./gradlew :androidApp:assembleDebug          # Android build check (launches nothing)
```

CI runs `scripts/check_decisions.sh`, `scripts/check_test_data.sh` (no test data in the pod binary, decision
`data-next-to-its-code`), clippy with warnings denied, the Rust tests (with
[cargo-nextest](https://nexte.st), which runs tests from different binaries in parallel, plus `cargo test --doc`,
which nextest skips), a dependency advisory check (`cargo-deny`), the JVM tests, and a simulator run whose
screenshots are uploaded as an artifact.

A crate's integration tests are one test binary, `tests/integration/main.rs`, with one module per file: add a test
file as a `mod` there, not as a new `tests/*.rs` (linking one binary per file was most of CI's build time). Pick a
file's tests with a filter, e.g. `cargo test -p server --test integration fixtures::`. The tests of a crate share a
process under `cargo test`, so don't assume process-wide state (such as the server's metrics counters) starts empty.

There are no on-device tests: UI is checked with the [headless simulator](simulator.md). Android's ICU regex engine
rejects some patterns that `java.util.regex` accepts, so `AndroidRegexSyntaxTest` checks every `Regex` in the shared
code on the JVM. Other Android-only runtime failures show only when the app runs on a phone.

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

**What it serves.** OAuth app registration, authorization code with PKCE (`S256` only, single-use codes), tokens with
refresh, `verify_credentials`, the home timeline with paging (five posts: a carousel, a boost, a landscape image, a
text-only post and a content-warning post), favourite and bookmark, statuses and threaded comments
(`fixtures/comments.json`), notifications, and media under `/storage/<file>`. Media are small PNGs with a digit or
letter, so screenshots show which image is visible.

**Admin endpoints** (test controls; in-memory state resets on restart):

- `POST /_admin/reset[?keep_auth=1]`: favourites, bookmarks, comments and counts back to the fixtures; faults,
  request log and grants cleared; apps and tokens dropped unless `keep_auth=1`.
- `GET /_admin/state[?tail=50]`: counts, created comment ids, grants, favourited and bookmarked ids, active faults and
  the last requests.
- `POST /_admin/faults {"rules": [...]}` appends fault rules; `DELETE /_admin/faults` clears them.
- `POST /_admin/notifications` injects a notification.

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

For re-recording `crates/app-pixelfed/tests/data/recorded/` and as a contract test, `dev/pixelfed/` runs a real Pixelfed (v0.15.2,
digest-pinned) with MariaDB and Redis in Docker (D20). It is loopback-only (`127.0.0.1:8090`), federation and
registration are off, and every secret in `dev/pixelfed/pixelfed.env` is a committed dev-only dummy.

```bash
scripts/pixelfed_dev.sh up       # start the container runtime if needed, compose up, first-run init
scripts/pixelfed_dev.sh seed     # users and posts; tokens in dev/pixelfed/.seed/ (gitignored)
scripts/pixelfed_dev.sh record   # re-record the home timelines in crates/app-pixelfed/tests/data/recorded/
scripts/pixelfed_dev.sh status   # | logs [svc] | down (keeps data) | reset (deletes all data)
```

Seeded users: `constructadmin` / `construct-dev-pass` (admin), `alice` / `alice-dev-pass`, `bob` / `bob-dev-pass`;
Pixelfed logs in by e-mail (`alice@example.test`). Point a pod at it with
`PIXELFED_INSTANCE_URL=http://localhost:8090` (use `localhost`: Pixelfed's web routes answer only for that host). The
script expects [Colima](https://github.com/abiosoft/colima) and the docker CLI with the compose plugin on `PATH`; on
macOS they can be installed as release binaries in `~/.local/bin` without Homebrew.

Ways the real instance differs from the fake, all handled by the adapter: OAuth routes are throttled to 10 requests a
minute; the home timeline hides boosts unless `include_reblogs=true` and hides text-only posts; JSON shapes differ in
small ways (covered by `test_pixelfed_recorded_timeline_normalization`); an unfavourite's count can lag; and Passport
rejects a token whose user id equals the OAuth client's id, so the dev stack starts client ids at 1000.

## Manual checks

The offline outbox, end to end on the desktop app:

```bash
cargo run -p server                                                    # terminal 1
cd app && CONSTRUCT_BASE_URL=http://127.0.0.1:8080 CONSTRUCT_WORKSPACE=pixelfed_sample_no_login ./gradlew :desktopApp:run
# stop the server: the menu button gets a badge and the menu says Offline
# click the heart and the bookmark: both change at once and the menu shows "2 pending"
sqlite3 ~/.construct/servers/*/cache.db "SELECT id, endpoint, status FROM action_outbox;"
# start the server again: within a few seconds the rows are sent and deleted
```

A new layout version reaching an open app: with the app open on a local server, add the next
`crates/app-pixelfed/layouts/pixelfed_sample_no_login/NNNN.json` and restart the server. The app reconnects its event
stream and shows the new version within seconds.
