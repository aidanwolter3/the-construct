//! Keeps the simulator's Pixelfed fixtures in step with the pod
//! (docs/DECISIONS.md D34):
//!
//! - `app/desktopApp/src/sim/resources/pixelfed_feed_bundle.json` is the signed-in feed bundle
//!   the pod makes from the fake upstream, with the newest
//!   `layouts/pixelfed/NNNN.json` and the fake's own media URLs;
//! - `app/desktopApp/src/sim/resources/pixelfed_screen_data.json` holds the comments screen data of
//!   every fixture post with the post itself (`{path: DataBundle}`), made by the pod's own
//!   normalization from the fake upstream at a fixed "now", with avatar URLs
//!   pointing at the fake's `/storage/` (which the simulator serves), and the
//!   post screen data of every fixture post (D48);
//! - every simulator bundle holds its layout as this build's app gets it:
//!   a `core://Workspace` as the `core://Page` it expands to (decision
//!   `plumbing-and-porcelain`, issue #126), the shell blueprints as
//!   `blueprint_<id>_page.json`, and the hand-written bundles from their
//!   workspace sources in `crates/server/tests/data/simulator/` (and the
//!   protocol tests' counter bundle, the shipped sample bundle).
//!
//! `UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::` rewrites them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{DataBundle, EntityRecord, UiNode};
use serde_json::Value;
use server::pixelfed::{
    MediaAllowlist, MediaUrls, comment_uri, comments_query, complete_comment_thread,
    normalize_comments, normalize_post_screen, normalize_timeline,
};

const FAKE_URL: &str = "http://127.0.0.1:8081";
const PROXY: &str = "http://pod/v1/pixelfed/media";
const SEED: &str = "seed-token";
const OWN_ACCOUNT: &str = "703641122334455667";
/// 2026-11-01T12:00:00Z.
const NOW: i64 = 1_793_534_400;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../app/desktopApp/src/sim/resources")
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
        "app/desktopApp/src/sim/resources/{name} is out of date; run UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::"
    );
}

fn newest_layout() -> UiNode {
    server::layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == "pixelfed")
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
/// `pixelfed` layout.
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
    v["workspace"] = plumbing(&spec.workspace);
    check_or_write(
        "pixelfed_feed_bundle.json",
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
        // The post screen's data; the comments screen shows the post too.
        let raw = get(format!("/api/v1/statuses/{id}")).await;
        let mut media = MediaUrls::new(PROXY, &allow);
        let mut post = normalize_post_screen(id, &raw, &mut media).unwrap();
        for e in post.entities.values_mut() {
            unproxy(e, &media);
        }
        let mut media = MediaUrls::new(PROXY, &allow);
        let comments = normalize_comments(id, &context, OWN_ACCOUNT, NOW, &mut media);
        let mut bundle = post.clone();
        let mut order = Vec::new();
        for mut c in comments {
            // The simulator serves /storage/<file> from fake-pixelfed/fixtures/media.
            unproxy(&mut c, &media);
            let uri = comment_uri(c.get_str("id").unwrap());
            order.push(uri.clone());
            bundle.entities.insert(uri, c);
        }
        bundle.queries.insert(comments_query(id), order);
        out.insert(
            format!("/v1/pixelfed/screens/comments?post_id={id}"),
            bundle,
        );
        out.insert(format!("/v1/pixelfed/screens/post?id={id}"), post);
    }
    assert_eq!(out.len(), 10);
    check_or_write(
        "pixelfed_screen_data.json",
        &(serde_json::to_string_pretty(&out).unwrap() + "\n"),
    );
}

/// A layout as this build's app gets it: a `core://Workspace` as the
/// `core://Page` it expands to (decision `plumbing-and-porcelain`), anything
/// else as it is.
fn plumbing(layout: &UiNode) -> Value {
    let state = server::state_in_memory();
    let v = server::layouts::node_value(layout);
    let page = server::porcelain::to_plumbing(state.store(), state.catalog(), &v).unwrap();
    assert!(!page.to_string().contains("core://Use"), "{page}");
    page
}

/// The shell's blueprints (home, Settings, the inbox) for the simulator's
/// fixture mode, as a pod serves them to this build:
/// `blueprint_<id>_page.json`.
#[test]
fn shell_blueprints() {
    for id in ["shell", "settings", "inbox"] {
        let file = server::layouts::parse_files(&server::catalog(), &server::layout_files())
            .unwrap()
            .into_iter()
            .rfind(|f| f.app_id == id)
            .unwrap();
        let page = plumbing(&file.layout);
        assert_eq!(page["type"], "core://Page");
        let mut text = serde_json::to_string_pretty(&page).unwrap();
        text.push('\n');
        check_or_write(&format!("blueprint_{id}_page.json"), &text);
    }
}

/// The simulator's other bundles, each with its layout as this build gets it
/// ([`plumbing`]), from a bundle whose layout is a workspace.
#[test]
fn simulator_bundles() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (source, name) in [
        (
            "crates/server/tests/data/simulator/custom_kitchen_bundle.json",
            "custom_kitchen_bundle.json",
        ),
        (
            "crates/server/tests/data/simulator/row_long_text_bundle.json",
            "row_long_text_bundle.json",
        ),
        (
            "crates/protocol/tests/data/counter_home_bundle.json",
            "counter_home_bundle.json",
        ),
        (
            "crates/app-pixelfed/sample/bundle.json",
            "pixelfed_sample_bundle.json",
        ),
    ] {
        let text = std::fs::read_to_string(root.join(source)).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        let layout: UiNode = serde_json::from_value(v["workspace"].clone()).unwrap();
        v["workspace"] = plumbing(&layout);
        assert_eq!(v["workspace"]["type"], "core://Page", "{source}");
        check_or_write(name, &(serde_json::to_string_pretty(&v).unwrap() + "\n"));
    }
}
