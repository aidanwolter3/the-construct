//! Android App Links for the sign-in redirect (decision
//! `android-app-link-sign-in-redirect`).
//!
//! The Android app receives its OAuth redirect on
//! `https://<this server>/oauth/android` instead of the custom scheme
//! `construct://oauth`, which any installed app can also claim (security
//! audit #31). Android hands such an https link only to an app whose signing
//! certificate this server lists in `/.well-known/assetlinks.json`, so no
//! other app can receive it.
//!
//! When the browser does not hand the link to the app (the domain is not
//! verified on this device, or the browser keeps redirects that no tap
//! started), the browser loads `/oauth/android` from this server: a page with
//! one button that opens the app by package name, so the code still reaches
//! only that app.
//!
//! Nothing here knows an app (D40): it is about the Android client, which all
//! apps share.

use axum::{
    extract::{RawQuery, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::Host;
use std::sync::Arc;

/// Path of the https redirect the Android app registers.
pub const REDIRECT_PATH: &str = "/oauth/android";
/// Path Android fetches to verify the app's https intent filter.
pub const ASSET_LINKS_PATH: &str = "/.well-known/assetlinks.json";
/// The Android app's package name (`applicationId`).
pub const DEFAULT_ANDROID_PACKAGE: &str = "org.theconstruct.android";

/// Which Android app may open this server's https links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidAppLinks {
    /// `CONSTRUCT_ANDROID_PACKAGE` (default [`DEFAULT_ANDROID_PACKAGE`]).
    pub package: String,
    /// SHA-256 fingerprints of the signing certificates allowed to open the
    /// links (`CONSTRUCT_ANDROID_CERT_SHA256`, comma-separated), normalised
    /// to `AA:BB:…`. Usually the release key's, plus any debug keys that
    /// should get App Links too. Empty: no `assetlinks.json` (404), so no
    /// build is verified and the app keeps `construct://oauth`.
    pub cert_sha256: Vec<String>,
}

impl Default for AndroidAppLinks {
    fn default() -> Self {
        Self {
            package: DEFAULT_ANDROID_PACKAGE.to_owned(),
            cert_sha256: Vec::new(),
        }
    }
}

impl AndroidAppLinks {
    /// From `CONSTRUCT_ANDROID_PACKAGE` and `CONSTRUCT_ANDROID_CERT_SHA256`.
    /// A fingerprint that is not 32 hex bytes is skipped with a warning; a
    /// package name that is not one is replaced by the default.
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let package = match var("CONSTRUCT_ANDROID_PACKAGE") {
            Some(p) if valid_package(&p) => p,
            Some(p) => {
                tracing::warn!(package = %p, "CONSTRUCT_ANDROID_PACKAGE is not a package name; using the default");
                DEFAULT_ANDROID_PACKAGE.to_owned()
            }
            None => DEFAULT_ANDROID_PACKAGE.to_owned(),
        };
        let mut cert_sha256 = Vec::new();
        for entry in var("CONSTRUCT_ANDROID_CERT_SHA256")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match normalize_fingerprint(entry) {
                Some(f) => cert_sha256.push(f),
                None => tracing::warn!(
                    entry,
                    "ignoring CONSTRUCT_ANDROID_CERT_SHA256 entry that is not a SHA-256 fingerprint"
                ),
            }
        }
        Self {
            package,
            cert_sha256,
        }
    }

    /// The Digital Asset Links statement, or `None` with no fingerprints.
    pub fn asset_links(&self) -> Option<serde_json::Value> {
        (!self.cert_sha256.is_empty()).then(|| {
            json!([{
                "relation": ["delegate_permission/common.handle_all_urls"],
                "target": {
                    "namespace": "android_app",
                    "package_name": self.package,
                    "sha256_cert_fingerprints": self.cert_sha256,
                },
            }])
        })
    }
}

/// `AA:BB:…` (32 bytes, uppercase) from that form or 64 bare hex digits.
pub fn normalize_fingerprint(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if s.contains(':') && s.split(':').any(|part| part.len() != 2) {
        return None;
    }
    let upper = hex.to_ascii_uppercase();
    let pairs: Vec<&str> = (0..32).map(|i| &upper[i * 2..i * 2 + 2]).collect();
    Some(pairs.join(":"))
}

/// A Java-style package name: dot-separated segments of
/// `[A-Za-z][A-Za-z0-9_]*`, at least two.
fn valid_package(p: &str) -> bool {
    let segments: Vec<&str> = p.split('.').collect();
    segments.len() >= 2
        && segments.iter().all(|s| {
            s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// `GET /.well-known/assetlinks.json`. Served directly (Android's verifier
/// follows no redirects), 404 when no fingerprint is configured.
pub(crate) async fn asset_links(State(host): State<Arc<Host>>) -> Response {
    match host.config().android.asset_links() {
        Some(body) => {
            let mut resp = axum::Json(body).into_response();
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            );
            resp
        }
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({ "error": "not_found" })),
        )
            .into_response(),
    }
}

/// A query a sign-in redirect may carry into the page: the characters RFC
/// 3986 allows unencoded in a query, minus `'` and `;`. Nothing that could
/// end the link's data part (`#`), start an intent field (`;`) or leave the
/// HTML attribute (quotes, `<`, `>`); `&` is escaped.
fn redirect_query_ok(q: &str) -> bool {
    !q.is_empty()
        && q.len() <= 4096
        && q.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'.'
                        | b'_'
                        | b'~'
                        | b'%'
                        | b'&'
                        | b'='
                        | b'+'
                        | b'/'
                        | b':'
                        | b'@'
                        | b'!'
                        | b'$'
                        | b'('
                        | b')'
                        | b'*'
                        | b','
                        | b'?'
                )
        })
}

/// `GET /oauth/android?code=…&state=…`: reached only when the browser kept
/// the redirect instead of handing it to the app. One button opens the app
/// by package (an `intent:` link, which Chrome and Firefox only send to that
/// package) with the same query on `construct://oauth`; the app's engine
/// matches it to the pending sign-in by `state` as usual. No script, not
/// cached, no referrer.
pub(crate) async fn redirect_page(
    State(host): State<Arc<Host>>,
    RawQuery(query): RawQuery,
) -> Response {
    let package = &host.config().android.package;
    let (status, body) = match query.as_deref().filter(|q| redirect_query_ok(q)) {
        Some(q) => (
            StatusCode::OK,
            format!(
                "<p>Almost done. Open The Construct to finish signing in.</p>\
                 <p><a class=\"button\" href=\"intent://oauth?{q}#Intent;scheme=construct;package={package};end\">\
                 Open The Construct</a></p>\
                 <p class=\"hint\">If nothing happens, make sure the app is installed and start the sign-in \
                 again from the app.</p>",
                q = q.replace('&', "&amp;"),
            ),
        ),
        None => (
            StatusCode::BAD_REQUEST,
            "<p>This page finishes a sign-in in The Construct app, but the link is incomplete.</p>\
             <p class=\"hint\">Start the sign-in again from the app.</p>"
                .to_owned(),
        ),
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>The Construct sign-in</title><style>\
         body{{font-family:system-ui,sans-serif;margin:2rem 1rem;max-width:32rem;line-height:1.5;color:#1a1a1a}}\
         .button{{display:inline-block;padding:.75rem 1.25rem;border-radius:.5rem;background:#1a1a1a;color:#fff;\
         text-decoration:none;font-weight:600}}.hint{{color:#555;font-size:.9rem}}\
         </style></head><body><h1>The Construct</h1>{body}</body></html>"
    );
    let mut resp = (status, html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
        ),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_normalised() {
        let colon = "14:6d:e9:83:c5:73:06:50:d8:ee:b9:95:2f:34:fc:64:16:a0:83:42:e6:1d:be:a8:8a:04:96:b2:3f:cf:44:e5";
        let want = colon.to_ascii_uppercase();
        assert_eq!(normalize_fingerprint(colon).as_deref(), Some(want.as_str()));
        let bare = colon.replace(':', "");
        assert_eq!(normalize_fingerprint(&bare).as_deref(), Some(want.as_str()));
        for bad in [
            "",
            "14:6D",
            &bare[..62],
            &format!("{bare}00"),
            &bare.replace('1', "g"),
            "146:D:E9:83:C5:73:06:50:D8:EE:B9:95:2F:34:FC:64:16:A0:83:42:E6:1D:BE:A8:8A:04:96:B2:3F:CF:44:E5",
        ] {
            assert_eq!(normalize_fingerprint(bad), None, "{bad}");
        }
    }

    #[test]
    fn package_names() {
        for ok in ["org.theconstruct.android", "a.b", "com.example.app_2"] {
            assert!(valid_package(ok), "{ok}");
        }
        for bad in ["", "single", "org..x", "org.1x", "org.x;end", "org.x-y"] {
            assert!(!valid_package(bad), "{bad}");
        }
    }

    #[test]
    fn asset_links_need_a_fingerprint() {
        assert_eq!(AndroidAppLinks::default().asset_links(), None);
        let links = AndroidAppLinks {
            cert_sha256: vec!["AA:BB".into()],
            ..AndroidAppLinks::default()
        };
        let body = links.asset_links().unwrap();
        assert_eq!(body[0]["target"]["package_name"], DEFAULT_ANDROID_PACKAGE);
        assert_eq!(body[0]["target"]["sha256_cert_fingerprints"][0], "AA:BB");
        assert_eq!(
            body[0]["relation"][0],
            "delegate_permission/common.handle_all_urls"
        );
    }

    #[test]
    fn redirect_queries() {
        for ok in [
            "code=abc&state=xyz",
            "code=a%2Fb&state=s",
            "code=a/b:c&state=s",
            "error=access_denied&error_description=The+user+said+no&state=s",
        ] {
            assert!(redirect_query_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "code=\"><script>",
            "code=a b",
            "code=a#Intent;package=evil;end",
            "code=a;package=x",
            "code=a'b",
            "code=a{b}",
            &"a".repeat(4097),
        ] {
            assert!(!redirect_query_ok(bad), "{bad}");
        }
    }
}
