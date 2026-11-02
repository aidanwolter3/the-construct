//! Pod HTTP routes for the Pixelfed adapter (auth, home feed, actions, media).
//!
//! Auth model (docs/DECISIONS.md D17): the pod exchanges the client's PKCE
//! code for the upstream token, stores it, and hands out a random pod session
//! token (`Authorization: Bearer <session>`); only SHA-256(session) is stored.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::upstream::{SCOPES, UpstreamError};
use super::{
    Account, MediaUrls, Status, hex, layout, normalize_account, normalize_comment,
    normalize_comments, normalize_timeline, post_state,
};
use crate::store::{PixelfedAccount, PixelfedApp, SignIn};
use crate::{ApiError, AppState, api_error, blocking, json_error, parse_json_body};

/// Path prefix of the media proxy.
pub const MEDIA_PATH: &str = "/v1/pixelfed/media";
/// Largest media body the proxy relays (25 MB).
pub const MAX_MEDIA_BYTES: u64 = 25 * 1024 * 1024;
/// Statuses fetched per home timeline request.
const TIMELINE_LIMIT: u32 = 20;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/auth/pixelfed/app", post(register_app))
        .route("/v1/auth/pixelfed/token", post(exchange_token))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/pixelfed/posts/like", post(like))
        .route("/v1/pixelfed/posts/bookmark", post(bookmark))
        .route("/v1/pixelfed/posts/comment", post(comment))
        .route("/v1/pixelfed/posts/{id}/comments", get(comments))
        .route("/v1/pixelfed/media/{key}", get(media))
}

// ---------- helpers ----------

/// SHA-256 hex of a session token (what the sessions table stores).
pub fn session_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn new_session_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(buf)
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| !t.is_empty())
}

pub(crate) fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

/// The stored upstream account if the request carries a valid pod session.
async fn signed_in_account(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<Option<PixelfedAccount>, ApiError> {
    let Some(token) = bearer(headers) else {
        return Ok(None);
    };
    let hash = session_hash(token);
    let instance = state.pixelfed.instance_url.clone();
    let ttl = state.pixelfed.session_ttl_secs;
    blocking(state, move |s| {
        if !s.session_valid(&hash, ttl)? {
            return Ok(None);
        }
        s.pixelfed_account(&instance)
    })
    .await
}

/// `<scheme>://<Host>/v1/pixelfed/media`, from the request so that clients
/// reaching the pod as e.g. 10.0.2.2 get URLs they can load.
fn proxy_base(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| {
            !h.is_empty()
                && h.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'[' | b']')
                })
        })
        .unwrap_or("localhost");
    let scheme = match headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
    {
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        _ => "http",
    };
    format!("{scheme}://{host}{MEDIA_PATH}")
}

async fn save_media_keys(state: &Arc<AppState>, media: MediaUrls) -> Result<(), ApiError> {
    if media.keys.is_empty() {
        return Ok(());
    }
    blocking(state, move |s| s.save_media_keys(&media.keys)).await
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn invalid_request(expected: Value) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request", "expected": expected }),
    )
}

fn instance_not_allowed(state: &AppState, got: &str) -> Response {
    tracing::warn!(
        instance_url = got,
        "auth request for a foreign instance rejected"
    );
    json_error(
        StatusCode::BAD_REQUEST,
        json!({
            "error": "instance_not_allowed",
            "instance_url": got,
            "allowed": state.pixelfed.instance_url,
        }),
    )
}

/// 502 for an unreachable upstream or one that answered 5xx.
fn bad_gateway(e: &UpstreamError) -> Response {
    tracing::warn!(error = %e, "Pixelfed upstream failure");
    let body = match e {
        UpstreamError::Unreachable(detail) => {
            json!({ "error": "upstream_unreachable", "detail": detail })
        }
        UpstreamError::Status(status, detail) => {
            json!({ "error": "upstream_error", "status": status.as_u16(), "detail": detail })
        }
    };
    json_error(StatusCode::BAD_GATEWAY, body)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The upstream rejected `stale.access_token` with 401: refresh it with the
/// stored refresh token (serialized; a concurrent caller that already
/// refreshed it gets the new token without a second upstream call).
/// `Ok(None)`: no usable refresh path (no refresh token, app gone, or the
/// upstream rejected the refresh), so the caller signs out. `Err`: the
/// upstream was unreachable / 5xx during the refresh (or the store failed).
async fn refresh_account(
    state: &Arc<AppState>,
    stale: &PixelfedAccount,
) -> Result<Option<PixelfedAccount>, UpstreamError> {
    let _guard = state.pixelfed_refresh.lock().await;
    let instance = state.pixelfed.instance_url.clone();
    let store_err = |e: ApiError| UpstreamError::Unreachable(format!("pod store: {e:?}"));
    let current = {
        let i = instance.clone();
        blocking(state, move |s| s.pixelfed_account(&i))
            .await
            .map_err(store_err)?
    };
    let Some(current) = current else {
        return Ok(None);
    };
    if current.access_token != stale.access_token {
        return Ok(Some(current)); // refreshed (or re-signed-in) meanwhile
    }
    let (Some(refresh), Some(redirect)) =
        (current.refresh_token.clone(), current.redirect_uri.clone())
    else {
        return Ok(None);
    };
    let app = {
        let i = instance.clone();
        blocking(state, move |s| s.pixelfed_app(&i, &redirect))
            .await
            .map_err(store_err)?
    };
    let Some(app) = app else {
        return Ok(None);
    };
    match state
        .upstream
        .refresh_token(&app.client_id, &app.client_secret, &refresh)
        .await
    {
        Ok(t) => {
            let fresh = PixelfedAccount {
                access_token: t.access_token,
                // Passport rotates it; keep the old one if none came back.
                refresh_token: t.refresh_token.or(Some(refresh)),
                expires_at: t.expires_in.map(|s| now_secs().saturating_add(s)),
                ..current
            };
            let (i, a) = (instance, fresh.clone());
            blocking(state, move |s| s.save_pixelfed_account(&i, &a))
                .await
                .map_err(store_err)?;
            tracing::info!(username = %fresh.username, "upstream access token refreshed");
            Ok(Some(fresh))
        }
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            tracing::warn!(status = status.as_u16(), %detail, "upstream token refresh rejected");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Run an upstream call with the stored token; on a 401, refresh the token
/// once and retry. A 401 that survives (no refresh possible, or the fresh
/// token is rejected too) is returned as `Status(401)`. Also returns the
/// access token of the last attempt (the one a surviving 401 rejected).
async fn with_token<T, F, Fut>(
    state: &Arc<AppState>,
    account: PixelfedAccount,
    call: F,
) -> (Result<T, UpstreamError>, String)
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, UpstreamError>>,
{
    let token = account.access_token.clone();
    match call(token.clone()).await {
        Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, detail)) => {
            match refresh_account(state, &account).await {
                Ok(Some(fresh)) => {
                    let token = fresh.access_token;
                    (call(token.clone()).await, token)
                }
                Ok(None) => (
                    Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, detail)),
                    token,
                ),
                Err(e) => (Err(e), token),
            }
        }
        other => (other, token),
    }
}

/// Redirect URIs the pod registers apps for: the client's loopback
/// (`http://127.0.0.1:<port>/callback`, `http://localhost:<port>/callback`),
/// `construct://oauth`, and exact `PIXELFED_EXTRA_REDIRECTS` entries.
fn redirect_allowed(uri: &str, extra: &[String]) -> bool {
    if uri == "construct://oauth" || extra.iter().any(|e| e == uri) {
        return true;
    }
    ["http://127.0.0.1:", "http://localhost:"]
        .iter()
        .filter_map(|prefix| uri.strip_prefix(prefix))
        .filter_map(|rest| rest.strip_suffix("/callback"))
        .any(|port| {
            (1..=5).contains(&port.len())
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|p| p > 0)
        })
}

/// Is this upstream error the token endpoint rejecting the client itself
/// (unknown client id / wrong secret)?
fn is_invalid_client(e: &UpstreamError) -> bool {
    matches!(e, UpstreamError::Status(s, detail)
        if *s == StatusCode::UNAUTHORIZED || (s.is_client_error() && detail.contains("invalid_client")))
}

// ---------- auth ----------

/// `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}`.
async fn register_app(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Some(instance_url), Some(redirect_uri)) = (
        str_field(&payload, "instance_url"),
        str_field(&payload, "redirect_uri"),
    ) else {
        return invalid_request(json!({ "instance_url": "string", "redirect_uri": "string" }));
    };
    if !state.pixelfed.accepts(instance_url) {
        return instance_not_allowed(&state, instance_url);
    }
    if !redirect_allowed(redirect_uri, &state.pixelfed.extra_redirects) {
        tracing::warn!(
            redirect_uri,
            "app registration for a disallowed redirect_uri rejected"
        );
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "redirect_uri_not_allowed",
                "redirect_uri": redirect_uri,
                "allowed": ["http://127.0.0.1:<port>/callback", "http://localhost:<port>/callback",
                            "construct://oauth", "PIXELFED_EXTRA_REDIRECTS entries"],
            }),
        );
    }
    let instance = state.pixelfed.instance_url.clone();
    let redirect = redirect_uri.to_owned();
    let cached = {
        let (i, r) = (instance.clone(), redirect.clone());
        match blocking(&state, move |s| s.pixelfed_app(&i, &r)).await {
            Ok(c) => c,
            Err(e) => return e.into_response(),
        }
    };
    // A cached app may be unknown upstream (instance reset, app deleted): the
    // authorize page would then fail with invalid_client before the pod sees
    // a token call. Check it with a client_credentials grant; only a definite
    // invalid_client drops it (unreachable / 5xx / 429 / other: keep it).
    let cached = match cached {
        Some(app) => match state
            .upstream
            .client_credentials(&app.client_id, &app.client_secret)
            .await
        {
            Ok(()) => Some(app),
            Err(e) if is_invalid_client(&e) => {
                tracing::warn!(client_id = %app.client_id, error = %e,
                    "cached Pixelfed app is unknown upstream; re-registering");
                let (i, r) = (instance.clone(), redirect.clone());
                if let Err(e) = blocking(&state, move |s| s.delete_pixelfed_app(&i, &r)).await {
                    return e.into_response();
                }
                None
            }
            Err(e) => {
                tracing::info!(client_id = %app.client_id, error = %e,
                    "could not validate the cached Pixelfed app; keeping it");
                Some(app)
            }
        },
        None => None,
    };
    let app = match cached {
        Some(app) => app,
        None => match state.upstream.register_app(&redirect).await {
            Ok(reg) => {
                let app = PixelfedApp {
                    client_id: reg.client_id,
                    client_secret: reg.client_secret,
                };
                let (i, r, a) = (instance.clone(), redirect.clone(), app.clone());
                if let Err(e) = blocking(&state, move |s| s.save_pixelfed_app(&i, &r, &a)).await {
                    return e.into_response();
                }
                tracing::info!(client_id = %app.client_id, redirect_uri = %redirect, "registered Pixelfed app");
                app
            }
            Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "app_registration_failed", "status": status.as_u16(), "detail": detail }),
                );
            }
            Err(e) => return bad_gateway(&e),
        },
    };
    Json(json!({
        "client_id": app.client_id,
        "scopes": SCOPES,
        "authorize_endpoint": format!("{}/oauth/authorize", state.pixelfed.public_url),
    }))
    .into_response()
}

/// `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}`.
async fn exchange_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let fields =
        ["instance_url", "code", "code_verifier", "redirect_uri"].map(|k| str_field(&payload, k));
    let [
        Some(instance_url),
        Some(code),
        Some(verifier),
        Some(redirect_uri),
    ] = fields
    else {
        return invalid_request(json!({
            "instance_url": "string", "code": "string", "code_verifier": "string", "redirect_uri": "string"
        }));
    };
    if !state.pixelfed.accepts(instance_url) {
        return instance_not_allowed(&state, instance_url);
    }
    let instance = state.pixelfed.instance_url.clone();
    let app = {
        let (i, r) = (instance.clone(), redirect_uri.to_owned());
        match blocking(&state, move |s| s.pixelfed_app(&i, &r)).await {
            Ok(Some(app)) => app,
            Ok(None) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "app_not_registered", "redirect_uri": redirect_uri,
                            "detail": "call POST /v1/auth/pixelfed/app first" }),
                );
            }
            Err(e) => return e.into_response(),
        }
    };
    let oauth_failed = |status: StatusCode, detail: String| {
        tracing::info!(status = status.as_u16(), %detail, "OAuth exchange rejected upstream");
        json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "oauth_exchange_failed", "status": status.as_u16(), "detail": detail }),
        )
    };
    let token = match state
        .upstream
        .exchange_code(
            &app.client_id,
            &app.client_secret,
            code,
            verifier,
            redirect_uri,
        )
        .await
    {
        Ok(t) => t,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            if status == StatusCode::UNAUTHORIZED {
                // invalid_client: the upstream forgot our app (e.g. it was reset);
                // re-register on the next /app call.
                let (i, r) = (instance.clone(), redirect_uri.to_owned());
                let _ = blocking(&state, move |s| s.delete_pixelfed_app(&i, &r)).await;
            }
            return oauth_failed(status, detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let account: Account = match state.upstream.verify_credentials(&token.access_token).await {
        Ok(v) => match Account::deserialize(&v) {
            Ok(a) => a,
            Err(e) => {
                return bad_gateway(&UpstreamError::Unreachable(format!(
                    "unexpected verify_credentials response: {e}"
                )));
            }
        },
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return oauth_failed(status, detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let mut media = MediaUrls::new(&proxy_base(&headers), &state.media_allow);
    let user = normalize_account(&account, &mut media);
    // Owner pinning (D17): only the owner's account may sign in, so another
    // user of the instance cannot replace the stored account.
    let cfg = &state.pixelfed;
    let not_owner = || {
        tracing::warn!(account_id = %account.id, username = ?user.get_str("username"),
            "Pixelfed sign-in by an account that does not own this pod rejected");
        json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "account_not_owner",
                    "detail": "this pod is pinned to another Pixelfed account" }),
        )
    };
    if cfg
        .owner_account_id
        .as_ref()
        .is_some_and(|owner| *owner != account.id)
    {
        return not_owner();
    }
    let replace_owner = cfg.owner_account_id.is_some() || cfg.allow_account_change;
    let ttl = cfg.session_ttl_secs;
    let stored = PixelfedAccount {
        access_token: token.access_token,
        account_id: account.id.clone(),
        username: user.get_str("username").unwrap_or_default().to_owned(),
        refresh_token: token.refresh_token,
        expires_at: token.expires_in.map(|s| now_secs().saturating_add(s)),
        redirect_uri: Some(redirect_uri.to_owned()),
    };
    let session = new_session_token();
    let hash = session_hash(&session);
    {
        let i = instance.clone();
        let result = blocking(&state, move |s| {
            s.sign_in_pixelfed(&i, &stored, &hash, replace_owner, ttl)
        })
        .await;
        match result {
            Ok(SignIn::Ok) => {}
            Ok(SignIn::NotOwner { .. }) => return not_owner(),
            Err(e) => return e.into_response(),
        }
    }
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(account_id = %account.id, username = ?user.get_str("username"), "Pixelfed sign-in: session created");
    Json(json!({
        "session_token": session,
        "account": {
            "id": account.id,
            "username": user.get("username"),
            "display_name": user.get("display_name"),
            "avatar_url": user.get("avatar_url"),
        },
    }))
    .into_response()
}

/// `POST /v1/auth/logout` (bearer): deletes the pod session. The stored
/// upstream token is kept (single tenant; other sessions may use it).
async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return unauthorized();
    };
    let hash = session_hash(token);
    match blocking(&state, move |s| s.delete_session(&hash)).await {
        Ok(true) => {
            tracing::info!("pod session deleted (logout)");
            Json(json!({ "ok": true })).into_response()
        }
        Ok(false) => unauthorized(),
        Err(e) => e.into_response(),
    }
}

// ---------- home feed ----------

/// Response header on a 401 from `GET /v1/workspaces/pixelfed_home`.
pub const SESSION_HEADER: &str = "x-construct-session";

/// `GET /v1/workspaces/pixelfed_home`. Without a bearer: 200 signed-out
/// bundle. With a bearer that no longer gets a feed (unknown session, no
/// stored account, or the upstream rejected the stored token): 401 with the
/// signed-out bundle as the body and `X-Construct-Session: invalid`, so the
/// client learns its session is stale (it clears it and refetches without
/// the bearer). The signed-in feed's layout is the latest layout version
/// (`X-Construct-Layout-Version`); the signed-out screen is not versioned.
pub async fn home_workspace(state: &Arc<AppState>, headers: &HeaderMap) -> Response {
    let had_bearer = bearer(headers).is_some();
    let signed_out = || {
        let body = Json(layout::signed_out(&state.pixelfed.instance_url));
        if had_bearer {
            (
                StatusCode::UNAUTHORIZED,
                [
                    (header::WWW_AUTHENTICATE, "Bearer"),
                    (header::HeaderName::from_static(SESSION_HEADER), "invalid"),
                ],
                body,
            )
                .into_response()
        } else {
            body.into_response()
        }
    };
    let account = match signed_in_account(state, headers).await {
        Ok(Some(a)) => a,
        Ok(None) => return signed_out(),
        Err(e) => return e.into_response(),
    };
    let username = account.username.clone();
    let (result, last_token) = with_token(state, account, |token| async move {
        state.upstream.home_timeline(&token, TIMELINE_LIMIT).await
    })
    .await;
    let statuses = match result {
        Ok(s) => s,
        Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, _)) => {
            // The upstream token was revoked or expired and could not be
            // refreshed: forget it, but only if it is still the stored one (a
            // sign-in or refresh may have replaced it meanwhile).
            let instance = state.pixelfed.instance_url.clone();
            let deleted = blocking(state, move |s| {
                s.delete_pixelfed_account_if_token(&instance, &last_token)
            })
            .await
            .unwrap_or(false);
            tracing::warn!(%username, deleted, "upstream rejected the stored token; signed out");
            return signed_out();
        }
        Err(e) => return bad_gateway(&e),
    };
    let mut media = MediaUrls::new(&proxy_base(headers), &state.media_allow);
    let timeline = normalize_timeline(&statuses, &mut media);
    if let Err(e) = save_media_keys(state, media).await {
        return e.into_response();
    }
    tracing::info!(
        posts = timeline.order.len(),
        entities = timeline.entities.len(),
        "pixelfed_home served"
    );
    let served = match crate::layouts::active_async(state, layout::WORKSPACE_ID).await {
        Ok(Some(v)) => v,
        Ok(None) => unreachable!("pixelfed_home has versions after the layout sync"),
        Err(e) => return e.into_response(),
    };
    let mut spec = layout::home_feed(timeline.entities, timeline.order);
    spec.workspace = served.layout;
    (crate::layouts::served_headers(served.version), Json(spec)).into_response()
}

// ---------- actions ----------

#[derive(Clone, Copy)]
enum Toggle {
    Like,
    Bookmark,
}

async fn like(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    toggle(state, headers, body, Toggle::Like).await
}

async fn bookmark(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    toggle(state, headers, body, Toggle::Bookmark).await
}

fn valid_status_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// `POST /v1/pixelfed/posts/{like,bookmark} {id, liked|bookmarked}` ->
/// 200 `{ok, id, is_liked, like_count, is_bookmarked}`. No session: 401;
/// upstream 4xx: same status (the client rolls back); upstream 5xx or
/// unreachable: 502 (the client retries).
async fn toggle(state: Arc<AppState>, headers: HeaderMap, body: Bytes, kind: Toggle) -> Response {
    let account = match signed_in_account(&state, &headers).await {
        Ok(Some(a)) => a,
        Ok(None) => return unauthorized(),
        Err(e) => return e.into_response(),
    };
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let flag = match kind {
        Toggle::Like => "liked",
        Toggle::Bookmark => "bookmarked",
    };
    let id = match &payload["id"] {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
    .filter(|id| valid_status_id(id));
    let (Some(id), Some(on)) = (id, payload[flag].as_bool()) else {
        return invalid_request(json!({ "id": "status id", flag: "bool" }));
    };
    let action = match (kind, on) {
        (Toggle::Like, true) => "favourite",
        (Toggle::Like, false) => "unfavourite",
        (Toggle::Bookmark, true) => "bookmark",
        (Toggle::Bookmark, false) => "unbookmark",
    };
    let (result, _) = with_token(&state, account, |token| {
        let (state, id) = (&state, &id);
        async move { state.upstream.status_action(&token, id, action).await }
    })
    .await;
    match result {
        Ok(v) => match Status::deserialize(&v) {
            Ok(status) => {
                let target = status.target();
                let st = post_state(target);
                tracing::info!(id = %target.id, action, is_liked = ?st.get_bool("is_liked"),
                    like_count = ?st.get_i64("like_count"), is_bookmarked = ?st.get_bool("is_bookmarked"),
                    "Pixelfed action applied");
                Json(json!({
                    "ok": true,
                    "id": target.id,
                    "is_liked": st.get("is_liked"),
                    "like_count": st.get("like_count"),
                    "is_bookmarked": st.get("is_bookmarked"),
                }))
                .into_response()
            }
            Err(e) => bad_gateway(&UpstreamError::Unreachable(format!(
                "unexpected status response: {e}"
            ))),
        },
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            tracing::info!(%id, action, status = status.as_u16(), %detail, "Pixelfed action rejected upstream");
            api_error(
                status,
                json!({ "error": "upstream_rejected", "status": status.as_u16(), "detail": detail }),
            )
            .into_response()
        }
        Err(e) => bad_gateway(&e),
    }
}

// ---------- comments ----------

/// Longest comment the pod forwards (Pixelfed's default limit).
pub const MAX_COMMENT_CHARS: usize = 500;

/// An upstream 4xx on a comments call, passed through with the pod's error shape
/// (`post_not_found` for 404).
fn comment_rejected(id: &str, status: StatusCode, detail: &str) -> Response {
    tracing::info!(%id, status = status.as_u16(), %detail, "Pixelfed comments call rejected upstream");
    let error = if status == StatusCode::NOT_FOUND {
        "post_not_found"
    } else {
        "upstream_rejected"
    };
    api_error(
        status,
        json!({ "error": error, "status": status.as_u16(), "detail": detail }),
    )
    .into_response()
}

/// `GET /v1/pixelfed/posts/{id}/comments` -> 200 `{post_id, comments: [...]}` in thread
/// order (see [`normalize_comments`]). Not cached by the client; read on demand. No session:
/// 401; upstream 4xx: same status (404 `post_not_found`); 5xx / unreachable: 502.
async fn comments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let account = match signed_in_account(&state, &headers).await {
        Ok(Some(a)) => a,
        Ok(None) => return unauthorized(),
        Err(e) => return e.into_response(),
    };
    let id = id.trim().to_owned();
    if !valid_status_id(&id) {
        return invalid_request(json!({ "id": "status id" }));
    }
    let own = account.account_id.clone();
    let (result, _) = with_token(&state, account, |token| {
        let (state, id) = (&state, &id);
        async move { state.upstream.status_context(&token, id).await }
    })
    .await;
    let context = match result {
        Ok(v) => v,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return comment_rejected(&id, status, &detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let mut media = MediaUrls::new(&proxy_base(&headers), &state.media_allow);
    let list = normalize_comments(&id, &context, &own, &mut media);
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(%id, comments = list.len(), "Pixelfed comments served");
    Json(json!({ "post_id": id, "comments": list })).into_response()
}

/// `POST /v1/pixelfed/posts/comment {id, text, client_id?}` -> 200 `{ok, id, comment}`:
/// posts `text` as a reply to post `id`. This is an outbox endpoint (the client's
/// optimistic `reply_count + 1` is rolled back on a 4xx): blank or too long text is 400,
/// no session 401, upstream 4xx the same status, 5xx / unreachable 502 (retried).
/// `client_id` (a client-made nonce, `[A-Za-z0-9_-]{8,64}`) becomes the upstream
/// `Idempotency-Key`, so a retry after a lost response does not post twice where the
/// upstream honours it.
async fn comment(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let account = match signed_in_account(&state, &headers).await {
        Ok(Some(a)) => a,
        Ok(None) => return unauthorized(),
        Err(e) => return e.into_response(),
    };
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let expected = || {
        invalid_request(json!({
            "id": "status id", "text": format!("1..={MAX_COMMENT_CHARS} characters"),
            "client_id": "optional nonce",
        }))
    };
    let id = match &payload["id"] {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
    .filter(|id| valid_status_id(id));
    let text = payload["text"].as_str().map(str::trim).unwrap_or("");
    let (Some(id), false) = (
        id,
        text.is_empty() || text.chars().count() > MAX_COMMENT_CHARS,
    ) else {
        return expected();
    };
    let key = match &payload["client_id"] {
        Value::Null => None,
        Value::String(k)
            if (8..=64).contains(&k.len())
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) =>
        {
            Some(format!("construct-{k}"))
        }
        _ => return expected(),
    };
    let own = account.account_id.clone();
    let (result, _) = with_token(&state, account, |token| {
        let (state, id, key) = (&state, &id, key.as_deref());
        async move { state.upstream.create_reply(&token, id, text, key).await }
    })
    .await;
    let created = match result {
        Ok(v) => v,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return comment_rejected(&id, status, &detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let status = match Status::deserialize(&created) {
        Ok(s) => s,
        Err(e) => {
            return bad_gateway(&UpstreamError::Unreachable(format!(
                "unexpected status response: {e}"
            )));
        }
    };
    let mut media = MediaUrls::new(&proxy_base(&headers), &state.media_allow);
    let comment = normalize_comment(&status, 0, &own, &mut media);
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(post = %id, comment = %status.id, "Pixelfed comment posted");
    Json(json!({ "ok": true, "id": id, "comment": comment })).into_response()
}

// ---------- media proxy ----------

/// `GET /v1/pixelfed/media/{key}`: streams a media/avatar URL the pod emitted.
/// No auth: keys are opaque hashes of URLs that are public upstream anyway.
async fn media(State(state): State<Arc<AppState>>, Path(key): Path<String>) -> Response {
    let not_found = || {
        json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "unknown_media_key" }),
        )
    };
    if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return not_found();
    }
    let url = match blocking(&state, move |s| s.media_url(&key)).await {
        Ok(Some(url)) => url,
        Ok(None) => return not_found(),
        Err(e) => return e.into_response(),
    };
    // Normalization only emits keys for allowlisted URLs, but keys stored by
    // an older pod (or before the allowlist changed) are checked again here.
    if !state.media_allow.allows(&url) {
        tracing::warn!(host = ?reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)),
            "media proxy refused a URL outside the allowlist");
        return json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "media_host_not_allowed" }),
        );
    }
    // URLs built with the public base (e.g. 10.0.2.2) are fetched via the
    // instance base the pod can reach.
    let cfg = &state.pixelfed;
    let fetch_url = match url.strip_prefix(&cfg.public_url) {
        Some(rest) if cfg.public_url != cfg.instance_url && rest.starts_with('/') => {
            format!("{}{rest}", cfg.instance_url)
        }
        _ => url,
    };
    let resp = match state.upstream.fetch_media(&fetch_url).await {
        Ok(r) => r,
        Err(UpstreamError::Status(StatusCode::NOT_FOUND | StatusCode::GONE, _)) => {
            return not_found();
        }
        Err(e) => return bad_gateway(&e),
    };
    // The URL came from upstream content (possibly a federated post), so the
    // response is served as inert media only: a non-media Content-Type (e.g.
    // text/html) becomes application/octet-stream, sniffing is off, and the
    // CSP sandboxes anything a browser might still render on the pod origin.
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .filter(|v| {
            v.to_str().is_ok_and(|t| {
                let t = t.trim_start().to_ascii_lowercase();
                (t.starts_with("image/") && !t.starts_with("image/svg"))
                    || t.starts_with("video/")
                    || t.starts_with("audio/")
            })
        })
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    let mut out = Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=86400"),
        )
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        )
        .header(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
    if resp.content_length().is_some_and(|n| n > MAX_MEDIA_BYTES) {
        tracing::warn!(
            length = resp.content_length(),
            "media larger than the proxy cap refused"
        );
        return json_error(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "media_too_large", "max_bytes": MAX_MEDIA_BYTES }),
        );
    }
    if let Some(v) = resp.headers().get(header::CONTENT_LENGTH) {
        out = out.header(header::CONTENT_LENGTH, v.clone());
    }
    out.body(Body::from_stream(capped_stream(resp, MAX_MEDIA_BYTES)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The response body as a stream that fails (aborting the client's
/// response) once more than `max` bytes have arrived, for upstreams that
/// send no or a wrong Content-Length.
fn capped_stream(
    resp: reqwest::Response,
    max: u64,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    futures_util::stream::unfold((Some(resp), 0u64), move |(resp, seen)| async move {
        let mut resp = resp?;
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let seen = seen.saturating_add(chunk.len() as u64);
                if seen > max {
                    tracing::warn!(max, "media body exceeded the proxy cap; aborted");
                    Some((
                        Err(std::io::Error::other("media exceeds the size cap")),
                        (None, seen),
                    ))
                } else {
                    Some((Ok(chunk), (Some(resp), seen)))
                }
            }
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e)), (None, seen))),
        }
    })
}
