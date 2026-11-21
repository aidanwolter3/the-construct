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

/// What a tap or long press at `at` did, for the browser's console (a
/// click that seems to do nothing says why): the node it hit and each
/// effect's [`summary`](construct_render::interact::Effect::summary), or
/// that nothing tappable was there. A field's text is never shown.
pub fn tap_log(
    what: &str,
    at: (f32, f32),
    out: Option<&construct_render::interact::Outcome>,
) -> String {
    let (x, y) = (at.0.round(), at.1.round());
    let Some(out) = out else {
        return format!("{what} at {x},{y}: nothing there to tap");
    };
    let node = match &out.node {
        Some(n) => {
            let label = n
                .text_content
                .as_deref()
                .filter(|_| !n.node_type.contains("TextField"))
                .map(|t| {
                    let t: String = t.chars().take(60).collect();
                    format!(" \"{t}\"")
                })
                .unwrap_or_default();
            format!(" on {} {}{label}", n.node_type, n.id)
        }
        None => String::new(),
    };
    let effects: Vec<String> = out.effects.iter().map(|e| e.summary()).collect();
    let did = match effects.as_slice() {
        [] => "nothing".to_owned(),
        [only] if only == "nothing" => "nothing (it has no action)".to_owned(),
        _ => effects.join(", "),
    };
    format!("{what} at {x},{y}{node}: {did}")
}

/// `from`'s entities and queries over `into`'s.
pub fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// A page on the owner's own site (decision `site-origins`, `mount.js`)
/// calls this before [`start`] or [`start_public`]: the pod is at `server`
/// (an origin), reached with the site's browser session `token` (none for
/// a public link); `page` is the page's own path on the site.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn connect(server: String, token: Option<String>, page: String) {
    web::connect(&server, token, &page);
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

    fn node(id: &str, node_type: &str, text: &str) -> construct_render::SemanticNode {
        serde_json::from_value(serde_json::json!({
            "id": id, "node_type": node_type, "bounds": [0.0, 0.0, 10.0, 10.0],
            "text_content": text, "interactive": true,
        }))
        .unwrap()
    }

    #[test]
    fn a_click_that_does_nothing_says_why() {
        use construct_render::{Effect, Outcome};
        assert_eq!(
            super::tap_log("tap", (10.4, 20.6), None),
            "tap at 10,21: nothing there to tap"
        );
        let no_action = Outcome {
            node: Some(node("root/0", "core://Text", "Hello")),
            effects: vec![Effect::Nothing],
        };
        assert_eq!(
            super::tap_log("tap", (1.0, 2.0), Some(&no_action)),
            "tap at 1,2 on core://Text root/0 \"Hello\": nothing (it has no action)"
        );
        // A row whose screen no layout shown has: the route is named.
        let unmatched = Outcome {
            node: Some(node("row/i0", "core://Tappable", "Open Fix it")),
            effects: vec![Effect::NotSent {
                intent: serde_json::json!({
                    "intent": "action.navigation.open",
                    "target_uri": "collections://tasks/r1",
                    "presentation": "Push",
                }),
            }],
        };
        assert_eq!(
            super::tap_log("tap", (5.0, 5.0), Some(&unmatched)),
            "tap at 5,5 on core://Tappable row/i0 \"Open Fix it\": not_sent \
             action.navigation.open target_uri=collections://tasks/r1 (no screen here matches it)"
        );
        let opened = Outcome {
            node: Some(node("row/i0", "core://Tappable", "Open Fix it")),
            effects: vec![
                Effect::OpenedScreen {
                    screen: "task".into(),
                },
                Effect::LoadFailed {
                    what: "/v1/collections/tasks/row?id=r1".into(),
                    error: "HTTP 403 sign_in_required".into(),
                },
            ],
        };
        assert_eq!(
            super::tap_log("tap", (5.0, 5.0), Some(&opened)),
            "tap at 5,5 on core://Tappable row/i0 \"Open Fix it\": opened_screen task, \
             load_failed /v1/collections/tasks/row?id=r1: HTTP 403 sign_in_required"
        );
    }

    #[test]
    fn a_click_log_never_shows_what_was_typed() {
        use construct_render::{Effect, Outcome};
        let typed = Outcome {
            node: Some(node("f", "core://TextField", "cpod_secret")),
            effects: vec![
                Effect::FormSet {
                    field: "code".into(),
                    value: "cpod_secret".into(),
                },
                Effect::SignedIn {
                    token: "tok_secret".into(),
                    code: "cpod_secret".into(),
                },
                Effect::Copy {
                    text: "cpod_secret".into(),
                },
                Effect::NotSent {
                    intent: serde_json::json!({
                        "intent": "action.auth.session", "payload": { "code": "cpod_secret" },
                    }),
                },
            ],
        };
        let line = super::tap_log("tap", (0.0, 0.0), Some(&typed));
        assert!(!line.contains("secret"), "{line}");
        assert!(line.contains("form_set code"), "{line}");
    }
}
