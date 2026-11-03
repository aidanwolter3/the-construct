//! The Construct pod as it ships: [`pod_core`] serving the compile-time
//! adapters (D40). Only this crate knows which apps the pod has; `main.rs`
//! runs it, and tests build the same pod with [`state_in_memory`] and friends.
//!
//! The core's modules are re-exported (`server::layouts`, `server::store`,
//! ...), `server::pixelfed` is the Pixelfed adapter crate and
//! `server::recipes` the recipes adapter crate.

use std::path::Path;
use std::sync::Arc;

pub use app_pixelfed as pixelfed;
pub use app_pixelfed::{DEFAULT_PIXELFED_URL, PixelfedAdapter, PixelfedConfig};
pub use app_recipes as recipes;
pub use pod_core::*;

/// The pod's adapters, in `GET /v1/apps` order: the Pixelfed feed (with the
/// `pixelfed` sign-in), the recipes, then the `pixelfed_demo` dev app.
pub fn adapters(pixelfed: PixelfedConfig) -> Vec<Arc<dyn AppAdapter>> {
    vec![
        Arc::new(PixelfedAdapter::new(pixelfed)),
        Arc::new(app_recipes::RecipesAdapter::new()),
        app_pixelfed::apps::demo_adapter(),
    ]
}

/// Every layout file the pod ships, per app in number order.
pub fn layout_files() -> Vec<layouts::LayoutSource> {
    let mut files = app_pixelfed::apps::files();
    files.extend_from_slice(app_recipes::FILES);
    files
}

/// A catalog of the pod's apps (no store), for checking layout files.
pub fn catalog() -> Catalog {
    Catalog::new(adapters(PixelfedConfig::default())).expect("the pod's adapters are valid")
}

/// Force-parse all embedded fixtures. Call at startup to fail fast.
pub fn validate_fixtures() {
    app_pixelfed::apps::validate_fixtures();
}

/// The pod over `store` with the default Pixelfed config. Panics on a
/// layout sync error.
pub fn state_new(store: store::Store) -> AppState {
    AppState::new(store, adapters(PixelfedConfig::default()))
}

/// The pod over a private in-memory database (tests).
pub fn state_in_memory() -> AppState {
    AppState::in_memory(adapters(PixelfedConfig::default()))
}

/// The pod backed by `<dir>/server.db`.
pub fn open_dir(dir: &Path) -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
    AppState::open_dir(dir, adapters(PixelfedConfig::default()))
}

/// Router over a fresh in-memory pod, hooks off (tests).
pub fn app_in_memory() -> axum::Router {
    app(Arc::new(state_in_memory()))
}

/// Configure the Pixelfed adapter of a built pod.
pub trait WithPixelfed: Sized {
    /// Point the Pixelfed adapter at another instance.
    fn with_pixelfed(self, config: PixelfedConfig) -> Self;
    /// The Pixelfed adapter's current config.
    fn pixelfed_config(&self) -> PixelfedConfig;
}

impl WithPixelfed for AppState {
    fn with_pixelfed(self, config: PixelfedConfig) -> Self {
        self.adapter::<PixelfedAdapter>()
            .expect("the pod has the Pixelfed adapter")
            .configure(config);
        self
    }

    fn pixelfed_config(&self) -> PixelfedConfig {
        self.adapter::<PixelfedAdapter>()
            .expect("the pod has the Pixelfed adapter")
            .config()
    }
}
