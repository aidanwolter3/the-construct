//! Keeps the simulator's Pixelfed fixtures in step with the pod
//! (docs/DECISIONS.md D34):
//!
//! - `fixtures/pixelfed_home_feed_bundle.json`'s layout is the newest
//!   `layouts/pixelfed_home/NNNN.json`;
//! - `fixtures/pixelfed_screen_data.json` holds the comments screen data of
//!   every fixture post (`{path: DataBundle}`), made by the pod's own
//!   normalization from the fake upstream at a fixed "now", with avatar URLs
//!   pointing at the fake's `/storage/` (which the simulator serves).
//!
//! `UPDATE_FIXTURES=1 cargo test -p server --test fixtures` rewrites them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{DataBundle, UiNode};
use serde_json::Value;
use server::pixelfed::{
    MediaAllowlist, MediaUrls, comment_uri, comments_query, normalize_comments,
};

const FAKE_URL: &str = "http://127.0.0.1:8081";
const SEED: &str = "seed-token";
const OWN_ACCOUNT: &str = "703641122334455667";
/// 2026-11-01T12:00:00Z.
const NOW: i64 = 1_793_534_400;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn update() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

fn check_or_write(name: &str, want: &str) {
    let path = fixture(name);
    if update() {
        std::fs::write(&path, want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "fixtures/{name} is out of date; run UPDATE_FIXTURES=1 cargo test -p server --test fixtures"
    );
}

fn newest_layout() -> UiNode {
    server::layouts::parse_files(server::layouts::FILES)
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == "pixelfed_home")
        .unwrap()
        .layout
}

#[test]
fn feed_fixture_uses_the_newest_layout() {
    let path = fixture("pixelfed_home_feed_bundle.json");
    let mut bundle: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    bundle["workspace"] = server::layouts::node_value(&newest_layout());
    check_or_write(
        "pixelfed_home_feed_bundle.json",
        &(serde_json::to_string_pretty(&bundle).unwrap() + "\n"),
    );
}

#[tokio::test]
async fn screen_data_fixture_is_the_pods_comments_data() {
    let state = Arc::new(
        fake_pixelfed::AppState::new(fake_pixelfed::Config {
            seed_token: Some(SEED.to_owned()),
            ..fake_pixelfed::Config::embedded(FAKE_URL)
        })
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = format!("http://{}", listener.local_addr().unwrap());
    let router = fake_pixelfed::app(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let get = |path: String| {
        let client = client.clone();
        let url = format!("{local}{path}");
        async move {
            client
                .get(url)
                .bearer_auth(SEED)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let timeline = get("/api/v1/timelines/home?include_reblogs=true".into()).await;
    let mut out: BTreeMap<String, DataBundle> = BTreeMap::new();
    for status in timeline.as_array().unwrap() {
        let post = if status["reblog"].is_object() {
            &status["reblog"]
        } else {
            status
        };
        let id = post["id"].as_str().unwrap();
        let context = get(format!("/api/v1/statuses/{id}/context")).await;
        let allow = MediaAllowlist::new([FAKE_URL], []);
        let mut media = MediaUrls::new("http://pod/v1/pixelfed/media", &allow);
        let comments = normalize_comments(id, &context, OWN_ACCOUNT, NOW, &mut media);
        let mut bundle = DataBundle::default();
        let mut order = Vec::new();
        for mut c in comments {
            // The simulator serves /storage/<file> from fixtures/pixelfed/media.
            if let Some(proxied) = c.get_str("avatar_url").map(str::to_owned) {
                let key = proxied.rsplit('/').next().unwrap();
                c.set("avatar_url", media.keys[key].clone());
            }
            let uri = comment_uri(c.get_str("id").unwrap());
            order.push(uri.clone());
            bundle.entities.insert(uri, c);
        }
        bundle.queries.insert(comments_query(id), order);
        out.insert(
            format!("/v1/pixelfed/screens/comments?post_id={id}"),
            bundle,
        );
    }
    assert_eq!(out.len(), 5);
    check_or_write(
        "pixelfed_screen_data.json",
        &(serde_json::to_string_pretty(&out).unwrap() + "\n"),
    );
}
