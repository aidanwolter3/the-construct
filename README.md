# Project Canvas

A server-driven UI shell: a Rust protocol, server and client core, rendered by a Compose
Multiplatform desktop app through UniFFI. The design is in [DESIGN.md](DESIGN.md), decisions
are in [docs/DECISIONS.md](docs/DECISIONS.md), and status is in [docs/PROGRESS.md](docs/PROGRESS.md).

Milestones 1–3 are implemented: the protocol crate, the Axum server and the macOS desktop app.

## Layout

```
crates/protocol      UiNode / ActionIntent / WorkspaceSpec (hydration bundle) / EntityRecord
crates/server        Axum server: GET /v1/workspaces/{home,pixelfed_demo}
crates/client-core   CanvasEngine: fetches the workspace, holds state, applies optimistic mutations
crates/client-ffi    UniFFI bridge (EngineHandle, FfiStateSnapshot, StateObserver) + uniffi-bindgen bin
fixtures/            counter_home_bundle.json, pixelfed_home_bundle.json (DESIGN.md §2.2C)
scripts/build_ffi.sh builds libclient_ffi.dylib and regenerates the Kotlin bindings
kmp-app/             Gradle project: :shared (model + Compose renderer), :desktopApp (JVM, JNA)
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

Terminal 1: start the server on 127.0.0.1:8080 (set `CANVAS_ADDR` to use another address).

```bash
cargo run -p server
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq .
```

Terminal 2: build the FFI library and bindings, then run the desktop app.

```bash
scripts/build_ffi.sh            # optional: :desktopApp:compileKotlin runs it automatically
                                # (dylib built to target/aarch64-apple-darwin/release, copied to target/release)
cd kmp-app
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
./gradlew :desktopApp:run
```

A "Project Canvas" window opens with the counter layout served by the server. Clicking
**Increment** or **Reset** prints the intent to stdout and the count updates:

```
[desktop] dispatch: {"intent":"action.mutation.optimistic",...}
[CanvasEngine] dispatch: {...}
[CanvasEngine] core://counters/home#count = 1
```

Options for the desktop app (set as `-D` JVM properties or environment variables):

- `canvas.baseUrl` / `CANVAS_BASE_URL` (default `http://127.0.0.1:8080`)
- `canvas.workspace` / `CANVAS_WORKSPACE` (default `home`; `pixelfed_demo` shows the Pixelfed feed fixture)

Separately, `-PskipRustBuild` is a Gradle property (`./gradlew -PskipRustBuild :desktopApp:run`) that skips
the cargo build step.

Clicks are applied locally only: there is no server sync until the action outbox lands in M5, so the count
resets when the app restarts. The engine caches the last fetched bundle per workspace in
`~/.canvas/workspace_<id>.json` (e.g. `workspace_home.json`) and uses it when the server is unreachable;
it retries the fetch every 5 seconds until the server responds.

## Tests

```bash
cargo test --workspace
cd kmp-app && ./gradlew :shared:allTests :desktopApp:test
```
