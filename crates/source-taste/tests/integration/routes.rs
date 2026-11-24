//! Signing in, the kept feed, reactions and the data paths.

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::json;

use crate::fake::{Fake, TOKEN, call, pod, serve, signed_in, taste_layout, tool, workspace};

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
    assert_eq!(status["counts_text"], "2 liked · 1 disliked");
    let feed = ws["queries"]["taste:feed"].as_array().unwrap().clone();
    assert_eq!(feed.len(), 6);
    let first = &ws["entities"][feed[0].as_str().unwrap()];
    assert_eq!(first["key"], "shop0_example__1000");
    assert_eq!(first["price_text"], "$98");
    assert_eq!(first["why_text"], "On the edge");
    assert!(first["image"].as_str().unwrap().ends_with("&width=720"));
    assert_eq!(first["photo_count"], 3);
    assert!(
        first["image_0"]
            .as_str()
            .unwrap()
            .contains("coat-0-worn.jpg")
    );
    assert_eq!(first["next_2"], 0);
    assert_eq!(first["chance_text"], "50% your taste");
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
    // A reacted piece leaves the deck, liked or skipped, and the pod tops the
    // deck up by itself once fewer than a few are left.
    let ws = workspace(&pod, &app).await;
    let feed: Vec<String> = ws["queries"]["taste:feed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap().to_owned())
        .collect();
    assert!(
        !feed.contains(&"taste://items/shop0_example__1000".to_owned()),
        "{feed:?}"
    );
    assert!(
        !feed.contains(&"taste://items/shop0_example__1001".to_owned()),
        "{feed:?}"
    );
    assert_eq!(
        ws["entities"]["taste://status/pod"]["counts_text"],
        "1 liked · 1 disliked"
    );
    let mut topped = false;
    for _ in 0..100 {
        if fake.pages.load(Ordering::SeqCst) == 2 {
            topped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(topped, "the deck was topped up");
    let mut len = 0;
    for _ in 0..100 {
        let ws = workspace(&pod, &app).await;
        len = ws["queries"]["taste:feed"].as_array().unwrap().len();
        if len == 10 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(len, 10, "4 left and a page of 6");
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
    assert_eq!(item["why_text"], "On the edge", "the feed's reason stays");
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
    // The similar pieces again for the piece's sheet (the taste_item core
    // layout), under one fixed name, each a copy tagged with the piece.
    let sheet = b["queries"]["taste:sheet:similar"].as_array().unwrap();
    assert_eq!(sheet.len(), 3);
    for uri in sheet {
        let uri = uri.as_str().unwrap();
        assert!(
            uri.starts_with("taste://sheet-similar/shop0_example__1000/"),
            "{uri}"
        );
        let e = &b["entities"][uri];
        assert_eq!(e["of"], "shop0_example__1000");
        assert_eq!(e["uri"], uri);
        assert!(
            e["key"]
                .as_str()
                .is_some_and(|k| k != "shop0_example__1000"),
            "{e}"
        );
    }

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

    // One taste to browse: its likes (marked liked) first, then the nearest pieces.
    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/one-taste?n=0",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["entities"]["taste://tastes/0"]["count_text"], "5 pieces");
    assert_eq!(
        b["entities"]["taste://tastes/0"]["attributes_text"],
        "rugged workwear · earth tones"
    );
    let grid = b["queries"]["taste:one-taste:0"].as_array().unwrap();
    assert_eq!(grid.len(), 5);
    let liked: Vec<bool> = grid
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()]["liked"] == true)
        .collect();
    assert_eq!(liked, vec![true, true, false, false, false]);
    let (status, _) = call(
        &pod,
        "GET",
        "/v1/taste/screens/one-taste?n=3",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &pod,
        "GET",
        "/v1/taste/screens/one-taste?n=x",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

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

#[tokio::test]
async fn the_map_zooms_into_tiles_and_lists_an_area() {
    let (pod, fake, _) = signed_in().await;
    let whole = "0-0-1000000-1000000";
    let (status, b) = call(
        &pod,
        "GET",
        &format!("/v1/taste/screens/map?area={whole}"),
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let tiles = b["queries"][format!("taste:tiles:{whole}")]
        .as_array()
        .unwrap();
    assert_eq!(tiles.len(), 16);
    let tile = |n: usize| &b["entities"][tiles[n].as_str().unwrap()];
    assert_eq!(
        tile(0)["opens"],
        "taste://map/0-0-250000-250000",
        "a crowded tile zooms"
    );
    assert_eq!(
        tile(1)["opens"],
        "taste://area/250000-0-500000-250000",
        "a few: their pieces"
    );
    assert_eq!(tile(15)["opens"], "", "an empty tile opens nothing");
    assert_eq!(tile(0)["color"], "#C98A5B");
    assert!(tile(0)["image"].as_str().unwrap().ends_with("&width=360"));
    let area = &b["entities"][format!("taste://areas/{whole}")];
    assert_eq!(area["title"], "Your taste map");
    assert_eq!(area["count_text"], "82 pieces");

    let zoomed = "0-0-250000-250000";
    let (status, b) = call(
        &pod,
        "GET",
        &format!("/v1/taste/screens/map?area={zoomed}"),
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(
        b["entities"][format!("taste://areas/{zoomed}")]["title"],
        "Zoomed in ×4"
    );
    let sent = *fake.boxes.lock().unwrap().last().unwrap();
    assert_eq!(
        (sent.x0, sent.y0, sent.x1, sent.y1, sent.n),
        (0.0, 0.0, 0.25, 0.25, Some(4))
    );

    let (status, b) = call(
        &pod,
        "GET",
        &format!("/v1/taste/screens/area?area={zoomed}"),
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(
        b["queries"][format!("taste:area:{zoomed}")]
            .as_array()
            .unwrap()
            .len(),
        5
    );

    for bad in ["0-0-0-1", "x", "0-0-1000001-1000000", "..%2F"] {
        let (status, _) = call(
            &pod,
            "GET",
            &format!("/v1/taste/screens/map?area={bad}"),
            Some("phone"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn the_scatter_is_the_whole_map_packed_and_kept() {
    use protocol::scatter::{Mark, decode};
    let (pod, fake, _) = signed_in().await;
    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/scatter",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let map = &b["entities"]["taste://maps/catalog"];
    assert_eq!(map["count"], 5);
    assert_eq!(map["reactions_text"], "1 liked · 1 disliked");
    assert_eq!(map["modelled"], true);
    let points = decode(map["points"].as_str().unwrap()).unwrap();
    assert_eq!(points.len(), 5);
    assert_eq!(points[0].mark, Mark::Highlight);
    assert_eq!(points[4].mark, Mark::Muted);
    assert_eq!(points[2].mark, Mark::Plain);
    assert!(points[3].value.is_none(), "an unknown chance stays unknown");
    assert!((points[0].value.unwrap() - 0.9).abs() < 0.01);
    assert!((points[1].x - 0.3).abs() < 0.001 && (points[1].y - 0.7).abs() < 0.001);
    // Six bytes a point, base64: small enough for tens of thousands.
    assert!(map["points"].as_str().unwrap().len() <= 5 * 8 + 4);

    // Kept: asked again, the service isn't.
    call(
        &pod,
        "GET",
        "/v1/taste/screens/scatter",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(fake.maps.load(Ordering::SeqCst), 1);

    // A like marks its point at once, from the kept map.
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/react",
        Some("phone"),
        Some(json!({"id": "shop0.example:1002", "liked": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/scatter",
        Some("phone"),
        None,
    )
    .await;
    let points = decode(
        b["entities"]["taste://maps/catalog"]["points"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(points[2].mark, Mark::Highlight);
    assert_eq!(
        fake.maps.load(Ordering::SeqCst),
        1,
        "within a moment of a reaction, still kept"
    );
}

#[tokio::test]
async fn the_kept_map_is_forgotten_with_the_service() {
    let (pod, fake, _) = signed_in().await;
    let scatter = || {
        call(
            &pod,
            "GET",
            "/v1/taste/screens/scatter",
            Some("phone"),
            None,
        )
    };
    scatter().await;
    assert_eq!(fake.maps.load(Ordering::SeqCst), 1);
    let sign_in = |url: String| {
        call(
            &pod,
            "POST",
            "/v1/taste/sign-in",
            Some("phone"),
            Some(json!({"url": url, "token": TOKEN})),
        )
    };
    // Unplugged from Settings (or by Claude), then another service: its map.
    tool(&pod, "unplug_jack", json!({"id": "taste"})).await;
    let other = std::sync::Arc::new(Fake::default());
    assert_eq!(sign_in(serve(other.clone()).await).await.0, StatusCode::OK);
    scatter().await;
    assert_eq!(other.maps.load(Ordering::SeqCst), 1);
    // Signed in again over it, without signing out: the new one's too.
    let third = std::sync::Arc::new(Fake::default());
    assert_eq!(sign_in(serve(third.clone()).await).await.0, StatusCode::OK);
    scatter().await;
    assert_eq!(third.maps.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_point_opens_its_piece() {
    let (pod, _, _) = signed_in().await;
    let (status, b) = call(
        &pod,
        "GET",
        "/v1/taste/screens/point?n=1",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let point = &b["entities"]["taste://points/1"];
    assert_eq!(point["id"], "shop0.example:1001", "{point}");
    assert!(point["title"].as_str().is_some_and(|t| !t.is_empty()));
    assert!(b["queries"]["taste:point-similar:1"].is_array());
    assert!(b["queries"]["taste:point-contrast:1"].is_array());
    let (status, _) = call(
        &pod,
        "GET",
        "/v1/taste/screens/point?n=99",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &pod,
        "GET",
        "/v1/taste/screens/point?n=x",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_piece_the_service_no_longer_has_leaves_the_deck() {
    let (pod, fake, _) = signed_in().await;
    let app = taste_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    let first = ws["queries"]["taste:feed"][0].as_str().unwrap().to_owned();
    let id = ws["entities"][&first]["id"].as_str().unwrap().to_owned();
    fake.gone.lock().unwrap().push(id.clone());
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/taste/react",
        Some("phone"),
        Some(json!({"id": id, "skipped": true})),
    )
    .await;
    // Not an error: the device keeps the swipe instead of putting the card back.
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["gone"], true);
    let ws = workspace(&pod, &app).await;
    let feed: Vec<String> = ws["queries"]["taste:feed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            ws["entities"][u.as_str().unwrap()]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(!feed.contains(&id), "{feed:?}");
}
