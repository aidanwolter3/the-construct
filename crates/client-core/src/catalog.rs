//! Layout catalog API (docs/DECISIONS.md D26, D27): blocking calls to the
//! pod's `/v1/layouts` endpoints for a layout browser. Each call runs on the
//! caller's thread (Kotlin calls them on `Dispatchers.IO`, never the UI
//! thread) with the engine's base URL and current pod session, and returns
//! the pod's JSON body unchanged.
//!
//! Errors are a [`LayoutApiError`] with a short, human `reason`. A 401 for
//! the current session signs out like a fetch 401 (D19) and refetches. A
//! successful activate / restore that changed the active layout refetches the
//! workspace before returning, so the snapshot already shows the new layout
//! (the SSE echo arrives too; both are idempotent, D24) and PENDING outbox
//! rows are re-applied by the refetch merge (D14).

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{CanvasEngine, Inner};

/// Timeout for one catalog request.
const LAYOUT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// `reason` of [`LayoutApiError::Offline`].
pub const LAYOUTS_OFFLINE: &str = "Can't reach your pod. Check your connection and try again.";
/// `reason` of a 401 on activate / restore.
pub const LAYOUTS_SIGN_IN_TO_CHANGE: &str = "Sign in to change layouts.";
/// `reason` of a 401 on a read (`pixelfed_home` layouts need a session).
pub const LAYOUTS_SIGN_IN_TO_VIEW: &str = "Sign in to see these layouts.";

/// Why a catalog call failed. `reason` is a short sentence for the UI; the
/// URL, status and body go to the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutApiError {
    /// The pod could not be reached (transport error or timeout).
    Offline { reason: String },
    /// HTTP 401: no or a stale session.
    Unauthorized { reason: String },
    /// HTTP 404: unknown layout, version or workspace.
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
fn error_for(status: u16, body: &str, write: bool) -> LayoutApiError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v.get("error").and_then(Value::as_str).unwrap_or("");
    match status {
        401 => LayoutApiError::Unauthorized {
            reason: if write {
                LAYOUTS_SIGN_IN_TO_CHANGE
            } else {
                LAYOUTS_SIGN_IN_TO_VIEW
            }
            .to_owned(),
        },
        404 => LayoutApiError::NotFound {
            reason: match code {
                "version_not_found" => "That version doesn't exist.",
                "workspace_not_found" => "This workspace has no layouts.",
                _ => "That layout doesn't exist.",
            }
            .to_owned(),
        },
        _ => {
            let reason = match code {
                "invalid_layout_id" => "That isn't a valid layout id.".to_owned(),
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
    /// One catalog request: `GET` (body `None`) or `POST` with a JSON body,
    /// with the current session's bearer. 2xx -> the body. A 401 for the
    /// session that is still current signs out and refetches.
    fn layout_request(&self, path: &str, body: Option<&Value>) -> Result<String, LayoutApiError> {
        let url = format!("{}{path}", self.base_url);
        let write = body.is_some();
        let token = self.session_token();
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(LAYOUT_HTTP_TIMEOUT)
            .build()
            .map_err(|e| LayoutApiError::Server {
                status: 0,
                reason: format!("cannot start the request: {e}"),
            })?;
        let mut req = match body {
            Some(b) => client.post(&url).json(b),
            None => client.get(&url),
        };
        if let Some(t) = &token {
            req = req.bearer_auth(t);
        }
        let method = if write { "POST" } else { "GET" };
        let sent = req.send().and_then(|r| {
            let status = r.status().as_u16();
            r.text().map(|t| (status, t))
        });
        let (status, text) = match sent {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[CanvasEngine] layouts: {method} {url} failed: {e}");
                return Err(LayoutApiError::Offline {
                    reason: LAYOUTS_OFFLINE.to_owned(),
                });
            }
        };
        if (200..300).contains(&status) {
            return Ok(text);
        }
        let err = error_for(status, &text, write);
        let start: String = text.trim().chars().take(200).collect();
        eprintln!(
            "[CanvasEngine] layouts: {method} {url} -> HTTP {status}: {start} (shown: {})",
            err.reason()
        );
        if status == 401
            && let Some(t) = &token
            && let Some(snap) = self.expire_session(t, "layout catalog request")
        {
            self.notify(snap);
            if !self.signal.is_shut_down() {
                self.refresh();
            }
        }
        Err(err)
    }

    /// Refetch after a write that changed the active layout.
    fn refresh_after_write(&self, what: &str) {
        if self.signal.is_shut_down() {
            return;
        }
        println!("[CanvasEngine] layouts: {what} changed the active layout; refetching");
        self.refresh();
    }
}

impl CanvasEngine {
    /// `GET /v1/layouts?workspace_id=<engine's workspace>`: `{"layouts":
    /// [{id, workspace_id, title, description, preview_hint, builtin,
    /// current_version, created_at, updated_at, active, active_version?}],
    /// "workspaces": [...], "workspace_id", "workspace_revision"}` (D26).
    /// Blocking.
    pub fn list_layouts(&self) -> Result<String, LayoutApiError> {
        let ws = &self.inner.workspace_id;
        self.inner
            .layout_request(&format!("/v1/layouts?workspace_id={}", segment(ws)), None)
    }

    /// `GET /v1/layouts/<id>/versions[?before=V&limit=N]`: one page of the
    /// layout's history, newest first (`next_before` when older versions
    /// remain; pass it as `before` for the next page). Blocking.
    pub fn layout_versions(
        &self,
        layout_id: &str,
        before: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String, LayoutApiError> {
        let mut query = Vec::new();
        if let Some(b) = before {
            query.push(format!("before={b}"));
        }
        if let Some(l) = limit {
            query.push(format!("limit={l}"));
        }
        let mut path = format!("/v1/layouts/{}/versions", segment(layout_id));
        if !query.is_empty() {
            path = format!("{path}?{}", query.join("&"));
        }
        self.inner.layout_request(&path, None)
    }

    /// `GET /v1/layouts/<id>/versions/<n>`: the version's metadata plus the
    /// full `layout` (`UiNode`). Blocking.
    pub fn layout_version(&self, layout_id: &str, version: u32) -> Result<String, LayoutApiError> {
        self.inner.layout_request(
            &format!("/v1/layouts/{}/versions/{version}", segment(layout_id)),
            None,
        )
    }

    /// `POST /v1/layouts/<id>/activate` (`{}`, or `{"version": n}` to restore
    /// version n first): `{workspace_id, revision, layout_id, layout_version,
    /// unchanged, restored_version?}`. When it changed this engine's
    /// workspace the workspace is refetched before this returns. Blocking.
    pub fn activate_layout(
        &self,
        layout_id: &str,
        version: Option<u32>,
    ) -> Result<String, LayoutApiError> {
        let body = match version {
            Some(v) => json!({ "version": v }),
            None => json!({}),
        };
        let text = self.inner.layout_request(
            &format!("/v1/layouts/{}/activate", segment(layout_id)),
            Some(&body),
        )?;
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        let ours = v["workspace_id"].as_str() == Some(self.inner.workspace_id.as_str());
        if ours && v["unchanged"].as_bool() != Some(true) {
            self.inner.refresh_after_write("activate");
        }
        Ok(text)
    }

    /// `POST /v1/layouts/<id>/restore {"version": n}`: appends a copy of
    /// version n (`{layout_id, workspace_id, version, restored_from,
    /// unchanged, active, revision?}`). When the layout is this engine's
    /// active one and something was appended, the workspace is refetched
    /// before this returns. Blocking.
    pub fn restore_layout_version(
        &self,
        layout_id: &str,
        version: u32,
    ) -> Result<String, LayoutApiError> {
        let text = self.inner.layout_request(
            &format!("/v1/layouts/{}/restore", segment(layout_id)),
            Some(&json!({ "version": version })),
        )?;
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        let ours = v["workspace_id"].as_str() == Some(self.inner.workspace_id.as_str());
        if ours && v["active"].as_bool() == Some(true) && v["unchanged"].as_bool() != Some(true) {
            self.inner.refresh_after_write("restore");
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(segment("home.counter"), "home.counter");
        assert_eq!(segment("a/b c?"), "a%2Fb%20c%3F");
    }

    #[test]
    fn error_sentences() {
        assert_eq!(
            error_for(401, "", true),
            LayoutApiError::Unauthorized {
                reason: LAYOUTS_SIGN_IN_TO_CHANGE.into()
            }
        );
        assert_eq!(error_for(401, "", false).reason(), LAYOUTS_SIGN_IN_TO_VIEW);
        assert_eq!(
            error_for(404, r#"{"error":"version_not_found"}"#, false).reason(),
            "That version doesn't exist."
        );
        assert_eq!(
            error_for(404, "", false).reason(),
            "That layout doesn't exist."
        );
        assert_eq!(
            error_for(400, r#"{"error":"invalid_layout_id"}"#, false),
            LayoutApiError::Server {
                status: 400,
                reason: "That isn't a valid layout id.".into()
            }
        );
        assert_eq!(
            error_for(503, "boom", false).reason(),
            "The pod had a problem (HTTP 503). Please try again."
        );
        assert_eq!(
            error_for(409, r#"{"error":"conflict"}"#, true).reason(),
            "The pod answered HTTP 409 (conflict)."
        );
    }
}
