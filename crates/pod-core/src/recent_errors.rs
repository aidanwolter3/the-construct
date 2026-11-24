//! The pod's recent failed requests (decision `recent-server-errors`): the
//! last [`KEEP`] answers of 400 or more that the pod's own routes gave,
//! kept in memory per pod ([`Pod::local`]), so Server health can say which
//! request failed, for which device and why, where the process-wide
//! counters ([`crate::metrics`]) only count statuses.
//!
//! What is kept is chosen so nothing secret is: the route's pattern
//! (axum's [`MatchedPath`], e.g. `/v1/uploads/{id}`, never the path that was
//! asked for, which can hold a token, and never the query), the method, the
//! status, the answer's `error` code when the answer is a small JSON object
//! (read from a body already in memory, never a streamed one), and the
//! device id the request named ([`protocol::DEVICE_HEADER`]). No body,
//! header value, bearer or pod code. 401 (a stale session, routine) and 404
//! are not kept; requests no route matched never reach this layer, nor do
//! those the host refused before the pod (they have no pod).
//!
//! Each pod has its own list: another pod's requests never reach it, and
//! the list goes when the process restarts, like the counters.

use std::collections::VecDeque;
use std::sync::Mutex;

use axum::{
    body::{Body, HttpBody},
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::Response,
};
use serde_json::Value;

use crate::Pod;
use crate::util::now_secs;

/// How many failed requests a pod keeps, newest first.
pub const KEEP: usize = 50;
/// The largest answer whose `error` code is read.
const MAX_CODE_BODY: u64 = 4096;

/// One failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentError {
    /// When it was answered (Unix seconds).
    pub at: i64,
    pub method: String,
    /// The route's pattern, e.g. `/v1/taste/react`.
    pub route: String,
    pub status: u16,
    /// The answer's `error`, e.g. `taste_unreachable`.
    pub code: Option<String>,
    /// The device id the request named.
    pub device: Option<String>,
}

/// A pod's list ([`Pod::local`]).
#[derive(Debug, Default)]
pub struct RecentErrors(Mutex<VecDeque<RecentError>>);

impl RecentErrors {
    /// Keep `e`, dropping the oldest past [`KEEP`].
    pub fn push(&self, e: RecentError) {
        let mut l = self.0.lock().unwrap_or_else(|e| e.into_inner());
        l.push_front(e);
        l.truncate(KEEP);
    }

    /// The list, newest first.
    pub fn list(&self) -> Vec<RecentError> {
        let l = self.0.lock().unwrap_or_else(|e| e.into_inner());
        l.iter().cloned().collect()
    }
}

/// `pod`'s failed requests, newest first.
pub fn recent(pod: &Pod) -> Vec<RecentError> {
    pod.local::<RecentErrors>().list()
}

/// Whether an answer of `status` is kept.
pub fn kept(status: StatusCode) -> bool {
    status.as_u16() >= 400 && !matches!(status.as_u16(), 401 | 404)
}

/// The route layer that keeps them (outermost of the pod's route layers, so
/// it sees the answers the others give too).
pub(crate) async fn layer(State(pod): State<Pod>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned());
    let method: String = req
        .method()
        .as_str()
        .chars()
        .filter(char::is_ascii_alphabetic)
        .take(16)
        .collect();
    let device = device(req.headers());
    let resp = next.run(req).await;
    if !kept(resp.status()) {
        return resp;
    }
    let status = resp.status().as_u16();
    let (resp, code) = error_code(resp).await;
    pod.local::<RecentErrors>().push(RecentError {
        at: now_secs(),
        method,
        route: route.unwrap_or_else(|| "?".to_owned()),
        status,
        code,
        device,
    });
    resp
}

/// The request's device id, when well-formed.
fn device(headers: &HeaderMap) -> Option<String> {
    headers
        .get(protocol::DEVICE_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|d| crate::devices::valid_device_id(d))
        .map(str::to_owned)
}

/// The answer's `error` code, when it is a JSON object of a known small
/// size (already in memory): the answer is given back whole either way.
async fn error_code(resp: Response) -> (Response, Option<String>) {
    let json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/json"));
    let small = resp
        .body()
        .size_hint()
        .exact()
        .is_some_and(|n| n <= MAX_CODE_BODY);
    if !json || !small {
        return (resp, None);
    }
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_CODE_BODY as usize).await else {
        // Can't happen for a body of known size; answer without one.
        return (Response::from_parts(parts, Body::empty()), None);
    };
    let code = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .filter(|c| is_code(c));
    (Response::from_parts(parts, Body::from(bytes)), code)
}

/// A code is one word (`taste_unreachable`), never free text.
fn is_code(c: &str) -> bool {
    (1..=64).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: i64) -> RecentError {
        RecentError {
            at: n,
            method: "POST".into(),
            route: "/v1/x".into(),
            status: 502,
            code: None,
            device: None,
        }
    }

    #[test]
    fn keeps_the_newest() {
        let l = RecentErrors::default();
        for n in 0..(KEEP as i64 + 5) {
            l.push(e(n));
        }
        let got = l.list();
        assert_eq!(got.len(), KEEP);
        assert_eq!(got[0].at, KEEP as i64 + 4);
        assert_eq!(got[KEEP - 1].at, 5);
    }

    #[test]
    fn which_statuses_and_codes() {
        assert!(kept(StatusCode::BAD_GATEWAY));
        assert!(kept(StatusCode::CONFLICT));
        assert!(kept(StatusCode::FORBIDDEN));
        assert!(!kept(StatusCode::UNAUTHORIZED));
        assert!(!kept(StatusCode::NOT_FOUND));
        assert!(!kept(StatusCode::OK));
        assert!(is_code("taste_unreachable"));
        assert!(!is_code("the service said: no"));
        assert!(!is_code(""));
    }
}
