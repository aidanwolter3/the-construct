//! Pod HTTP routes for the Pixelfed adapter (auth, home feed, actions, media).
//!
//! Auth model (docs/DECISIONS.md D17, D40): the pod exchanges the client's PKCE
//! code for the upstream token, stores it, and records the `pixelfed` sign-in
//! on a pod session (`Authorization: Bearer <session>`, owned by the core;
//! only SHA-256(session) is stored). Each session uses the instance it signed
//! in to (D45); one account is stored per instance.

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::session::{
    Caller, Session, bearer, new_session_token, session_hash, sign_in_required,
};
use pod_core::util::now_secs;
use pod_core::{
    ApiError, Hydration, Pod, RequestCtx, api_error, blocking, json_error, parse_json_body,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::upstream::{SCOPES, UpstreamError};
use super::{
    Account, MediaUrls, Status, layout, normalize_account, normalize_comment, normalize_comments,
    normalize_timeline, post_state,
};
use crate::adapter::{PixelfedAdapter, Runtime, SIGN_IN};
use crate::config::Instance;
use crate::db::{PixelfedAccount, PixelfedApp, PixelfedStore, SignIn};

/// Path prefix of the media proxy.
pub const MEDIA_PATH: &str = "/v1/pixelfed/media";
/// Largest media body the proxy relays (25 MB).
pub const MAX_MEDIA_BYTES: u64 = 25 * 1024 * 1024;
/// Statuses fetched per home timeline request.
const TIMELINE_LIMIT: u32 = 20;

pub fn router() -> Router<Pod> {
    Router::new()
        .route("/v1/auth/pixelfed/app", post(register_app))
        .route("/v1/auth/pixelfed/token", post(exchange_token))
        .route("/v1/pixelfed/posts/like", post(like))
        .route("/v1/pixelfed/posts/bookmark", post(bookmark))
        .route("/v1/pixelfed/posts/comment", post(comment))
        .route("/v1/pixelfed/posts/create", post(crate::posting::create))
        .route(
            "/v1/pixelfed/posts/pending/retry",
            post(crate::posting::retry),
        )
        .route(
            "/v1/pixelfed/posts/pending/discard",
            post(crate::posting::discard),
        )
        .route("/v1/pixelfed/screens/comments", get(comments_screen))
        .route("/v1/pixelfed/screens/post", get(post_screen))
        .route("/v1/pixelfed/media/{key}", get(media))
}

// ---------- helpers ----------

/// The Pixelfed adapter of this pod (registered by the `server` binary).
pub(crate) fn adapter(state: &Pod) -> &PixelfedAdapter {
    state
        .adapter::<PixelfedAdapter>()
        .expect("the Pixelfed routes are mounted by the Pixelfed adapter")
}

/// The instance and stored upstream account of the request's pod session.
/// Without one: 401 for no or a stale session, 403 `sign_in_required` for a
/// valid session that lacks the `pixelfed` sign-in (the client keeps it, D46).
async fn signed_in_account(
    state: &Pod,
    headers: &HeaderMap,
) -> Result<(Instance, PixelfedAccount), Box<Response>> {
    let caller = state
        .caller(headers)
        .await
        .map_err(|e| Box::new(e.into_response()))?;
    let found = match caller.session() {
        Some(session) => session_account(state, session)
            .await
            .map_err(|e| Box::new(e.into_response()))?,
        None => None,
    };
    found.ok_or_else(|| Box::new(sign_in_required(&caller)))
}

/// Whether this pod may use instances other than the default
/// ([`crate::OTHER_INSTANCES_SETTING`]; read once per pod).
#[derive(Default)]
struct PodRules(std::sync::OnceLock<bool>);

pub(crate) async fn others_allowed(state: &Pod) -> Result<bool, ApiError> {
    let rules = state.local::<PodRules>();
    if let Some(v) = rules.0.get() {
        return Ok(*v);
    }
    let setting = blocking(state, |s| s.pod_setting(crate::OTHER_INSTANCES_SETTING)).await?;
    let others = setting.as_deref() != Some("0");
    let _ = rules.0.set(others);
    Ok(others)
}

/// Serializes one pod's upstream token refreshes (Passport rotates the
/// refresh token, so two concurrent refreshes would revoke each other).
#[derive(Default)]
struct RefreshLock(tokio::sync::Mutex<()>);

/// The instance `session` signed in to and its stored account. A session
/// from before D45 uses the default instance; one whose instance this pod no
/// longer accepts (the configuration changed) has none.
pub(crate) async fn session_account(
    state: &Pod,
    session: &Session,
) -> Result<Option<(Instance, PixelfedAccount)>, ApiError> {
    if !session.has(SIGN_IN) {
        return Ok(None);
    }
    let rt = adapter(state).runtime();
    let others = others_allowed(state).await?;
    let hash = session.token_hash.clone();
    let stored = blocking(state, move |s| s.session_instance(&hash)).await?;
    let instance = match stored {
        None => rt.config.default_instance(),
        Some(url) => match rt.resolve(&url, others) {
            Some(i) => i,
            None => {
                tracing::warn!(instance = %url, "session's Pixelfed instance is no longer allowed");
                return Ok(None);
            }
        },
    };
    Ok(stored_account(state, &instance)
        .await?
        .map(|account| (instance, account)))
}

/// The stored upstream account of `instance` (one per instance).
async fn stored_account(
    state: &Pod,
    instance: &Instance,
) -> Result<Option<PixelfedAccount>, ApiError> {
    let url = instance.url.clone();
    blocking(state, move |s| s.pixelfed_account(&url)).await
}

/// `<scheme>://<Host><public prefix>/v1/pixelfed/media`, from the request so
/// that clients reaching the pod as e.g. 10.0.2.2 get URLs they can load (the
/// prefix: a pod host's public route for this pod, D46).
pub(crate) fn proxy_base(headers: &HeaderMap) -> String {
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
    let prefix = pod_core::public_prefix(headers);
    format!("{scheme}://{host}{prefix}{MEDIA_PATH}")
}

pub(crate) async fn save_media_keys(state: &Pod, media: MediaUrls) -> Result<(), ApiError> {
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

pub(crate) fn invalid_request(expected: Value) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request", "expected": expected }),
    )
}

fn instance_not_allowed(rt: &Runtime, got: &str) -> Response {
    tracing::warn!(
        instance_url = got,
        "auth request for an instance this pod does not accept rejected"
    );
    let mut allowed = vec![json!(rt.config.instance_url)];
    allowed.extend(rt.config.allowed_instances.iter().map(|u| json!(u)));
    if rt.config.any_instance {
        allowed.push(json!("any public https:// instance"));
    }
    json_error(
        StatusCode::BAD_REQUEST,
        json!({
            "error": "instance_not_allowed",
            "instance_url": got,
            "allowed": allowed,
        }),
    )
}

/// 502 for an unreachable upstream or one that answered 5xx. The detail
/// (upstream body, reqwest errors with host names, store errors) is logged
/// only; clients get a stable code and the upstream status.
fn bad_gateway(e: &UpstreamError) -> Response {
    tracing::warn!(error = %e, "Pixelfed upstream failure");
    let body = match e {
        UpstreamError::Unreachable(_) => json!({ "error": "upstream_unreachable" }),
        UpstreamError::Status(status, _) => {
            json!({ "error": "upstream_error", "status": status.as_u16() })
        }
    };
    json_error(StatusCode::BAD_GATEWAY, body)
}

/// The upstream rejected `stale.access_token` with 401: refresh it with the
/// stored refresh token (serialized; a concurrent caller that already
/// refreshed it gets the new token without a second upstream call).
/// `Ok(None)`: no usable refresh path (no refresh token, app gone, or the
/// upstream rejected the refresh), so the caller signs out. `Err`: the
/// upstream was unreachable / 5xx during the refresh (or the store failed).
async fn refresh_account(
    state: &Pod,
    inst: &Instance,
    stale: &PixelfedAccount,
) -> Result<Option<PixelfedAccount>, UpstreamError> {
    let pf = adapter(state);
    let lock = state.local::<RefreshLock>();
    let _guard = lock.0.lock().await;
    let rt = pf.runtime();
    let instance = inst.url.clone();
    let store_err = |e: ApiError| {
        tracing::error!(?e, "pod store failed during a Pixelfed token refresh");
        UpstreamError::Unreachable("pod store".into())
    };
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
    match rt
        .api(inst)
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
            tracing::info!("upstream access token refreshed");
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
pub(crate) async fn with_token<T, F, Fut>(
    state: &Pod,
    instance: &Instance,
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
            match refresh_account(state, instance, &account).await {
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
async fn register_app(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let rt = adapter(&state).runtime();
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
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let Some(inst) = rt.resolve(instance_url, others) else {
        return instance_not_allowed(&rt, instance_url);
    };
    let upstream = rt.api(&inst);
    if !redirect_allowed(redirect_uri, &rt.config.extra_redirects) {
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
    let instance = inst.url.clone();
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
        Some(app) => match upstream
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
        None => match upstream.register_app(&redirect).await {
            Ok(reg) => {
                let app = PixelfedApp {
                    client_id: reg.client_id,
                    client_secret: reg.client_secret,
                };
                let (i, r, a) = (instance.clone(), redirect.clone(), app.clone());
                if let Err(e) = blocking(&state, move |s| s.save_pixelfed_app(&i, &r, &a)).await {
                    return e.into_response();
                }
                tracing::info!(client_id = %app.client_id, redirect_uri = %redirect,
                    instance = %instance, "registered Pixelfed app");
                app
            }
            Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
                tracing::warn!(status = status.as_u16(), %detail, "Pixelfed app registration rejected upstream");
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "app_registration_failed", "status": status.as_u16() }),
                );
            }
            Err(e) => return bad_gateway(&e),
        },
    };
    Json(json!({
        "client_id": app.client_id,
        "scopes": SCOPES,
        "authorize_endpoint": format!("{}/oauth/authorize", rt.config.public_base(&inst)),
    }))
    .into_response()
}

/// `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}`.
async fn exchange_token(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let rt = adapter(&state).runtime();
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
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let Some(inst) = rt.resolve(instance_url, others) else {
        return instance_not_allowed(&rt, instance_url);
    };
    let upstream = rt.api(&inst);
    let instance = inst.url.clone();
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
            json!({ "error": "oauth_exchange_failed", "status": status.as_u16() }),
        )
    };
    let token = match upstream
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
    let account: Account = match upstream.verify_credentials(&token.access_token).await {
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
    let mut media = MediaUrls::new(&proxy_base(&headers), rt.media_for(others));
    let user = normalize_account(&account, &mut media);
    // Owner pinning (D17): only the owner's account may sign in, so another
    // user of the instance cannot replace the stored account.
    let cfg = &rt.config;
    let not_owner = || {
        tracing::warn!("Pixelfed sign-in by an account that does not own this pod rejected");
        json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "account_not_owner",
                    "detail": "this pod is pinned to another Pixelfed account" }),
        )
    };
    if cfg
        .required_owner(&inst)
        .is_some_and(|owner| owner != account.id)
    {
        return not_owner();
    }
    let replace_owner = cfg.replaces_owner(&inst);
    let ttl = state.session_ttl_secs();
    let stored = PixelfedAccount {
        access_token: token.access_token,
        account_id: account.id.clone(),
        username: user.get_str("username").unwrap_or_default().to_owned(),
        refresh_token: token.refresh_token,
        expires_at: token.expires_in.map(|s| now_secs().saturating_add(s)),
        redirect_uri: Some(redirect_uri.to_owned()),
    };
    // A caller that already has a valid pod session keeps it and gains the
    // `pixelfed` sign-in (one session collects every app's sign-ins, D40).
    let session = match state.caller(&headers).await {
        Ok(Caller::Signed(_)) => bearer(&headers).unwrap_or_default().to_owned(),
        Ok(_) => new_session_token(),
        Err(e) => return e.into_response(),
    };
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
    tracing::info!(instance = %inst.url, "Pixelfed sign-in: session signed in");
    Json(json!({
        "session_token": session,
        "instance_url": inst.url,
        "account": {
            "id": account.id,
            "username": user.get("username"),
            "display_name": user.get("display_name"),
            "avatar_url": user.get("avatar_url"),
        },
    }))
    .into_response()
}

// ---------- home feed ----------

/// The data of `GET /v1/workspaces/pixelfed` (the `home` feed, [`pod_core::AppAdapter::feed`];
/// the core has checked the caller's session has the `pixelfed` sign-in).
/// No stored account, or the upstream rejected the stored token: a stale
/// session (401 with the signed-out screen, so the client clears its session).
/// The core adds the latest layout version the client can draw.
pub async fn home_feed(pf: &PixelfedAdapter, state: &Pod, req: &RequestCtx) -> Hydration {
    let rt = pf.runtime();
    let app = crate::apps::PIXELFED;
    let signed_out = || Hydration::StaleSession(layout::signed_out(&rt.config, app.id, app.title));
    let found = match &req.session {
        Some(session) => session_account(state, session).await,
        None => Ok(None),
    };
    let (instance, account) = match found {
        Ok(Some(found)) => found,
        Ok(None) => return signed_out(),
        Err(e) => return Hydration::Error(e.into_response()),
    };
    let upstream = rt.api(&instance);
    let (result, last_token) = with_token(state, &instance, account, |token| {
        let upstream = &upstream;
        async move { upstream.home_timeline(&token, TIMELINE_LIMIT).await }
    })
    .await;
    let statuses = match result {
        Ok(s) => s,
        Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, _)) => {
            // The upstream token was revoked or expired and could not be
            // refreshed: forget it, but only if it is still the stored one (a
            // sign-in or refresh may have replaced it meanwhile).
            let url = instance.url.clone();
            let deleted = blocking(state, move |s| {
                s.delete_pixelfed_account_if_token(&url, &last_token)
            })
            .await
            .unwrap_or(false);
            tracing::warn!(deleted, "upstream rejected the stored token; signed out");
            return signed_out();
        }
        Err(e) => return Hydration::Error(bad_gateway(&e)),
    };
    let others = match others_allowed(state).await {
        Ok(o) => o,
        Err(e) => return Hydration::Error(e.into_response()),
    };
    let mut media = MediaUrls::new(&proxy_base(&req.headers), rt.media_for(others));
    let mut timeline = normalize_timeline(&statuses, &mut media);
    // Posts on their way (D50) go first: placeholders, then finished posts
    // the timeline does not list yet.
    if let Some(session) = &req.session {
        let listed: std::collections::HashSet<String> = statuses
            .iter()
            .flat_map(|s| [s["id"].as_str(), s["reblog"]["id"].as_str()])
            .flatten()
            .map(str::to_owned)
            .collect();
        let (placeholders, missing) =
            match crate::posting::placeholders(state, session, req, &listed).await {
                Ok(p) => p,
                Err(e) => return Hydration::Error(e.into_response()),
            };
        let mut first = Vec::new();
        for (uri, post, media_entities) in placeholders {
            timeline.entities.extend(media_entities);
            timeline.entities.insert(uri.clone(), post);
            first.push(uri);
        }
        for id in missing.iter().filter(|id| valid_status_id(id)) {
            let (result, _) = with_token(
                state,
                &instance,
                account_again(state, &instance).await,
                |token| {
                    let (upstream, id) = (&upstream, id);
                    async move { upstream.get_status(&token, id).await }
                },
            )
            .await;
            match result.ok().and_then(|v| Status::deserialize(&v).ok()) {
                Some(status) => {
                    let post = super::normalize_post(&status, &mut media, &mut timeline.entities);
                    let uri = super::post_uri(&status.id);
                    timeline.entities.insert(uri.clone(), post);
                    first.push(uri);
                }
                None => tracing::info!(%id, "a new post is not readable yet"),
            }
        }
        if !first.is_empty() {
            timeline.order.retain(|u| !first.contains(u));
            first.append(&mut timeline.order);
            timeline.order = first;
        }
    }
    if let Err(e) = save_media_keys(state, media).await {
        return Hydration::Error(e.into_response());
    }
    tracing::info!(
        posts = timeline.order.len(),
        entities = timeline.entities.len(),
        instance = %instance.url,
        "pixelfed served"
    );
    Hydration::Data(layout::home_feed(timeline.entities, timeline.order))
}

/// The stored account of `instance` again (after `with_token` consumed it;
/// a refresh may have replaced its token).
async fn account_again(state: &Pod, instance: &Instance) -> PixelfedAccount {
    stored_account(state, instance)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ---------- actions ----------

#[derive(Clone, Copy)]
enum Toggle {
    Like,
    Bookmark,
}

async fn like(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    toggle(state, headers, body, Toggle::Like).await
}

async fn bookmark(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    toggle(state, headers, body, Toggle::Bookmark).await
}

pub(crate) fn valid_status_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// `POST /v1/pixelfed/posts/{like,bookmark} {id, liked|bookmarked}` ->
/// 200 `{ok, id, is_liked, like_count, is_bookmarked}`. No session: 401;
/// upstream 4xx: same status (the client rolls back); upstream 5xx or
/// unreachable: 502 (the client retries).
async fn toggle(state: Pod, headers: HeaderMap, body: Bytes, kind: Toggle) -> Response {
    let rt = adapter(&state).runtime();
    let (instance, account) = match signed_in_account(&state, &headers).await {
        Ok(found) => found,
        Err(r) => return *r,
    };
    let upstream = rt.api(&instance);
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
    let (result, _) = with_token(&state, &instance, account, |token| {
        let (upstream, id) = (&upstream, &id);
        async move { upstream.status_action(&token, id, action).await }
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
                json!({ "error": "upstream_rejected", "status": status.as_u16() }),
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
/// (`post_not_found` for 404). The upstream body is logged, not returned.
fn comment_rejected(id: &str, status: StatusCode, detail: &str) -> Response {
    tracing::info!(%id, status = status.as_u16(), %detail, "Pixelfed comments call rejected upstream");
    let error = if status == StatusCode::NOT_FOUND {
        "post_not_found"
    } else {
        "upstream_rejected"
    };
    api_error(status, json!({ "error": error, "status": status.as_u16() })).into_response()
}

/// `GET /v1/pixelfed/screens/comments?post_id=<id>`: the data of the comments screen
/// (docs/DECISIONS.md D34), a `DataBundle`: one `pixelfed://comments/<id>` entity per
/// comment ([`normalize_comment`]) and `queries["comments:<post_id>"]` in thread order
/// ([`normalize_comments`]), replies to comments included ([`super::complete_comment_thread`]),
/// plus the post itself, listed as `queries["post:<post_id>"]` ([`super::normalize_post_screen`]).
/// When `post_id` is a comment, the post above it is used ([`super::thread_root`]), and
/// both queries still use the id asked for. No session: 401; upstream 4xx: same status (404
/// `post_not_found`); 5xx / unreachable: 502.
async fn comments_screen(
    State(state): State<Pod>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rt = adapter(&state).runtime();
    let (instance, account) = match signed_in_account(&state, &headers).await {
        Ok(found) => found,
        Err(r) => return *r,
    };
    let upstream = rt.api(&instance);
    let id = params
        .get("post_id")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    if !valid_status_id(&id) {
        return invalid_request(json!({ "post_id": "status id" }));
    }
    let own = account.account_id.clone();
    // The post: `id` itself, or the post above it when `id` is a comment (an
    // older notification links to the comment a reply answers).
    let (result, token) = with_token(&state, &instance, account, |token| {
        let (upstream, id) = (&upstream, &id);
        async move { upstream.status(&token, id).await }
    })
    .await;
    let raw = match result {
        Ok(v) => v,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return comment_rejected(&id, status, &detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let (root, _) = super::thread_root(raw, |parent| {
        let (upstream, token) = (&upstream, &token);
        async move {
            match upstream.status(token, &parent).await {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(%parent, error = %e, "a comment's parent is unavailable");
                    None
                }
            }
        }
    })
    .await;
    let Some(post_id) = Status::deserialize(&root)
        .ok()
        .map(|s| s.target().id.clone())
        .filter(|p| valid_status_id(p))
    else {
        return api_error(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "upstream_unparseable" }),
        )
        .into_response();
    };
    let context = match upstream.status_context(&token, &post_id).await {
        Ok(v) => v,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return comment_rejected(&post_id, status, &detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    // Pixelfed lists only the direct replies; ask each comment for its own.
    let context = super::complete_comment_thread(&post_id, context, |comment_id| {
        let (upstream, token) = (&upstream, &token);
        async move {
            match upstream.status_context(token, &comment_id).await {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(%comment_id, error = %e, "replies to a comment unavailable");
                    None
                }
            }
        }
    })
    .await;
    // Pixelfed can omit a comment for a while after it was posted.
    let mut context = context;
    rt.recent_comments
        .merge_into(&instance.url, &own, &post_id, &mut context);
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let mut media = MediaUrls::new(&proxy_base(&headers), rt.media_for(others));
    let list = normalize_comments(&post_id, &context, &own, now_secs(), &mut media);
    // The post above the comments, listed as `post:<id>` (the id asked for).
    let post = super::normalize_post_screen(&id, &root, &mut media);
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(%id, %post_id, comments = list.len(), "Pixelfed comments served");
    let mut bundle = post.unwrap_or_default();
    let mut order = Vec::with_capacity(list.len());
    for c in list {
        let uri = super::comment_uri(c.get_str("id").unwrap_or_default());
        order.push(uri.clone());
        bundle.entities.insert(uri, c);
    }
    bundle.queries.insert(super::comments_query(&id), order);
    Json(bundle).into_response()
}

/// `GET /v1/pixelfed/screens/post?id=<id>`: the data of the post screen (D48), a
/// `DataBundle` with the post, its author and media, and `queries["post:<id>"]`
/// ([`normalize_post_screen`]). No session: 401; upstream 4xx: same status (404
/// `post_not_found`); 5xx / unreachable or not a status: 502.
async fn post_screen(
    State(state): State<Pod>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rt = adapter(&state).runtime();
    let (instance, account) = match signed_in_account(&state, &headers).await {
        Ok(found) => found,
        Err(r) => return *r,
    };
    let upstream = rt.api(&instance);
    let id = params
        .get("id")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    if !valid_status_id(&id) {
        return invalid_request(json!({ "id": "status id" }));
    }
    let (result, _) = with_token(&state, &instance, account, |token| {
        let (upstream, id) = (&upstream, &id);
        async move { upstream.status(&token, id).await }
    })
    .await;
    let raw = match result {
        Ok(v) => v,
        Err(UpstreamError::Status(status, detail)) if status.is_client_error() => {
            return comment_rejected(&id, status, &detail);
        }
        Err(e) => return bad_gateway(&e),
    };
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let mut media = MediaUrls::new(&proxy_base(&headers), rt.media_for(others));
    let Some(bundle) = super::normalize_post_screen(&id, &raw, &mut media) else {
        return api_error(
            StatusCode::BAD_GATEWAY,
            json!({ "error": "upstream_unparseable" }),
        )
        .into_response();
    };
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(%id, "Pixelfed post served");
    Json(bundle).into_response()
}

/// `POST /v1/pixelfed/posts/comment {id, text, client_id?}` -> 200 `{ok, id, comment}`:
/// posts `text` as a reply to post `id`. This is an outbox endpoint (the client's
/// optimistic `reply_count + 1` is rolled back on a 4xx): blank or too long text is 400,
/// no session 401, upstream 4xx the same status, 5xx / unreachable 502 (retried).
/// `client_id` (a client-made nonce, `[A-Za-z0-9_-]{8,64}`) becomes the upstream
/// `Idempotency-Key`, so a retry after a lost response does not post twice where the
/// upstream honours it.
async fn comment(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let rt = adapter(&state).runtime();
    let (instance, account) = match signed_in_account(&state, &headers).await {
        Ok(found) => found,
        Err(r) => return *r,
    };
    let upstream = rt.api(&instance);
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
    let (result, _) = with_token(&state, &instance, account, |token| {
        let (upstream, id, key) = (&upstream, &id, key.as_deref());
        async move { upstream.create_reply(&token, id, text, key).await }
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
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    rt.recent_comments
        .record(&instance.url, &own, created.clone());
    let mut media = MediaUrls::new(&proxy_base(&headers), rt.media_for(others));
    let comment = normalize_comment(&status, &id, 0, &own, now_secs(), &mut media);
    if let Err(e) = save_media_keys(&state, media).await {
        return e.into_response();
    }
    tracing::info!(post = %id, comment = %status.id, "Pixelfed comment posted");
    Json(json!({ "ok": true, "id": id, "comment": comment })).into_response()
}

// ---------- media proxy ----------

/// `GET /v1/pixelfed/media/{key}`: streams a media/avatar URL the pod emitted.
/// No auth: keys are opaque hashes of URLs that are public upstream anyway.
async fn media(State(state): State<Pod>, Path(key): Path<String>) -> Response {
    let rt = adapter(&state).runtime();
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
    // Normalization only emits keys for allowed URLs, but keys stored by
    // an older pod (or before the allowlist changed) are checked again here.
    let others = match others_allowed(&state).await {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let Some(route) = rt.media_for(others).route(&url) else {
        tracing::warn!(host = ?reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)),
            "media proxy refused a URL outside the allowlist");
        return json_error(
            StatusCode::FORBIDDEN,
            json!({ "error": "media_host_not_allowed" }),
        );
    };
    // URLs built with the public base (e.g. 10.0.2.2) are fetched via the
    // instance base the pod can reach.
    let cfg = &rt.config;
    let fetch_url = match url.strip_prefix(&cfg.public_url) {
        Some(rest) if cfg.public_url != cfg.instance_url && rest.starts_with('/') => {
            format!("{}{rest}", cfg.instance_url)
        }
        _ => url,
    };
    let resp = match rt.clients.fetch_media(&fetch_url, route).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// L4 (#31): upstream bodies and error strings stay in the log.
    #[tokio::test]
    async fn errors_do_not_echo_upstream_details() {
        let secret = "internal-host.flycast:8080 <html>stack trace</html>";
        let b = body(bad_gateway(&UpstreamError::Unreachable(secret.into()))).await;
        assert_eq!(b, json!({ "error": "upstream_unreachable" }));
        let b = body(bad_gateway(&UpstreamError::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            secret.into(),
        )))
        .await;
        assert_eq!(b, json!({ "error": "upstream_error", "status": 500 }));
        let b = body(comment_rejected("1", StatusCode::NOT_FOUND, secret)).await;
        assert_eq!(b["error"], "post_not_found");
        assert!(!b.to_string().contains("stack"), "{b}");
    }
}
