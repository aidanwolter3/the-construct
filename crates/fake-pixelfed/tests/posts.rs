//! Posting on the fake Pixelfed: media uploads (v2 processing, v1 at once),
//! posts with photos at the top of the home timeline.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fake_pixelfed::{AppState, Config, app, default_fixtures_dir};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "http://fake.test:9999";
const SEED: &str = "seed-token";

fn setup() -> (Arc<AppState>, Router) {
    let state = Arc::new(
        AppState::new(Config {
            fixtures_dir: default_fixtures_dir(),
            public_url: BASE.to_owned(),
            auto_approve: false,
            seed_token: Some(SEED.to_owned()),
        })
        .expect("fixtures load"),
    );
    let router = app(Arc::clone(&state));
    (state, router)
}

async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value, Vec<u8>) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        bytes,
    )
}

/// A 3x2 PNG's first bytes (enough for its size).
fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend_from_slice(&3u32.to_be_bytes());
    b.extend_from_slice(&2u32.to_be_bytes());
    b.extend_from_slice(&[8, 2, 0, 0, 0]);
    b
}

fn multipart(path: &str, ct: &str, bytes: &[u8]) -> Request<Body> {
    let boundary = "XBOUNDARYX";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"description\"\r\n\r\nA cat\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"p\"\r\n\
         Content-Type: {ct}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {SEED}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

fn json_req(method: Method, path: &str, body: Option<Value>, key: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {SEED}"));
    if let Some(k) = key {
        b = b.header("idempotency-key", k);
    }
    match body {
        Some(j) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

#[tokio::test]
async fn photos_are_processed_then_posted_to_the_top_of_the_timeline() {
    let (state, router) = setup();
    // v2: processing (202, no URL) until polled once.
    let (s, m, _) = send(&router, multipart("/api/v2/media", "image/png", &png())).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{m}");
    assert!(m["url"].is_null());
    assert_eq!(m["description"], "A cat");
    assert_eq!(m["meta"]["original"]["width"], 3);
    let id = m["id"].as_str().unwrap().to_owned();
    // Posting it now is refused: still processing.
    let post = |media: Value, key: &str| {
        json_req(
            Method::POST,
            "/api/v1/statuses",
            Some(json!({"status": "Hello", "media_ids": media})),
            Some(key),
        )
    };
    let (s, body, _) = send(&router, post(json!([id]), "k0")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (s, m, _) = send(
        &router,
        json_req(Method::GET, &format!("/api/v1/media/{id}"), None, None),
    )
    .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT, "{m}");
    let (s, m, _) = send(
        &router,
        json_req(Method::GET, &format!("/api/v1/media/{id}"), None, None),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let url = m["url"].as_str().unwrap().to_owned();
    assert!(
        url.starts_with(&format!("{BASE}/storage/uploads/")),
        "{url}"
    );
    // The file itself.
    let path = url.trim_start_matches(BASE);
    let (s, _, bytes) = send(&router, Request::get(path).body(Body::empty()).unwrap()).await;
    assert_eq!((s, bytes), (StatusCode::OK, png()));

    // v1 is ready at once; two photos make an album.
    let (s, m2, _) = send(&router, multipart("/api/v1/media", "image/png", &png())).await;
    assert_eq!(s, StatusCode::OK);
    let id2 = m2["id"].as_str().unwrap().to_owned();
    let (s, status, _) = send(&router, post(json!([id, id2]), "k1")).await;
    assert_eq!(s, StatusCode::OK, "{status}");
    assert_eq!(status["pf_type"], "photo:album");
    assert_eq!(status["content_text"], "Hello");
    assert_eq!(status["media_attachments"].as_array().unwrap().len(), 2);
    assert!(status["in_reply_to_id"].is_null());
    let post_id = status["id"].as_str().unwrap().to_owned();
    assert_eq!(state.timeline_ids()[0], post_id);
    // Same key: the same post; media can't be attached twice.
    let (_, again, _) = send(&router, post(json!([id, id2]), "k1")).await;
    assert_eq!(again["id"], post_id.as_str());
    let (s, _, _) = send(&router, post(json!([id]), "k2")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(state.created_ids(), std::slice::from_ref(&post_id));
    // On the home timeline, first.
    let (_, tl, _) = send(
        &router,
        json_req(Method::GET, "/api/v1/timelines/home", None, None),
    )
    .await;
    assert_eq!(tl[0]["id"], post_id.as_str());

    // No media, too many, or a non-image: refused.
    let (s, _, _) = send(&router, post(json!([]), "k3")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = send(&router, post(json!(["1", "2", "3", "4", "5"]), "k4")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = send(&router, multipart("/api/v2/media", "text/html", b"<b>")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Ready at once when configured; a reset forgets the media.
    state.set_media_processing_polls(0);
    let (s, _, _) = send(&router, multipart("/api/v2/media", "image/png", &png())).await;
    assert_eq!(s, StatusCode::OK);
    state.reset(true);
    assert!(state.media_ids().is_empty());
}
