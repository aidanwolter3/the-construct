# Data files live next to the code that reads them

Date: 2026-11-04. Related: D8, D20, D34, [fake-pixelfed-own-directory](fake-pixelfed-own-directory.md),
[recipes-start-empty](recipes-start-empty.md).

The root `fixtures/` folder mixed one file the pod ships (`pixelfed_home_bundle.json`, the `pixelfed_demo`
app's data, embedded in the server binary) with simulator content, engine test data, recorded Pixelfed responses
and seed images for a dev script. Nothing about a file's place said whether it was test data, and the pod's
Dockerfile copied the whole folder for the one shipped file. `crates/app-recipes/data/test-seed.json` had the same
problem: test-only recipes compiled into the pod through a public function. A renamed root folder (`testdata/`)
was considered, but almost every file has one reader, and a shared folder hides who that is.

Decision: each data file lives with the code that reads it, so its place says whether it ships.

| Data | Where | Ships |
|---|---|---|
| The `pixelfed_demo` app's posts | `crates/app-pixelfed/demo/bundle.json` | yes |
| Real Pixelfed responses (D20) | `crates/app-pixelfed/tests/data/recorded/` | no |
| The minimal counter bundle (engine, protocol and Kotlin engine tests) | `crates/protocol/tests/data/` | no |
| Made-up recipes for tests | `crates/app-recipes/tests/data/test-seed.json` | no |
| Simulator content: feed and events bundles, screen data, the Kitchen app, notifications | `kmp-app/desktopApp/src/sim/resources/` | no |
| Seed images for `scripts/pixelfed_dev.sh` | `scripts/pixelfed_dev/images/` | no |

- Test and dev data sit under a `tests/` folder, the simulator's `sim` source set (which is already off the
  shipping app's classpath), `scripts/` or `fake-pixelfed/`. Anything else is built into the pod or the apps.
- Files read by more than one crate stay with their most natural owner and the others point there: the counter
  bundle is a protocol sample, and the simulator reads the demo bundle from the adapter crate. The simulator's
  `fixturePath` (`Simulator.kt`) is the one place that maps fixture names to paths.
- Generated simulator files are still written by Rust tests (`UPDATE_FIXTURES=1`), now into
  `kmp-app/desktopApp/src/sim/resources/`.
- Test-only code that other crates' tests call is behind a feature that only dev-dependencies enable
  (`app-recipes`'s `test-seed`, which gates `db::seed_test_recipes`), so the pod binary doesn't hold the data.
- `scripts/check_test_data.sh` (CI) fails when Rust code under a crate's `src/` embeds a file from a `tests/`
  folder outside a `#[cfg(test)]` module or a `#[cfg(test)]` / `#[cfg(any(test, …))]` attribute. It is a line
  heuristic, not a parser: it treats everything after a `#[cfg(test)] mod` as test code.

Older decisions name the old `fixtures/…` paths; this table is where those files are now.
