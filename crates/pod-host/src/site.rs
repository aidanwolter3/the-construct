//! The owner's own website on another origin (decision `site-origins`,
//! `CONSTRUCT_SITE_ORIGINS`): a static site (`https://www.example.com`)
//! loads the web client from this server (`/web/mount.js`) and draws a
//! pod or a public link in its own page, talking to this server across
//! origins.
//!
//! - **CORS** ([`cors`]): requests whose `Origin` is a listed site get
//!   `Access-Control-Allow-Origin` (never `-Credentials`: no cookie crosses),
//!   and their preflights are answered here. Everything else is as before.
//! - **Sign-in** ([`sign_in_page`], [`sign_in_submit`], [`token`]): the site
//!   sends the browser to `GET /web/site`, where the pod code is typed on
//!   this server's own page, never the site's. The form redirects back to the
//!   site with a single-use code (5 minutes, PKCE S256), which the site's
//!   script trades at `POST /web/site/token` for a browser session: the same
//!   `web` credential the cookie holds, here sent as a bearer. It ends with
//!   `POST /web/logout` or after `CONSTRUCT_SESSION_TTL_DAYS`.
//!
//! The codes share `oauth_codes` with the OAuth server, under a client id
//! that starts with [`CLIENT_PREFIX`], which the OAuth token endpoint
//! refuses (a site's code never becomes an MCP credential) and which is the
//! only kind [`token`] takes.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Form, Json,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pod_core::session::session_hash;
use pod_core::{json_error, metrics, parse_json_body};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::db::OAuthCode;
use crate::oauth::FormFromRequest;
use crate::{Host, TOKEN_PREFIX, client_keys, peer};

/// The client id of a site's sign-in codes: `site:<origin>`.
pub const CLIENT_PREFIX: &str = "site:";
/// The sign-in page and its form.
pub const SIGN_IN_PATH: &str = "/web/site";
/// Where the site trades its code for a session.
pub const TOKEN_PATH: &str = "/web/site/token";
/// The query parameter the site gets its code in (`code` is the upstream
/// sign-ins' own, decision `web-upstream-sign-in`).
pub const CODE_PARAM: &str = "construct_code";

/// The headers a site's page may send: the web client's own.
const ALLOWED_HEADERS: &str = "authorization, content-type, x-construct-client, \
     x-construct-features, x-construct-device, x-construct-build, x-construct-device-name";

/// `CONSTRUCT_SITE_ORIGINS`: the sites that may load the web client and
/// talk to this server (`https://host[:port]`, or `http://localhost` and
/// `http://127.0.0.1` for development). Empty: none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteOrigins(pub Vec<String>);

impl SiteOrigins {
    /// Comma-separated; an entry that isn't an origin is left out, with a
    /// warning.
    pub fn from_env() -> Self {
        let raw = std::env::var("CONSTRUCT_SITE_ORIGINS").unwrap_or_default();
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Self {
        let mut out = Vec::new();
        for o in raw.split(',').map(str::trim).filter(|o| !o.is_empty()) {
            let o = o.trim_end_matches('/').to_ascii_lowercase();
            if valid_origin(&o) {
                if !out.contains(&o) {
                    out.push(o);
                }
            } else {
                tracing::warn!(origin = %o, "CONSTRUCT_SITE_ORIGINS: not an origin; left out");
            }
        }
        Self(out)
    }

    pub fn allows(&self, origin: &str) -> bool {
        self.0.iter().any(|o| o == origin)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `https://<host>[:port]` (a DNS name), or `http://localhost[:port]` and
/// `http://127.0.0.1[:port]`; lowercase, no path.
pub fn valid_origin(o: &str) -> bool {
    let (secure, rest) = if let Some(r) = o.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = o.strip_prefix("http://") {
        (false, r)
    } else {
        return false;
    };
    let (name, port) = match rest.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (rest, None),
    };
    if let Some(p) = port
        && (!(1..=5).contains(&p.len()) || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    if !secure {
        return name == "localhost" || name == "127.0.0.1";
    }
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= 2
        && name.len() <= 253
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The request's `Origin` when it is a listed site.
fn site_origin<'a>(host: &Host, headers: &'a HeaderMap) -> Option<&'a str> {
    let o = headers.get(header::ORIGIN)?.to_str().ok()?;
    host.config.sites.allows(o).then_some(o)
}

/// Paths a site's script never needs: the OAuth server and the cookie's
/// sign-in, which stay same-origin only.
fn cors_excluded(path: &str) -> bool {
    path.starts_with("/oauth/") || path == "/web/session"
}

/// The CORS layer: a listed site's preflight is answered here (204, without
/// reaching a route), and its other requests get
/// `Access-Control-Allow-Origin` on the answer. No credentials: the site's
/// requests carry a bearer, never this server's cookie.
pub(crate) async fn cors(
    State(host): State<Arc<Host>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let origin = site_origin(&host, req.headers())
        .filter(|_| !cors_excluded(req.uri().path()))
        .and_then(|o| HeaderValue::from_str(o).ok());
    let Some(origin) = origin else {
        return next.run(req).await;
    };
    if req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut r = StatusCode::NO_CONTENT.into_response();
        let h = r.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, HEAD, POST, PUT, PATCH, DELETE"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOWED_HEADERS),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        h.append(header::VARY, HeaderValue::from_static("Origin"));
        return r;
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    r
}

/// What a sign-in asks for, checked.
struct Ask {
    origin: String,
    /// The site's page to come back to: a path.
    ret: String,
    state: String,
    challenge: String,
}

impl Ask {
    fn from(host: &Host, p: &HashMap<String, String>) -> Option<Ask> {
        let get = |k: &str| p.get(k).map(|v| v.trim().to_owned());
        let origin = get("origin")?;
        let ret = get("return")?;
        let state = get("state")?;
        let challenge = get("challenge")?;
        let b64url = |s: &str| {
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        let ok = host.config.sites.allows(&origin)
            && valid_return(&ret)
            && (16..=128).contains(&state.len())
            && b64url(&state)
            && challenge.len() == 43
            && b64url(&challenge);
        ok.then_some(Ask {
            origin,
            ret,
            state,
            challenge,
        })
    }

    fn redirect_uri(&self) -> String {
        format!("{}{}", self.origin, self.ret)
    }

    fn hidden(&self) -> String {
        [
            ("origin", &self.origin),
            ("return", &self.ret),
            ("state", &self.state),
            ("challenge", &self.challenge),
        ]
        .iter()
        .map(|(k, v)| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(v)))
        .collect()
    }
}

/// A path on the site: `/`, then letters, digits and `/._-`, no `//`.
fn valid_return(r: &str) -> bool {
    r.starts_with('/')
        && !r.contains("//")
        && r.len() <= 200
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The site's host name, as the page names it.
fn site_name(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, h)| h)
}

/// The sign-in page: the code form for `ask` (with `error` above it), or
/// only `message` for a request that can't be served.
fn page(ask: Option<&Ask>, message: &str, error: Option<&str>) -> Response {
    let mut form_action = "'self'".to_owned();
    let body = match ask {
        Some(a) => {
            form_action = format!("'self' {}", a.origin);
            let site = esc(site_name(&a.origin));
            format!(
                "<p>Open your pod on <b>{site}</b>.</p>\
                 <p class=\"small\">Your pod code stays on this page. {site} gets a session for this \
                 browser, which ends when you log out there.</p>{}\
                 <form method=\"post\" action=\"{SIGN_IN_PATH}\">{}\
                 <label>Your pod code<br><input name=\"pod_code\" type=\"password\" \
                 autocomplete=\"current-password\" placeholder=\"cpod_…\" required autofocus></label>\
                 <div class=\"row\"><button>Open my pod</button></div></form>",
                error
                    .map(|e| format!("<p class=\"error\">{}</p>", esc(e)))
                    .unwrap_or_default(),
                a.hidden(),
            )
        }
        None => format!("<p>{}</p>", esc(message)),
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Open your pod</title><style>\
         body{{font:16px/1.5 system-ui,sans-serif;max-width:28rem;margin:3rem auto;padding:0 1rem;color:#1b1b1f}}\
         input{{width:100%;padding:.6rem;font:inherit;border:1px solid #79747e;border-radius:8px;box-sizing:border-box}}\
         button{{padding:.6rem 1.2rem;font:inherit;border-radius:20px;border:0;background:#6750a4;color:#fff}}\
         .row{{margin-top:1rem}}.error{{color:#b3261e}}.small{{font-size:.85rem;color:#49454f}}\
         </style></head><body><h1>The Construct</h1>{body}</body></html>"
    );
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; form-action {form_action}; frame-ancestors 'none'"
    );
    let mut r = Html(html).into_response();
    if let Ok(v) = HeaderValue::from_str(&csp) {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

const BAD_REQUEST: &str =
    "This sign-in link isn't one this server accepts. Go back to the site and try again.";

/// `GET /web/site?origin=&return=&state=&challenge=`: the code form.
pub(crate) async fn sign_in_page(
    State(host): State<Arc<Host>>,
    Query(p): Query<HashMap<String, String>>,
) -> Response {
    match Ask::from(&host, &p) {
        Some(a) => page(Some(&a), "", None),
        None => {
            let mut r = page(None, BAD_REQUEST, None);
            *r.status_mut() = StatusCode::BAD_REQUEST;
            r
        }
    }
}

/// `POST /web/site` (the form): the pod code, checked and rate-limited like
/// the code screen, for a redirect to the site with a single-use code.
pub(crate) async fn sign_in_submit(State(host): State<Arc<Host>>, req: Request) -> Response {
    let keys = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip);
    let Form(p): Form<HashMap<String, String>> = match Form::from_request_parts_and_body(req).await
    {
        Ok(f) => f,
        Err(r) => return r,
    };
    let Some(ask) = Ask::from(&host, &p) else {
        let mut r = page(None, BAD_REQUEST, None);
        *r.status_mut() = StatusCode::BAD_REQUEST;
        return r;
    };
    if !host.code_tries.allow(&keys) {
        metrics::inc(metrics::RATE_LIMITED, &[("limit", "site_pod_code")]);
        return page(
            Some(&ask),
            "",
            Some("Too many tries. Wait a minute and try again."),
        );
    }
    let typed = p
        .get("pod_code")
        .map(|c| c.trim().to_owned())
        .unwrap_or_default();
    let pod = if typed.starts_with(TOKEN_PREFIX) && typed.len() <= 128 {
        host.pod_for_token(&typed).await.unwrap_or(None)
    } else {
        None
    };
    let Some(pod_id) = pod else {
        metrics::inc(metrics::AUTH_FAILURES, &[("kind", "site_pod_code")]);
        return page(
            Some(&ask),
            "",
            Some("That code didn't open a pod. Check it and try again."),
        );
    };
    let code = random_code();
    let row = OAuthCode {
        client_id: format!("{CLIENT_PREFIX}{}", ask.origin),
        pod_id,
        redirect_uri: ask.redirect_uri(),
        challenge: ask.challenge.clone(),
    };
    let hash = session_hash(&code);
    let stored = host
        .blocking(move |h| {
            h.store
                .insert_code(&hash, &row, crate::oauth::CODE_TTL_SECS)
        })
        .await;
    if !matches!(stored, Ok(Ok(()))) {
        return page(
            None,
            "Something went wrong on the server. Please try again.",
            None,
        );
    }
    tracing::info!(site = %ask.origin, "a site's sign-in: code issued");
    let to = format!(
        "{}?{CODE_PARAM}={code}&state={}",
        ask.redirect_uri(),
        ask.state
    );
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(&to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn random_code() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(buf)
}

/// `POST /web/site/token {code, verifier}` from a listed site's page (its
/// `Origin` must be the one the code was made for): a browser session of the
/// pod, with the pod code's sign-ins (decision `web-sessions-share-sign-ins`)
/// -> `{session_token, expires_in}`; 400 `invalid_grant` for a code that is
/// unknown, used, expired, another site's or another verifier's.
pub(crate) async fn token(State(host): State<Arc<Host>>, req: Request) -> Response {
    let Some(origin) = site_origin(&host, req.headers()).map(str::to_owned) else {
        return json_error(StatusCode::FORBIDDEN, json!({ "error": "not_a_site" }));
    };
    let headers = req.headers().clone();
    let Ok(body) = axum::body::to_bytes(req.into_body(), 16 * 1024).await else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request" }),
        );
    };
    let v = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Some(code), Some(verifier)) = (v["code"].as_str(), v["verifier"].as_str()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request" }),
        );
    };
    let hash = session_hash(code);
    let found = match host.blocking(move |h| h.store.take_code(&hash)).await {
        Ok(Ok(f)) => f,
        _ => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "host_error" }),
            );
        }
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let Some(c) = found
        .filter(|c| c.client_id == format!("{CLIENT_PREFIX}{origin}") && c.challenge == challenge)
    else {
        metrics::inc(metrics::AUTH_FAILURES, &[("kind", "site_code")]);
        return json_error(StatusCode::BAD_REQUEST, json!({ "error": "invalid_grant" }));
    };
    let pod_id = c.pod_id.clone();
    let pinned = match host
        .blocking(move |h| h.store.pod_token_hash(&pod_id))
        .await
    {
        Ok(Ok(h)) => h,
        _ => None,
    };
    let ttl = pod_core::session::session_ttl_from_env();
    match crate::web::new_browser_session(&host, &c.pod_id, pinned.as_deref(), ttl).await {
        Ok(session) => {
            tracing::info!(site = %origin, "a site opened a pod");
            let mut r =
                Json(json!({ "session_token": session, "expires_in": ttl })).into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(r) => *r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_and_https_but_for_localhost() {
        for o in [
            "https://www.example.com",
            "https://example.com:8443",
            "http://localhost:8000",
            "http://127.0.0.1",
        ] {
            assert!(valid_origin(o), "{o}");
        }
        for o in [
            "http://example.com",
            "https://example.com/",
            "https://example.com/path",
            "https://localhost",
            "https://*.example.com",
            "https://exa_mple.com",
            "https://example.com:",
            "https://-a.example.com",
            "example.com",
            "https://user@example.com",
        ] {
            assert!(!valid_origin(o), "{o}");
        }
        let s = SiteOrigins::parse(" https://WWW.Example.com/ , nope, https://www.example.com");
        assert_eq!(s.0, vec!["https://www.example.com".to_owned()]);
        assert!(s.allows("https://www.example.com"));
        assert!(!s.allows("https://example.com"));
    }

    #[test]
    fn returns_are_paths_on_the_site() {
        for r in ["/", "/construct/", "/a/b.html"] {
            assert!(valid_return(r), "{r}");
        }
        for r in [
            "",
            "construct",
            "//evil.example",
            "/a?b=c",
            "/a#b",
            "/a\"b",
            "/a//b",
        ] {
            assert!(!valid_return(r), "{r}");
        }
    }
}
