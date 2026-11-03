//! Uploads (docs/DECISIONS.md D49, docs/POSTING.md): files a client sends
//! before an action that needs them (photos for a post). The core stores them
//! and knows nothing about what they are for; an adapter's route receives
//! their ids, reads them ([`Uploads::get`]), claims them while it works and
//! releases them when it is done.
//!
//! - `POST /v1/uploads` (a signed-in session): the raw body, `Content-Type`
//!   `image/jpeg|png|webp` (checked against the file's first bytes),
//!   `Idempotency-Key` (required; the same key and session give the same id).
//!   Answers `{"upload_id"}` (128 random bits, hex).
//! - `GET /v1/uploads/{id}`: the file, readable without a bearer (a public
//!   path, like the media proxy), so a placeholder can show it on any device.
//!   Gone once deleted.
//!
//! Bytes live in the pod's own database (deleted with the pod). Unclaimed
//! uploads go after [`UNCLAIMED_TTL_SECS`], released ones at the next sweep.

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{OptionalExtension, params};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::util::{hex, now_secs};
use crate::{ApiError, Pod, RequestCtx, Session, api_error, blocking, json_error, public_prefix};

/// Where uploads are posted and read.
pub const UPLOADS_PATH: &str = "/v1/uploads";
/// The public prefix of an upload's URL.
pub const PUBLIC_PATH: &str = "/v1/uploads/";
/// Default largest upload (`CONSTRUCT_UPLOAD_MAX_BYTES`).
pub const DEFAULT_MAX_BYTES: usize = 20 * 1024 * 1024;
/// `CONSTRUCT_UPLOAD_MAX_BYTES`, else [`DEFAULT_MAX_BYTES`] (at least 1 KB).
pub fn max_bytes_from_env() -> usize {
    std::env::var("CONSTRUCT_UPLOAD_MAX_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1024)
        .unwrap_or(DEFAULT_MAX_BYTES)
}

/// Most unclaimed uploads one session may hold.
pub const MAX_UNCLAIMED_PER_SESSION: i64 = 40;
/// Unclaimed uploads are deleted after this long.
pub const UNCLAIMED_TTL_SECS: i64 = 24 * 3600;
/// How often the sweeper runs.
pub const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// Longest idempotency key.
const MAX_KEY_LEN: usize = 128;

/// An upload's metadata, for an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRef {
    pub id: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub owner: Option<String>,
}

/// The kinds of file accepted, by their first bytes.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The core's upload routes (merged by [`crate::app`]).
pub(crate) fn router(max_bytes: usize) -> Router<Pod> {
    Router::new()
        .route(
            UPLOADS_PATH,
            post(create_upload).layer(DefaultBodyLimit::max(max_bytes)),
        )
        .route("/v1/uploads/{id}", get(read_upload))
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random number generator");
    hex(&b)
}

async fn create_upload(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let session = match pod.caller(&headers).await {
        Ok(c) => match c.into_session() {
            Some(s) => s,
            None => {
                return json_error(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": "session_required" }),
                );
            }
        },
        Err(e) => return e.into_response(),
    };
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| {
            !k.is_empty() && k.len() <= MAX_KEY_LEN && k.bytes().all(|b| b.is_ascii_graphic())
        })
        .map(str::to_owned);
    let Some(key) = key else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "idempotency_key_required" }),
        );
    };
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let Some(kind) = sniff(&body) else {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({ "error": "unsupported_upload", "accepted": ["image/jpeg", "image/png", "image/webp"] }),
        );
    };
    if declared != kind {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({ "error": "content_type_mismatch", "detected": kind }),
        );
    }
    let sha = hex(&Sha256::digest(&body));
    let hash = session.token_hash.clone();
    let result =
        blocking(&pod, move |s| {
            s.with_conn(|c| {
                if let Some(id) = c
                    .query_row(
                        "SELECT id FROM uploads WHERE token_hash = ?1 AND idem_key = ?2",
                        params![hash, key],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    return Ok(Ok(id));
                }
                let unclaimed: i64 = c.query_row(
                    "SELECT COUNT(*) FROM uploads WHERE token_hash = ?1 AND owner IS NULL",
                    [&hash],
                    |r| r.get(0),
                )?;
                if unclaimed >= MAX_UNCLAIMED_PER_SESSION {
                    return Ok(Err(()));
                }
                let id = new_id();
                c.execute(
                "INSERT INTO uploads (id, token_hash, idem_key, content_type, size, sha256, data,
                     created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, hash, key, kind, body.len() as i64, sha, body.as_ref(), now_secs()],
            )?;
                Ok(Ok(id))
            })
        })
        .await;
    match result {
        Ok(Ok(id)) => Json(json!({ "upload_id": id })).into_response(),
        // Transient for the client (429 is retried), not a rollback.
        Ok(Err(())) => json_error(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "too_many_uploads" }),
        ),
        Err(e) => e.into_response(),
    }
}

async fn read_upload(State(pod): State<Pod>, Path(id): Path<String>) -> Response {
    let not_found = || json_error(StatusCode::NOT_FOUND, json!({ "error": "not_found" }));
    if !valid_id(&id) {
        return not_found();
    }
    let row = blocking(&pod, move |s| {
        s.with_conn(|c| {
            c.query_row(
                "SELECT content_type, data FROM uploads WHERE id = ?1",
                [&id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
        })
    })
    .await;
    match row {
        Ok(Some((ct, data))) => {
            let mut resp = Response::new(Body::from(data));
            let h = resp.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&ct) {
                h.insert(header::CONTENT_TYPE, v);
            }
            h.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            h.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; sandbox"),
            );
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            );
            resp
        }
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// What adapters use: [`crate::AppState::uploads`].
pub struct Uploads<'a> {
    pub(crate) pod: &'a Pod,
}

fn upload_ref(r: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRef> {
    Ok(UploadRef {
        id: r.get(0)?,
        content_type: r.get(1)?,
        size: r.get(2)?,
        sha256: r.get(3)?,
        owner: r.get(4)?,
    })
}

impl Uploads<'_> {
    /// Upload `id` if `session` made it (any other session gets `None`).
    pub async fn get(&self, id: &str, session: &Session) -> Result<Option<UploadRef>, ApiError> {
        if !valid_id(id) {
            return Ok(None);
        }
        let (id, hash) = (id.to_owned(), session.token_hash.clone());
        blocking(self.pod, move |s| {
            s.with_conn(|c| {
                c.query_row(
                    "SELECT id, content_type, size, sha256, owner FROM uploads
                     WHERE id = ?1 AND token_hash = ?2 AND released_at IS NULL",
                    params![id, hash],
                    upload_ref,
                )
                .optional()
            })
        })
        .await
    }

    /// The bytes of upload `id` (no session check: call [`Self::get`] or
    /// [`Self::claim`] first).
    pub async fn bytes(&self, id: &str) -> Result<Option<Vec<u8>>, ApiError> {
        let id = id.to_owned();
        blocking(self.pod, move |s| {
            s.with_conn(|c| {
                c.query_row("SELECT data FROM uploads WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()
            })
        })
        .await
    }

    /// Claim `ids` (all made by `session`, unclaimed or already `owner`'s)
    /// for `owner` (an adapter's key, e.g. `pixelfed:post:<id>`), so the
    /// sweeper keeps them. All or nothing: `Ok(false)` if any is missing,
    /// another session's, or someone else's.
    pub async fn claim(
        &self,
        ids: &[String],
        session: &Session,
        owner: &str,
    ) -> Result<bool, ApiError> {
        if ids.iter().any(|id| !valid_id(id)) {
            return Ok(false);
        }
        let (ids, hash, owner) = (ids.to_vec(), session.token_hash.clone(), owner.to_owned());
        blocking(self.pod, move |s| {
            s.transaction(|tx| {
                for id in &ids {
                    let n = tx.execute(
                        "UPDATE uploads SET owner = ?3
                         WHERE id = ?1 AND token_hash = ?2 AND released_at IS NULL
                             AND (owner IS NULL OR owner = ?3)",
                        params![id, hash, owner],
                    )?;
                    if n == 0 {
                        return Err(rusqlite::Error::QueryReturnedNoRows);
                    }
                }
                Ok(())
            })
            .map(|()| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                e => Err(e),
            })
        })
        .await
    }

    /// The uploads `owner` holds.
    pub async fn owned(&self, owner: &str) -> Result<Vec<UploadRef>, ApiError> {
        let owner = owner.to_owned();
        blocking(self.pod, move |s| {
            s.with_conn(|c| {
                let mut st = c.prepare(
                    "SELECT id, content_type, size, sha256, owner FROM uploads
                     WHERE owner = ?1 AND released_at IS NULL",
                )?;
                st.query_map([&owner], upload_ref)?.collect()
            })
        })
        .await
    }

    /// Release everything `owner` holds: deleted now (the adapter is done).
    pub async fn release(&self, owner: &str) -> Result<(), ApiError> {
        let owner = owner.to_owned();
        blocking(self.pod, move |s| {
            s.with_conn(|c| c.execute("DELETE FROM uploads WHERE owner = ?1", [&owner]))
        })
        .await
        .map(|_| ())
    }

    /// The URL a client loads upload `id` from without its bearer: absolute
    /// when the request names its host (`base`, as adapters build media URLs),
    /// under the pod's public prefix.
    pub fn public_path(id: &str, req: &RequestCtx) -> String {
        format!("{}{PUBLIC_PATH}{id}", public_prefix(&req.headers))
    }
}

/// Delete unclaimed uploads older than [`UNCLAIMED_TTL_SECS`]. Returns how
/// many went.
pub fn sweep(store: &crate::store::Store, now: i64) -> rusqlite::Result<usize> {
    store.with_conn(|c| {
        c.execute(
            "DELETE FROM uploads WHERE owner IS NULL AND created_at < ?1",
            [now - UNCLAIMED_TTL_SECS],
        )
    })
}

/// A JSON 400, for adapters refusing upload ids.
pub fn bad_uploads() -> ApiError {
    api_error(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({ "error": "unknown_uploads" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_accepted_kinds() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg"), None);
        assert_eq!(sniff(b"GIF89a"), None);
        assert!(valid_id(&new_id()));
        assert!(!valid_id("../x"));
    }
}
