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
//!
//! Caching (decision `web-client-cache`): the build names each script,
//! WebAssembly and style by a hash of its content (`app.<hash>.js`), so a
//! hashed name is served `immutable` and a deploy changes the names the page
//! asks for; fonts are `immutable` when asked for with the build's `?v=`.
//! Everything else (the pages) is `no-cache` with an `ETag`, answered `304`
//! when it hasn't changed.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

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
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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

/// Public links (decision `public-web-links`, `CONSTRUCT_PUBLIC_WEB=1`):
/// anyone with a link reads its copy, never the pod. A chosen short path,
/// `/<handle>/<slug>`, answers the same copy (decision
/// `public-links-share-the-closure`); it has no route here (see
/// [`short_path`]). The first form, `/@<handle>/<slug>`, redirects to it.
pub(crate) fn public_routes() -> Router<Arc<Host>> {
    Router::new()
        .route("/s/{link}", get(public_page))
        .route("/s/{link}/view", get(public_view))
        .route("/s/{link}/image", get(public_image))
        // Links sent before the short path lost its `@`: the page moves to
        // the new address; a page already open still reads its copy.
        .route("/@{handle}/{slug}", get(short_variant))
        .route("/@{handle}/{slug}/view", get(short_view))
        // Calendar feeds (decision `ical-feeds`): a subscriber's app reads
        // the text the pod last rendered, never the pod.
        .route("/ical/{file}", get(calendar_feed))
        .route("/@{handle}/{slug}/image", get(short_image))
        // The same pages as people's apps write them: a trailing slash, and
        // `@` sent as `%40`. Each redirects to the page's own address, so
        // the page reads its own path.
        .route("/s/{link}/", get(link_slash))
        .route("/@{handle}/{slug}/", get(short_variant))
        .route("/%40{handle}/{slug}", get(short_variant))
        .route("/%40{handle}/{slug}/", get(short_variant))
        // Under /web, where the owner's own app lives.
        .route("/web/s/{link}", get(link_slash))
        .route("/web/@{handle}/{slug}", get(short_variant))
        .route("/web/%40{handle}/{slug}", get(short_variant))
}

/// What a short path, `/<handle>/<slug>` (and `…/view`, `…/image`, `…/`,
/// and `…/<row>` for one row's screen, decision `public-row-links`),
/// answers, or `None` for any other request. The host's fallback asks this
/// before it forwards a request to a pod: a route for `/{handle}/{slug}`
/// would also take the pods' own two-segment paths (`/v1/<x>`) and their
/// other methods. A handle can't be a first segment the host or a pod
/// routes (`v1`, `web`, `oauth`, …: `RESERVED_HANDLES`), so the two never
/// meet.
pub(crate) async fn short_path(
    host: &Arc<Host>,
    method: &axum::http::Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
) -> Option<Response> {
    if !matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
        return None;
    }
    let path = uri.path().strip_prefix('/')?;
    let mut parts = path.split('/');
    let (handle, slug) = (parts.next()?, parts.next()?);
    let rest = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (lower_h, lower_s) = (handle.to_ascii_lowercase(), slug.to_ascii_lowercase());
    if !pod_core::construct::share::valid_handle(&lower_h)
        || !pod_core::public_copy::is_slug(&lower_s)
    {
        return None;
    }
    if !host.config.public_links() {
        return Some(paused().await);
    }
    let lower = lower_h == handle && lower_s == slug;
    let named = Named::Short(lower_h.clone(), lower_s.clone());
    Some(match rest {
        None if lower => page_of(host.clone(), named, headers.clone()).await,
        None | Some("") => moved(format!("/{lower_h}/{lower_s}")),
        Some("view") if lower => view_of(host.clone(), named).await,
        Some("image") if lower => {
            image_of(host.clone(), named, uri.query().map(str::to_owned)).await
        }
        Some(row) if lower && protocol::public_rows::is_row_name(row) => {
            row_page(host.clone(), named, Some(row), headers.clone()).await
        }
        Some(_) => return None,
    })
}

/// A permanent redirect to `path` (the page's own address).
fn moved(path: String) -> Response {
    match HeaderValue::from_str(&path) {
        Ok(v) => {
            let mut r = StatusCode::PERMANENT_REDIRECT.into_response();
            r.headers_mut().insert(header::LOCATION, v);
            r
        }
        Err(_) => not_found(),
    }
}

/// `/s/<link>/` (or under `/web`): the link at its own address.
async fn link_slash(Path(link): Path<String>) -> Response {
    if !pod_core::public_copy::is_link(&link) {
        return not_found();
    }
    moved(format!("/s/{link}"))
}

/// `/@<handle>/<slug>` (the first form), `/@<handle>/<slug>/`,
/// `/%40<handle>/<slug>`, under `/web`, in any case: the short path as
/// `/<handle>/<slug>`.
async fn short_variant(Path((handle, slug)): Path<(String, String)>) -> Response {
    let (handle, slug) = (handle.to_ascii_lowercase(), slug.to_ascii_lowercase());
    if !pod_core::construct::share::valid_handle(&handle) || !pod_core::public_copy::is_slug(&slug)
    {
        return not_found();
    }
    moved(format!("/{handle}/{slug}"))
}

/// Where the host doesn't serve public links (`CONSTRUCT_PUBLIC_WEB`
/// unset): their addresses answer that sharing is paused, and the copies
/// stay stored for when it is back.
pub(crate) fn paused_routes() -> Router<Arc<Host>> {
    Router::new()
        .route("/s/{link}", get(paused))
        .route("/s/{link}/{*rest}", get(paused))
        .route("/@{handle}/{slug}", get(paused))
        .route("/@{handle}/{slug}/{*rest}", get(paused))
        .route("/ical/{file}", get(paused))
}

/// The page a public link answers while sharing is paused.
async fn paused() -> Response {
    let page = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <meta name=\"robots\" content=\"noindex\"><title>Sharing is paused</title>\
        <style>body{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 16px;\
        color:#222;background:#fafafa}@media(prefers-color-scheme:dark){body{color:#eee;background:#161616}}</style>\
        </head><body><h1>Sharing is paused on this server</h1>\
        <p>This page was shared from The Construct. Its owner's server isn't showing shared pages right now; \
        the link will work again when it does.</p></body></html>";
    let mut r = (StatusCode::SERVICE_UNAVAILABLE, page).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::RETRY_AFTER, HeaderValue::from_static("3600"));
    r
}

/// `GET /ical/<token>.ics`: a calendar feed's text as its pod last rendered
/// it (decision `ical-feeds`), for a calendar app that subscribed to it.
async fn calendar_feed(State(host): State<Arc<Host>>, Path(file): Path<String>) -> Response {
    use pod_core::construct::share::Shelf;
    let Some(token) = file
        .strip_suffix(".ics")
        .filter(|t| pod_core::ical::is_token(t))
        .map(str::to_owned)
    else {
        return not_found();
    };
    let shelf = crate::shelf::HostShelf::new(host.store.clone()).with_public_links(true);
    let found = host
        .blocking(move |_| shelf.calendar_by_token(&token))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(c) = found else {
        return not_found();
    };
    let mut r = c.ics.into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    r
}

/// How a visitor names a copy: its link, or a short path.
enum Named {
    Link(String),
    Short(String, String),
}

/// A live public copy by its link or short path.
async fn public_copy(
    host: &Arc<Host>,
    named: Named,
) -> Option<pod_core::construct::share::PublicCopy> {
    use pod_core::construct::share::Shelf;
    match &named {
        Named::Link(l) if !pod_core::public_copy::is_link(l) => return None,
        Named::Short(h, s)
            if !pod_core::construct::share::valid_handle(h)
                || !pod_core::public_copy::is_slug(s) =>
        {
            return None;
        }
        _ => {}
    }
    let shelf = crate::shelf::HostShelf::new(host.store.clone()).with_public_links(true);
    host.blocking(move |_| match named {
        Named::Link(l) => shelf.public_by_link(&l),
        Named::Short(h, s) => shelf.public_by_slug(&h, &s),
    })
    .await
    .ok()?
    .ok()?
}

/// Text for HTML.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `GET /s/<link>`: the web client's page for a visitor, titled with the
/// copy's title (a chat app's link preview shows it).
async fn public_page(
    State(host): State<Arc<Host>>,
    Path(link): Path<String>,
    headers: HeaderMap,
) -> Response {
    page_of(host, Named::Link(link), headers).await
}

async fn page_of(host: Arc<Host>, named: Named, headers: HeaderMap) -> Response {
    row_page(host, named, None, headers).await
}

/// The page, opening one row's screen when `row` names one (decision
/// `public-row-links`): titled with the row's title then the copy's. A name
/// no row has (a row renamed or gone) answers the page as it is, which
/// opens on the list.
async fn row_page(
    host: Arc<Host>,
    named: Named,
    row: Option<&str>,
    headers: HeaderMap,
) -> Response {
    let (Some(d), Some(c)) = (dir(&host), public_copy(&host, named).await) else {
        return not_found();
    };
    let Some((page, kind)) = file(d, "index.html").await else {
        return not_found();
    };
    let title = match row.and_then(|r| protocol::public_rows::row(&c.copy, r)) {
        Some(r) if !r.title.is_empty() => escape_html(&format!("{} · {}", r.title, c.title)),
        _ => escape_html(&c.title),
    };
    let page = String::from_utf8_lossy(&page).replace(
        "<title>The Construct</title>",
        &format!(
            "<title>{title}</title>\n  <meta property=\"og:title\" content=\"{title}\">\n  \
             <meta property=\"og:site_name\" content=\"The Construct\">\n  \
             <meta name=\"robots\" content=\"noindex\">"
        ),
    );
    served(page.into_bytes(), kind, Cache::Revalidate, &headers)
}

/// `GET /s/<link>/view`: the copy a visitor's page draws, unchanged
/// (`{format, layout, title, node, data, item?, params?, layouts?}`).
async fn public_view(State(host): State<Arc<Host>>, Path(link): Path<String>) -> Response {
    view_of(host, Named::Link(link)).await
}

async fn short_view(
    State(host): State<Arc<Host>>,
    Path((handle, slug)): Path<(String, String)>,
) -> Response {
    view_of(host, Named::Short(handle, slug)).await
}

async fn view_of(host: Arc<Host>, named: Named) -> Response {
    match public_copy(&host, named).await {
        Some(c) => {
            let mut r = axum::Json(c.copy).into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            r
        }
        None => not_found(),
    }
}

/// `GET /s/<link>/image?url=<url>`: an image the copy names, passed on
/// (only URLs that appear in the copy, so it is no open proxy).
async fn public_image(
    State(host): State<Arc<Host>>,
    Path(link): Path<String>,
    RawQuery(q): RawQuery,
) -> Response {
    image_of(host, Named::Link(link), q).await
}

async fn short_image(
    State(host): State<Arc<Host>>,
    Path((handle, slug)): Path<(String, String)>,
    RawQuery(q): RawQuery,
) -> Response {
    image_of(host, Named::Short(handle, slug), q).await
}

async fn image_of(host: Arc<Host>, named: Named, q: Option<String>) -> Response {
    let Some(c) = public_copy(&host, named).await else {
        return not_found();
    };
    let Some(url) = pod_core::image_proxy::url_param(q) else {
        return json_error(StatusCode::BAD_REQUEST, json!({ "error": "invalid_url" }));
    };
    let quoted = Value::String(url.to_string()).to_string();
    if !c.copy.to_string().contains(&quoted) {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "not_in_this_page" }),
        );
    }
    proxy_image(&host, &c.owner_pod, url).await
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
        "ico" => "image/x-icon",
        "png" => "image/png",
        _ => return None,
    };
    let bytes = tokio::fs::read(base.join(name)).await.ok()?;
    Some((bytes, kind))
}

/// How a file of the client may be cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cache {
    /// Its name or `?v=` changes with its content: kept until it expires.
    Immutable,
    /// Asked again on every load, answered `304` while it hasn't changed.
    Revalidate,
}

impl Cache {
    fn header(self) -> &'static str {
        match self {
            Cache::Immutable => "public, max-age=31536000, immutable",
            Cache::Revalidate => "no-cache",
        }
    }
}

/// A name the build gave a hash of its content (`app.<16 hex>.js`).
fn is_hashed(name: &str) -> bool {
    let mut parts = name.rsplit('.');
    let (Some(_ext), Some(hash), Some(stem)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !stem.is_empty()
        && hash.len() == 16
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The content's validator: a quoted hash of it.
fn etag(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let hex: String = d[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Whether the request's `If-None-Match` names `tag` (or `*`).
fn not_modified(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == tag || t == "*")
}

fn served(bytes: Vec<u8>, kind: &'static str, cache: Cache, req: &HeaderMap) -> Response {
    let tag = etag(&bytes);
    let mut r = if not_modified(req, &tag) {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        r
    } else {
        let mut r = Response::new(Body::from(bytes));
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
        r
    };
    let h = r.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&tag).expect("hex"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache.header()),
    );
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
async fn page(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    match dir(&host) {
        Some(d) => match file(d, "index.html").await {
            Some((b, k)) => served(b, k, Cache::Revalidate, &headers),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `GET /web/<file>`: the client's script, WebAssembly, style and icon; a hashed
/// name is kept for good.
async fn asset(
    State(host): State<Arc<Host>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let cache = if is_hashed(&name) {
        Cache::Immutable
    } else {
        Cache::Revalidate
    };
    match dir(&host) {
        Some(d) => match file(d, &name).await {
            Some((b, k)) => served(b, k, cache, &headers),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `GET /web/fonts/<file>[?v=<hash>]`: the renderer's fonts, fetched by the
/// client instead of built into it, with the build's hash of the fonts (kept
/// for good) or without (revalidated).
async fn font(
    State(host): State<Arc<Host>>,
    Path(name): Path<String>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let versioned = q
        .unwrap_or_default()
        .split('&')
        .any(|kv| kv.strip_prefix("v=").is_some_and(|v| !v.is_empty()));
    let cache = if versioned {
        Cache::Immutable
    } else {
        Cache::Revalidate
    };
    match dir(&host) {
        Some(d) => match file(&d.join("fonts"), &name).await {
            Some((b, k)) => served(b, k, cache, &headers),
            None => not_found(),
        },
        None => not_found(),
    }
}

/// `POST /web/session {code}` (with `x-construct-client: web`): a session
/// of the pod the code opens, with the sign-ins the code's session has
/// (decision `web-sessions-share-sign-ins`), as a cookie -> `{ok}`; 401 `pod_not_found`,
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
    // The browser starts signed in where the pod code is (decision
    // `web-sessions-share-sign-ins`): typing the code proved it's the owner.
    match pod_core::session::share_sign_ins(&open.pod, &session_hash(&code), &hash).await {
        Ok(n) if n > 0 => tracing::info!(sign_ins = n, "the browser got the pod code's sign-ins"),
        Ok(_) => {}
        Err(e) => tracing::warn!(status = %e.0, "the browser's session got no sign-ins"),
    }
    let added = host
        .blocking(move |h| {
            h.store
                .insert_web_credential(&pod_id, &hash, session_ttl_from_env())
        })
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
    let Ok(Some((pod_id, CredentialKind::Web))) = host.credential(&token).await else {
        return json_error(StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));
    };
    let Some(url) = pod_core::image_proxy::url_param(q) else {
        return json_error(StatusCode::BAD_REQUEST, json!({ "error": "invalid_url" }));
    };
    proxy_image(&host, &pod_id, url).await
}

/// Images one pod's pages and clients may pass through the proxies per
/// minute (`/web/image`, `/s/<link>/image` and the native clients'
/// `/v1/image`, counted together).
pub const IMAGES_PER_MINUTE: u32 = 120;

/// Whether `pod` may pass one more image through a proxy now: every attempt
/// counts, refused ones too.
pub(crate) fn image_allowed(host: &Host, pod: &str) -> bool {
    let ok = host.image_fetches.allow(pod);
    if !ok {
        pod_core::metrics::inc(pod_core::metrics::RATE_LIMITED, &[("limit", "web_image")]);
    }
    ok
}

/// A public `http(s)` image for `pod`'s page, passed on by
/// [`pod_core::image_proxy::proxy`], at most [`IMAGES_PER_MINUTE`] tries per
/// pod (429 `rate_limited`).
async fn proxy_image(host: &Host, pod: &str, url: reqwest::Url) -> Response {
    if !image_allowed(host, pod) {
        return crate::rate_limited();
    }
    pod_core::image_proxy::proxy(url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_names_are_the_builds() {
        for name in [
            "app.0123456789abcdef.js",
            "construct_web_bg.0123456789abcdef.wasm",
        ] {
            assert!(is_hashed(name), "{name}");
        }
        for name in [
            "app.js",
            "index.html",
            ".0123456789abcdef.js",
            "app.0123456789ABCDEF.js",
            "app.0123456789abcde.js",
            "app.0123456789abcdeg.js",
        ] {
            assert!(!is_hashed(name), "{name}");
        }
    }

    #[test]
    fn if_none_match_names_the_etag() {
        let tag = etag(b"page");
        let mut h = HeaderMap::new();
        assert!(!not_modified(&h, &tag));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("\"other\", W/{tag}")).unwrap(),
        );
        assert!(not_modified(&h, &tag));
        let r = served(b"page".to_vec(), "text/html", Cache::Revalidate, &h);
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(r.headers()[header::ETAG], tag.as_str());
    }
}
