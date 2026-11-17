//! The clock: std's on native, the browser's in a web build (decision
//! `web-client`), where `std::time::Instant::now()` and
//! `SystemTime::now()` panic. The same types either way.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(target_arch = "wasm32")]
pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};
