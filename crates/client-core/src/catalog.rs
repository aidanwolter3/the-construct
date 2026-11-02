//! App catalog API (docs/DECISIONS.md D27, D32): blocking, read-only calls to
//! the pod's public `/v1/apps` endpoints for the menu and the version picker.
//! Each call runs on the caller's thread (Kotlin calls them on
//! `Dispatchers.IO`, never the UI thread) with the engine's base URL and
//! returns the pod's JSON body unchanged.
//!
//! The endpoints are public, so no bearer is sent and a 401 (which the pod
//! does not answer for them) is just an [`LayoutApiError::Unauthorized`]
//! error: it never signs out. Clients cannot change layouts: the pod serves
//! each app's latest version, and picking an older one is client-local.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::{ConstructEngine, Inner};

/// Timeout for one catalog request.
const CATALOG_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// `reason` of [`LayoutApiError::Offline`].
pub const LAYOUTS_OFFLINE: &str = "Can't reach your pod. Check your connection and try again.";
/// `reason` of a 401 (not expected: the app endpoints are public).
pub const LAYOUTS_UNAUTHORIZED: &str = "The pod refused to list apps.";

/// Why a catalog call failed. `reason` is a short sentence for the UI; the
/// URL, status and body go to the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutApiError {
    /// The pod could not be reached (transport error or timeout).
    Offline { reason: String },
    /// HTTP 401 (not expected for these public endpoints).
    Unauthorized { reason: String },
    /// HTTP 404: unknown app or version.
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
fn segment(s: &str) -> String {
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
            reason: match code {
                "version_not_found" => "That version doesn't exist.",
                _ => "That app doesn't exist.",
            }
            .to_owned(),
        },
        _ => {
            let reason = match code {
                "invalid_workspace_id" => "That isn't a valid app id.".to_owned(),
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
    /// One public `GET`; 2xx -> the body.
    fn catalog_get(&self, path: &str) -> Result<String, LayoutApiError> {
        let url = format!("{}{path}", self.base_url);
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(CATALOG_HTTP_TIMEOUT)
            .build()
            .map_err(|e| LayoutApiError::Server {
                status: 0,
                reason: format!("cannot start the request: {e}"),
            })?;
        let sent = client.get(&url).send().and_then(|r| {
            let status = r.status().as_u16();
            r.text().map(|t| (status, t))
        });
        let (status, text) = match sent {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ConstructEngine] apps: GET {url} failed: {e}");
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
            "[ConstructEngine] apps: GET {url} -> HTTP {status}: {start} (shown: {})",
            err.reason()
        );
        Err(err)
    }
}

impl ConstructEngine {
    /// `GET /v1/apps`: `{"apps":[{id, title, icon, latest_version,
    /// updated_at}]}`. Blocking.
    pub fn list_apps(&self) -> Result<String, LayoutApiError> {
        self.inner.catalog_get("/v1/apps")
    }

    /// `GET /v1/apps/<id>/versions[?before=N&limit=L]`: one page of the app's
    /// history, newest first (`{app_id, latest_version, versions:[{version,
    /// created_at, source, note?}], next_before?}`; pass `next_before` as
    /// `before` for the next page). Blocking.
    pub fn app_versions(
        &self,
        app_id: &str,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Result<String, LayoutApiError> {
        let mut query = Vec::new();
        if let Some(b) = before {
            query.push(format!("before={b}"));
        }
        if let Some(l) = limit {
            query.push(format!("limit={l}"));
        }
        let mut path = format!("/v1/apps/{}/versions", segment(app_id));
        if !query.is_empty() {
            path = format!("{path}?{}", query.join("&"));
        }
        self.inner.catalog_get(&path)
    }

    /// `GET /v1/apps/<id>/versions/<n>`: `{app_id, version, created_at,
    /// source, note?, layout}` (`layout` is the full `UiNode`). Blocking.
    pub fn app_version(&self, app_id: &str, version: i64) -> Result<String, LayoutApiError> {
        self.inner
            .catalog_get(&format!("/v1/apps/{}/versions/{version}", segment(app_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(segment("pixelfed_home"), "pixelfed_home");
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
            error_for(404, r#"{"error":"version_not_found"}"#).reason(),
            "That version doesn't exist."
        );
        assert_eq!(
            error_for(404, r#"{"error":"app_not_found"}"#).reason(),
            "That app doesn't exist."
        );
        assert_eq!(error_for(404, "").reason(), "That app doesn't exist.");
        assert_eq!(
            error_for(400, r#"{"error":"invalid_workspace_id"}"#),
            LayoutApiError::Server {
                status: 400,
                reason: "That isn't a valid app id.".into()
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
