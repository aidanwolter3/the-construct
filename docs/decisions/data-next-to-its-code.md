# Data files live next to the code that reads them

Date: 2026-11-04. Related: D8, D20, D34, [fake-pixelfed-own-directory](fake-pixelfed-own-directory.md),
[recipes-start-empty](recipes-start-empty.md).

The root `fixtures/` folder mixed one file the pod ships (`pixelfed_sample_bundle.json`, the `pixelfed_sample_no_login`
app's data, embedded in the server binary) with simulator content, engine test data, recorded Pixelfed responses
and seed images for a dev script. Nothing about a file's place said whether it was test data, and the pod's
Dockerfile copied the whole folder for the one shipped file. `crates/app-recipes/data/test-seed.json` had the same
problem: test-only recipes compiled into the pod through a public function. A renamed root folder (`testdata/`)
was considered, but almost every file has one reader, and a shared folder hides who that is.

Decision: each data file lives with the code that reads it, so its place says whether it ships.

| Data | Where | Ships |
|---|---|---|
| The `pixelfed_sample_no_login` app's posts | `crates/app-pixelfed/sample/bundle.json` | yes |
| Real Pixelfed responses (D20) | `crates/app-pixelfed/tests/data/recorded/` | no |
| The minimal counter bundle (engine, protocol and Kotlin engine tests) | `crates/protocol/tests/data/` | no |
| Made-up recipes for tests | `crates/app-recipes/tests/data/test-seed.json` | no |
| Simulator content: feed and events bundles, screen data, the Kitchen app, notifications | `app/desktopApp/src/sim/resources/` | no |
| Seed images for `scripts/pixelfed_dev.sh` | `scripts/pixelfed_dev/images/` | no |

- Test and dev data sit under a `tests/` folder, the simulator's `sim` source set (which is already off the
  shipping app's classpath), `scripts/` or `fake-pixelfed/`. Anything else is built into the pod or the apps.
- Files read by more than one crate stay with their most natural owner and the others point there: the counter
  bundle is a protocol sample, and the simulator reads the demo bundle from the adapter crate. The simulator's
  `fixturePath` (`Simulator.kt`) is the one place that maps fixture names to paths.
- Generated simulator files are still written by Rust tests (`UPDATE_FIXTURES=1`), now into
  `app/desktopApp/src/sim/resources/`.
- Test-only code that other crates' tests call is behind a feature that only dev-dependencies enable
  (`app-recipes`'s `test-seed`, which gates `db::seed_test_recipes`), so the pod binary doesn't hold the data.
- `scripts/check_test_data.sh` (CI) fails when Rust code under a crate's `src/` embeds a file from a `tests/`
  folder outside a `#[cfg(test)]` module or a `#[cfg(test)]` / `#[cfg(any(test, …))]` attribute. It is a line
  heuristic, not a parser: it treats everything after a `#[cfg(test)] mod` as test code.

Test data is made up, never real: invented people, places, venues and groups, and `.example` domains for
anything that isn't a real service the code talks to. The data used to borrow real names (the developer's own
name as the demo post's author, real towns, a real comedy club and venue, a real improv troupe, a Meetup page);
they were replaced with fictional ones. What stays real is reference data, not personal data: IANA time zones
(`America/Denver`, whose daylight-saving dates the tests rely on), `pixelfed.social` where it is the sign-in
placeholder or an instance address being parsed, and recognised ticket hosts such as `eventbrite.com`. The one
deployed layout that named a real troupe in a placeholder (`layouts/events/0001.json`, when the app id was `watcher`) was deleted with the
other old layout files (decision `retired-layout-files`).

Older decisions name the old `fixtures/…` paths and the old example names; this decision says where the files
are now and what replaced the names.
