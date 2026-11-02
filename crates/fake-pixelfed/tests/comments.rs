//! Comments on the fake Pixelfed: reading threads, adding and deleting comments.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fake_pixelfed::{AppState, Config, MAX_STATUS_CHARS, app, default_fixtures_dir};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "http://fake.test:9999";
const SEED: &str = "seed-token";

const CAROUSEL: &str = "781254316723458049";
const REBLOG_OUTER: &str = "781249870123446273";
const REBLOG_INNER: &str = "781101134560870401";
const ME: &str = "703641122334455667";

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

async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {SEED}"));
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = match body {
        Some(j) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(j.to_string())
        }
        None => Body::empty(),
    };
    let resp = router.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn context(router: &Router, id: &str) -> Value {
    let (status, j) = call(
        router,
        Method::GET,
        &format!("/api/v1/statuses/{id}/context"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    j
}

async fn status(router: &Router, id: &str) -> Value {
    let (s, j) = call(
        router,
        Method::GET,
        &format!("/api/v1/statuses/{id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    j
}

async fn comment(router: &Router, parent: &str, text: &str) -> (StatusCode, Value) {
    call(
        router,
        Method::POST,
        "/api/v1/statuses",
        Some(json!({ "status": text, "in_reply_to_id": parent })),
        &[],
    )
    .await
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn every_post_has_as_many_fixture_comments_as_its_replies_count() {
    let (state, router) = setup();
    let (_, timeline) = call(
        &router,
        Method::GET,
        "/api/v1/timelines/home?include_reblogs=true",
        None,
        &[],
    )
    .await;
    let mut checked = 0;
    for outer in timeline.as_array().unwrap() {
        let post = if outer["reblog"].is_object() {
            &outer["reblog"]
        } else {
            outer
        };
        let id = post["id"].as_str().unwrap();
        let replies = state.reply_ids(id);
        assert_eq!(
            replies.len() as i64,
            post["replies_count"].as_i64().unwrap(),
            "post {id}"
        );
        let ctx = context(&router, id).await;
        assert_eq!(ctx["ancestors"], json!([]));
        for d in ctx["descendants"].as_array().unwrap() {
            // Every comment's count matches its own direct replies too.
            let did = d["id"].as_str().unwrap();
            assert_eq!(
                state.reply_ids(did).len() as i64,
                d["replies_count"].as_i64().unwrap(),
                "comment {did}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 5);
    assert_eq!(state.report(0)["counts"]["comments"], 34);
}

#[tokio::test]
async fn context_is_a_depth_first_thread_with_full_statuses() {
    let (_, router) = setup();
    let ctx = context(&router, CAROUSEL).await;
    let d = &ctx["descendants"];
    assert_eq!(
        ids(d),
        [
            "781300000000000101",
            "781300000000000102",
            "781300000000000103",
            "781300000000000104",
            "781300000000000105",
            "781300000000000106",
            "781300000000000107",
        ]
    );
    let first = &d[0];
    assert_eq!(first["in_reply_to_id"], CAROUSEL);
    assert_eq!(first["in_reply_to_account_id"], "698765432100012345");
    assert_eq!(first["account"]["username"], "bob");
    assert_eq!(
        first["account"]["avatar"],
        format!("{BASE}/storage/avatar-bob.png")
    );
    assert_eq!(first["replies_count"], 1);
    assert_eq!(first["favourites_count"], 3);
    assert_eq!(first["url"], format!("{BASE}/p/bob/781300000000000101"));
    assert_eq!(d[1]["in_reply_to_id"], "781300000000000101");
    // Plain text is escaped; newlines become <br />.
    assert_eq!(d[6]["content"], "<p>Number 3 for me.<br />That blue!</p>");
    let tram = context(&router, REBLOG_INNER).await;
    let amp = tram["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "781300000000000208")
        .unwrap();
    assert_eq!(amp["content"], "<p>Black &amp; white version? &lt;3</p>");
    assert_eq!(amp["content_text"], "Black & white version? <3");
    // The signed-in user's own comment carries the plain account (no `source`).
    assert_eq!(d[5]["account"]["id"], ME);
    assert!(d[5]["account"].get("source").is_none());

    // From inside the thread: ancestors root first, then the rest below.
    let mid = context(&router, "781300000000000102").await;
    assert_eq!(ids(&mid["ancestors"]), [CAROUSEL, "781300000000000101"]);
    assert_eq!(ids(&mid["descendants"]), ["781300000000000103"]);
}

#[tokio::test]
async fn context_of_a_reblog_is_the_reblogged_thread() {
    let (_, router) = setup();
    let outer = context(&router, REBLOG_OUTER).await;
    let inner = context(&router, REBLOG_INNER).await;
    assert_eq!(outer, inner);
    assert_eq!(inner["descendants"].as_array().unwrap().len(), 13);
}

#[tokio::test]
async fn context_and_status_need_a_token_and_a_known_id() {
    let (_, router) = setup();
    let unauth = Request::get(format!("/api/v1/statuses/{CAROUSEL}/context"))
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(unauth).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let (s, _) = call(
        &router,
        Method::GET,
        "/api/v1/statuses/1/context",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&router, Method::GET, "/api/v1/statuses/1", None, &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &router,
        Method::GET,
        &format!("/api/v1/statuses/{CAROUSEL}/nope"),
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn adding_a_comment_updates_the_thread_and_the_count() {
    let (state, router) = setup();
    let (s, created) = comment(&router, CAROUSEL, "  Lovely <3\nSecond line  ").await;
    assert_eq!(s, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert!(id.parse::<u128>().unwrap() > 781300000000000508);
    assert_eq!(created["in_reply_to_id"], CAROUSEL);
    assert_eq!(created["account"]["id"], ME);
    assert_eq!(created["content"], "<p>Lovely &lt;3<br />Second line</p>");
    assert_eq!(created["replies_count"], 0);
    assert!(
        created["created_at"]
            .as_str()
            .unwrap()
            .ends_with(".000000Z")
    );

    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 5);
    let (_, timeline) = call(&router, Method::GET, "/api/v1/timelines/home", None, &[]).await;
    assert_eq!(timeline[0]["id"], CAROUSEL);
    assert_eq!(timeline[0]["replies_count"], 5);
    let ctx = context(&router, CAROUSEL).await;
    assert_eq!(ids(&ctx["descendants"]).last().copied(), Some(id.as_str()));
    assert_eq!(state.created_ids(), std::slice::from_ref(&id));
    assert_eq!(state.report(0)["created"], json!([id]));

    // A reply to a comment nests under it.
    let (s, nested) = comment(&router, "781300000000000104", "Same question!").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        status(&router, "781300000000000104").await["replies_count"],
        2
    );
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 5);
    let ctx = context(&router, CAROUSEL).await;
    let order = ids(&ctx["descendants"]);
    let pos = order.iter().position(|i| *i == nested["id"]).unwrap();
    assert_eq!(order[pos - 1], "781300000000000105");

    // Reset brings the fixture threads back.
    state.reset(true);
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 4);
    assert_eq!(
        context(&router, CAROUSEL).await["descendants"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert!(state.created_ids().is_empty());
    let (s, again) = comment(&router, CAROUSEL, "after reset").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["id"], id, "ids restart after a reset");
}

#[tokio::test]
async fn commenting_on_a_reblog_comments_on_the_reblogged_post() {
    let (_, router) = setup();
    let (s, c) = comment(&router, REBLOG_OUTER, "via the boost").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["in_reply_to_id"], REBLOG_INNER);
    assert_eq!(status(&router, REBLOG_INNER).await["replies_count"], 13);
    assert_eq!(
        status(&router, REBLOG_OUTER).await["reblog"]["replies_count"],
        13
    );
}

#[tokio::test]
async fn form_bodies_work_too() {
    let (_, router) = setup();
    let body =
        serde_urlencoded::to_string([("status", "form comment"), ("in_reply_to_id", CAROUSEL)])
            .unwrap();
    let req = Request::post("/api/v1/statuses")
        .header(header::AUTHORIZATION, format!("Bearer {SEED}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_comments_are_rejected() {
    let (state, router) = setup();
    let (s, j) = comment(&router, CAROUSEL, "   ").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    let long = "x".repeat(MAX_STATUS_CHARS + 1);
    let (s, _) = comment(&router, CAROUSEL, &long).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = comment(&router, CAROUSEL, &"é".repeat(MAX_STATUS_CHARS)).await;
    assert_eq!(s, StatusCode::OK, "the limit counts characters");
    let (s, _) = comment(&router, "999", "hello").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, j) = call(
        &router,
        Method::POST,
        "/api/v1/statuses",
        Some(json!({ "status": "a new post" })),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    let (s, _) = call(
        &router,
        Method::POST,
        "/api/v1/statuses",
        Some(json!({ "status": "x", "in_reply_to_id": CAROUSEL, "visibility": "secret" })),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let unauth = Request::post("/api/v1/statuses")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "status": "x", "in_reply_to_id": CAROUSEL }).to_string(),
        ))
        .unwrap();
    let resp = router.clone().oneshot(unauth).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(state.created_ids().len(), 1);
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 5);
}

#[tokio::test]
async fn idempotency_key_replays_the_first_comment() {
    let (state, router) = setup();
    let body = json!({ "status": "only once", "in_reply_to_id": CAROUSEL });
    let key = [("idempotency-key", "abc-123")];
    let (s1, a) = call(
        &router,
        Method::POST,
        "/api/v1/statuses",
        Some(body.clone()),
        &key,
    )
    .await;
    let (s2, b) = call(
        &router,
        Method::POST,
        "/api/v1/statuses",
        Some(body.clone()),
        &key,
    )
    .await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_eq!(a["id"], b["id"]);
    assert_eq!(state.created_ids().len(), 1);
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 5);
    let other = [("idempotency-key", "abc-456")];
    let (_, c) = call(
        &router,
        Method::POST,
        "/api/v1/statuses",
        Some(body),
        &other,
    )
    .await;
    assert_ne!(a["id"], c["id"]);
}

#[tokio::test]
async fn comments_can_be_liked() {
    let (state, router) = setup();
    let (s, j) = call(
        &router,
        Method::POST,
        "/api/v1/statuses/781300000000000101/favourite",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["favourited"], true);
    assert_eq!(j["favourites_count"], 4);
    assert!(
        state
            .favourited_ids()
            .contains(&"781300000000000101".to_owned())
    );
}

#[tokio::test]
async fn only_own_comments_can_be_deleted() {
    let (state, router) = setup();
    // Someone else's comment.
    let (s, _) = call(
        &router,
        Method::DELETE,
        "/api/v1/statuses/781300000000000101",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A fixture comment by the signed-in account.
    let (s, deleted) = call(
        &router,
        Method::DELETE,
        "/api/v1/statuses/781300000000000106",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(deleted["text"], "Adding Boulder to the list.");
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 3);
    assert!(
        !state
            .reply_ids(CAROUSEL)
            .contains(&"781300000000000106".to_owned())
    );
    let (s, _) = call(
        &router,
        Method::GET,
        "/api/v1/statuses/781300000000000106",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A new one, then gone again.
    let (_, c) = comment(&router, CAROUSEL, "oops").await;
    let uri = format!("/api/v1/statuses/{}", c["id"].as_str().unwrap());
    let (s, _) = call(&router, Method::DELETE, &uri, None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&router, Method::DELETE, &uri, None, &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(status(&router, CAROUSEL).await["replies_count"], 3);
}

#[test]
fn a_fixture_dir_without_comments_still_loads() {
    let dir = std::env::temp_dir().join(format!("fake-pf-nocomments-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["timeline_home.json", "account_verify_credentials.json"] {
        std::fs::copy(default_fixtures_dir().join(name), dir.join(name)).unwrap();
    }
    let state = AppState::new(Config {
        fixtures_dir: dir.clone(),
        public_url: BASE.to_owned(),
        auto_approve: false,
        seed_token: None,
    })
    .expect("loads without comments.json");
    assert!(state.reply_ids(CAROUSEL).is_empty());
    assert_eq!(state.status(CAROUSEL).unwrap()["replies_count"], 4);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_fixtures_include_comments() {
    assert!(fake_pixelfed::embedded_fixture_names().any(|n| n == "comments.json"));
    let state = AppState::new(Config::embedded(BASE)).unwrap();
    assert_eq!(state.reply_ids(CAROUSEL).len(), 4);
}
