//! Keeps the simulator's Pixelfed fixtures in step with the pod
//! (docs/DECISIONS.md D34):
//!
//! - `fixtures/pixelfed_home_feed_bundle.json` is the signed-in feed bundle
//!   the pod makes from the fake upstream, with the newest
//!   `layouts/pixelfed_home/NNNN.json` and the fake's own media URLs;
//! - `fixtures/pixelfed_screen_data.json` holds the comments screen data of
//!   every fixture post (`{path: DataBundle}`), made by the pod's own
//!   normalization from the fake upstream at a fixed "now", with avatar URLs
//!   pointing at the fake's `/storage/` (which the simulator serves).
//!
//! `UPDATE_FIXTURES=1 cargo test -p server --test fixtures` rewrites them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{DataBundle, EntityRecord, UiNode};
use serde_json::Value;
use server::pixelfed::{
    MediaAllowlist, MediaUrls, comment_uri, comments_query, complete_comment_thread,
    normalize_comments, normalize_timeline,
};

const FAKE_URL: &str = "http://127.0.0.1:8081";
const PROXY: &str = "http://pod/v1/pixelfed/media";
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
    server::layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == "pixelfed_home")
        .unwrap()
        .layout
}

/// Every string field holding a proxied media URL back to the fake's own URL
/// (the simulator serves `/storage/<file>`).
fn unproxy(entity: &mut EntityRecord, media: &MediaUrls) {
    let fields: Vec<(String, Value)> = entity
        .fields()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in fields {
        if let Some(url) = v.as_str()
            && url.starts_with(PROXY)
        {
            let key = url.rsplit('/').next().unwrap();
            entity.set(k, media.keys[key].clone());
        }
    }
}

async fn fake_upstream() -> (String, reqwest::Client) {
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
    (local, reqwest::Client::new())
}

async fn get(base: &str, client: &reqwest::Client, path: &str) -> Value {
    client
        .get(format!("{base}{path}"))
        .bearer_auth(SEED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The signed-in feed as the pod serves it (fixed media URLs), with the newest
/// `pixelfed_home` layout.
#[tokio::test]
async fn feed_fixture_is_the_pods_feed() {
    let (base, client) = fake_upstream().await;
    let timeline = get(
        &base,
        &client,
        "/api/v1/timelines/home?include_reblogs=true",
    )
    .await;
    let allow = MediaAllowlist::new([FAKE_URL], []);
    let mut media = MediaUrls::new(PROXY, &allow);
    let normalized = normalize_timeline(timeline.as_array().unwrap(), &mut media);
    let mut data = server::pixelfed::layout::home_feed(normalized.entities, normalized.order);
    for e in data.entities.values_mut() {
        unproxy(e, &media);
    }
    // What the core serves: the adapter's data under the newest layout.
    let spec = protocol::WorkspaceSpec {
        version: server::BUNDLE_VERSION.into(),
        workspace: newest_layout(),
        entities: data.entities,
        queries: data.queries,
    };
    let mut v = serde_json::to_value(&spec).unwrap();
    // Through text, so f32 fields are not widened (see layouts::node_value).
    v["workspace"] = server::layouts::node_value(&spec.workspace);
    check_or_write(
        "pixelfed_home_feed_bundle.json",
        &(serde_json::to_string_pretty(&v).unwrap() + "\n"),
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
        let context = complete_comment_thread(id, context, |c| {
            let get = &get;
            async move { Some(get(format!("/api/v1/statuses/{c}/context")).await) }
        })
        .await;
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
