//! construct-render's integration tests (one binary, docs/testing.md).

mod accessibility;
mod bars_merge;
mod bench;
mod button_styles;
mod common;
#[cfg(feature = "remote")]
mod construct_stub;
mod dates;
mod device;
mod edge_fades;
#[cfg(feature = "remote")]
mod engine;
mod golden;
#[cfg(feature = "gpu")]
mod gpu;
mod hardening;
mod interaction;
mod kept;
mod layout_math;
mod list_trailing;
mod main_axis;
mod markdown_text;
mod media;
mod panels;
mod phone_width;
mod reuse;
mod screen_sizes;
mod scroll_thumb;
mod swipe_cards;
mod system_bars;
mod text_styles;
mod writes;
