//! construct-render's integration tests (one binary, docs/testing.md).

mod common;
mod golden;
#[cfg(feature = "gpu")]
mod gpu;
mod interaction;
mod layout_math;
