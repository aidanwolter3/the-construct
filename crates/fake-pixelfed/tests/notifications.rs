//! The fake's notifications (D47): `GET /api/v1/notifications` with Mastodon paging, the
//! seeded fixture notifications and `POST /_admin/notifications`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use fake_pixelfed::{AppState, Config, FaultRule, app};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "http://fake.test:9999";
const SEED: &str = "seed-token";
/// The signed-in account's newest comment (it has one like in the fixtures).
const MY_COMMENT: &str = "781300000000000504";
const CAROUSEL: &str = "781254316723458049";

fn fake() -> (Arc<AppState>, Router) {
    let state = Arc::new(
        AppState::new(Config {
            fixtures_dir: Default::default(),
            public_url: BASE.to_owned(),
            auto_approve: true,
            seed_token: Some(SEED.to_owned()),
        })
        .unwrap(),
    );
    (Arc::clone(&state), app(state))
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    };
    let resp = router.clone().oneshot(req.unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn fixture_notifications_are_seeded() {
    let (_, router) = fake();
    let (status, headers, list) =
        call(&router, "GET", "/api/v1/notifications", Some(SEED), None).await;
    assert_eq!(status, StatusCode::OK);
    // Newest first: carol's reply (Oct 31), bob's like (Oct 30), carol's reply (Oct 29).
    assert_eq!(ids(&list), ["3", "2", "1"]);
    let n = &list[0];
    assert_eq!(n["type"], "mention");
    assert_eq!(n["account"]["username"], "carol");
    assert_eq!(n["status"]["id"], "781300000000000404");
    assert_eq!(n["status"]["in_reply_to_account_id"], "703641122334455667");
    assert_eq!(n["created_at"], "2026-10-31T08:30:00.000000Z");
    let like = &list[1];
    assert_eq!(like["type"], "favourite");
    assert_eq!(like["status"]["id"], MY_COMMENT);
    assert!(like["account"]["username"].is_string());
    let link = headers[header::LINK].to_str().unwrap();
    assert!(link.contains("max_id=1&"), "{link}");
    assert!(link.contains("min_id=3&"), "{link}");
}

#[tokio::test]
async fn needs_a_token_and_faults_apply() {
    let (state, router) = fake();
    let (status, _, _) = call(&router, "GET", "/api/v1/notifications", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(&router, "GET", "/api/v1/notifications", Some("nope"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    state.add_faults([FaultRule::status(503)
        .path("/api/v1/notifications")
        .times(1)]);
    let (status, _, _) = call(&router, "GET", "/api/v1/notifications", Some(SEED), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _, _) = call(&router, "GET", "/api/v1/notifications", Some(SEED), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn admin_injects_notifications() {
    let (state, router) = fake();
    // A like by alice: about the signed-in account's newest status by default.
    let (status, _, n) = call(
        &router,
        "POST",
        "/_admin/notifications",
        None,
        Some(json!({"type": "favourite", "from": "alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{n}");
    assert_eq!(n["id"], "4");
    assert_eq!(n["type"], "favourite");
    assert_eq!(n["account"]["username"], "alice");
    assert_eq!(n["status"]["id"], MY_COMMENT);
    // A follow has no status; a comment with text writes a reply first.
    let (_, _, follow) = call(
        &router,
        "POST",
        "/_admin/notifications",
        None,
        Some(json!({"type": "follow", "from": "bob"})),
    )
    .await;
    assert!(follow["status"].is_null());
    let (status, _, comment) = call(
        &router,
        "POST",
        "/_admin/notifications",
        None,
        Some(
            json!({"type": "mention", "from": "carol", "status_id": MY_COMMENT,
                    "text": "@constructdemo agreed!"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{comment}");
    let reply = &comment["status"];
    assert_eq!(reply["in_reply_to_id"], MY_COMMENT);
    assert_eq!(reply["in_reply_to_account_id"], "703641122334455667");
    assert_eq!(reply["content_text"], "@constructdemo agreed!");
    assert_eq!(reply["account"]["username"], "carol");
    assert_eq!(state.reply_ids(MY_COMMENT), [reply["id"].as_str().unwrap()]);
    // A boost of a given post.
    let (_, _, boost) = call(
        &router,
        "POST",
        "/_admin/notifications",
        None,
        Some(json!({"type": "reblog", "from": "bob", "status_id": CAROUSEL})),
    )
    .await;
    assert_eq!(boost["status"]["id"], CAROUSEL);
    assert_eq!(
        ids(&Value::from(state.notifications())),
        ["7", "6", "5", "4", "3", "2", "1"]
    );

    // Bad input.
    for (body, want) in [
        (json!({"type": "favourite", "from": "nobody"}), 400),
        (json!({"type": "", "from": "bob"}), 400),
        (
            json!({"type": "favourite", "from": "bob", "status_id": "123"}),
            404,
        ),
        (json!({"from": "bob"}), 400),
    ] {
        let (status, _, _) = call(
            &router,
            "POST",
            "/_admin/notifications",
            None,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status.as_u16(), want, "{body}");
    }

    // A reset restores the fixture notifications.
    state.reset(true);
    assert_eq!(state.notifications().len(), 3);
    let (_, _, n) = call(
        &router,
        "POST",
        "/_admin/notifications",
        None,
        Some(json!({"type": "follow", "from": "bob"})),
    )
    .await;
    assert_eq!(n["id"], "4");
}

#[tokio::test]
async fn mastodon_paging() {
    let (state, router) = fake();
    for _ in 0..5 {
        state.add_notification("follow", "bob", None, None).unwrap();
    }
    // 1..=8 now. Newest page of 3.
    let get = |q: &str| {
        let router = router.clone();
        let uri = format!("/api/v1/notifications?{q}");
        async move { call(&router, "GET", &uri, Some(SEED), None).await.2 }
    };
    assert_eq!(ids(&get("limit=3").await), ["8", "7", "6"]);
    assert_eq!(ids(&get("limit=3&max_id=6").await), ["5", "4", "3"]);
    // since_id: the newest page above it; min_id: the page right above it.
    assert_eq!(ids(&get("limit=3&since_id=2").await), ["8", "7", "6"]);
    assert_eq!(ids(&get("limit=3&min_id=2").await), ["5", "4", "3"]);
    assert_eq!(ids(&get("limit=3&min_id=5").await), ["8", "7", "6"]);
    assert_eq!(ids(&get("min_id=8").await), Vec::<&str>::new());
    assert_eq!(ids(&get("max_id=3&since_id=1").await), ["2"]);
    // Garbage values are ignored; limit is clamped.
    assert_eq!(ids(&get("limit=0&min_id=x").await), ["8"]);
    assert_eq!(get("limit=500").await.as_array().unwrap().len(), 8);
}
