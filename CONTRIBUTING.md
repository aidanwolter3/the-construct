# Contributing

Thanks for helping. This page covers setup, the checks to run, and the rules that keep the architecture intact.
Read [docs/architecture.md](docs/architecture.md) first if you are new to the codebase.

> **No license yet.** The project hasn't chosen a license, so it can't accept outside code contributions for now:
> each contribution would stay its author's, which would complicate choosing a license later. Issues, bug reports
> and feedback are very welcome.

## Setup

- **Rust** through [rustup](https://rustup.rs). The version is pinned in `rust-toolchain.toml` (CI reads the same
  file); rustup installs it on the first build.
- **JDK 21** for the Kotlin clients and the simulator. Gradle runs on the JDK in `JAVA_HOME` (or `java` on `PATH`);
  any JDK 21 works. On a Mac, Android Studio's bundled one is
  `export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"`. To pin a JDK for Gradle without
  exporting `JAVA_HOME`, put `org.gradle.java.home=<path>` in your own `~/.gradle/gradle.properties`, not in the
  repository's.
- **The engine library** for the JVM: on macOS, `scripts/build_ffi.sh` builds `libclient_ffi.dylib` (Apple Silicon)
  and regenerates the Kotlin bindings; Gradle runs it before compiling the desktop app. On Linux, run
  `cargo build -p client-ffi --release` and pass `-PskipRustBuild` (the bindings are checked in).
- **Android** (optional): an Android SDK with an NDK, `cargo install cargo-ndk`, and
  `rustup target add aarch64-linux-android x86_64-linux-android`. See [docs/clients.md](docs/clients.md#android).

On Ubuntu, `scripts/cloud_setup.sh` installs everything the tests and the simulator need and warms the Gradle cache.

## Checks

Run these before pushing; CI runs the same and keeps `main` green:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
scripts/check_decisions.sh && scripts/check_test_data.sh
cd app && ./gradlew -PskipRustBuild :shared:allTests :desktopApp:test
```

UI changes are checked with the [headless simulator](docs/simulator.md): there are no emulator or on-device tests.
Some things (the Android permission prompt, push with the app killed, the macOS window) can only be checked on a
device; say so in the PR when a change needs that. More in [docs/testing.md](docs/testing.md).

## Rules

1. **The core knows no app.** The engine (`client-core`), the shared UI and `pod-core` handle layouts, entities and
   intents in general (D34, D35, D40). App changes ship as layout files plus adapter code in `crates/app-*`. Only new
   generic vocabulary (a node type, an intent, a field) needs a client build.
2. **Layouts are append-only files.** A new version is the next `layouts/<app>/NNNN.json` in the app's adapter crate; never edit or renumber a
   deployed one, or the server refuses to start. Old files may be deleted, oldest first (decision
   `retired-layout-files`). See [docs/layouts.md](docs/layouts.md).
3. **Vocabulary is named features.** Every node type, intent or field a layout can use has a file in
   `crates/protocol/features/` and an arm in `protocol::required_features`. Remove vocabulary by moving its file to
   `features/removed/`, never by editing layouts. See
   [crates/protocol/features/README.md](crates/protocol/features/README.md).
4. **Migrations, not resets.** Schema changes to the server's databases need a migration that works on existing
   pods: staging upgrades in place.
5. **Keep Compose keys stable** in `app/shared`, so UI state survives layout updates.
6. **Record decisions.** An architecture decision gets a new file, `docs/decisions/<topic>.md`, following
   [docs/decisions/README.md](docs/decisions/README.md). Don't rewrite old decisions; mark them superseded. Renaming
   something they mention (an app id, a folder) is the one exception: update the name in them too, so a search finds
   it, and list the rename in [docs/decisions/README.md](docs/decisions/README.md).
   `docs/DECISIONS.md` is closed at D52.

After changing a `pixelfed` layout or the fake Pixelfed's comments, regenerate the simulator fixtures:
`UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::`.

## Naming

The product is "The Construct". Kotlin package `org.theconstruct`; environment variables `CONSTRUCT_*`; data
directories `~/.construct*`; OAuth redirect `construct://oauth`.

## Commits and pull requests

- Commit messages: an imperative subject of at most 70 characters, a blank line, then a body explaining what and
  why, wrapped at 70 characters.
- Prefer a branch and a pull request for anything non-trivial. Fill in
  [the template](.github/pull_request_template.md), including:
  - **New or removed feature:** no, or which layout features were added or moved to `removed/`;
  - **Client update needed:** whether users need a new desktop or Android build, or the change ships through the
    server and layout files alone.
- Pull requests that change the interface (layouts, shared UI, node types) include simulator screenshots of the
  changed screens, before and after where it helps.
- Keep a pull request to one change; file follow-up work as an issue.

## Staging

Every push to `main` deploys the staging server, `https://the-construct-pod.fly.dev`, once the Rust job passes.
To try it from a script, make a temporary pod (24 hours, deleted afterwards):

```bash
token=$(curl -fsS -X POST https://the-construct-pod.fly.dev/v1/pods/temporary | jq -r .session_token)
curl -fsS -H "Authorization: Bearer $token" https://the-construct-pod.fly.dev/v1/apps | jq
```

Never put pod codes or tokens in git or files.
