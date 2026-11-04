//! The host's OAuth 2.1 authorization server for Claude (decision
//! `mcp-connector`): how a user connects Claude to their pod.
//!
//! - `GET /.well-known/oauth-protected-resource[/v1/mcp]` (RFC 9728): the
//!   MCP resource and this server as its authorization server.
//! - `GET /.well-known/oauth-authorization-server` (RFC 8414).
//! - `POST /oauth/register` (RFC 7591): a public client (no secret) with its
//!   exact redirect URIs (`https://…`, or `http://localhost|127.0.0.1…`).
//! - `GET /oauth/authorize`: a page where the user types their pod code;
//!   `POST /oauth/authorize`: checks it (rate-limited like the code screen)
//!   and redirects back with a single-use code (PKCE S256 required, 5 min).
//! - `POST /oauth/token`: the code (with its verifier) for an access token:
//!   a new `mcp` credential of that pod (`cmcp_…`), good only for the pod's
//!   `/v1/mcp` until revoked (`server pods revoke-mcp`).
//!
//! The access token does not expire and no refresh token is issued: the
//! credential is revocable, reaches nothing but the MCP endpoint, and MCP
//! clients must not assume refresh tokens.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Form, Json,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pod_core::json_error;
use pod_core::session::session_hash;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::db::{OAuthClient, OAuthCode};
use crate::{Host, TOKEN_PREFIX, client_keys, peer, rate_limited};

/// The prefix of every MCP credential.
pub const MCP_TOKEN_PREFIX: &str = "cmcp_";
/// The path of the MCP endpoint (on every pod).
pub const MCP_PATH: &str = "/v1/mcp";
/// The one scope.
pub const SCOPE: &str = "mcp";
/// How long an authorization code lives.
pub const CODE_TTL_SECS: i64 = 300;
const MAX_REDIRECT_URIS: usize = 5;
const MAX_URI_LEN: usize = 2048;

/// The host's public base URL: `CONSTRUCT_PUBLIC_URL`, else from the
/// request (`https` behind Fly or with `X-Forwarded-Proto: https`).
pub fn base_url(host: &Host, headers: &HeaderMap) -> String {
    if let Some(u) = &host.config.public_url {
        return u.trim_end_matches('/').to_owned();
    }
    let name = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let https = host.config.trust_fly_client_ip
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    format!("{}://{name}", if https { "https" } else { "http" })
}

/// The 401 for `/v1/mcp` without a usable credential: where to find how to
/// get one (RFC 9728 §5.1).
pub fn mcp_unauthorized(host: &Host, headers: &HeaderMap) -> Response {
    let meta = format!(
        "{}/.well-known/oauth-protected-resource",
        base_url(host, headers)
    );
    let mut r = json_error(
        StatusCode::UNAUTHORIZED,
        json!({ "error": "unauthorized", "detail": "connect Claude to your pod first" }),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "Bearer resource_metadata=\"{meta}\", scope=\"{SCOPE}\""
    )) {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    r
}

pub(crate) async fn protected_resource(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
) -> Response {
    let base = base_url(&host, &headers);
    Json(json!({
        "resource": format!("{base}{MCP_PATH}"),
        "authorization_servers": [base],
        "scopes_supported": [SCOPE],
        "bearer_methods_supported": ["header"],
        "resource_name": "The Construct pod",
    }))
    .into_response()
}

pub(crate) async fn authorization_server(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
) -> Response {
    let base = base_url(&host, &headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
        "authorization_response_iss_parameter_supported": true,
    }))
    .into_response()
}

/// A redirect URI a client may register: `https://…`, or plain HTTP to the
/// local machine (Claude Code's loopback); no fragment.
fn valid_redirect(u: &str) -> bool {
    if u.len() > MAX_URI_LEN || u.contains('#') || u.chars().any(char::is_whitespace) {
        return false;
    }
    if let Some(rest) = u.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    let Some(rest) = u.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    let name = match authority.rsplit_once(':') {
        Some((n, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => n,
        _ => authority,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

fn random_token(prefix: &str) -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random number generator");
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(buf))
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

/// `POST /oauth/register` (RFC 7591).
pub(crate) async fn register(State(host): State<Arc<Host>>, req: Request) -> Response {
    let key = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip).narrow;
    if !host.registrations.allow(&key) {
        return rate_limited();
    }
    let headers = req.headers().clone();
    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                "body too large",
            );
        }
    };
    let meta = match pod_core::parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(_) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                "expected JSON",
            );
        }
    };
    let uris: Vec<String> = meta["redirect_uris"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS || !uris.iter().all(|u| valid_redirect(u))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "1-5 redirect URIs, each https:// or http://localhost / 127.0.0.1, no fragment",
        );
    }
    let name: String = meta["client_name"]
        .as_str()
        .unwrap_or("An MCP client")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let client = OAuthClient {
        client_id: random_token("cc_"),
        name: if name.is_empty() {
            "An MCP client".into()
        } else {
            name
        },
        redirect_uris: uris,
    };
    let stored = client.clone();
    match host.blocking(move |h| h.store.insert_client(&stored)).await {
        Ok(Ok(())) => {}
        _ => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "store error",
            );
        }
    }
    tracing::info!(client = %client.name, "OAuth client registered");
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "client_id": client.client_id,
            "client_id_issued_at": pod_core::util::now_secs(),
            "client_name": client.name,
            "redirect_uris": client.redirect_uris,
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "scope": SCOPE,
        })),
    )
        .into_response()
}

/// The checked parameters of an authorization request.
struct AuthRequest {
    client: OAuthClient,
    redirect_uri: String,
    challenge: String,
    state: Option<String>,
}

/// Check an authorization request. `Err(r)`: answer with `r` (an error page
/// when the redirect URI can't be trusted, else a redirect error).
async fn check_request(
    host: &Arc<Host>,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
) -> Result<AuthRequest, Box<Response>> {
    let page_error = |msg: &str| {
        Box::new(page(
            None,
            &format!("This link can't connect to your pod: {msg}."),
            None,
        ))
    };
    let Some(client_id) = p.get("client_id").cloned() else {
        return Err(page_error("no client_id"));
    };
    let lookup = client_id.clone();
    let client = match host.blocking(move |h| h.store.client(&lookup)).await {
        Ok(Ok(Some(c))) => c,
        Ok(Ok(None)) => return Err(page_error("unknown client")),
        _ => return Err(page_error("a server error")),
    };
    let redirect_uri = match p.get("redirect_uri") {
        Some(u) if client.redirect_uris.contains(u) => u.clone(),
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        _ => return Err(page_error("the redirect URI isn't registered")),
    };
    let state = p.get("state").cloned();
    let back = |error: &str| {
        Box::new(redirect_with(
            &redirect_uri,
            &[("error", error)],
            state.as_deref(),
            &base_url(host, headers),
        ))
    };
    if p.get("response_type").map(String::as_str) != Some("code") {
        return Err(back("unsupported_response_type"));
    }
    let challenge = match (
        p.get("code_challenge"),
        p.get("code_challenge_method").map(String::as_str),
    ) {
        (Some(c), Some("S256")) if (43..=128).contains(&c.len()) => c.clone(),
        _ => return Err(back("invalid_request")),
    };
    if let Some(resource) = p.get("resource")
        && !resource_ok(host, headers, resource)
    {
        return Err(back("invalid_target"));
    }
    Ok(AuthRequest {
        client,
        redirect_uri,
        challenge,
        state,
    })
}

/// RFC 8707: a `resource` must be this host's MCP endpoint (or the host).
fn resource_ok(host: &Host, headers: &HeaderMap, resource: &str) -> bool {
    let base = base_url(host, headers);
    let r = resource.trim_end_matches('/');
    r == base || r == format!("{base}{MCP_PATH}")
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn redirect_with(
    uri: &str,
    params: &[(&str, &str)],
    state: Option<&str>,
    issuer: &str,
) -> Response {
    let mut q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", percent(v)))
        .collect();
    if let Some(s) = state {
        q.push(format!("state={}", percent(s)));
    }
    q.push(format!("iss={}", percent(issuer)));
    let sep = if uri.contains('?') { '&' } else { '?' };
    let location = format!("{uri}{sep}{}", q.join("&"));
    match HeaderValue::from_str(&location) {
        Ok(v) => (StatusCode::FOUND, [(header::LOCATION, v)]).into_response(),
        Err(_) => page(None, "This link can't connect to your pod.", None),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The authorize page: `form` = `(client name, hidden params, redirect
/// origin)` to show the code form; `message` above it.
fn page(
    form: Option<(&str, &HashMap<String, String>, &str)>,
    message: &str,
    error: Option<&str>,
) -> Response {
    let mut body = String::new();
    let mut form_action = "'self'".to_owned();
    if let Some((client, params, origin)) = form {
        form_action = format!("'self' {origin}");
        let hidden: String = [
            "client_id",
            "redirect_uri",
            "response_type",
            "code_challenge",
            "code_challenge_method",
            "state",
            "scope",
            "resource",
        ]
        .iter()
        .filter_map(|k| params.get(*k).map(|v| (k, v)))
        .map(|(k, v)| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(v)))
        .collect();
        body = format!(
            "<p><b>{}</b> wants to read your pod's apps and data sources, and to create and edit your \
             <b>custom apps</b>. It can't use your apps' accounts or change built-in apps.</p>\
             <p>Connecting gives access to <b>{}</b>. The name above is the one the app gave itself; \
             only connect if you started this from Claude and that is where it should go.</p>\
             {}\
             <form method=\"post\" action=\"/oauth/authorize\">{hidden}\
             <label>Your pod code<br><input name=\"pod_code\" type=\"password\" autocomplete=\"off\" \
             placeholder=\"cpod_…\" required></label>\
             <div class=\"row\"><button name=\"decision\" value=\"allow\">Connect</button>\
             <button name=\"decision\" value=\"deny\" formnovalidate class=\"secondary\">Cancel</button></div></form>\
             <p class=\"small\">You can disconnect it later with <code>server pods revoke-mcp</code>.</p>",
            esc(client),
            esc(destination(origin)),
            error
                .map(|e| format!("<p class=\"error\">{}</p>", esc(e)))
                .unwrap_or_default(),
        );
    }
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Connect to your pod</title><style>\
         body{{font:16px/1.5 system-ui,sans-serif;max-width:28rem;margin:3rem auto;padding:0 1rem;color:#1b1b1f}}\
         input{{width:100%;padding:.6rem;font:inherit;border:1px solid #79747e;border-radius:8px;box-sizing:border-box}}\
         button{{padding:.6rem 1.2rem;font:inherit;border-radius:20px;border:0;background:#6750a4;color:#fff;margin-right:.5rem}}\
         .secondary{{background:#e8def8;color:#1d192b}}.row{{margin-top:1rem}}.error{{color:#b3261e}}.small{{font-size:.85rem;color:#49454f}}\
         </style></head><body><h1>The Construct</h1><p>{}</p>{body}</body></html>",
        esc(message)
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

/// Who gets the code, for the consent page: the redirect's host (the
/// client's name is whatever it registered, so it can't be trusted alone).
fn destination(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, host)| host)
}

fn origin_of(uri: &str) -> String {
    match uri.split_once("://") {
        Some((scheme, rest)) => {
            format!("{scheme}://{}", rest.split(['/', '?']).next().unwrap_or(""))
        }
        None => String::new(),
    }
}

/// `GET /oauth/authorize`.
pub(crate) async fn authorize_page(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Query(p): Query<HashMap<String, String>>,
) -> Response {
    match check_request(&host, &headers, &p).await {
        Ok(r) => page(
            Some((&r.client.name, &p, &origin_of(&r.redirect_uri))),
            "Connect Claude to your pod",
            None,
        ),
        Err(r) => *r,
    }
}

/// `POST /oauth/authorize` (the form).
pub(crate) async fn authorize_submit(State(host): State<Arc<Host>>, req: Request) -> Response {
    let key = client_keys(req.headers(), peer(&req), host.config.trust_fly_client_ip).narrow;
    let headers = req.headers().clone();
    let Form(p): Form<HashMap<String, String>> = match Form::from_request_parts_and_body(req).await
    {
        Ok(f) => f,
        Err(r) => return r,
    };
    let r = match check_request(&host, &headers, &p).await {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let issuer = base_url(&host, &headers);
    if p.get("decision").map(String::as_str) == Some("deny") {
        return redirect_with(
            &r.redirect_uri,
            &[("error", "access_denied")],
            r.state.as_deref(),
            &issuer,
        );
    }
    if !host.code_tries.allow(&key) {
        return page(
            Some((&r.client.name, &p, &origin_of(&r.redirect_uri))),
            "Connect Claude to your pod",
            Some("Too many tries. Wait a minute and try again."),
        );
    }
    let code_typed = p
        .get("pod_code")
        .map(|c| c.trim().to_owned())
        .unwrap_or_default();
    let pod = if code_typed.starts_with(TOKEN_PREFIX) && code_typed.len() <= 128 {
        host.pod_for_token(&code_typed).await.unwrap_or(None)
    } else {
        None
    };
    let Some(pod_id) = pod else {
        tracing::info!("OAuth: pod code rejected");
        return page(
            Some((&r.client.name, &p, &origin_of(&r.redirect_uri))),
            "Connect Claude to your pod",
            Some("That pod code didn't work. Check it and try again."),
        );
    };
    let code = random_token("");
    let row = OAuthCode {
        client_id: r.client.client_id.clone(),
        pod_id,
        redirect_uri: r.redirect_uri.clone(),
        challenge: r.challenge.clone(),
    };
    let hash = session_hash(&code);
    match host
        .blocking(move |h| h.store.insert_code(&hash, &row, CODE_TTL_SECS))
        .await
    {
        Ok(Ok(())) => {}
        _ => {
            return page(
                None,
                "Something went wrong on the server. Please try again.",
                None,
            );
        }
    }
    tracing::info!(client = %r.client.name, "OAuth: pod connected; code issued");
    redirect_with(
        &r.redirect_uri,
        &[("code", &code)],
        r.state.as_deref(),
        &issuer,
    )
}

/// `POST /oauth/token` (form-encoded, RFC 6749 §4.1.3 with PKCE).
pub(crate) async fn token(State(host): State<Arc<Host>>, req: Request) -> Response {
    let headers = req.headers().clone();
    let Form(p): Form<HashMap<String, String>> = match Form::from_request_parts_and_body(req).await
    {
        Ok(f) => f,
        Err(_) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected a form body",
            );
        }
    };
    if p.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code",
        );
    }
    let (Some(code), Some(verifier), Some(client_id)) =
        (p.get("code"), p.get("code_verifier"), p.get("client_id"))
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code, code_verifier and client_id are required",
        );
    };
    if let Some(resource) = p.get("resource")
        && !resource_ok(&host, &headers, resource)
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "not this server's MCP endpoint",
        );
    }
    let hash = session_hash(code);
    let found = match host.blocking(move |h| h.store.take_code(&hash)).await {
        Ok(Ok(f)) => f,
        _ => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "store error",
            );
        }
    };
    let bad = || {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "the code is unknown, used, expired or not yours",
        )
    };
    let Some(c) = found else { return bad() };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let redirect_ok = p.get("redirect_uri").is_none_or(|u| *u == c.redirect_uri);
    if c.client_id != *client_id || challenge != c.challenge || !redirect_ok {
        return bad();
    }
    let lookup = c.client_id.clone();
    let label = match host.blocking(move |h| h.store.client(&lookup)).await {
        Ok(Ok(Some(client))) => client.name,
        _ => "An MCP client".into(),
    };
    let access = random_token(MCP_TOKEN_PREFIX);
    let access_hash = session_hash(&access);
    let pod_id = c.pod_id.clone();
    let stored = label.clone();
    match host
        .blocking(move |h| {
            h.store
                .insert_mcp_credential(&pod_id, &access_hash, &stored)
        })
        .await
    {
        Ok(Ok(())) => {}
        _ => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "store error",
            );
        }
    }
    tracing::info!(client = %label, "OAuth: MCP credential issued");
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "access_token": access, "token_type": "Bearer", "scope": SCOPE })),
    )
        .into_response()
}

/// `Form` from a whole request (the handlers also need its headers).
trait FormFromRequest: Sized {
    async fn from_request_parts_and_body(req: Request) -> Result<Self, Response>;
}

impl FormFromRequest for Form<HashMap<String, String>> {
    async fn from_request_parts_and_body(req: Request) -> Result<Self, Response> {
        use axum::extract::FromRequest;
        Form::<HashMap<String, String>>::from_request(req, &())
            .await
            .map_err(IntoResponse::into_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris() {
        for ok in [
            "https://claude.ai/api/mcp/auth_callback",
            "http://localhost:33418/callback",
            "http://127.0.0.1/cb",
        ] {
            assert!(valid_redirect(ok), "{ok}");
        }
        for bad in [
            "http://evil.example/cb",
            "https://x/cb#frag",
            "javascript:alert(1)",
            "https:///nohost",
            "http://localhost.evil.example/cb",
            "ftp://x",
        ] {
            assert!(!valid_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn redirects_carry_state_and_issuer() {
        let r = redirect_with(
            "https://c.example/cb?x=1",
            &[("code", "a b")],
            Some("s&t"),
            "https://pod.example",
        );
        assert_eq!(
            r.headers()[header::LOCATION],
            "https://c.example/cb?x=1&code=a%20b&state=s%26t&iss=https%3A%2F%2Fpod.example"
        );
    }
}
