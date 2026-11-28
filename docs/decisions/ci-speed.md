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

**Main's runs don't wait for each other** (2026-11-25). The workflow's concurrency group was one per ref, so a push to
main waited for the run before it to finish (one waited 21 minutes before starting). Now only a pull request's runs
share a group, and a new push cancels the old run; every other run has a group of its own. Deploys still go one at a
time through the deploy job's own group (`deploy-staging`), and the deploy job first checks that its commit is still
main's head: if a newer commit landed, its run deploys that one instead, so a slower run of an older commit can't put
staging back. A run whose newer successor fails its checks leaves staging where it was, as a red main does anyway.

**A nextest run shares the core between its processes** (2026-11-28). nextest runs each test in a process of its
own, so `load_shared` helped `cargo test` but not CI: each server test still built the core (about 1.2 s of its
1.4 s alone), and the workspace's tests took about 655 s of test time, 163 s wall, on 4 cores. Of that build,
three quarters is the older files of the core layouts, which a pod's `@core/` names keep as their history, so
building only the newest isn't an option. `load_shared` now also keeps each core it builds as a file under the
system's temporary directory, `construct-core-cache/<NEXTEST_RUN_ID>/<hash>.json`, and a later process of the same
run reads it instead of building it. The hash covers the test binary's path and everything `load` reads by content
(not by address, which differs between processes), and the run id is new for every run, so a file never outlives
the build that wrote it: no stale core after a change to a file or to the builder's code. Outside nextest there is
no run id and nothing is read or written; a file that can't be read or written means building the core as before.
Files are written whole and renamed into place, and runs older than a day are removed when a run writes its first.
The same run in the same session: about 400 s of test time, 98 s wall; the server tests from 327 s to 138 s.

Considered: optimizing `pod-core`, `construct-elements` and `protocol` in dev builds cut the core build from about
1.6 s to 0.9 s but made every rebuild of those crates slower; a cache kept across runs would need a key for the
builder's own code, which the run id makes unnecessary.
