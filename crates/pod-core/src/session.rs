//! Pod sessions and sign-ins (D17, D40).
//!
//! One pod session is shared by every app. A client holds only the random
//! session token (`Authorization: Bearer <token>`); the store keeps
//! SHA-256(token). Adapters register sign-ins ([`crate::AppAdapter::sign_ins`]);
//! completing one records it on the session
//! ([`crate::store::open_session_on`]), and an app that requires it
//! ([`crate::AuthPolicy::SignIn`]) is served only to sessions that have it.
//! `POST /v1/auth/logout` ends the session and every sign-in on it.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use protocol::WorkspaceSpec;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{ApiError, AppState, Pod, blocking};

/// Response header on a 401 for a stale session (`invalid`): the client
/// clears its session and refetches without the bearer.
pub const SESSION_HEADER: &str = "x-construct-session";

/// Default pod session lifetime (`CONSTRUCT_SESSION_TTL_DAYS`).
pub const DEFAULT_SESSION_TTL_DAYS: i64 = 90;

/// The session lifetime in seconds from `CONSTRUCT_SESSION_TTL_DAYS`
/// (1..=36500 days; anything else warns and uses the default).
pub fn session_ttl_from_env() -> i64 {
    let raw = std::env::var("CONSTRUCT_SESSION_TTL_DAYS")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let days = match raw.map(|v| v.parse::<i64>()) {
        None => DEFAULT_SESSION_TTL_DAYS,
        Some(Ok(d)) if (1..=36_500).contains(&d) => d,
        Some(_) => {
            tracing::warn!(
                "CONSTRUCT_SESSION_TTL_DAYS is not a number of days in 1..=36500; using {DEFAULT_SESSION_TTL_DAYS}"
            );
            DEFAULT_SESSION_TTL_DAYS
        }
    };
    days * 86_400
}

/// SHA-256 hex of a session token (what the sessions table stores).
pub fn session_hash(token: &str) -> String {
    crate::util::hex(&Sha256::digest(token.as_bytes()))
}

/// A new random session token (256 bits, URL-safe base64).
pub fn new_session_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(buf)
}

/// The bearer token of a request, if any.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| !t.is_empty())
}

/// 401 `{"error": "unauthorized"}` with `WWW-Authenticate: Bearer`.
pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

/// 401 with [`SESSION_HEADER`]`: invalid` and `screen` (a signed-out
/// screen) as the body.
pub fn stale_session(screen: WorkspaceSpec) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::WWW_AUTHENTICATE, "Bearer"),
            (header::HeaderName::from_static(SESSION_HEADER), "invalid"),
        ],
        Json(screen),
    )
        .into_response()
}

/// A valid pod session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// SHA-256 hex of the token ([`session_hash`]).
    pub token_hash: String,
    /// The sign-ins completed on it, sorted.
    pub sign_ins: Vec<String>,
}

impl Session {
    pub fn has(&self, sign_in: &str) -> bool {
        self.sign_ins.iter().any(|s| s == sign_in)
    }
}

/// Who is calling, by the request's bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// No bearer.
    Anonymous,
    /// A bearer that is not a valid session (unknown or expired).
    Stale,
    /// A valid session.
    Signed(Session),
}

impl Caller {
    pub fn session(&self) -> Option<&Session> {
        match self {
            Caller::Signed(s) => Some(s),
            _ => None,
        }
    }

    pub fn into_session(self) -> Option<Session> {
        match self {
            Caller::Signed(s) => Some(s),
            _ => None,
        }
    }

    /// Whether the caller's session has `sign_in`.
    pub fn has(&self, sign_in: &str) -> bool {
        self.session().is_some_and(|s| s.has(sign_in))
    }
}

impl AppState {
    /// The caller of a request: no bearer, a stale one, or a valid session
    /// with its sign-ins.
    pub async fn caller(self: &Pod, headers: &HeaderMap) -> Result<Caller, ApiError> {
        let Some(token) = bearer(headers) else {
            return Ok(Caller::Anonymous);
        };
        let hash = session_hash(token);
        let ttl = self.session_ttl_secs();
        blocking(self, move |s| {
            if !s.session_valid(&hash, ttl)? {
                return Ok(Caller::Stale);
            }
            let sign_ins = s.session_sign_ins(&hash)?;
            Ok(Caller::Signed(Session {
                token_hash: hash,
                sign_ins,
            }))
        })
        .await
    }
}

pub(crate) fn router() -> Router<Pod> {
    Router::new().route("/v1/auth/logout", post(logout))
}

/// `POST /v1/auth/logout` (bearer): deletes the pod session and its sign-ins.
/// Upstream accounts are kept (single tenant; other sessions may use them).
async fn logout(State(state): State<Pod>, headers: HeaderMap) -> Response {
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
