# Faster CI: build the core once per process, optimize dependencies, parallel scenarios

Date: 2026-11-25. Related: [ci-container-image](ci-container-image.md), [content-addressed-construct](content-addressed-construct.md).

A pull request's CI took about 15 minutes, nearly all of it the Rust job: 8 minutes of tests, 3 of simulator
scenarios. The tests' time was mostly one thing: opening a pod builds this build's core layouts into elements
(`construct::core::load`: parse every component and layout file, expand, materialize and hash them), about 2 s in a
debug build, and a server test opens several pods (the sharing tests a dozen). 71 tests took over 10 s each.

Decision:

- **Build the core once per process.** `construct::open` uses `construct::core::load_shared`, which keeps each
  catalog's built `Core` for the life of the process. The key is everything `load` reads from the catalog: each
  file's place and its text's address (layout and component texts are `'static`, so an address is one text), and each
  source's manifest, core layouts and jacks. A server opens every pod with the same catalog, so production pods open
  faster too (a host opening pods on their first request, or the scheduler waking them). A failed build isn't kept;
  it stops the pod's start as before.
- **Optimize dependencies in dev builds** (`opt-level = 1` for `[profile.dev.package."*"]`). What's left of a test is
  serde_json, memchr, allocation and B-trees, several times slower unoptimized. Dependencies change rarely and are
  cached (rust-cache in CI, `target/` locally), so the cost is a slower first build; the workspace's own crates stay
  at opt-level 0 and build as fast as before. Optimizing `construct-elements` too gained little (1.2 s to 1.0 s for
  the core build) and was left out.
- **Run the simulator scenarios in parallel** (`scripts/sim_scenarios.sh`, one per core, `SIM_JOBS` to change it),
  the longest first, each into its own log printed in order at the end; a failed one still fails the script.

Measured in a 4-core cloud session: the workspace tests went from about 1960 s of test time (CI's last run) to 645 s,
163 s wall; the scenarios from about 3 minutes to 24 s.

Not done: splitting the Rust job across runners (nextest partitions). Worth it if the tests grow slow again; each
runner then compiles the tests too, so it pays only while the tests take much longer than their build.

**The cache key follows the root `Cargo.toml`** (2026-11-25, the same day). rust-cache keys a cache by Cargo.lock
and the members' manifests, not the root manifest that holds the `[profile]`s: after the opt-level change every run
restored the old build on a full key match, rebuilt every dependency (about 5 minutes) and saved nothing, since the key
already existed. Each workflow's rust-cache step now adds `hashFiles('Cargo.toml')` to its key.
