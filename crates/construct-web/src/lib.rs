//! The web client (decision `web-client`): the one renderer
//! (`construct-render`) compiled to WebAssembly and drawn into a canvas by
//! its CPU rasterizer, over the pod's HTTP API on the page's own origin.
//! [`construct`] is the part with no I/O (tested natively); `web` is the
//! browser half, built only for `wasm32`.

pub mod construct;
#[cfg(target_arch = "wasm32")]
mod web;

use protocol::DataBundle;

/// `from`'s entities and queries over `into`'s.
pub fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// The page calls this once its canvas is there.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start() -> Result<(), wasm_bindgen::JsValue> {
    web::start().await
}

/// A public link's page (decisions `public-web-links`,
/// `public-links-share-the-closure`) calls this with its path:
/// `/s/<link>` or `/@<handle>/<slug>`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start_public(base: String) -> Result<(), wasm_bindgen::JsValue> {
    web::start_public(&base).await
}
