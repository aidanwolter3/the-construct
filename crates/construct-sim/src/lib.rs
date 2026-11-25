//! The headless simulator (docs/simulator.md): a script of taps, typing,
//! assertions and shots drives a session of the Rust engine
//! (`construct-render`) on fixture bundles ([`fixture`]) or a live pod, drawn
//! on the CPU to PNGs. It is the project's UI check, in CI and in cloud
//! sessions: no window, no GPU, no JVM.

pub mod fixture;
pub mod run;
/// The script language and screen sizes, shared with the pod's journey
/// walks (they live in `construct-render`).
pub use construct_render::script;
pub use construct_render::script::screen;
