//! Posting photos (docs/POSTING.md, D49/D50): the client's outbox uploads the
//! photos to the pod (`/v1/uploads`), then posts
//! `POST /v1/pixelfed/posts/create {caption, media: [upload ids], client_id}`.
//! The pod answers 202 at once and a job does the rest:
//!
//! 1. each photo goes to Pixelfed (`POST /api/v2/media`, `/api/v1/media` on
//!    instances without v2),
//! 2. the job waits until Pixelfed has processed it (`GET /api/v1/media/{id}`),
//! 3. it posts (`POST /api/v1/statuses` with `media_ids[]`, idempotent on the
//!    client's id),
//! 4. it releases the uploads and tells the session's clients to reload
//!    (`event: data`).
//!
//! Until Pixelfed lists the post, the home feed shows a placeholder at the top
//! under the URI the client's own placeholder has
//! (`pixelfed://posts/pending-<client_id>`), so nothing flickers: "Posting…",
//! or, when Pixelfed refused it, the reason with Retry and Discard.
//! Rows live in `pixelfed_pending_posts`, so a restart resumes them
//! ([`resume`]).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use pod_core::session::sign_in_required;
use pod_core::util::now_secs;
use pod_core::{ApiError, Pod, RequestCtx, Session, blocking, parse_json_body};
use protocol::EntityRecord;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::layout::WORKSPACE_ID;
use crate::routes::{adapter, invalid_request, session_account, with_token};
use crate::upstream::UpstreamError;

/// Most photos in one post (Pixelfed's default album size).
pub const MAX_PHOTOS: usize = 4;
/// Longest caption (Pixelfed's default).
pub const MAX_CAPTION_CHARS: usize = 500;
/// Upstream attempts before a post is marked failed (transient errors).
pub const MAX_ATTEMPTS: i64 = 6;
/// Longest wait for Pixelfed to process one photo.
pub const PROCESSING_TIMEOUT: Duration = Duration::from_secs(120);
/// A finished post's row is kept this long (a late retry of its create is
/// still recognized; the feed shows the post even if Pixelfed lags).
pub const DONE_KEEP_SECS: i64 = 10 * 60;
/// Prefix of a placeholder's id in its URI.
pub const PENDING_PREFIX: &str = "pending-";

/// The placeholder label while a post is on its way.
pub const POSTING_LABEL: &str = "Posting…";

/// The adapter's table (its migration 2).
pub const MIGRATION: &str = "CREATE TABLE pixelfed_pending_posts (
     token_hash TEXT NOT NULL,
     client_id  TEXT NOT NULL,
     caption    TEXT NOT NULL,
     upload_ids TEXT NOT NULL,
     media_ids  TEXT NOT NULL DEFAULT '[]',
     state      TEXT NOT NULL,
     error      TEXT,
     status_id  TEXT,
     attempts   INTEGER NOT NULL DEFAULT 0,
     created_at INTEGER NOT NULL,
     updated_at INTEGER NOT NULL,
     PRIMARY KEY (token_hash, client_id)
 );";

/// One row of `pixelfed_pending_posts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPost {
    pub token_hash: String,
    pub client_id: String,
    pub caption: String,
    pub upload_ids: Vec<String>,
    /// Pixelfed's media id per upload, as far as uploaded ("" before).
    pub media_ids: Vec<String>,
    /// `posting`, `failed` or `done`.
    pub state: String,
    pub error: Option<String>,
    pub status_id: Option<String>,
    pub attempts: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLUMNS: &str = "token_hash, client_id, caption, upload_ids, media_ids, state, error, \
     status_id, attempts, created_at, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PendingPost> {
    let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
    Ok(PendingPost {
        token_hash: r.get(0)?,
        client_id: r.get(1)?,
        caption: r.get(2)?,
        upload_ids: list(r.get(3)?),
        media_ids: list(r.get(4)?),
        state: r.get(5)?,
        error: r.get(6)?,
        status_id: r.get(7)?,
        attempts: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

/// The upload owner key of a post (its claim on the pod's uploads).
fn owner(token_hash: &str, client_id: &str) -> String {
    format!("pixelfed:post:{token_hash}:{client_id}")
}

pub fn pending_uri(client_id: &str) -> String {
    crate::post_uri(&format!("{PENDING_PREFIX}{client_id}"))
}

fn valid_client_id(k: &str) -> bool {
    (8..=64).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

async fn load(pod: &Pod, hash: &str, client_id: &str) -> Result<Option<PendingPost>, ApiError> {
    let (h, c) = (hash.to_owned(), client_id.to_owned());
    blocking(pod, move |s| {
        s.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM pixelfed_pending_posts WHERE token_hash = ?1 AND client_id = ?2"),
                params![h, c],
                row,
            )
            .optional()
        })
    })
    .await
}

/// A session's pending posts, newest first.
pub async fn for_session(pod: &Pod, hash: &str) -> Result<Vec<PendingPost>, ApiError> {
    let h = hash.to_owned();
    blocking(pod, move |s| {
        s.with_conn(|conn| {
            let mut st = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM pixelfed_pending_posts WHERE token_hash = ?1
                 ORDER BY created_at DESC, client_id"
            ))?;
            st.query_map([&h], row)?.collect()
        })
    })
    .await
}

async fn update(pod: &Pod, p: &PendingPost) -> Result<(), ApiError> {
    let p = p.clone();
    blocking(pod, move |s| {
        s.with_conn(|conn| {
            conn.execute(
                "UPDATE pixelfed_pending_posts SET media_ids = ?3, state = ?4, error = ?5,
                     status_id = ?6, attempts = ?7, updated_at = ?8
                 WHERE token_hash = ?1 AND client_id = ?2",
                params![
                    p.token_hash,
                    p.client_id,
                    serde_json::to_string(&p.media_ids).unwrap_or_else(|_| "[]".into()),
                    p.state,
                    p.error,
                    p.status_id,
                    p.attempts,
                    now_secs()
                ],
            )
        })
    })
    .await
    .map(|_| ())
}

async fn delete(pod: &Pod, hash: &str, client_id: &str) -> Result<(), ApiError> {
    let (h, c) = (hash.to_owned(), client_id.to_owned());
    blocking(pod, move |s| {
        s.with_conn(|conn| {
            conn.execute(
                "DELETE FROM pixelfed_pending_posts WHERE token_hash = ?1 AND client_id = ?2",
                params![h, c],
            )
        })
    })
    .await
    .map(|_| ())
}

/// The session a stored row belongs to, as the core knows it now.
async fn session_of(pod: &Pod, hash: &str) -> Result<Option<Session>, ApiError> {
    let h = hash.to_owned();
    blocking(pod, move |s| {
        if !s.session_exists(&h)? {
            return Ok(None);
        }
        Ok(Some(Session {
            sign_ins: s.session_sign_ins(&h)?,
            pinned: s.session_pinned(&h)?,
            token_hash: h,
        }))
    })
    .await
}

/// The caller's session with the `pixelfed` sign-in, else the response to send.
async fn signed_in_session(pod: &Pod, headers: &HeaderMap) -> Result<Session, Box<Response>> {
    let caller = pod
        .caller(headers)
        .await
        .map_err(|e| Box::new(e.into_response()))?;
    match caller.session() {
        Some(s) if s.has(crate::adapter::SIGN_IN) => Ok(s.clone()),
        _ => Err(Box::new(sign_in_required(&caller))),
    }
}

fn client_id_of(payload: &Value) -> Option<String> {
    payload["client_id"]
        .as_str()
        .filter(|k| valid_client_id(k))
        .map(str::to_owned)
}

/// `POST /v1/pixelfed/posts/create {caption?, media: [upload id, ...], client_id}`
/// -> 202 `{ok, id}` (the placeholder's URI): 1..=[`MAX_PHOTOS`] uploads of
/// this session, a caption of at most [`MAX_CAPTION_CHARS`]. Bad input 400,
/// unknown uploads 422, no session 401/403. The same `client_id` again answers
/// 202 without posting twice.
pub async fn create(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let session = match signed_in_session(&pod, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let expected = || {
        invalid_request(json!({
            "caption": format!("0..={MAX_CAPTION_CHARS} characters"),
            "media": format!("1..={MAX_PHOTOS} upload ids"), "client_id": "nonce",
        }))
    };
    let caption = match &payload["caption"] {
        Value::Null => String::new(),
        Value::String(c) if c.trim().chars().count() <= MAX_CAPTION_CHARS => c.trim().to_owned(),
        _ => return expected(),
    };
    let media: Option<Vec<String>> = payload["media"].as_array().and_then(|a| {
        a.iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    });
    let (Some(media), Some(client_id)) = (media, client_id_of(&payload)) else {
        return expected();
    };
    let unique: HashSet<&String> = media.iter().collect();
    if media.is_empty() || media.len() > MAX_PHOTOS || unique.len() != media.len() {
        return expected();
    }
    let hash = session.token_hash.clone();
    let accepted = || {
        (
            StatusCode::ACCEPTED,
            Json(json!({ "ok": true, "id": pending_uri(&client_id) })),
        )
            .into_response()
    };
    match load(&pod, &hash, &client_id).await {
        Ok(Some(_)) => return accepted(),
        Ok(None) => {}
        Err(e) => return e.into_response(),
    }
    match pod
        .uploads()
        .claim(&media, &session, &owner(&hash, &client_id))
        .await
    {
        Ok(true) => {}
        Ok(false) => return pod_core::uploads::bad_uploads().into_response(),
        Err(e) => return e.into_response(),
    }
    let (h, c, cap, m) = (
        hash.clone(),
        client_id.clone(),
        caption,
        serde_json::to_string(&media).unwrap_or_default(),
    );
    let inserted = blocking(&pod, move |s| {
        s.with_conn(|conn| {
            let now = now_secs();
            conn.execute(
                "INSERT OR IGNORE INTO pixelfed_pending_posts
                     (token_hash, client_id, caption, upload_ids, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'posting', ?5, ?5)",
                params![h, c, cap, m, now],
            )
        })
    })
    .await;
    if let Err(e) = inserted {
        return e.into_response();
    }
    tracing::info!(photos = media.len(), "Pixelfed post queued");
    spawn(&pod, &hash, &client_id);
    accepted()
}

/// `POST /v1/pixelfed/posts/pending/retry {client_id}`: post a failed one again.
pub async fn retry(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    pending_action(pod, headers, body, true).await
}

/// `POST /v1/pixelfed/posts/pending/discard {client_id}`: forget a pending or
/// failed post (and its photos).
pub async fn discard(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    pending_action(pod, headers, body, false).await
}

async fn pending_action(pod: Pod, headers: HeaderMap, body: Bytes, retry: bool) -> Response {
    let session = match signed_in_session(&pod, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let Some(client_id) = client_id_of(&payload) else {
        return invalid_request(json!({ "client_id": "nonce" }));
    };
    let hash = session.token_hash.clone();
    let found = match load(&pod, &hash, &client_id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return pod_core::json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "post_not_found" }),
            );
        }
        Err(e) => return e.into_response(),
    };
    if retry {
        if found.state == "failed" {
            let p = PendingPost {
                state: "posting".into(),
                error: None,
                attempts: 0,
                ..found
            };
            if let Err(e) = update(&pod, &p).await {
                return e.into_response();
            }
            spawn(&pod, &hash, &client_id);
        }
    } else {
        // A post already done stays (it is on Pixelfed now).
        if found.state != "done" {
            if let Err(e) = delete(&pod, &hash, &client_id).await {
                return e.into_response();
            }
            let _ = pod.uploads().release(&owner(&hash, &client_id)).await;
        }
        pod.publish_data(WORKSPACE_ID, &session);
    }
    Json(json!({ "ok": true, "id": pending_uri(&client_id) })).into_response()
}

/// Posts whose job runs in this pod (so one row never gets two jobs).
#[derive(Default)]
struct Running(Mutex<HashSet<String>>);

/// Start the job of a row (no-op if it is already running).
pub fn spawn(pod: &Pod, hash: &str, client_id: &str) {
    let key = format!("{hash}:{client_id}");
    let running = pod.local::<Running>();
    if !running
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone())
    {
        return;
    }
    let (pod, hash, client_id) = (pod.clone(), hash.to_owned(), client_id.to_owned());
    tokio::spawn(async move {
        run(&pod, &hash, &client_id).await;
        running
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    });
}

/// [`pod_core::AppAdapter::resume`]: restart the jobs a restart interrupted.
pub async fn resume(pod: &Pod) {
    let rows = blocking(pod, |s| {
        s.with_conn(|conn| {
            let mut st = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM pixelfed_pending_posts WHERE state = 'posting'"
            ))?;
            st.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>()
        })
    })
    .await;
    match rows {
        Ok(rows) => {
            if !rows.is_empty() {
                tracing::info!(posts = rows.len(), "resuming Pixelfed posts");
            }
            for p in rows {
                spawn(pod, &p.token_hash, &p.client_id);
            }
        }
        Err(e) => tracing::error!(?e, "cannot read pending Pixelfed posts"),
    }
}

/// Why one attempt stopped.
enum Stop {
    /// Try again later (upstream down, 5xx, timeouts).
    Transient(String),
    /// Pixelfed refused it (shown to the user).
    Refused(String),
}

/// A short reason from an upstream error body (`{"error": "..."}`).
fn reason(detail: &str) -> String {
    let text = serde_json::from_str::<Value>(detail)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| detail.to_owned());
    let mut text = text.trim().to_owned();
    if text.chars().count() > 160 {
        text = text.chars().take(160).collect::<String>() + "…";
    }
    if text.is_empty() {
        "Pixelfed refused the post.".into()
    } else {
        text
    }
}

fn classify(e: UpstreamError) -> Stop {
    match e {
        UpstreamError::Status(StatusCode::UNAUTHORIZED, _) => {
            Stop::Refused("Sign in to Pixelfed again, then retry.".into())
        }
        UpstreamError::Status(s, d)
            if s.is_client_error()
                && s != StatusCode::TOO_MANY_REQUESTS
                && s != StatusCode::REQUEST_TIMEOUT =>
        {
            Stop::Refused(reason(&d))
        }
        other => Stop::Transient(other.to_string()),
    }
}

/// The job: attempts with backoff until done, refused, or out of attempts.
async fn run(pod: &Pod, hash: &str, client_id: &str) {
    loop {
        let Ok(Some(mut p)) = load(pod, hash, client_id).await else {
            return;
        };
        if p.state != "posting" {
            return;
        }
        let outcome = attempt(pod, &mut p).await;
        let session = session_of(pod, hash).await.ok().flatten();
        match outcome {
            Ok(()) => {
                tracing::info!(status = ?p.status_id, "Pixelfed post published");
                let _ = pod.uploads().release(&owner(hash, client_id)).await;
                if let Some(s) = &session {
                    pod.publish_data(WORKSPACE_ID, s);
                }
                return;
            }
            Err(Stop::Refused(why)) => {
                tracing::info!(reason = %why, "Pixelfed refused a post");
                p.state = "failed".into();
                p.error = Some(why);
                let _ = update(pod, &p).await;
                if let Some(s) = &session {
                    pod.publish_data(WORKSPACE_ID, s);
                }
                return;
            }
            Err(Stop::Transient(why)) => {
                p.attempts += 1;
                tracing::warn!(attempt = p.attempts, error = %why, "Pixelfed post attempt failed");
                if p.attempts >= MAX_ATTEMPTS {
                    p.state = "failed".into();
                    p.error = Some("Pixelfed didn't answer. Try again later.".into());
                    let _ = update(pod, &p).await;
                    if let Some(s) = &session {
                        pod.publish_data(WORKSPACE_ID, s);
                    }
                    return;
                }
                let _ = update(pod, &p).await;
                let secs = 2u64.saturating_pow(p.attempts as u32).min(60);
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
        }
    }
}

/// One attempt: upload what isn't uploaded, wait for processing, post. Saves
/// its progress in `p` (and the row) as it goes.
async fn attempt(pod: &Pod, p: &mut PendingPost) -> Result<(), Stop> {
    let store_err = |e: ApiError| Stop::Transient(format!("pod store: {e:?}"));
    let Some(session) = session_of(pod, &p.token_hash).await.map_err(store_err)? else {
        return Err(Stop::Refused("This device's session ended.".into()));
    };
    let Some((instance, account)) = session_account(pod, &session).await.map_err(store_err)? else {
        return Err(Stop::Refused(
            "Sign in to Pixelfed again, then retry.".into(),
        ));
    };
    let rt = adapter(pod).runtime();
    let upstream = rt.api(&instance);
    p.media_ids.resize(p.upload_ids.len(), String::new());

    // 1. Upload.
    for i in 0..p.upload_ids.len() {
        if !p.media_ids[i].is_empty() {
            continue;
        }
        let up = pod.uploads();
        let Some(meta) = up
            .owned(&owner(&p.token_hash, &p.client_id))
            .await
            .map_err(store_err)?
            .into_iter()
            .find(|u| u.id == p.upload_ids[i])
        else {
            return Err(Stop::Refused(
                "A photo of this post is gone; discard it.".into(),
            ));
        };
        let Some(bytes) = up.bytes(&meta.id).await.map_err(store_err)? else {
            return Err(Stop::Refused(
                "A photo of this post is gone; discard it.".into(),
            ));
        };
        let (result, _) = with_token(pod, &instance, account.clone(), |token| {
            let (upstream, bytes, ct) = (&upstream, bytes.clone(), meta.content_type.clone());
            async move { upstream.upload_media(&token, bytes, &ct).await }
        })
        .await;
        let media = result.map_err(classify)?;
        let Some(id) = media["id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| media["id"].as_u64().map(|n| n.to_string()))
            .filter(|id| crate::routes::valid_status_id(id))
        else {
            return Err(Stop::Transient(format!(
                "media answer without an id: {media}"
            )));
        };
        p.media_ids[i] = id;
        update(pod, p).await.map_err(store_err)?;
    }

    // 2. Wait until each is processed.
    for id in p.media_ids.clone() {
        let deadline = tokio::time::Instant::now() + PROCESSING_TIMEOUT;
        let mut wait = Duration::from_millis(250);
        loop {
            let (result, _) = with_token(pod, &instance, account.clone(), |token| {
                let (upstream, id) = (&upstream, &id);
                async move { upstream.media_status(&token, id).await }
            })
            .await;
            if result.map_err(classify)?.is_some() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Stop::Transient("a photo is still processing".into()));
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(5));
        }
    }

    // 3. Post.
    let key = format!("construct-{}", p.client_id);
    let (result, _) = with_token(pod, &instance, account, |token| {
        let (upstream, caption, media, key) = (&upstream, &p.caption, &p.media_ids, &key);
        async move { upstream.create_post(&token, caption, media, key).await }
    })
    .await;
    let status = result.map_err(classify)?;
    p.status_id = status["id"].as_str().map(str::to_owned);
    p.state = "done".into();
    p.error = None;
    update(pod, p).await.map_err(store_err)?;
    Ok(())
}

/// The feed's placeholders for `session` (newest first), and the ids of
/// finished posts the timeline (`listed`) does not show yet. Rows of posts
/// finished more than [`DONE_KEEP_SECS`] ago are dropped here.
pub async fn placeholders(
    pod: &Pod,
    session: &Session,
    req: &RequestCtx,
    listed: &HashSet<String>,
) -> Result<
    (
        Vec<(String, EntityRecord, Vec<(String, EntityRecord)>)>,
        Vec<String>,
    ),
    ApiError,
> {
    let rows = for_session(pod, &session.token_hash).await?;
    let base = public_base(&req.headers);
    let mut out = Vec::new();
    let mut missing = Vec::new();
    let now = now_secs();
    for p in rows {
        if p.state == "done" {
            // Kept a while (hidden) so a late retry of the create still
            // answers 202 instead of refusing released uploads.
            if now - p.updated_at >= DONE_KEEP_SECS {
                delete(pod, &p.token_hash, &p.client_id).await?;
            } else if let Some(id) = p.status_id.as_ref().filter(|id| !listed.contains(*id)) {
                missing.push(id.clone());
            }
            continue;
        }
        let mut media = Vec::new();
        for up in &p.upload_ids {
            let mut m = EntityRecord::default();
            let url = format!("{base}{}{up}", pod_core::uploads::PUBLIC_PATH);
            m.set("url", url.clone());
            m.set("preview_url", url);
            m.set("type", "image");
            m.set("aspect_ratio", 1.0);
            media.push((crate::media_uri(&format!("{PENDING_PREFIX}{up}")), m));
        }
        let mut post = EntityRecord::default();
        post.set("client_id", p.client_id.as_str());
        post.set("author_name", "You");
        post.set("caption", p.caption.as_str());
        post.set(
            "media_uris",
            media.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
        );
        if p.state == "failed" {
            post.set("failed", true);
            post.set(
                "error",
                format!(
                    "Couldn't post: {}",
                    p.error.as_deref().unwrap_or("Pixelfed refused it.")
                ),
            );
        } else {
            post.set("pending_label", POSTING_LABEL);
        }
        out.push((pending_uri(&p.client_id), post, media));
    }
    Ok((out, missing))
}

/// `<scheme>://<Host><public prefix>` of a request (where public pod paths
/// load from), like the media proxy's base.
fn public_base(headers: &HeaderMap) -> String {
    let media = crate::routes::proxy_base(headers);
    media
        .strip_suffix(crate::routes::MEDIA_PATH)
        .unwrap_or(&media)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_short_sentences() {
        assert_eq!(
            reason(r#"{"error":"Validation failed: too long"}"#),
            "Validation failed: too long"
        );
        assert_eq!(reason(""), "Pixelfed refused the post.");
        assert!(reason(&"x".repeat(500)).chars().count() <= 161);
        assert!(valid_client_id("abcdef0123456789"));
        assert!(!valid_client_id("short"));
        assert_eq!(pending_uri("abcdefgh"), "pixelfed://posts/pending-abcdefgh");
    }
}
