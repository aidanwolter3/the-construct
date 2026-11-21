# CI's Rust job runs in its own container image

Date: 2026-11-11. Related: issue #246, [scroll-physics](scroll-physics.md) (whose follow-up, PR #245, hit this).

The Rust job installed lavapipe (`mesa-vulkan-drivers`, which construct-render's GPU tests draw on, and must) with
`apt-get` on every run. On 2026-11-11 the runners couldn't resolve Ubuntu's mirrors for over an hour: every run
failed at that step before building anything, PR #245 merged with that check red, and main's run after it failed
too, so staging didn't deploy.

Decision: the job runs in an image, `.github/ci/Dockerfile`, with everything it installs: the pinned Rust
(`rust-toolchain.toml`), cargo-nextest, lavapipe, the simulator's fonts. A first job, `image`, tags it with a hash
of the Dockerfile and `rust-toolchain.toml` and builds and pushes it to `ghcr.io/aidanwolter3/the-construct-ci`
only when that tag isn't there, so a run normally downloads nothing from package mirrors, and a pull request that
changes either file gets its own image before its tests. The rest of the time a mirror outage can't fail CI; one
during a change to the image fails that change's run only.

- **Debian** (`debian:trixie-slim`, pinned by digest, Dependabot proposes updates): its packages come from Debian's
  servers, not the runners' Ubuntu mirrors that failed.
- **uid 1001** (the runner's user): the workspace the runner mounts is its, and tests that need a non-root user
  (a read-only cache file) still run; as root they skip.
- **The other jobs** stay on the runner: the Android build uses the runner's SDK and NDK, and `audit` and `deploy`
  install nothing from apt.

**Only main publishes it** (2026-11-21, issue #441). At first the `image` job ran on pull requests too, with
`packages: write`: a same-repo pull request runs its own copy of `ci.yml`, so it could push any image under the tag
main reads, and main's Rust job, the deploy gate, would run in it. Now:

- `image` runs only on main (pushes, the weekly run, manual runs) and is the only job with `packages: write`. It
  pushes the image as `main-<hash>` and records its digest in an Actions cache entry keyed by the hash. Main's
  cache entries can be written only by main's runs (a pull request's are scoped to its own ref), so main's Rust job
  runs in the image by the digest main recorded, not by a tag anyone with `packages: write` could move. With no
  entry (a new hash, or the entry evicted after a week unused) main builds and pushes again.
- `image-check` runs everywhere else, with `packages: read`. When `main-<hash>` exists, the Rust job runs in it.
  When the change makes a new hash, it builds the image without pushing, to show it builds, and the Rust job runs
  in the base's image: a change to the image is tested in it only after it merges (the Rust job still installs a
  changed `rust-toolchain.toml` itself). Before main-only publishing, tags had no `main-` prefix; the check still
  accepts the base's old tag until main has published its own.
- The build is pinned where it downloads: rustup-init and the cargo-nextest tarball at fixed versions, checked
  against sha256s in the Dockerfile. Debian packages still come from `apt-get` at build time, so two builds of one
  hash can differ; only main builds the image that main runs.

Anyone who can push a branch can still change `ci.yml` in it, and with it the permissions its run asks for; what
this rules out is the default workflow handing a pull request the registry main trusts.

Considered: caching the `.deb` with `actions/cache` (an empty cache still needs the mirror, and the cache expires
after a week unused); self-hosted runners on Fly Machines, with a warm build cache and bigger machines (faster, but
a public repo's fork pull requests would run on them, and the runners need upkeep). The image is a step toward the
latter: a self-hosted runner could run the same image.
