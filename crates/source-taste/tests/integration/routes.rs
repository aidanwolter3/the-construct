//! Signing in, the kept feed, reactions and the data paths.

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::json;

use crate::fake::{Fake, TOKEN, call, pod, serve, signed_in, taste_layout, workspace};

#[tokio::test]
async fn sign_in_checks_the_service_first() {
    let fake = std::sync::Arc::new(Fake::default());
    let url = serve(fake.clone()).await;
    let pod = pod();
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "taste_refused_token");
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/sign-in",
        None,
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a session is needed");
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], url.trim_start_matches("http://"));
}

#[tokio::test]
async fn the_feed_is_kept_and_fetched_only_when_asked() {
    let (pod, fake, _) = signed_in().await;
    let app = taste_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    let status = &ws["entities"]["taste://status/pod"];
    assert_eq!(status["signed_in"], true, "{ws}");
    assert_eq!(status["counts_text"], "2 liked · 1 skipped");
    let feed = ws["queries"]["taste:feed"].as_array().unwrap().clone();
    assert_eq!(feed.len(), 6);
    let first = &ws["entities"][feed[0].as_str().unwrap()];
    assert_eq!(first["key"], "shop0_example__1000");
    assert_eq!(first["price_text"], "$98");
    assert_eq!(first["why_text"], "Near your taste · 1");
    assert!(first["image"].as_str().unwrap().ends_with("&width=720"));
    // Fetching the data again doesn't take another page.
    workspace(&pod, &app).await;
    assert_eq!(fake.pages.load(Ordering::SeqCst), 1);

    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/more",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ws = workspace(&pod, &app).await;
    assert_eq!(ws["queries"]["taste:feed"].as_array().unwrap().len(), 12);

    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/refresh",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws = workspace(&pod, &app).await;
    let feed = ws["queries"]["taste:feed"].as_array().unwrap();
    assert_eq!(feed.len(), 6);
    assert_eq!(feed[0], "taste://items/shop2_example__1000");
    assert_eq!(fake.pages.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn reactions_reach_the_service_and_stick() {
    let (pod, fake, _) = signed_in().await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/react",
        Some("phone"),
        Some(json!({"id": "shop0.example:1000", "liked": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/react",
        Some("phone"),
        Some(json!({"id": "shop0_example__1001", "skipped": "true"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = fake.events.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![
            json!({"id": "shop0.example:1000", "kind": "like"}),
            json!({"id": "shop0.example:1001", "kind": "skip"}),
        ]
    );
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["taste://items/shop0_example__1000"]["liked"],
        true
    );
    assert_eq!(
        ws["entities"]["taste://items/shop0_example__1001"]["skipped"],
        true
    );
    assert_eq!(
        ws["entities"]["taste://status/pod"]["counts_text"],
        "1 liked · 1 skipped"
    );
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/react",
        Some("phone"),
        Some(json!({"id": "shop0.example:1000"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a reaction says what it is"
    );
}

#[tokio::test]
async fn screens_answer_their_lists() {
    let (pod, fake, _) = signed_in().await;
    // The feed is kept, so the item screen keeps its card's reason.
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/item?key=shop0_example__1000",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let item = &b["entities"]["taste://items/shop0_example__1000"];
    assert_eq!(item["id"], "shop0.example:1000");
    assert_eq!(
        item["why_text"], "Near your taste · 1",
        "the feed's reason stays"
    );
    assert_eq!(
        b["queries"]["taste:similar:shop0_example__1000"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        b["queries"]["taste:contrast:shop0_example__1000"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/steer?key=shop0_example__1000&q=more%20relaxed",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(
        b["queries"]["taste:steer:shop0_example__1000"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/search?q=olive%20coat",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(
        b["entities"]["taste://search/pod"]["count_text"],
        "2 pieces"
    );
    assert_eq!(
        fake.queries.lock().unwrap().clone(),
        vec!["more relaxed".to_owned(), "olive coat".to_owned()]
    );

    let (status, b) = call(&pod, "GET", "/v1/taste/screens/likes", Some("phone"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["queries"]["taste:likes"].as_array().unwrap().len(), 1);

    let (status, b) = call(&pod, "GET", "/v1/taste/screens/taste", Some("phone"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        b["entities"]["taste://tastes/0"]["attributes_text"],
        "rugged workwear · earth tones · cotton canvas"
    );
    let pieces = b["queries"]["taste:taste_items"].as_array().unwrap();
    assert_eq!(pieces.len(), 2);
    assert_eq!(b["entities"][pieces[0].as_str().unwrap()]["taste"], "0");
    assert_eq!(b["entities"]["taste://summary/pod"]["written"], false);
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/summary",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, b) = call(&pod, "GET", "/v1/taste/screens/taste", Some("phone"), None).await;
    assert_eq!(
        b["entities"]["taste://summary/pod"]["written_text"],
        "Written at 2 likes"
    );

    let (status, _) = call(
        &pod,
        "GET",
        "/v1/taste/screens/item?key=..%2Fx",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_service_that_is_down_says_so_and_keeps_the_feed() {
    let (pod, fake, _) = signed_in().await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    fake.down.store(true, Ordering::SeqCst);
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/more",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let ws = workspace(&pod, &app).await;
    assert_eq!(ws["queries"]["taste:feed"].as_array().unwrap().len(), 6);
    assert!(
        ws["entities"]["taste://status/pod"]["error"]
            .as_str()
            .unwrap()
            .contains("502"),
        "{ws}"
    );
}

#[tokio::test]
async fn signing_out_forgets_everything() {
    let (pod, _, _) = signed_in().await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/sign-out",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws = workspace(&pod, &app).await;
    assert_eq!(ws["entities"]["taste://status/pod"]["signed_in"], false);
    assert_eq!(ws["queries"]["taste:feed"], json!([]));
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/more",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn private_hosts_are_refused_by_default() {
    let pod: pod_core::Pod = std::sync::Arc::new(pod_core::AppState::in_memory(vec![
        std::sync::Arc::new(source_taste::TasteAdapter::new(false)),
        source_shell::adapter(),
    ]));
    pod.store()
        .create_session(&pod_core::session::session_hash("phone"))
        .unwrap();
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/sign-in",
        Some("phone"),
        Some(json!({"url": "http://127.0.0.1:9", "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "invalid_url");
}
