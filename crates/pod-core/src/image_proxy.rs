//! Another site's image, fetched by the pod for a client (issue #443): the
//! native clients' `GET /v1/image?url=<url>` (a signed-in session) and the
//! web client's `/web/image` (`pod_host::web`) both answer through
//! [`proxy`]. A layout can name any image URL, from a scraped page or an
//! adopted layout; fetched here, the device never reaches its own network
//! (LAN, loopback) for one, nor shows the site its address.
//!
//! Only `http(s)` to public hosts on their default port (names that resolve to private addresses
//! are refused at connect time, [`crate::net::OutboundClient`]), only raster
//! images, at most [`MAX_IMAGE_BYTES`].
//!
//! Answers are kept for a while ([`ImageCache`]: [`CACHE_TTL`], at most
//! [`CACHE_BYTES`] in all), so a photo paged back to, or a card seen again on
//! another device, isn't fetched from its site again.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;

use crate::{Pod, json_error};

/// The native clients' route.
pub const IMAGE_PATH: &str = "/v1/image";
/// Largest image passed on.
pub const MAX_IMAGE_BYTES: usize = 10 << 20;
/// Opened on its own, a passed-on image runs nothing.
const IMAGE_CSP: &str = "default-src 'none'; sandbox";
/// How long a fetched image is kept.
pub const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// The most the kept images take in all; the least recently used go first.
pub const CACHE_BYTES: usize = 64 << 20;
/// An image larger than this isn't kept (it would push out many others).
const CACHE_ONE_MAX: usize = 4 << 20;

/// Images fetched lately, by url: their type and bytes, when fetched and
/// when last served. Shared by every pod of the server (the images are
/// public pages' anyway).
#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<String, CachedImage>,
    used: usize,
    tick: u64,
}

struct CachedImage {
    kind: String,
    bytes: Bytes,
    fetched: Instant,
    used: u64,
}

impl ImageCache {
    /// The kept answer for `url`, if it is fresh.
    pub fn get(&mut self, url: &str, now: Instant) -> Option<(String, Bytes)> {
        self.tick += 1;
        let tick = self.tick;
        let fresh = self
            .entries
            .get(url)
            .is_some_and(|e| now.duration_since(e.fetched) < CACHE_TTL);
        if !fresh {
            if let Some(e) = self.entries.remove(url) {
                self.used -= e.bytes.len();
            }
            return None;
        }
        let e = self.entries.get_mut(url)?;
        e.used = tick;
        Some((e.kind.clone(), e.bytes.clone()))
    }

    /// Keeps an answer, dropping the least recently served ones to fit.
    pub fn put(&mut self, url: &str, kind: &str, bytes: Bytes, now: Instant) {
        if bytes.len() > CACHE_ONE_MAX {
            return;
        }
        self.tick += 1;
        if let Some(old) = self.entries.remove(url) {
            self.used -= old.bytes.len();
        }
        self.used += bytes.len();
        self.entries.insert(
            url.to_owned(),
            CachedImage {
                kind: kind.to_owned(),
                bytes,
                fetched: now,
                used: self.tick,
            },
        );
        while self.used > CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.used -= e.bytes.len();
            }
        }
    }

    /// Bytes kept.
    pub fn bytes(&self) -> usize {
        self.used
    }
}

fn cache() -> &'static Mutex<ImageCache> {
    static CACHE: OnceLock<Mutex<ImageCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

pub(crate) fn router() -> Router<Pod> {
    Router::new().route(IMAGE_PATH, get(image))
}

/// `GET /v1/image?url=<url>`: a signed-in session only (the pod fetches for
/// its own clients, not for anyone).
async fn image(State(pod): State<Pod>, headers: HeaderMap, RawQuery(q): RawQuery) -> Response {
    match pod.caller(&headers).await {
        Ok(c) if c.session().is_some() => {}
        Ok(_) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                json!({ "error": "session_required" }),
            );
        }
        Err(e) => return e.into_response(),
    }
    let Some(url) = url_param(q) else {
        return json_error(StatusCode::BAD_REQUEST, json!({ "error": "invalid_url" }));
    };
    proxy(url).await
}

/// The `url` parameter of a query, parsed.
pub fn url_param(q: Option<String>) -> Option<reqwest::Url> {
    reqwest::Url::parse(&format!("http://q/?{}", q.unwrap_or_default()))
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "url")
                .map(|(_, v)| v.into_owned())
        })
        .and_then(|u| reqwest::Url::parse(&u).ok())
}

/// The proxies' one client (public websites, bodies capped at
/// [`MAX_IMAGE_BYTES`]).
fn client() -> &'static crate::net::OutboundClient {
    static CLIENT: std::sync::OnceLock<crate::net::OutboundClient> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::net::OutboundClient::builder(Duration::from_secs(15))
            .web()
            .body_cap(MAX_IMAGE_BYTES)
            .build()
    })
}

/// A public `http(s)` image, passed on: public hosts on their default port
/// only (names that resolve to private addresses are refused at connect
/// time), images only, at most [`MAX_IMAGE_BYTES`].
pub async fn proxy(url: reqwest::Url) -> Response {
    let client = client();
    if !client.allows(&url) {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "not_a_public_url" }),
        );
    }
    let key = url.to_string();
    if let Some((kind, bytes)) = cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key, Instant::now())
    {
        return image_response(&kind, bytes);
    }
    let Ok(resp) = client.get(url).send().await else {
        return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "fetch_failed" }));
    };
    let kind = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !resp.status().is_success() || !is_raster_image(&kind) {
        return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "not_an_image" }));
    }
    match resp.bytes().await {
        Ok(bytes) => {
            let bytes = Bytes::from(bytes);
            cache()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .put(&key, &kind, bytes.clone(), Instant::now());
            image_response(&kind, bytes)
        }
        Err(e) if e.is_too_large() => {
            json_error(StatusCode::BAD_GATEWAY, json!({ "error": "too_large" }))
        }
        Err(_) => json_error(StatusCode::BAD_GATEWAY, json!({ "error": "fetch_failed" })),
    }
}

/// An image type the proxy passes on: not SVG, a document that runs scripts
/// when opened on this site (and the clients' decoder can't draw it).
fn is_raster_image(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    kind.starts_with("image/") && !kind.starts_with("image/svg")
}

/// Another site's image as this site serves it: never read as anything but
/// its type, and opened on its own it runs nothing.
fn image_response(kind: &str, bytes: impl Into<Body>) -> Response {
    let mut r = Response::new(bytes.into());
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(kind) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(IMAGE_CSP),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_passes_raster_images_only() {
        for kind in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
            assert!(is_raster_image(kind), "{kind}");
        }
        for kind in [
            "image/svg+xml",
            "Image/SVG+XML; charset=utf-8",
            " image/svg+xml",
            "text/html",
            "",
        ] {
            assert!(!is_raster_image(kind), "{kind}");
        }
    }

    #[test]
    fn the_cache_keeps_fresh_images_within_its_budget() {
        let now = Instant::now();
        let mut c = ImageCache::default();
        c.put("https://a.example/1.jpg", "image/jpeg", Bytes::from(vec![1; 1000]), now);
        let (kind, b) = c.get("https://a.example/1.jpg", now).unwrap();
        assert_eq!((kind.as_str(), b.len()), ("image/jpeg", 1000));
        // Stale: gone.
        assert!(c.get("https://a.example/1.jpg", now + CACHE_TTL).is_none());
        assert_eq!(c.bytes(), 0);
        // Too large to keep.
        c.put("https://a.example/big.jpg", "image/jpeg", Bytes::from(vec![0; CACHE_ONE_MAX + 1]), now);
        assert!(c.get("https://a.example/big.jpg", now).is_none());
        // Over the budget, the least recently served go first.
        let n = CACHE_BYTES / (3 << 20) + 2;
        for i in 0..n {
            c.put(&format!("https://a.example/{i}"), "image/png", Bytes::from(vec![0; 3 << 20]), now);
        }
        assert!(c.bytes() <= CACHE_BYTES);
        assert!(c.get("https://a.example/0", now).is_none());
        assert!(c.get(&format!("https://a.example/{}", n - 1), now).is_some());
    }

    #[test]
    fn a_proxied_image_runs_nothing_opened_on_its_own() {
        let r = image_response("image/png", vec![1, 2, 3]);
        let h = r.headers();
        assert_eq!(h[header::CONTENT_TYPE], "image/png");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert!(csp.contains("sandbox"), "{csp}");
    }

    #[tokio::test]
    async fn private_hosts_are_not_fetched() {
        for url in [
            "http://192.168.1.1/a.png",
            "http://127.0.0.1:8081/a.png",
            "http://localhost/a.png",
            "http://[::1]/a.png",
            "http://router.lan/a.png",
            "file:///etc/passwd",
        ] {
            let r = proxy(reqwest::Url::parse(url).unwrap()).await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{url}");
        }
    }
}
