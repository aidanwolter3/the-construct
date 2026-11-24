//! Keeps the simulator's Pixelfed fixtures in step with the pod
//! (docs/DECISIONS.md D34):
//!
//! - `crates/construct-sim/data/pixelfed_feed_bundle.json` is the signed-in feed bundle
//!   the pod makes from the fake upstream, with the newest
//!   `layouts/pixelfed/NNNN.json` and the fake's own media URLs;
//! - `crates/construct-sim/data/pixelfed_screen_data.json` holds the comments screen data of
//!   every fixture post with the post itself (`{path: DataBundle}`), made by the pod's own
//!   normalization from the fake upstream at a fixed "now", with avatar URLs
//!   pointing at the fake's `/storage/` (which the simulator serves), and the
//!   post screen data of every fixture post (D48);
//! - every simulator bundle holds its layout as this build's app gets it:
//!   a `core://Workspace` as the `core://Page` it expands to (decision
//!   `plumbing-and-porcelain`, issue #126), the shell core layouts as
//!   `core_layout_<id>_page.json`, and the hand-written bundles from their
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
        .join("../construct-sim/data")
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
        "crates/construct-sim/data/{name} is out of date; run UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::"
    );
}

/// This build's `@core/` elements.
fn core() -> pod_core::construct::core::Core {
    pod_core::construct::core::load(&server::catalog()).unwrap()
}

/// Core layout `id`'s newest file as this build's clients draw it.
fn newest(id: &str) -> Value {
    let core = core();
    let el = core.current(&format!("@core/{id}")).unwrap();
    construct_elements::materialize(&core, el).unwrap().node
}

fn newest_layout() -> UiNode {
    serde_json::from_value(newest("pixelfed")).unwrap()
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
    let mut media = MediaUrls::new(PROXY, &allow, "test-secret");
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
    // As the construct materializes it (not through the types, which
    // leave defaults out).
    v["workspace"] = newest("pixelfed");
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
        let mut media = MediaUrls::new(PROXY, &allow, "test-secret");
        let mut post = normalize_post_screen(id, &raw, &mut media).unwrap();
        for e in post.entities.values_mut() {
            unproxy(e, &media);
        }
        let mut media = MediaUrls::new(PROXY, &allow, "test-secret");
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
    let v = server::layouts::node_value(layout);
    let page = pod_core::construct::core::plumbing(&core(), &v).unwrap();
    assert!(!page.to_string().contains("core://Use"), "{page}");
    page
}

/// The shell's core layouts (home, Settings, the inbox) for the simulator's
/// fixture mode, as a pod serves them to this build:
/// `core_layout_<id>_page.json`.
#[test]
fn shell_core_layouts() {
    for id in ["shell", "settings", "inbox"] {
        let page = newest(id);
        assert_eq!(page["type"], "core://Page");
        let mut text = serde_json::to_string_pretty(&page).unwrap();
        text.push('\n');
        check_or_write(&format!("core_layout_{id}_page.json"), &text);
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
            "crates/server/tests/data/simulator/core_set_bundle.json",
            "core_set_bundle.json",
        ),
        (
            "crates/server/tests/data/simulator/tab_shell_bundle.json",
            "tab_shell_bundle.json",
        ),
        (
            "crates/protocol/tests/data/counter_home_bundle.json",
            "counter_home_bundle.json",
        ),
        (
            "crates/source-pixelfed/sample/bundle.json",
            "pixelfed_sample_bundle.json",
        ),
    ] {
        let text = std::fs::read_to_string(root.join(source)).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        // A page written with components is expanded as it stands; a
        // workspace goes through the typed layout first.
        v["workspace"] = if v["workspace"]["type"] == "core://Page" {
            let page = pod_core::construct::core::plumbing(&core(), &v["workspace"]).unwrap();
            assert!(!page.to_string().contains("core://Use"), "{page}");
            page
        } else {
            let layout: UiNode = serde_json::from_value(v["workspace"].clone()).unwrap();
            plumbing(&layout)
        };
        assert_eq!(v["workspace"]["type"], "core://Page", "{source}");
        check_or_write(name, &(serde_json::to_string_pretty(&v).unwrap() + "\n"));
    }
}

/// The simulator's `server_health` pages: the `monitor` source's core layout
/// as this build's clients draw it, over made-up data
/// (`tests/data/simulator/server_health_data.json`): with every feed, and
/// (`server_health_off`) as a pod without monitoring gets it, the devices,
/// the pod's failed requests and the source's 403.
#[test]
fn server_health_bundles() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/simulator/server_health_data.json");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let page = newest(server::monitor::APP_ID);
    assert_eq!(page["type"], "core://Page");
    assert!(!page.to_string().contains("core://Use"), "{page}");
    for (name, feeds, error) in [
        (
            "server_health_bundle.json",
            &["overview", "devices", "errors"][..],
            None,
        ),
        (
            "server_health_off_bundle.json",
            &["devices", "errors"][..],
            Some("error 403"),
        ),
    ] {
        let mut entities = serde_json::Map::new();
        let mut queries = serde_json::Map::new();
        for feed in feeds {
            entities.extend(data[feed]["entities"].as_object().unwrap().clone());
            queries.extend(data[feed]["queries"].as_object().unwrap().clone());
        }
        // What the pod adds for the source (`pod_core::compose`).
        let mut source = serde_json::json!({
            "id": "monitor", "title": "Server monitoring", "signed_in": true,
        });
        if let Some(error) = error {
            source["error"] = error.into();
        }
        entities.insert("core://sources/monitor".into(), source);
        let bundle = serde_json::json!({
            "version": "1.0.0",
            "workspace": page,
            "entities": entities,
            "queries": queries,
        });
        check_or_write(
            name,
            &(serde_json::to_string_pretty(&bundle).unwrap() + "\n"),
        );
    }
}

/// A made-up taste map: 2,400 pieces in twelve clusters (a fixed
/// pseudo-random walk, so the fixture never changes), point 0 in the
/// middle, the model's chance rising toward the upper left, likes among the
/// warmest and skips among the coolest.
fn made_up_map() -> server::taste::api::MapData {
    let mut seed: u64 = 0x5eed_1234;
    let mut rnd = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as f32) / (1u64 << 31) as f32
    };
    let centres: Vec<[f32; 2]> = (0..12)
        .map(|_| [0.1 + 0.8 * rnd(), 0.1 + 0.8 * rnd()])
        .collect();
    let mut m = server::taste::api::MapData::default();
    let mut chances = Vec::new();
    for i in 0..2400 {
        let [x, y] = if i == 0 {
            [0.5, 0.5]
        } else {
            let c = centres[i % centres.len()];
            let (a, r) = (rnd() * std::f32::consts::TAU, 0.09 * rnd().sqrt());
            [
                (c[0] + r * a.cos()).clamp(0.0, 1.0),
                (c[1] + r * a.sin()).clamp(0.0, 1.0),
            ]
        };
        let p = (1.15 - 0.9 * x - 0.6 * y + 0.15 * (rnd() - 0.5)).clamp(0.0, 1.0);
        m.ids.push(format!("shop{}.example:{}", i % 7, 5000 + i));
        m.xy.push([x, y]);
        chances.push(p);
        if i == 0 || (p > 0.55 && i % 5 == 0) {
            m.liked.insert(i);
        } else if p < 0.15 && i % 7 == 0 {
            m.skipped.insert(i);
        }
    }
    m.p = Some(chances);
    m
}

/// The simulator's `taste` page: the `taste` source's core layout over
/// made-up answers of a taste service (`tests/data/simulator/taste_data.json`),
/// made into entities by the source's own code, and the data paths of its
/// screens (`taste_screen_data.json`).
#[test]
fn taste_bundles() {
    use server::taste::bundle;
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/simulator/taste_data.json");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let kept = data["feed"].as_array().cloned().unwrap_or_default();
    // As the pod answers: the kept feed's reasons filled in.
    let list = |v: &Value| bundle::with_why(&v.as_array().cloned().unwrap_or_default(), &kept);
    let page = newest(server::taste::APP_ID);
    assert_eq!(page["type"], "core://Page");
    // The piece's sheet, a core layout the taste layout opens by id.
    let sheet = newest(server::taste::ITEM_ID);
    assert_eq!(sheet["type"], "core://Page");
    check_or_write(
        &format!("core_layout_{}_page.json", server::taste::ITEM_ID),
        &(serde_json::to_string_pretty(&sheet).unwrap() + "\n"),
    );

    let mut feed = protocol::DataBundle::default();
    bundle::status(
        &mut feed,
        &bundle::Status {
            signed_in: true,
            host: "taste.example".into(),
            likes: data["status"]["likes"].as_i64().unwrap(),
            skips: data["status"]["skips"].as_i64().unwrap(),
            error: None,
        },
    );
    // As the pod serves the deck: only the pieces not reacted to yet.
    let waiting: Vec<Value> = list(&data["feed"])
        .into_iter()
        .filter(|i| !server::taste::db::reacted(i))
        .collect();
    bundle::put_items(&mut feed, bundle::FEED_QUERY, &waiting);
    let mut entities = serde_json::to_value(&feed.entities).unwrap();
    // What the pod adds for the source (`pod_core::compose`).
    entities["core://sources/taste"] =
        serde_json::json!({"id": "taste", "title": "Taste", "signed_in": true});
    let b = serde_json::json!({
        "version": "1.0.0",
        "workspace": page,
        "entities": entities,
        "queries": feed.queries,
    });
    check_or_write(
        "taste_bundle.json",
        &(serde_json::to_string_pretty(&b).unwrap() + "\n"),
    );

    let mut screens = serde_json::Map::new();
    for (id, s) in data["items"].as_object().unwrap() {
        let key = bundle::key_of(id);
        let mut b = protocol::DataBundle::default();
        bundle::put_items(
            &mut b,
            &format!("taste:similar:{key}"),
            &list(&s["similar"]),
        );
        bundle::put_items(
            &mut b,
            &format!("taste:contrast:{key}"),
            &list(&s["contrast"]),
        );
        let item = bundle::with_why(std::slice::from_ref(&s["item"]), &kept).remove(0);
        // As the pod answers: the similar pieces again, for the piece's sheet.
        bundle::put_sheet_similar(&mut b, &key, &list(&s["similar"]));
        bundle::put_sheet_piece(&mut b, &item);
        bundle::put_item(&mut b, &item);
        screens.insert(
            format!("/v1/taste/screens/item?key={key}"),
            serde_json::to_value(&b).unwrap(),
        );
        // The same piece as point 0 of the map (the dot in its middle).
        let mut pt = protocol::DataBundle::default();
        bundle::put_items(&mut pt, "taste:point-similar:0", &list(&s["similar"]));
        bundle::put_items(&mut pt, "taste:point-contrast:0", &list(&s["contrast"]));
        bundle::put_item(&mut pt, &item);
        if let Some((_, mut rec)) = bundle::item(&item) {
            rec.0.insert("uri".into(), Value::from("taste://points/0"));
            pt.entities.insert("taste://points/0".into(), rec);
        }
        screens.insert(
            "/v1/taste/screens/point?n=0".into(),
            serde_json::to_value(&pt).unwrap(),
        );
    }
    // The whole map: made-up pieces in clusters, liked where the model's
    // chance is high and skipped where it is low.
    let mut map = protocol::DataBundle::default();
    let (uri, rec) = bundle::scatter(&made_up_map());
    map.entities.insert(uri, rec);
    screens.insert(
        "/v1/taste/screens/scatter".into(),
        serde_json::to_value(&map).unwrap(),
    );
    let mut likes = protocol::DataBundle::default();
    bundle::put_items(&mut likes, bundle::LIKES_QUERY, &list(&data["likes"]));
    screens.insert(
        "/v1/taste/screens/likes".into(),
        serde_json::to_value(&likes).unwrap(),
    );
    let mut taste = protocol::DataBundle::default();
    bundle::tastes(&mut taste, &data["taste"]);
    screens.insert(
        "/v1/taste/screens/taste".into(),
        serde_json::to_value(&taste).unwrap(),
    );
    // Each taste to browse, as the service answers GET /api/taste/<n>: its
    // likes, then the feed's pieces as the nearest ones not judged yet.
    for (n, t) in data["taste"]["tastes"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let mut v = t.clone();
        let liked: Vec<&Value> = t["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| &i["id"])
            .collect();
        let more: Vec<Value> = list(&data["feed"])
            .into_iter()
            .filter(|i| !liked.contains(&&i["id"]))
            .collect();
        v["more"] = Value::Array(more);
        let mut b = protocol::DataBundle::default();
        bundle::one_taste(&mut b, n, &v);
        screens.insert(
            format!("/v1/taste/screens/one-taste?n={n}"),
            serde_json::to_value(&b).unwrap(),
        );
    }
    for (area, tiles) in data["map"].as_object().unwrap() {
        let mut m = protocol::DataBundle::default();
        bundle::map(&mut m, area, tiles);
        screens.insert(
            format!("/v1/taste/screens/map?area={area}"),
            serde_json::to_value(&m).unwrap(),
        );
    }
    // The box the scenario selects on the scatter.
    let selected = "200000-200000-450000-450000";
    let mut a = protocol::DataBundle::default();
    let first_area = data["area"].as_object().unwrap().values().next().unwrap();
    bundle::put_items(&mut a, &bundle::area_query(selected), &list(first_area));
    screens.insert(
        format!("/v1/taste/screens/area?area={selected}"),
        serde_json::to_value(&a).unwrap(),
    );
    for (area, items) in data["area"].as_object().unwrap() {
        let mut a = protocol::DataBundle::default();
        bundle::put_items(&mut a, &bundle::area_query(area), &list(items));
        screens.insert(
            format!("/v1/taste/screens/area?area={area}"),
            serde_json::to_value(&a).unwrap(),
        );
    }
    check_or_write(
        "taste_screen_data.json",
        &(serde_json::to_string_pretty(&Value::Object(screens)).unwrap() + "\n"),
    );
}
