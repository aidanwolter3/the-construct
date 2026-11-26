//! The fit check (decision `taste-fit-check`): a piece's own size chart
//! first, the brand's size guide when it has none, judged against the
//! owner's `fit_targets`. Stores, pages and the model are fakes; the data
//! is made up.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Router, extract::State, response::Html, routing::get};
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Value, json};
use source_taste::{TasteAdapter, db, fit};

use crate::fake;

/// A model that answers every chart read from a script, counting calls.
#[derive(Default)]
struct Model {
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl construct_agent::Transport for Model {
    fn post<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<(u16, Value), String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let prompt = body["messages"][0]["content"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                body["messages"][0]["content"][0]["text"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        self.prompts.lock().unwrap().push(prompt.clone());
        let nulls = json!({"waist": null, "rise": null, "thigh": null, "inseam": null,
            "leg_opening": null});
        let size = |s: &str, chest: f64, length: f64, sleeve: f64| {
            let mut v = nulls.clone();
            v["size"] = json!(s);
            v["chest"] = json!(chest);
            v["length"] = json!(length);
            v["shoulder"] = json!(null);
            v["sleeve"] = json!(sleeve);
            v
        };
        // The piece's own chart has 33 inch sleeves; the brand's guide fits.
        let answer = if prompt.contains("Oxford Shirt") && prompt.contains("Sleeve") {
            if prompt.contains("Brand size guide") {
                json!({"found": true, "basis": "garment", "unit": "in", "chest_is": "flat",
                    "sleeve_from": "center_back",
                    "sizes": [size("M", 20.5, 29.5, 35.0), size("L", 22.0, 30.5, 36.0)]})
            } else {
                json!({"found": true, "basis": "garment", "unit": "in", "chest_is": "flat",
                    "sleeve_from": "center_back",
                    "sizes": [size("M", 20.5, 29.5, 33.0), size("L", 22.0, 30.5, 34.0)]})
            }
        } else {
            json!({"found": false, "basis": "garment", "unit": "in", "chest_is": "unknown",
                "sleeve_from": "unknown", "sizes": []})
        };
        let text = answer.to_string();
        Box::pin(async move {
            Ok((
                200,
                json!({"content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
                    "usage": {"input_tokens": 900, "output_tokens": 120}}),
            ))
        })
    }
}

const ITEM_PAGE: &str = "<html><body><h1>Oxford Shirt</h1><p>A shirt in heavy oxford cloth.</p>\
    <table><tr><th>Size</th><th>Chest</th><th>Length</th><th>Sleeve</th></tr>\
    <tr><td>M</td><td>20.5</td><td>29.5</td><td>33</td></tr>\
    <tr><td>L</td><td>22</td><td>30.5</td><td>34</td></tr></table></body></html>";

const PLAIN_PAGE: &str = "<html><body><h1>Oxford Shirt</h1><p>A shirt in heavy oxford cloth.</p>\
    <a href=\"/pages/size-guide\">Size guide</a></body></html>";

const GUIDE_PAGE: &str = "<html><body><h1>Brand size guide</h1>\
    <table><tr><th>Size</th><th>Chest</th><th>Length</th><th>Sleeve</th></tr>\
    <tr><td>M</td><td>20.5</td><td>29.5</td><td>35</td></tr>\
    <tr><td>L</td><td>22</td><td>30.5</td><td>36</td></tr></table></body></html>";

#[derive(Default)]
struct Store {
    guide_reads: AtomicUsize,
}

async fn guide(State(s): State<Arc<Store>>) -> Html<&'static str> {
    s.guide_reads.fetch_add(1, Ordering::SeqCst);
    Html(GUIDE_PAGE)
}

async fn serve_store() -> (String, Arc<Store>) {
    let s = Arc::new(Store::default());
    let app = Router::new()
        .route("/products/charted", get(|| async { Html(ITEM_PAGE) }))
        .route("/products/plain", get(|| async { Html(PLAIN_PAGE) }))
        .route("/products/plain-too", get(|| async { Html(PLAIN_PAGE) }))
        .route("/pages/size-guide", get(guide))
        .with_state(s.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), s)
}

fn pod(model: Arc<Model>) -> Pod {
    let agent = construct_agent::Agent::new(model, Some("cheap".into()), "capable".into());
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(TasteAdapter::new(true).with_agent(Some(Arc::new(agent)))),
        Arc::new(source_collections::CollectionsAdapter::new()),
        source_shell::adapter(),
    ]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

/// The owner's targets for woven tops.
async fn targets(pod: &Pod) {
    fake::tool(
        pod,
        "define_collection",
        json!({"name": "fit_targets", "title": "Fit targets", "fields": [
            {"name": "kind", "type": "text"},
            {"name": "measure", "type": "text"},
            {"name": "low", "type": "number", "optional": true},
            {"name": "high", "type": "number", "optional": true},
        ]}),
    )
    .await;
    for (m, lo, hi) in [
        ("pits", 20.0, 21.0),
        ("length", 29.0, 30.0),
        ("sleeve", 34.5, 35.5),
    ] {
        fake::tool(
            pod,
            "add_row",
            json!({"collection": "fit_targets",
                "values": {"kind": "woven top", "measure": m, "low": lo, "high": hi}}),
        )
        .await;
    }
}

fn piece(id: &str, url: &str) -> Value {
    json!({"id": id, "brand": "Fernhill Supply", "title": "Oxford Shirt", "category": "shirt",
        "url": url})
}

/// Wait until every one of `ids` has a kept check.
async fn checked(
    pod: &Pod,
    ids: &[&str],
) -> std::collections::HashMap<String, (String, Option<Value>)> {
    let ids: Vec<String> = ids.iter().map(|s| (*s).to_owned()).collect();
    for _ in 0..200 {
        let want = ids.clone();
        let kept = pod.blocking(move |s| db::fits(s, &want)).await.unwrap();
        if kept.len() == ids.len() {
            return kept;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the pieces were never checked");
}

#[tokio::test]
async fn a_piece_is_judged_on_its_own_chart_first() {
    let (store, _) = serve_store().await;
    let model = Arc::new(Model::default());
    let pod = pod(model.clone());
    targets(&pod).await;
    let p = piece("shop.example:1", &format!("{store}/products/charted"));
    fit::schedule(&pod, vec![p.clone()], true);
    let kept = checked(&pod, &["shop.example:1"]).await;
    assert_eq!(kept["shop.example:1"].0, "item");
    let out = fit::annotate(pod.store(), std::slice::from_ref(&p));
    assert_eq!(out[0]["fit"]["state"], "no", "{}", out[0]);
    assert_eq!(out[0]["fit"]["text"], "Won't fit: sleeves 33 in");
    assert!(
        out[0]["fit"]["note"]
            .as_str()
            .unwrap()
            .starts_with("Item chart · M:"),
        "{}",
        out[0]
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    // The cards' entity carries it.
    let (_, rec) = source_taste::bundle::item(&out[0]).unwrap();
    assert_eq!(rec.0["fit_tone"], "error");
    assert_eq!(rec.0["fit_noted"], true);
}

#[tokio::test]
async fn without_a_chart_the_brands_guide_is_read_once() {
    let (store, s) = serve_store().await;
    let model = Arc::new(Model::default());
    let pod = pod(model.clone());
    targets(&pod).await;
    let a = piece("shop.example:2", &format!("{store}/products/plain"));
    fit::schedule(&pod, vec![a.clone()], true);
    let kept = checked(&pod, &["shop.example:2"]).await;
    assert_eq!(kept["shop.example:2"].0, "brand");
    let out = fit::annotate(pod.store(), &[a]);
    assert_eq!(out[0]["fit"]["text"], "Fits in M", "{}", out[0]);
    assert!(
        out[0]["fit"]["note"]
            .as_str()
            .unwrap()
            .starts_with("Brand chart, a guess · M:")
    );
    // Another piece of the same store and kind uses the kept guide.
    let b = piece("shop.example:3", &format!("{store}/products/plain-too"));
    fit::schedule(&pod, vec![b], true);
    let kept = checked(&pod, &["shop.example:3"]).await;
    assert_eq!(kept["shop.example:3"].0, "brand");
    assert_eq!(s.guide_reads.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn nothing_is_checked_without_targets_or_for_shoes() {
    let (store, _) = serve_store().await;
    let model = Arc::new(Model::default());
    let pod = pod(model.clone());
    let p = piece("shop.example:4", &format!("{store}/products/charted"));
    fit::schedule(&pod, vec![p.clone()], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    targets(&pod).await;
    let mut shoe = piece("shop.example:5", &format!("{store}/products/charted"));
    shoe["category"] = json!("loafers or dress shoes");
    fit::schedule(&pod, vec![shoe.clone()], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ids = vec!["shop.example:4".to_owned(), "shop.example:5".to_owned()];
    let kept = pod.blocking(move |s| db::fits(s, &ids)).await.unwrap();
    assert!(kept.is_empty(), "{kept:?}");
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let out = fit::annotate(pod.store(), &[shoe]);
    assert!(out[0].get("fit").is_none());
}

#[tokio::test]
async fn the_feed_carries_a_kept_fit() {
    let (pod, _fake, _) = fake::signed_in().await;
    let app = fake::taste_layout(&pod).await;
    let ws = fake::workspace(&pod, &app).await;
    let first = ws["queries"]["taste:feed"][0].as_str().unwrap().to_owned();
    // No targets on this pod (no collections): no fit fields set.
    let rec = &ws["entities"][&first];
    assert_eq!(rec["fit_state"], "", "{rec}");
    assert_eq!(rec["fit_checked"], false);
}
