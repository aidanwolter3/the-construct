//! The shopping kit (decision `shopping-kit`): `use_kit shopping` makes its
//! collections; a need is explored through the taste service in rounds
//! (`taste` fillers, here a fake explorer), each round anchored on what was
//! liked so far; a pick keeps a daily snapshot of its price (`snapshots`)
//! and notifies when it drops.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Map, Value, json};
use source_collections::explore::Explorer;
use source_collections::fill;
use source_collections::{Caps, CollectionsAdapter};

use crate::fillers::{call, filler_ids, ok, rows, tool};

/// Answers every request with three pieces whose keys name the round
/// (`r<n>-a`, from the request's distance), and records the requests.
#[derive(Debug, Default)]
struct FakeTaste {
    asked: Mutex<Vec<Map<String, Value>>>,
}

impl Explorer for FakeTaste {
    fn explore<'a>(
        &'a self,
        _pod: &'a Pod,
        request: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            let n = self.asked.lock().unwrap().len();
            self.asked.lock().unwrap().push(request.clone());
            Ok(["a", "b", "c"]
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    json!({
                        "key": format!("shop_example__r{n}{k}"),
                        "title": format!("Piece {n}{k}"),
                        "brand": "Maker",
                        "image": format!("https://cdn.shop.example/{n}{k}.jpg"),
                        "price": 100 + i * 20,
                        "price_text": format!("${}", 100 + i * 20),
                        "url": format!("https://shop.example/p/{n}{k}"),
                        "host": "shop.example",
                        "group": i % 2,
                        "group_label": if i % 2 == 0 { "Arched" } else { "Leaners" },
                        "group_ids": "shop_example__x1, shop_example__x2",
                    })
                    .as_object()
                    .unwrap()
                    .clone()
                })
                .collect())
        })
    }
}

fn pod(taste: Arc<FakeTaste>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_explorer(Some(taste as Arc<dyn Explorer>));
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn create(pod: &Pod, collection: &str, body: Value) -> String {
    let (s, v) = call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/create"),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

async fn update(pod: &Pod, collection: &str, body: Value) {
    let (s, v) = call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/update"),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

async fn row(pod: &Pod, collection: &str, id: &str) -> Value {
    rows(pod, collection)
        .await
        .into_iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row {id} in {collection}"))
}

/// Explore the round just made (what a write starts in the background).
async fn explore(pod: &Pod) {
    let f = filler_ids(pod, "shop_pieces").await.remove(0);
    for _ in 0..100 {
        if !matches!(
            fill::fill(pod, "shop_pieces", &f, false).await,
            fill::Outcome::Busy
        ) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the explorer stayed busy");
}

/// What the daily price check writes: `price_now`, as a per-row fill does
/// (computed fields, today's snapshot, then what follows).
async fn price_check(pod: &Pod, pick: &str, price: i64) {
    use source_collections::{compute, db, follow, snapshots};
    let caps = Caps::default();
    let now = pod_core::util::now_secs();
    let id = pick.to_owned();
    let followed = pod
        .store()
        .transaction(|tx| {
            let (_, def) = db::latest_def(tx, "shop_picks")?.unwrap();
            let mut r = db::row(tx, "shop_picks", &id)?.unwrap();
            let before = r.values.clone();
            let f = def.field("price_now").unwrap().id.clone();
            r.values.insert(f.clone(), json!(price));
            let mut write = Map::new();
            write.insert(f, json!(price));
            compute::after_write(tx, &def, Some(&before), &mut r, &write, now, caps)?;
            db::save_row(tx, "shop_picks", &r)?;
            let today = fill::today_on(tx, now)?;
            snapshots::take(tx, &def, &r, &today, now, caps)?;
            follow::settle(tx, now, caps)
        })
        .unwrap();
    followed.finish(pod);
}

/// An empty computed text is served as null.
fn blank(v: &Value) -> bool {
    v.as_str().is_none_or(str::is_empty)
}

async fn pieces_of(pod: &Pod, round: &str) -> Vec<Value> {
    rows(pod, "shop_pieces")
        .await
        .into_iter()
        .filter(|r| r["round"] == round)
        .collect()
}

#[tokio::test]
async fn use_kit_makes_the_shopping_collections_once() {
    let pod = pod(Arc::default());
    let v = ok(&pod, "use_kit", json!({"kit": "shopping"})).await;
    assert_eq!(
        v["collections"],
        json!([
            "shop_categories",
            "shop_needs",
            "shop_rounds",
            "shop_pieces",
            "shop_picks",
            "shop_prices"
        ])
    );
    assert_eq!(v["embed"]["ref"]["name"], "@core/shopping");
    let cats: Vec<Value> = rows(&pod, "shop_categories").await;
    let names: Vec<&str> = cats.iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names, ["Clothes", "Home", "Other"]);
    // Again: nothing new, nothing doubled.
    let again = ok(&pod, "use_kit", json!({"kit": "shopping"})).await;
    assert_eq!(again["created"], json!([]));
    assert_eq!(rows(&pod, "shop_categories").await.len(), 3);
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "shop_picks"}),
    )
    .await;
    assert_eq!(d["definition"]["alerts"].as_array().unwrap().len(), 2);
    assert_eq!(d["definition"]["snapshots"][0]["into"], "shop_prices");
    // The kit's collections keep their names.
    let (err, _) = tool(
        &pod,
        "use_kit",
        json!({"kit": "shopping", "collection": "mine"}),
    )
    .await;
    assert!(err);
}

#[tokio::test]
async fn a_need_is_explored_in_rounds_anchored_on_what_was_liked() {
    let taste = Arc::new(FakeTaste::default());
    let pod = pod(taste.clone());
    ok(&pod, "use_kit", json!({"kit": "shopping"})).await;
    let need = create(
        &pod,
        "shop_needs",
        json!({"name": "Rain shell", "category": "Clothes"}),
    )
    .await;
    let n = row(&pod, "shop_needs", &need).await;
    assert_eq!(n["explorable"], true);
    assert_eq!(n["state"], "choosing");
    assert_eq!(n["status"], "Nothing chosen yet");

    // Round 0: the kinds, broad and grouped.
    let r0 = create(
        &pod,
        "shop_rounds",
        json!({"need": need, "n": 0, "query": "Rain shell"}),
    )
    .await;
    explore(&pod).await;
    let asked = taste.asked.lock().unwrap()[0].clone();
    assert_eq!(asked["query"], "Rain shell");
    assert_eq!(asked["distance"], 0.5);
    assert_eq!(asked["spread"], 1.0);
    assert_eq!(asked["group"], true);
    let p0 = pieces_of(&pod, &r0).await;
    assert_eq!(p0.len(), 3);
    assert!(p0.iter().all(|p| p["need"] == need && p["current"] == true));
    assert_eq!(p0[0]["group_label"], "Arched");
    let n = row(&pod, "shop_needs", &need).await;
    assert_eq!(n["status"], "Exploring · round 0");

    // A like and a dislike, then round 1 made from the need's lists.
    let (a, b) = (p0[0]["id"].as_str().unwrap(), p0[1]["id"].as_str().unwrap());
    update(
        &pod,
        "shop_pieces",
        json!({"id": a, "reaction": "more", "shortlisted": true}),
    )
    .await;
    update(&pod, "shop_pieces", json!({"id": b, "reaction": "less"})).await;
    let n = row(&pod, "shop_needs", &need).await;
    assert_eq!(n["like_ids"], "shop_example__r0a");
    assert_eq!(n["unlike_ids"], "shop_example__r0b");
    assert_eq!(n["shortlist_count"], 1);
    let r1 = create(
        &pod,
        "shop_rounds",
        json!({"need": need, "n": 1, "query": "Rain shell", "like": n["like_ids"],
               "unlike": n["unlike_ids"], "exclude": n["shown_ids"]}),
    )
    .await;
    explore(&pod).await;
    let asked = taste.asked.lock().unwrap()[1].clone();
    assert_eq!(asked["like"], json!(["shop_example__r0a"]));
    assert_eq!(asked["unlike"], json!(["shop_example__r0b"]));
    assert_eq!(asked["exclude"].as_array().unwrap().len(), 3);
    assert_eq!(asked["distance"], 0.3);
    assert_eq!(asked["group"], false);
    // Only the latest round is current; the earlier one keeps its pieces.
    assert!(
        pieces_of(&pod, &r1)
            .await
            .iter()
            .all(|p| p["current"] == true)
    );
    assert!(
        pieces_of(&pod, &r0)
            .await
            .iter()
            .all(|p| p["current"] == false)
    );
    // A round is explored once: another fill asks nothing more.
    explore(&pod).await;
    assert_eq!(taste.asked.lock().unwrap().len(), 2);

    // Back a round: the one before is current again.
    let (s, v) = call(
        &pod,
        "POST",
        "/v1/collections/shop_rounds/delete",
        Some(json!({"id": r1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        pieces_of(&pod, &r0)
            .await
            .iter()
            .all(|p| p["current"] == true)
    );
}

#[tokio::test]
async fn a_pick_keeps_a_daily_price_and_notifies_when_it_drops() {
    let pod = pod(Arc::default());
    ok(&pod, "use_kit", json!({"kit": "shopping"})).await;
    let need = create(
        &pod,
        "shop_needs",
        json!({"name": "Full-length mirror", "category": "Home"}),
    )
    .await;
    assert_eq!(row(&pod, "shop_needs", &need).await["explorable"], false);
    let pick = create(
        &pod,
        "shop_picks",
        json!({"need": need, "product": "Alder Arch Floor Mirror", "store": "oakline.example",
               "chosen_price": 340}),
    )
    .await;
    let p = row(&pod, "shop_picks", &pick).await;
    assert_eq!(p["price_text"], "$340");
    assert_eq!(p["points"], 1);
    assert!(blank(&p["deal"]), "{p}");
    let n = row(&pod, "shop_needs", &need).await;
    assert_eq!(n["state"], "watching");
    assert_eq!(n["status"], "Alder Arch Floor Mirror");

    // Today's price, read later the same day: still one point, updated.
    price_check(&pod, &pick, 289).await;
    let prices = rows(&pod, "shop_prices").await;
    assert_eq!(prices.len(), 1, "{prices:?}");
    assert_eq!(prices[0]["price"], 289);
    let p = row(&pod, "shop_picks", &pick).await;
    assert_eq!(p["deal"], "Down from $340");
    source_collections::alerts::run(&pod, None).await;
    let sent = pod.stored_notifications().unwrap();
    assert_eq!(sent.len(), 1, "{sent:?}");
    // The daily snapshot adds nothing on a day that has one.
    source_collections::snapshots::daily(&pod).await;
    assert_eq!(rows(&pod, "shop_prices").await.len(), 1);

    // Not watched: no deal, and no price check.
    update(&pod, "shop_picks", json!({"id": pick, "watch": false})).await;
    let p = row(&pod, "shop_picks", &pick).await;
    assert!(blank(&p["deal"]), "{p}");
    assert!(blank(&p["price_query"]), "{p}");
    assert_eq!(row(&pod, "shop_needs", &need).await["state"], "ready");
}

#[tokio::test]
async fn a_taste_filler_names_what_it_reads() {
    let pod = pod(Arc::default());
    ok(
        &pod,
        "define_collection",
        json!({"name": "asks", "fields": [{"name": "q", "type": "text"}]}),
    )
    .await;
    let (err, v) = tool(
        &pod,
        "define_collection",
        json!({"name": "found", "fields": [
            {"name": "ask", "type": "ref", "collection": "asks"},
            {"name": "key", "type": "text", "optional": true}],
        "fillers": [{"kind": "taste", "for_each": {"collection": "asks", "link": "ask"},
                     "request": {"colour": "q"}, "map": {"key": "key"}}]}),
    )
    .await;
    assert!(err);
    assert!(
        v.to_string().contains("not a part of an explore request"),
        "{v}"
    );
    let (err, v) = tool(
        &pod,
        "define_collection",
        json!({"name": "found", "fields": [
            {"name": "ask", "type": "ref", "collection": "asks"},
            {"name": "key", "type": "text", "optional": true}],
        "fillers": [{"kind": "taste", "for_each": {"collection": "asks", "link": "ask"},
                     "request": {"query": "q"}, "map": {"key": "key"}}]}),
    )
    .await;
    assert!(!err, "{v}");
}
