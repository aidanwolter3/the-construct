//! A fake Pixelfed (Mastodon-compatible) upstream for local development and tests.
//!
//! Serves the fixtures in `fixtures/pixelfed/` with in-memory state: OAuth app registration, an
//! authorization-code + PKCE (S256) flow, bearer-token checks, the home timeline,
//! favourite/bookmark toggles and static media under `/storage/...`. Every `{{BASE}}` in the fixture
//! JSON is replaced by the configured public URL when the fixtures are loaded.

// Handlers return a ready `Response` as the error of their validation helpers.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The placeholder in fixture JSON that is replaced with the public base URL.
pub const BASE_PLACEHOLDER: &str = "{{BASE}}";

/// Authorization codes expire after this long.
const CODE_TTL: Duration = Duration::from_secs(600);

/// `fixtures/pixelfed` in this repository.
pub fn default_fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pixelfed")
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding `timeline_home.json`, `account_verify_credentials.json` and `media/`.
    pub fixtures_dir: PathBuf,
    /// Base URL (no trailing slash) used in every link the fake emits.
    pub public_url: String,
    /// Approve `GET /oauth/authorize` immediately with a 302 (as if the user clicked Authorize).
    pub auto_approve: bool,
    /// A pre-seeded valid access token (test hook).
    pub seed_token: Option<String>,
}

#[derive(Debug)]
struct OAuthApp {
    name: String,
    secret: String,
    redirect_uris: Vec<String>,
}

#[derive(Debug)]
struct AuthCode {
    client_id: String,
    redirect_uri: String,
    scope: String,
    challenge: String,
    issued: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    apps: HashMap<String, OAuthApp>,
    next_app_id: u64,
    codes: HashMap<String, AuthCode>,
    /// access token -> client_id
    tokens: HashMap<String, String>,
    /// Every status by id (timeline entries and the statuses they reblog), with `reblog` set to null.
    statuses: HashMap<String, Value>,
    /// Outer status id -> reblogged status id.
    reblog_of: HashMap<String, String>,
    /// Home timeline order (outer ids, newest first).
    timeline: Vec<String>,
}

impl Inner {
    /// The status JSON with its current state, with the reblogged status nested.
    fn render(&self, id: &str) -> Option<Value> {
        let mut status = self.statuses.get(id)?.clone();
        if let Some(inner) = self.reblog_of.get(id) {
            status["reblog"] = self.render(inner).unwrap_or(Value::Null);
        }
        Some(status)
    }

    /// Mastodon semantics: acting on a reblog acts on the reblogged status.
    fn target_id(&self, id: &str) -> Option<String> {
        let target = self.reblog_of.get(id).map_or(id, String::as_str);
        self.statuses
            .contains_key(target)
            .then(|| target.to_owned())
    }
}

pub struct AppState {
    inner: Mutex<Inner>,
    account: Value,
    media_dir: PathBuf,
    public_url: String,
    auto_approve: bool,
}

impl AppState {
    /// Loads the fixtures from `config.fixtures_dir`, replacing `{{BASE}}` with `config.public_url`.
    pub fn new(config: Config) -> std::io::Result<Self> {
        let public_url = config.public_url.trim_end_matches('/').to_owned();
        let load = |name: &str| -> std::io::Result<Value> {
            let path = config.fixtures_dir.join(name);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
            serde_json::from_str(&text.replace(BASE_PLACEHOLDER, &public_url))
                .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))
        };
        let timeline = load("timeline_home.json")?;
        let account = load("account_verify_credentials.json")?;

        let mut inner = Inner {
            next_app_id: 1,
            ..Inner::default()
        };
        let Value::Array(items) = timeline else {
            return Err(std::io::Error::other(
                "timeline_home.json must be a JSON array",
            ));
        };
        for mut status in items {
            let id = status_id(&status)?;
            if let Some(reblog) = status.get_mut("reblog").map(Value::take)
                && !reblog.is_null()
            {
                let inner_id = status_id(&reblog)?;
                inner.statuses.insert(inner_id.clone(), reblog);
                inner.reblog_of.insert(id.clone(), inner_id);
            }
            inner.statuses.insert(id.clone(), status);
            inner.timeline.push(id);
        }
        if let Some(token) = config.seed_token.filter(|t| !t.is_empty()) {
            inner.tokens.insert(token, "seed".to_owned());
        }
        Ok(Self {
            inner: Mutex::new(inner),
            account,
            media_dir: config.fixtures_dir.join("media"),
            public_url,
            auto_approve: config.auto_approve,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn status_id(status: &Value) -> std::io::Result<String> {
    status["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("status without a string id in timeline_home.json"))
}

pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/apps", post(create_app))
        .route(
            "/oauth/authorize",
            get(authorize_page).post(authorize_submit),
        )
        .route("/oauth/token", post(token))
        .route(
            "/api/v1/accounts/verify_credentials",
            get(verify_credentials),
        )
        .route("/api/v1/timelines/home", get(home_timeline))
        .route("/api/v1/statuses/{id}/{action}", post(status_action))
        .route("/storage/{*path}", get(media))
        .layer(middleware::from_fn(log_requests))
        .with_state(state)
}

async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let start = Instant::now();
    let resp = next.run(req).await;
    tracing::info!(
        "{method} {path} -> {} ({} ms)",
        resp.status().as_u16(),
        start.elapsed().as_millis()
    );
    resp
}

// ---------- helpers ----------

fn error_json(status: StatusCode, error: &str, description: &str) -> Response {
    let mut body = json!({ "error": error });
    if !description.is_empty() {
        body["error_description"] = json!(description);
    }
    (status, Json(body)).into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "Record not found" })),
    )
        .into_response()
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Parses a form-urlencoded or JSON body into string parameters. JSON arrays are joined with spaces
/// (e.g. `redirect_uris: [..]`), and form keys ending in `[]` are merged the same way.
fn parse_params(headers: &HeaderMap, body: &Bytes) -> Result<HashMap<String, String>, Response> {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        });
    let mut out: HashMap<String, String> = HashMap::new();
    let mut push = |key: &str, value: String| {
        let key = key.strip_suffix("[]").unwrap_or(key).to_owned();
        out.entry(key)
            .and_modify(|v| {
                v.push(' ');
                v.push_str(&value);
            })
            .or_insert(value);
    };
    if is_json {
        let map: Map<String, Value> = serde_json::from_slice(body).map_err(|e| {
            error_json(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid JSON body: {e}"),
            )
        })?;
        for (k, v) in map {
            match v {
                Value::Null => {}
                Value::String(s) => push(&k, s),
                Value::Array(items) => {
                    for item in items {
                        match item {
                            Value::String(s) => push(&k, s),
                            other => push(&k, other.to_string()),
                        }
                    }
                }
                other => push(&k, other.to_string()),
            }
        }
    } else {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_bytes(body).map_err(|e| {
            error_json(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid form body: {e}"),
            )
        })?;
        for (k, v) in pairs {
            push(&k, v);
        }
    }
    Ok(out)
}

fn param<'a>(params: &'a HashMap<String, String>, key: &str) -> &'a str {
    params.get(key).map_or("", |s| s.trim())
}

/// Checks `Authorization: Bearer <token>`; 401 otherwise.
fn require_token(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim());
    match token {
        Some(t) if state.lock().tokens.contains_key(t) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({ "error": "The access token is invalid" })),
        )
            .into_response()),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Base64url-no-pad SHA-256 of the verifier (RFC 7636 S256).
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn valid_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

// ---------- OAuth ----------

async fn create_app(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let name = param(&params, "client_name");
    let redirect_uris: Vec<String> = param(&params, "redirect_uris")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if name.is_empty() || redirect_uris.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "client_name and redirect_uris are required" })),
        )
            .into_response();
    }
    let scopes = match param(&params, "scopes") {
        "" => "read",
        s => s,
    };
    let website = params.get("website").filter(|w| !w.is_empty()).cloned();
    let mut inner = state.lock();
    let id = inner.next_app_id.to_string();
    inner.next_app_id += 1;
    let secret = random_token(30);
    tracing::info!(client_id = %id, name, redirect_uris = ?redirect_uris, scopes, "registered OAuth app");
    let body = json!({
        "id": id,
        "name": name,
        "website": website,
        "client_id": id,
        "client_secret": secret,
        "redirect_uri": redirect_uris.join("\n"),
        "vapid_key": "BCk-QqERU0q-CfYZjcuB6lnyyOYfJ2AifKqfeGIm7Z-HiTU5T9eTG5GxVA0_OH5mMlI4UkkDTpaZwozy0TzdZ2M=",
    });
    inner.apps.insert(
        id,
        OAuthApp {
            name: name.to_owned(),
            secret,
            redirect_uris,
        },
    );
    Json(body).into_response()
}

#[derive(Debug, Clone)]
struct AuthRequest {
    client_id: String,
    app_name: String,
    redirect_uri: String,
    scope: String,
    state: Option<String>,
    challenge: String,
}

fn validate_authorize(
    state: &AppState,
    params: &HashMap<String, String>,
) -> Result<AuthRequest, Response> {
    let bad = |desc: &str| Err(error_json(StatusCode::BAD_REQUEST, "invalid_request", desc));
    if param(params, "response_type") != "code" {
        return bad("response_type must be 'code'");
    }
    let client_id = param(params, "client_id");
    let redirect_uri = param(params, "redirect_uri");
    let inner = state.lock();
    let Some(app) = inner.apps.get(client_id) else {
        return Err(error_json(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "unknown client_id",
        ));
    };
    if !app.redirect_uris.iter().any(|u| u == redirect_uri) {
        return bad("redirect_uri does not match a registered redirect URI");
    }
    let challenge = param(params, "code_challenge");
    if challenge.is_empty() {
        return bad("code_challenge is required");
    }
    if param(params, "code_challenge_method") != "S256" {
        return bad("code_challenge_method must be S256");
    }
    Ok(AuthRequest {
        client_id: client_id.to_owned(),
        app_name: app.name.clone(),
        redirect_uri: redirect_uri.to_owned(),
        scope: match param(params, "scope") {
            "" => "read".to_owned(),
            s => s.to_owned(),
        },
        state: params.get("state").filter(|s| !s.is_empty()).cloned(),
        challenge: challenge.to_owned(),
    })
}

fn redirect_with(req: &AuthRequest, mut pairs: Vec<(&str, String)>) -> Response {
    if let Some(s) = &req.state {
        pairs.push(("state", s.clone()));
    }
    let query = serde_urlencoded::to_string(&pairs).unwrap_or_default();
    let sep = if req.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let location = format!("{}{sep}{query}", req.redirect_uri);
    match HeaderValue::from_str(&location) {
        Ok(v) => (StatusCode::FOUND, [(header::LOCATION, v)]).into_response(),
        Err(_) => error_json(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "bad redirect_uri",
        ),
    }
}

fn approve(state: &AppState, req: &AuthRequest) -> Response {
    let code = random_token(32);
    state.lock().codes.insert(
        code.clone(),
        AuthCode {
            client_id: req.client_id.clone(),
            redirect_uri: req.redirect_uri.clone(),
            scope: req.scope.clone(),
            challenge: req.challenge.clone(),
            issued: Instant::now(),
        },
    );
    tracing::info!(client_id = %req.client_id, redirect_uri = %req.redirect_uri, "authorization approved, code issued");
    redirect_with(req, vec![("code", code)])
}

async fn authorize_page(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let req = match validate_authorize(&state, &params) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if state.auto_approve || param(&params, "auto") == "1" {
        return approve(&state, &req);
    }
    let hidden: String = [
        ("response_type", "code"),
        ("client_id", req.client_id.as_str()),
        ("redirect_uri", req.redirect_uri.as_str()),
        ("scope", req.scope.as_str()),
        ("state", req.state.as_deref().unwrap_or("")),
        ("code_challenge", req.challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]
    .iter()
    .map(|(k, v)| {
        format!(
            "<input type=\"hidden\" name=\"{k}\" value=\"{}\">\n",
            html_escape(v)
        )
    })
    .collect();
    Html(format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>Authorize {name} - Fake Pixelfed</title></head>\n\
<body style=\"font-family: sans-serif; max-width: 28em; margin: 3em auto; padding: 0 1em\">\n\
<h1>Fake Pixelfed</h1>\n<p><b>{name}</b> wants to access your account <b>@{user}</b> with scopes \
<code>{scope}</code>.</p>\n<form method=\"post\" action=\"{base}/oauth/authorize\">\n{hidden}\
<button type=\"submit\" name=\"decision\" value=\"approve\">Authorize</button>\n\
<button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\n</form>\n</body></html>\n",
        name = html_escape(&req.app_name),
        user = html_escape(state.account["username"].as_str().unwrap_or("")),
        scope = html_escape(&req.scope),
        base = html_escape(&state.public_url),
    ))
    .into_response()
}

async fn authorize_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req = match validate_authorize(&state, &params) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if param(&params, "decision") == "deny" {
        tracing::info!(client_id = %req.client_id, "authorization denied");
        return redirect_with(&req, vec![("error", "access_denied".to_owned())]);
    }
    approve(&state, &req)
}

async fn token(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let invalid_grant = |desc: &str| {
        tracing::info!(reason = desc, "token request rejected");
        error_json(StatusCode::BAD_REQUEST, "invalid_grant", desc)
    };
    if param(&params, "grant_type") != "authorization_code" {
        return error_json(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code is supported",
        );
    }
    let client_id = param(&params, "client_id");
    let mut inner = state.lock();
    match inner.apps.get(client_id) {
        Some(app) if app.secret == param(&params, "client_secret") => {}
        _ => {
            tracing::info!(client_id, "token request rejected: invalid client");
            return error_json(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "Client authentication failed",
            );
        }
    }
    // Single use: the code is consumed by any exchange attempt, successful or not.
    let Some(code) = inner.codes.remove(param(&params, "code")) else {
        return invalid_grant("authorization code is invalid or was already used");
    };
    if code.client_id != client_id {
        return invalid_grant("authorization code was issued to another client");
    }
    if code.redirect_uri != param(&params, "redirect_uri") {
        return invalid_grant("redirect_uri does not match the authorization request");
    }
    if code.issued.elapsed() > CODE_TTL {
        return invalid_grant("authorization code has expired");
    }
    let verifier = param(&params, "code_verifier");
    if !valid_verifier(verifier) {
        return invalid_grant("code_verifier is missing or malformed");
    }
    if s256_challenge(verifier) != code.challenge {
        return invalid_grant("code_verifier does not match the code_challenge");
    }
    let access_token = random_token(32);
    inner
        .tokens
        .insert(access_token.clone(), client_id.to_owned());
    tracing::info!(client_id, scope = %code.scope, "access token issued");
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "scope": code.scope,
        "created_at": created_at,
    }))
    .into_response()
}

// ---------- API ----------

async fn verify_credentials(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = require_token(&state, &headers) {
        return r;
    }
    Json(state.account.clone()).into_response()
}

async fn home_timeline(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = require_token(&state, &headers) {
        return r;
    }
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 40);
    let max_id = params.get("max_id").and_then(|m| m.parse::<u128>().ok());
    let inner = state.lock();
    let ids: Vec<&String> = inner
        .timeline
        .iter()
        .filter(|id| max_id.is_none_or(|max| id.parse::<u128>().is_ok_and(|n| n < max)))
        .take(limit)
        .collect();
    let statuses: Vec<Value> = ids.iter().filter_map(|id| inner.render(id)).collect();
    let mut resp = Json(statuses).into_response();
    if ids.len() == limit
        && let Some(last) = ids.last()
        && let Ok(link) = HeaderValue::from_str(&format!(
            "<{}/api/v1/timelines/home?max_id={last}&limit={limit}>; rel=\"next\"",
            state.public_url
        ))
    {
        resp.headers_mut().insert(header::LINK, link);
    }
    resp
}

async fn status_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((id, action)): UrlPath<(String, String)>,
) -> Response {
    if let Err(r) = require_token(&state, &headers) {
        return r;
    }
    let (field, count_field, on) = match action.as_str() {
        "favourite" => ("favourited", Some("favourites_count"), true),
        "unfavourite" => ("favourited", Some("favourites_count"), false),
        "bookmark" => ("bookmarked", None, true),
        "unbookmark" => ("bookmarked", None, false),
        _ => return not_found(),
    };
    let mut inner = state.lock();
    let Some(target) = inner.target_id(&id) else {
        tracing::info!(status_id = %id, action, "unknown status");
        return not_found();
    };
    let status = inner.statuses.get_mut(&target).expect("target_id checked");
    let changed = status[field].as_bool() != Some(on);
    if changed {
        status[field] = json!(on);
        if let Some(cf) = count_field {
            let count = status[cf].as_i64().unwrap_or(0);
            status[cf] = json!(if on { count + 1 } else { (count - 1).max(0) });
        }
    }
    tracing::info!(
        status_id = %target,
        requested_id = %id,
        action,
        changed,
        favourited = %status["favourited"],
        favourites_count = %status["favourites_count"],
        bookmarked = %status["bookmarked"],
        "status updated"
    );
    Json(inner.render(&target)).into_response()
}

async fn media(State(state): State<Arc<AppState>>, UrlPath(path): UrlPath<String>) -> Response {
    let rel = Path::new(&path);
    if path.contains('\\') || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(bytes) = tokio::fs::read(state.media_dir.join(rel)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match rel
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        Body::from(bytes),
    )
        .into_response()
}
