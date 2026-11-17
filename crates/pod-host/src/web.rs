//! The web client's routes (decision `web-client`), only with
//! `CONSTRUCT_WEB=1`: the page and the built client (`scripts/web_build.sh`
//! writes them to `CONSTRUCT_WEB_DIR`), the pod code typed on the page
//! turned into a session cookie, and an image proxy (a canvas can't read
//! another site's pixels).
//!
//! The cookie is not the pod code. Typing the code makes an ordinary session
//! of the pod (it expires after `CONSTRUCT_SESSION_TTL_DAYS`, and logging
//! out deletes it) and a host credential of kind `web` for it; the code is
//! used once and kept nowhere in the browser. The host accepts the cookie
//! only on requests that also carry `x-construct-client: web` (a page of
//! another site can't send it without a preflight, which nothing answers)
//! and, where the browser says, from the same site, and never for `/v1/mcp`.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{Path, RawQuery, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::session::{new_session_token, session_hash, session_ttl_from_env};
use pod_core::{json_error, metrics, parse_json_body};
use serde_json::json;

use crate::db::CredentialKind;
use crate::{Host, TOKEN_PREFIX, client_keys, peer, rate_limited};

/// The browser's session cookie.
pub const COOKIE: &str = "construct_web";
/// What the web client sends on every request, and the host requires with
/// the cookie.
pub const CLIENT_HEADER: &str = "x-construct-client";
pub const CLIENT_WEB: &str = "web";

/// The page's policy: its own scripts (and WebAssembly), nothing inline,
/// nothing from elsewhere.
const PAGE_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
    base-uri 'none'; form-action 'self'";

/// Largest image the proxy passes on.
const MAX_IMAGE_BYTES: usize = 10 << 20;

/// `CONSTRUCT_WEB=1`: the web client is served from `dir`
/// (`CONSTRUCT_WEB_DIR`, default `target/web`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfig {
    pub dir: PathBuf,
}

impl WebConfig {
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned());
        (var("CONSTRUCT_WEB").as_deref() == Some("1")).then(|| WebConfig {
            dir: var("CONSTRUCT_WEB_DIR")
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| "target/web".into())
                .into(),
        })
    }
}

pub(crate) fn routes() -> Router<Arc<Host>> {
    Router::new()
        .route("/web", get(page))
        .route("/web/", get(page))
        .route("/web/session", post(open_session))
        .route("/web/logout", post(logout))
        .route("/web/image", get(image))
        .route("/web/fonts/{file}", get(font))
        .route("/web/{file}", get(asset))
}

/// The cookie's token on a request the host may take it on: the web client
/// is on, the request says it is the web client, and the browser (where it
/// says) sent it from this site.
pub(crate) fn cookie_token(host: &Host, headers: &HeaderMap) -> Option<String> {
    host.config.web.as_ref()?;
    if headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()) != Some(CLIENT_WEB) {
        return None;
    }
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && site != "same-origin"
    {
        return None;
    }
    cookie(headers, COOKIE)
}

/// A cookie's value.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
        .filter(|v| !v.is_empty())
}

fn set_cookie(host: &Host, value: &str, max_age: i64) -> HeaderValue {
    // `Secure` behind HTTPS (Fly); a local server is plain HTTP.
    let secure = if host.config.hsts { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .expect("a cookie")
}

fn dir(host: &Host) -> Option<&FsPath> {
    host.config.web.as_ref().map(|w| w.dir.as_path())
}

/// A file of the built client, by a plain name only.
async fn file(base: &FsPath, name: &str) -> Option<(Vec<u8>, &'static str)> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !ok {
        return None;
    }
    let kind = match name.rsplit_once('.')?.1 {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "ttf" => "font/ttf",
        _ => return None,
    };
    let bytes = tokio::fs::read(base.join(name)).await.ok()?;
    Some((bytes, kind))
}

fn served(bytes: Vec<u8>, kind: &'static str, cache: &'static str) -> Response {
    let mut r = Response::new(Body::from(bytes));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_CSP),
    );
    r
}

fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, json!({ "error": "not_found" }))
}

/// `GET /web`: the page (it asks for the pod code when there is no session).
async fn page(State(host): State<Arc<Host>>) -> Response {
    match dir(&host) {
        Some(d) => match file(d, "index.html").await {
            Some((b, k)) => served(b, k, "no-cache"),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `GET /web/<file>`: the client's script, WebAssembly and style.
async fn asset(State(host): State<Arc<Host>>, Path(name): Path<String>) -> Response {
    match dir(&host) {
        Some(d) => match file(d, &name).await {
            Some((b, k)) => served(b, k, "no-cache"),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `GET /web/fonts/<file>`: the renderer's fonts, fetched by the client
/// instead of built into it.
async fn font(State(host): State<Arc<Host>>, Path(name): Path<String>) -> Response {
    match dir(&host) {
        Some(d) => match file(&d.join("fonts"), &name).await {
            Some((b, k)) => served(b, k, "public, max-age=86400"),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `POST /web/session {code}` (with `x-construct-client: web`): a session
/// of the pod the code opens, as a cookie -> `{ok}`; 401 `pod_not_found`,
/// rate-limited like `POST /v1/pods/open`.
async fn open_session(State(host): State<Arc<Host>>, req: Request) -> Response {
    if req
        .headers()
        .get(CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        != Some(CLIENT_WEB)
    {
        return json_error(StatusCode::FORBIDDEN, json!({ "error": "web_client_only" }));
    }
    let keys = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip);
    if !host.code_tries.allow(&keys) {
        metrics::inc(metrics::RATE_LIMITED, &[("limit", "pod_code")]);
        return rate_limited();
    }
    let headers = req.headers().clone();
    let Ok(body) = axum::body::to_bytes(req.into_body(), 16 * 1024).await else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request" }),
        );
    };
    let code = match parse_json_body(&headers, &body) {
        Ok(v) => v["code"].as_str().map(|t| t.trim().to_owned()),
        Err(e) => return e.into_response(),
    };
    let refused = || {
        json_error(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "pod_not_found" }),
        )
    };
    let Some(code) = code.filter(|t| t.starts_with(TOKEN_PREFIX) && t.len() <= 128) else {
        return refused();
    };
    let pod_id = match host.pod_for_token(&code).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            metrics::inc(metrics::AUTH_FAILURES, &[("kind", "web_pod_code")]);
            return refused();
        }
        Err(r) => return *r,
    };
    let open = match host.pod(&pod_id).await {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let token = new_session_token();
    let hash = session_hash(&token);
    let h = hash.clone();
    if let Err(e) = open.pod.blocking(move |s| s.create_session(&h)).await {
        return e.into_response();
    }
    let added = host
        .blocking(move |h| h.store.insert_web_credential(&pod_id, &hash))
        .await;
    match added {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = %e, "host store error");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "host_error" }),
            );
        }
        Err(r) => return *r,
    }
    tracing::info!("a browser opened a pod");
    let mut r = axum::Json(json!({ "ok": true })).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(&host, &token, session_ttl_from_env()),
    );
    r
}

/// `POST /web/logout`: ends the browser's session (the pod's and the host's
/// credential) and clears the cookie. The pod code is untouched.
async fn logout(State(host): State<Arc<Host>>, req: Request) -> Response {
    let mut r = axum::Json(json!({ "ok": true })).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&host, "", 0));
    let Some(token) = cookie_token(&host, req.headers()) else {
        return r;
    };
    if let Ok(Some((pod_id, CredentialKind::Web))) = host.credential(&token).await {
        if let Ok(open) = host.pod(&pod_id).await {
            let hash = session_hash(&token);
            let _ = open.pod.blocking(move |s| s.delete_session(&hash)).await;
        }
        let hash = session_hash(&token);
        let _ = host
            .blocking(move |h| h.store.delete_web_credential(&hash))
            .await;
    }
    r
}

/// `GET /web/image?url=<url>` (a browser with a session): a public image of
/// another site, passed on so the canvas can draw it. Only `http(s)` to
/// public hosts (names that resolve to private addresses are refused at
/// connect time), only images, at most 10 MiB.
async fn image(State(host): State<Arc<Host>>, RawQuery(q): RawQuery, req: Request) -> Response {
    let Some(token) = cookie_token(&host, req.headers()) else {
        return json_error(StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));
    };
    if !matches!(
        host.credential(&token).await,
        Ok(Some((_, CredentialKind::Web)))
    ) {
        return json_error(StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));
    }
    let url = reqwest::Url::parse(&format!("http://q/?{}", q.unwrap_or_default()))
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "url")
                .map(|(_, v)| v.into_owned())
        });
    let Some(url) = url.and_then(|u| reqwest::Url::parse(&u).ok()) else {
        return json_error(StatusCode::BAD_REQUEST, json!({ "error": "invalid_url" }));
    };
    if !matches!(url.scheme(), "http" | "https")
        || !pod_core::net::is_public_host(url.host_str().unwrap_or(""))
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "not_a_public_url" }),
        );
    }
    let client = match pod_core::net::public_builder(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(_) => return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "fetch_failed" })),
    };
    let Ok(mut resp) = client.get(url).send().await else {
        return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "fetch_failed" }));
    };
    let kind = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !resp.status().is_success() || !kind.starts_with("image/") {
        return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "not_an_image" }));
    }
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            return json_error(StatusCode::BAD_GATEWAY, json!({ "error": "too_large" }));
        }
        bytes.extend_from_slice(&chunk);
    }
    let mut r = Response::new(Body::from(bytes));
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&kind) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    r
}
