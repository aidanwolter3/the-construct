//! construct-render's integration tests (one binary, docs/testing.md).

mod accessibility;
mod common;
mod device;
#[cfg(feature = "remote")]
mod engine;
mod golden;
#[cfg(feature = "gpu")]
mod gpu;
mod interaction;
mod layout_math;
mod media;
mod reuse;
mod writes;
