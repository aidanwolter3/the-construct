//! The web client (decision `web-client`): the one renderer
//! (`construct-render`) compiled to WebAssembly and drawn into a canvas by
//! its CPU rasterizer, over the pod's HTTP API on the page's own origin.
//! [`construct`] and [`signin`] are the parts with no I/O (tested
//! natively); `web` is the browser half, built only for `wasm32`.

pub mod construct;
pub mod signin;
#[cfg(target_arch = "wasm32")]
mod web;

use protocol::DataBundle;

/// What a browser is called among the user's devices (decision
/// `device-versions`), from its user agent: `Chrome on macOS`.
pub fn browser_name(ua: &str) -> String {
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("OPR/") {
        "Opera"
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("CrOS") {
        "ChromeOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        return browser.to_owned();
    };
    format!("{browser} on {os}")
}

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
/// `/s/<link>` or `/<handle>/<slug>`, and the name of the row it opens
/// (`/<handle>/<slug>/<row>`, decision `public-row-links`).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start_public(base: String, row: Option<String>) -> Result<(), wasm_bindgen::JsValue> {
    web::start_public(&base, row.as_deref()).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_browser_is_named_by_its_user_agent() {
        let mac_chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        assert_eq!(super::browser_name(mac_chrome), "Chrome on macOS");
        let win_edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                        (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";
        assert_eq!(super::browser_name(win_edge), "Edge on Windows");
        let linux_ff = "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";
        assert_eq!(super::browser_name(linux_ff), "Firefox on Linux");
        assert_eq!(super::browser_name("curl/8"), "Browser");
    }
}
