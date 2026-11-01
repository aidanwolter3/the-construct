# Progress Log

Running log of implementation work for the first demo (Milestones 1-3 of DESIGN.md).
Process: orchestrator coordinates separate implementation, code-review and QA passes.
Note: the environment restarted twice mid-work; work is now committed in small wip commits.

## Milestone 1: Protocol crate

- protocol crate implemented: `ast.rs` (UiNode, SlotSpec, Orientation, TextStyle, ListBinding),
  `intent.rs` (ActionIntent, PresentationMode), `bundle.rs` (WorkspaceSpec, EntityRecord).
- `fixtures/pixelfed_home_bundle.json` is the exact §2.2C bundle; `tests/roundtrip.rs` has 10 tests (pass).
- client-core, client-ffi, server are stubs so the workspace builds.
- Decisions recorded in docs/DECISIONS.md.
- Review/QA: pending.

- Review (independent agent): wire format matches DESIGN.md; added counter-fixture test and f32 aspect_ratio
  semantics test; silenced clippy large_enum_variant. Deferred: unknown fields are ignored (lenient), root node
  not forced to be core://Workspace.
- Status: DONE (12 tests).

## Milestone 2: Axum server

- crates/server: lib `app()` + bin; routes GET /healthz, /v1/workspaces/{home,pixelfed_demo}, 404 JSON for unknown,
  POST /v1/counters/home/{increment,reset} (stateless ack). Fixtures embedded with include_str! and validated at startup.
- Review+QA (independent agent): fixed HTTP/2 (h2c) not enabled, empty-body POST with JSON content-type
  rejected, non-JSON error bodies, path-vs-body error ordering, port-in-use message, ANSI colors in logs.
  Curl evidence: home returns 200 application/json; h2c works; unknown id returns 404 JSON.
- Status: DONE (10 tests).

## Milestone 3 (in progress)

- client-core CanvasEngine: background fetch thread (reqwest blocking + rustls), observers, D4 expr evaluator,
  offline JSON cache at <storage>/last_workspace.json. 13 tests. Snapshot workspace_json = layout node only;
  entities_json = URI-keyed map.
- kmp-app: Gradle 9.3.1 wrapper, Kotlin 2.3.20, Compose MP 1.11.0-beta01, material3 1.11.0-alpha05,
  kotlinx-serialization 1.10.0, JNA 5.17.0. :shared (jvm("desktop") target) has Model.kt (custom tagged-union
  serializers with Unknown fallback), Interpolate.kt, CanvasEngineBridge; 14 tests. :desktopApp placeholder window.
- Process note: two runs of agents overlapped in kmp-app after a restart; overlapping files from the
  earlier run were set aside in the scratchpad (interrupted-run/) and reused as reference.

## Next steps

1. client-ffi: UniFFI proc-macro EngineHandle/FfiStateSnapshot/StateObserver, uniffi-bindgen bin,
   scripts/build_ffi.sh -> bindings into kmp-app/desktopApp/src/main/kotlin/uniffi/client_ffi.
2. kmp renderer: DynamicRenderer.kt + CanvasApp.kt in shared (reference: scratchpad interrupted-run/).
3. Review client-core.
4. Integrate: desktopApp Main.kt uses EngineHandle via JNA; :desktopApp:run depends on build_ffi.
5. QA full demo (server + app + screenshot), README.md, M3 commit.
