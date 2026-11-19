//! Pod calls for Settings: the session's sign-ins (`/v1/auth/sign-ins`), and
//! the error type the engine's other pod calls (notifications, push) share.
//! Each call runs on the caller's thread (never the UI thread) with the
//! engine's base URL and returns the pod's JSON body unchanged.
//!
//! A 401 is just an [`LayoutApiError::Unauthorized`] error: it never signs
//! out (the layout fetch does that). Layouts themselves come from the
//! construct ([`crate::construct`], decision `one-layout-system`).

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::{ConstructEngine, Inner};

/// Timeout for one catalog request.
const CATALOG_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// `reason` of [`LayoutApiError::Offline`].
pub const LAYOUTS_OFFLINE: &str = "Can't reach your pod. Check your connection and try again.";
/// `reason` of a 401.
pub const LAYOUTS_UNAUTHORIZED: &str = "The pod refused this request.";

/// `reason` when the pod has no `GET /v1/auth/sign-ins` (an older pod).
pub const SIGN_INS_UNSUPPORTED: &str = "This pod can't list sign-ins yet.";

/// Why a catalog call failed. `reason` is a short sentence for the UI; the
/// URL, status and body go to the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutApiError {
    /// The pod could not be reached (transport error or timeout).
    Offline { reason: String },
    /// HTTP 401.
    Unauthorized { reason: String },
    /// HTTP 404: an unknown layout or path.
    NotFound { reason: String },
    /// Any other non-2xx status, or an unusable response.
    Server { status: u16, reason: String },
}

impl LayoutApiError {
    pub fn reason(&self) -> &str {
        match self {
            Self::Offline { reason }
            | Self::Unauthorized { reason }
            | Self::NotFound { reason }
            | Self::Server { reason, .. } => reason,
        }
    }
}

impl fmt::Display for LayoutApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for LayoutApiError {}

/// Percent-encode a path segment (everything but RFC 3986 unreserved).
pub(crate) fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The sentence shown for a non-2xx answer.
fn error_for(status: u16, body: &str) -> LayoutApiError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v.get("error").and_then(Value::as_str).unwrap_or("");
    match status {
        401 => LayoutApiError::Unauthorized {
            reason: LAYOUTS_UNAUTHORIZED.to_owned(),
        },
        404 => LayoutApiError::NotFound {
            reason: "That doesn't exist on your pod.".to_owned(),
        },
        _ => {
            let reason = match code {
                "invalid_request" => "The pod rejected the request.".to_owned(),
                "" if status >= 500 => {
                    format!("The pod had a problem (HTTP {status}). Please try again.")
                }
                "" => format!("The pod answered HTTP {status}."),
                c => format!("The pod answered HTTP {status} ({c})."),
            };
            LayoutApiError::Server { status, reason }
        }
    }
}

impl Inner {
    /// One `GET` (with the session, if any); 2xx -> the body.
    fn catalog_get(&self, path: &str) -> Result<String, LayoutApiError> {
        self.catalog_call(reqwest::Method::GET, path, None)
    }

    /// One request (with the session, if any, and a JSON body); 2xx -> the
    /// body.
    fn catalog_call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<String, LayoutApiError> {
        let url = format!("{}{path}", self.base_url);
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(CATALOG_HTTP_TIMEOUT)
            .build()
            .map_err(|e| LayoutApiError::Server {
                status: 0,
                reason: format!("cannot start the request: {e}"),
            })?;
        // The schema header lets the pod say which versions this build can draw.
        let mut req = client
            .request(method.clone(), &url)
            .header(protocol::FEATURES_HEADER, protocol::features_header())
            .header(protocol::DEVICE_HEADER, &self.device_id)
            .headers(crate::identity::headers());
        // A pod host (D46) needs the pod token to know whose apps these are.
        if let Some(t) = self.session_token() {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let sent = req.send().and_then(|r| {
            let status = r.status().as_u16();
            r.text().map(|t| (status, t))
        });
        let (status, text) = match sent {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ConstructEngine] apps: {method} {url} failed: {e}");
                return Err(LayoutApiError::Offline {
                    reason: LAYOUTS_OFFLINE.to_owned(),
                });
            }
        };
        if (200..300).contains(&status) {
            return Ok(text);
        }
        let err = error_for(status, &text);
        let start: String = text.trim().chars().take(200).collect();
        eprintln!(
            "[ConstructEngine] apps: {method} {url} -> HTTP {status}: {start} (shown: {})",
            err.reason()
        );
        Err(err)
    }
}

impl ConstructEngine {
    /// `GET /v1/auth/sign-ins`: the sign-ins on this device's pod
    /// session, for Settings: `{"sign_ins":[{id, title, server?, username?,
    /// apps:[{id, title}]}], "providers":[{id, title, signed_in, intent,
    /// server_placeholder?}]}` (`providers`: every sign-in the pod offers,
    /// with the intent that starts it; missing from older pods). Blocking. Without a session it answers
    /// `{"sign_ins":[]}` without a request. Like the other catalog calls, a
    /// 401 is only an error here: it never signs out (the workspace fetch
    /// does that). A pod without `/v1/auth/sign-ins` answers 404: [`SIGN_INS_UNSUPPORTED`].
    pub fn sign_ins(&self) -> Result<String, LayoutApiError> {
        if self.inner.session_token().is_none() {
            return Ok(r#"{"sign_ins":[]}"#.to_owned());
        }
        match self.inner.catalog_get("/v1/auth/sign-ins") {
            Err(LayoutApiError::NotFound { .. }) => Err(LayoutApiError::NotFound {
                reason: SIGN_INS_UNSUPPORTED.to_owned(),
            }),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(segment("pixelfed"), "pixelfed");
        assert_eq!(segment("a/b c?"), "a%2Fb%20c%3F");
    }

    #[test]
    fn error_sentences() {
        assert_eq!(
            error_for(401, ""),
            LayoutApiError::Unauthorized {
                reason: LAYOUTS_UNAUTHORIZED.into()
            }
        );
        assert_eq!(
            error_for(404, r#"{"error":"layout_not_found"}"#).reason(),
            "That doesn't exist on your pod."
        );
        assert_eq!(
            error_for(404, "").reason(),
            "That doesn't exist on your pod."
        );
        assert_eq!(
            error_for(400, r#"{"error":"invalid_request"}"#),
            LayoutApiError::Server {
                status: 400,
                reason: "The pod rejected the request.".into()
            }
        );
        assert_eq!(
            error_for(503, "boom").reason(),
            "The pod had a problem (HTTP 503). Please try again."
        );
        assert_eq!(
            error_for(409, r#"{"error":"conflict"}"#).reason(),
            "The pod answered HTTP 409 (conflict)."
        );
    }
}
