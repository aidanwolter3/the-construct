//! The Rust rendering engine (decision `rust-render-engine`).
//!
//! A plumbing [`protocol::UiNode`] tree and its data are laid out with taffy
//! ([`layout`]), turned into a [`scene::Scene`] of instanced SDF quads, text
//! runs and a semantic tree, and drawn by the CPU rasterizer ([`cpu`]) or by
//! wgpu ([`gpu`], feature `gpu`): offscreen, in a desktop window
//! (`construct-viewer`, feature `window`) or on an Android surface.
//! [`interact::Session`] keeps a layout's device state and answers virtual
//! taps, which is what the pod's `render_layout_preview` and
//! `dispatch_virtual_interaction` MCP tools run.

pub mod bind;
pub mod cpu;
pub mod dates;
pub mod demo;
pub mod encode;
pub mod icons;
pub mod images;
pub mod interact;
pub mod layout;
pub mod scene;
pub mod style;
pub mod text;
pub mod viewer;

#[cfg(feature = "gpu")]
pub mod gpu;

#[cfg(feature = "window")]
pub mod window;

#[cfg(all(feature = "remote", feature = "window"))]
pub mod pods;
#[cfg(feature = "remote")]
pub mod remote;

#[cfg(all(target_os = "android", feature = "window"))]
mod android;

pub use interact::{Effect, Outcome, Session, Target};
pub use layout::{Input, RenderError, SceneBuilder, Viewport};
pub use scene::{QuadInstance, Scene, SemanticNode, Tap};
