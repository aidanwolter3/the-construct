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

## Next steps

In flight (parallel, uncommitted until each finishes):
1. Independent review of protocol crate (may add tests in crates/protocol).
2. M2 server implementation in crates/server (lib `app()` + bin, include_str! fixtures, oneshot tests).
3. client-core CanvasEngine (reqwest blocking, background fetch thread, expr.rs for D4 subset).
4. kmp-app scaffold (shared model + renderer, desktopApp with temporary FixtureEngine behind CanvasEngineBridge).
Then: client-ffi UniFFI wrapper + scripts/build_ffi.sh + swap FixtureEngine for EngineHandle;
review + QA of each; README.md; milestone commits.
