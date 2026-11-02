//! Comments on a Pixelfed post (docs/DECISIONS.md D34): a blocking, read-only
//! call like the app catalog ([`crate::catalog`]), run on the caller's thread
//! (Kotlin calls it on `Dispatchers.IO`). Unlike the catalog it needs the
//! signed-in session: a 401 for the current session signs out exactly like a
//! workspace-fetch 401 and refetches.
//!
//! Posting a comment is not here: it is an ordinary outbox mutation
//! (`OptimisticMutate` on the post's `reply_count`, endpoint
//! `/v1/pixelfed/posts/comment`), so it is queued offline and retried.

use std::time::Duration;

use serde_json::Value;

use crate::catalog::{LAYOUTS_OFFLINE, LayoutApiError};
use crate::{ConstructEngine, Inner};

/// Timeout for one comments request (the pod calls the upstream in turn).
const COMMENTS_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// `reason` when signed out (or the session was rejected).
pub const COMMENTS_SIGNED_OUT: &str = "Sign in to see comments.";
/// `reason` of a 404.
pub const COMMENTS_POST_GONE: &str = "This post isn't available anymore.";

/// Status ids the pod accepts (`[A-Za-z0-9_-]{1,64}`), so the id is path-safe.
fn valid_post_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The sentence shown for a non-2xx answer.
fn error_for(status: u16, body: &str) -> LayoutApiError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v.get("error").and_then(Value::as_str).unwrap_or("");
    match status {
        401 => LayoutApiError::Unauthorized {
            reason: COMMENTS_SIGNED_OUT.to_owned(),
        },
        404 => LayoutApiError::NotFound {
            reason: COMMENTS_POST_GONE.to_owned(),
        },
        502 | 504 => LayoutApiError::Server {
            status,
            reason: "Pixelfed didn't answer. Please try again.".to_owned(),
        },
        _ => LayoutApiError::Server {
            status,
            reason: match code {
                "" if status >= 500 => {
                    format!("The pod had a problem (HTTP {status}). Please try again.")
                }
                "" => format!("The pod answered HTTP {status}."),
                c => format!("The pod answered HTTP {status} ({c})."),
            },
        },
    }
}

impl Inner {
    fn comments_get(&self, post_id: &str) -> Result<String, LayoutApiError> {
        if !valid_post_id(post_id) {
            return Err(LayoutApiError::NotFound {
                reason: COMMENTS_POST_GONE.to_owned(),
            });
        }
        let Some(token) = self.session_token() else {
            return Err(LayoutApiError::Unauthorized {
                reason: COMMENTS_SIGNED_OUT.to_owned(),
            });
        };
        let url = format!("{}/v1/pixelfed/posts/{post_id}/comments", self.base_url);
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(COMMENTS_HTTP_TIMEOUT)
            .build()
            .map_err(|e| LayoutApiError::Server {
                status: 0,
                reason: format!("cannot start the request: {e}"),
            })?;
        let sent = client.get(&url).bearer_auth(&token).send().and_then(|r| {
            let status = r.status().as_u16();
            r.text().map(|t| (status, t))
        });
        let (status, text) = match sent {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ConstructEngine] comments: GET {url} failed: {e}");
                return Err(LayoutApiError::Offline {
                    reason: LAYOUTS_OFFLINE.to_owned(),
                });
            }
        };
        if (200..300).contains(&status) {
            let ok = serde_json::from_str::<Value>(&text)
                .is_ok_and(|v| v.get("comments").is_some_and(Value::is_array));
            if ok {
                return Ok(text);
            }
            let start: String = text.trim().chars().take(200).collect();
            eprintln!("[ConstructEngine] comments: GET {url} is not a comments list: {start}");
            return Err(LayoutApiError::Server {
                status,
                reason: "The pod sent something unexpected.".to_owned(),
            });
        }
        let err = error_for(status, &text);
        let start: String = text.trim().chars().take(200).collect();
        eprintln!(
            "[ConstructEngine] comments: GET {url} -> HTTP {status}: {start} (shown: {})",
            err.reason()
        );
        if status == 401
            && let Some(snap) = self.expire_session(&token, "comments")
        {
            // Same as a fetch 401: signed out now, then the signed-out screen.
            self.notify(snap);
            self.refresh();
        }
        Err(err)
    }
}

impl ConstructEngine {
    /// `GET /v1/pixelfed/posts/<id>/comments` with the session: `{post_id,
    /// comments: [{id, in_reply_to_id, depth, author: {id, username,
    /// display_name, avatar_url}, text, created_at, like_count, is_liked,
    /// is_own}]}` in thread order. Blocking; not cached. Errors carry a
    /// sentence for the UI (signed out, offline, post gone, upstream down).
    pub fn post_comments(&self, post_id: &str) -> Result<String, LayoutApiError> {
        self.inner.comments_get(post_id.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_ids_must_be_path_safe() {
        assert!(valid_post_id("781254316723458049"));
        assert!(valid_post_id("abc_D-9"));
        for bad in ["", "a/b", "../x", "a b", "%2F", &"9".repeat(65)] {
            assert!(!valid_post_id(bad), "{bad}");
        }
    }

    #[test]
    fn error_sentences() {
        assert_eq!(error_for(401, "").reason(), COMMENTS_SIGNED_OUT);
        assert_eq!(
            error_for(404, r#"{"error":"post_not_found"}"#).reason(),
            COMMENTS_POST_GONE
        );
        assert_eq!(
            error_for(502, r#"{"error":"upstream_unreachable"}"#).reason(),
            "Pixelfed didn't answer. Please try again."
        );
        assert_eq!(
            error_for(500, "").reason(),
            "The pod had a problem (HTTP 500). Please try again."
        );
        assert_eq!(
            error_for(400, r#"{"error":"invalid_request"}"#).reason(),
            "The pod answered HTTP 400 (invalid_request)."
        );
    }
}
