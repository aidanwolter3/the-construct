//! construct-render's integration tests (one binary, docs/testing.md).

mod accessibility;
mod bench;
mod common;
#[cfg(feature = "remote")]
mod construct_stub;
mod dates;
mod device;
#[cfg(feature = "remote")]
mod engine;
mod golden;
#[cfg(feature = "gpu")]
mod gpu;
mod interaction;
mod layout_math;
mod main_axis;
mod media;
mod reuse;
mod swipe_cards;
mod system_bars;
mod text_styles;
mod writes;
