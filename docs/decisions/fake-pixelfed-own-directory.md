# The fake Pixelfed lives in its own top-level folder and deploys from its own workflow

Date: 2026-11-04. Related: D16, D46.

The fake Pixelfed upstream (D16) was spread over `crates/fake-pixelfed`, `fixtures/pixelfed/` (the timeline,
account and comments JSON plus `media/`, which it compiles into its binary) and `deploy/fly/fake-pixelfed.toml`,
and `ci.yml` deployed it to staging in the pod's deploy job, behind a hand-written "did its files change?" diff.
It is a test tool, not part of the product, so it read as one more product crate.

Moving it to a separate repository was considered and rejected: about 100 tests in `server`, `client-core`,
`app-pixelfed` and `e2e` start it in-process as a library and drive it through Rust methods (`add_faults`,
`requests`, grant counters, `expire_access_tokens`, `forget_apps`, ...), most of which have no HTTP admin
equivalent. A separate repository would mean a private git dependency (credentials in CI and cloud sessions) and
two PRs for every change that touches both the fake and a test that relies on it.

Decision: `fake-pixelfed/` at the repository root holds everything that is only the fake's: the crate (still a
workspace member and a dev-dependency of the crates above), `fixtures/` (what it serves and embeds), its
`Dockerfile` and `fly.toml`. `.github/workflows/fake-pixelfed.yml` runs its clippy and tests and deploys it to
`the-construct-fake-pixelfed.fly.dev` on a push to `main` that touches `fake-pixelfed/`, `Cargo.toml` or
`Cargo.lock` (GitHub's `paths` filter), or by hand. `ci.yml` deploys only the pod.

- `fixtures/pixelfed/` keeps what belongs to the real Pixelfed tooling: `recorded/` (real-instance responses,
  used by `app-pixelfed`'s tests) and `images/` (seed JPEGs for `scripts/pixelfed_dev.sh`).
- The simulator's fixture-mode images come from `fake-pixelfed/fixtures/media` (`findPixelfedMedia`).
- The fake's deploy no longer waits for the workspace tests in `ci.yml`, only its own; a push touching both the
  fake and the pod deploys them in either order. Pod tests still run against the same commit's fake.
- Both Docker builds use the repository root as context and copy `fake-pixelfed/`, since cargo needs every
  workspace member's manifest.
