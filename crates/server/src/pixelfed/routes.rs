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
    Account, MediaUrls, Status, hex, layout, normalize_account, normalize_timeline, post_state,
};
use crate::store::{PixelfedAccount, PixelfedApp};
use crate::{ApiError, AppState, api_error, blocking, json_error, parse_json_body};

/// Path prefix of the media proxy.
pub const MEDIA_PATH: &str = "/v1/pixelfed/media";
/// Statuses fetched per home timeline request.
const TIMELINE_LIMIT: u32 = 20;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/auth/pixelfed/app", post(register_app))
        .route("/v1/auth/pixelfed/token", post(exchange_token))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/pixelfed/posts/like", post(like))
        .route("/v1/pixelfed/posts/bookmark", post(bookmark))
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

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| !t.is_empty())
}

fn unauthorized() -> Response {
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
    blocking(state, move |s| {
        if !s.session_exists(&hash)? {
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
    let instance = state.pixelfed.instance_url.clone();
    let redirect = redirect_uri.to_owned();
    let cached = {
        let (i, r) = (instance.clone(), redirect.clone());
        match blocking(&state, move |s| s.pixelfed_app(&i, &r)).await {
            Ok(c) => c,
            Err(e) => return e.into_response(),
        }
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
        Ok(t) => t.access_token,
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
    let account: Account = match state.upstream.verify_credentials(&token).await {
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
    let mut media = MediaUrls::new(&proxy_base(&headers));
    let user = normalize_account(&account, &mut media);
    let stored = PixelfedAccount {
        access_token: token,
        account_id: account.id.clone(),
        username: user.get_str("username").unwrap_or_default().to_owned(),
    };
    let session = new_session_token();
    let hash = session_hash(&session);
    {
        let i = instance.clone();
        let result = blocking(&state, move |s| {
            s.save_pixelfed_account(&i, &stored)?;
            s.create_session(&hash)
        })
        .await;
        if let Err(e) = result {
            return e.into_response();
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
pub const SESSION_HEADER: &str = "x-canvas-session";

/// `GET /v1/workspaces/pixelfed_home`. Without a bearer: 200 signed-out
/// bundle. With a bearer that no longer gets a feed (unknown session, no
/// stored account, or the upstream rejected the stored token): 401 with the
/// signed-out bundle as the body and `X-Canvas-Session: invalid`, so the
/// client learns its session is stale (it clears it and refetches without
/// the bearer).
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
    let statuses = match state
        .upstream
        .home_timeline(&account.access_token, TIMELINE_LIMIT)
        .await
    {
        Ok(s) => s,
        Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, _)) => {
            // The upstream token was revoked or expired: forget it.
            tracing::warn!(username = %account.username, "upstream rejected the stored token; signed out");
            let instance = state.pixelfed.instance_url.clone();
            let _ = blocking(state, move |s| s.delete_pixelfed_account(&instance)).await;
            return signed_out();
        }
        Err(e) => return bad_gateway(&e),
    };
    let mut media = MediaUrls::new(&proxy_base(headers));
    let timeline = normalize_timeline(&statuses, &mut media);
    if let Err(e) = save_media_keys(state, media).await {
        return e.into_response();
    }
    tracing::info!(
        posts = timeline.order.len(),
        entities = timeline.entities.len(),
        "pixelfed_home served"
    );
    Json(layout::home_feed(timeline.entities, timeline.order)).into_response()
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
    match state
        .upstream
        .status_action(&account.access_token, &id, action)
        .await
    {
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
    let mut out = Response::builder().status(StatusCode::OK).header(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    for name in [header::CONTENT_TYPE, header::CONTENT_LENGTH] {
        if let Some(v) = resp.headers().get(&name) {
            out = out.header(name, v.clone());
        }
    }
    out.body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
