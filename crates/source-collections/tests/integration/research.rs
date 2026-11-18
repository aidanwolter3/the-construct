//! Research per row (decision `research-per-row`): an agent filler with
//! `for_each` researches each row of a shopping list on its own, with the
//! owner's preferences as context, and keeps each item's options as rows
//! linked back to it; `keep` holds an option someone put in their bag, and
//! a `repeat` alert notifies again each time a row comes back into a match.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Map, Value, json};
use source_collections::agent::{AgentAnswer, AgentRequest, FillAgent};
use source_collections::fill::{self, Outcome};
use source_collections::{Caps, CollectionsAdapter};

use crate::fillers::{call, filler_ids, ok, rows, settle, tool};

/// Answers each request with the options listed for the item its request
/// names, and records the requests.
#[derive(Debug, Default)]
struct Researcher {
    /// `(text in the request, rows)`.
    answers: Mutex<Vec<(String, Vec<Value>)>>,
    asked: Mutex<Vec<(String, usize)>>,
    /// A request with this text never answers (a research that runs on).
    hang: Mutex<Option<String>>,
}

impl FillAgent for Researcher {
    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async move {
            self.asked
                .lock()
                .unwrap()
                .push((req.prompt.to_owned(), req.current.len()));
            let hang = self.hang.lock().unwrap().clone();
            if hang.is_some_and(|h| req.prompt.contains(h.as_str())) {
                std::future::pending::<()>().await;
            }
            let rows = self
                .answers
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| req.prompt.contains(k.as_str()))
                .map(|(_, r)| r.clone())
                .unwrap_or_default();
            Ok(AgentAnswer {
                rows: rows
                    .iter()
                    .map(|r| r.as_object().unwrap().clone())
                    .collect::<Vec<Map<String, Value>>>(),
                searches: 1,
                fetches: 1,
            })
        })
    }
}

fn pod(agent: Arc<Researcher>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_agent(Some(agent as Arc<dyn FillAgent>));
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

/// The options of item `id`, as `(store, in_bag)`, by store.
async fn options_of(pod: &Pod, id: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = rows(pod, "options")
        .await
        .iter()
        .filter(|r| r["item"] == id)
        .map(|r| {
            (
                r["store"].as_str().unwrap().to_owned(),
                r["in_bag"].as_bool().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

async fn define(pod: &Pod, fills_per_day: u32) -> String {
    ok(
        pod,
        "define_collection",
        json!({"name": "items", "title": "Shopping", "fields": [
            {"name": "name", "type": "text"},
            {"name": "notes", "type": "text", "optional": true},
            {"name": "category", "type": "text", "optional": true},
            {"name": "done", "type": "bool", "default": false}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "prefs", "title": "My preferences", "fields": [
            {"name": "category", "type": "text"},
            {"name": "likes", "type": "text"}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "options", "title": "Options", "fields": [
            {"name": "item", "type": "ref", "collection": "items"},
            {"name": "store", "type": "text"},
            {"name": "url", "type": "text"},
            {"name": "price", "type": "number", "optional": true},
            {"name": "in_bag", "type": "bool", "default": false}],
        "fillers": [{"kind": "agent", "title": "Research",
            "prompt": "Find two or three good options to buy for this item.",
            "fields": ["store", "url", "price"], "key": "url", "fills_per_day": fills_per_day,
            "keep": [{"field": "in_bag", "equals": true}],
            "for_each": {"collection": "items", "link": "item",
                "where": [{"field": "done", "equals": false}],
                "context": [{"collection": "prefs", "fields": ["category", "likes"]}]}}]}),
    )
    .await;
    settle(pod, "options").await;
    filler_ids(pod, "options").await.remove(0)
}

fn option(store: &str, price: u32) -> Value {
    json!({"store": store, "url": format!("https://{store}.example/p"), "price": price})
}

#[tokio::test]
async fn each_item_is_researched_on_its_own_with_the_owners_preferences() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent.clone());
    let filler = define(&pod, 10).await;
    create(
        &pod,
        "prefs",
        json!({"category": "Clothes", "likes": "Soft fabrics, no logos"}),
    )
    .await;
    let tees = create(
        &pod,
        "items",
        json!({"name": "3 V-neck undershirts", "category": "Clothes"}),
    )
    .await;
    let jeans = create(&pod, "items", json!({"name": "2 pairs of jeans"})).await;
    *agent.answers.lock().unwrap() = vec![
        (
            "V-neck".into(),
            vec![option("tees", 30), option("basics", 25)],
        ),
        ("jeans".into(), vec![option("denim", 80)]),
    ];

    let out = fill::fill(&pod, "options", &filler, false).await;
    let Outcome::Filled { stored, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(stored.created, 3);
    {
        let asked = agent.asked.lock().unwrap();
        assert_eq!(asked.len(), 2, "one request per item");
        let tee_ask = &asked.iter().find(|a| a.0.contains("V-neck")).unwrap().0;
        assert!(
            tee_ask.starts_with("Find two or three good options"),
            "{tee_ask}"
        );
        assert!(
            tee_ask.contains("- name: 3 V-neck undershirts\n- category: Clothes"),
            "{tee_ask}"
        );
        assert!(
            !tee_ask.contains("done"),
            "true/false fields stay out: {tee_ask}"
        );
        assert!(!tee_ask.contains("jeans"), "only its own item: {tee_ask}");
        assert!(
            tee_ask.contains("From \"My preferences\" (1 rows):\n{\"category\":\"Clothes\",\"likes\":\"Soft fabrics, no logos\"}"),
            "{tee_ask}"
        );
    }
    assert_eq!(
        options_of(&pod, &tees).await,
        [("basics".into(), false), ("tees".into(), false)]
    );
    assert_eq!(options_of(&pod, &jeans).await, [("denim".into(), false)]);

    // Nothing changed: no model call.
    fill::fill(&pod, "options", &filler, false).await;
    assert_eq!(agent.asked.lock().unwrap().len(), 2);

    // An option goes in the bag; the tees' notes change, so they are
    // researched again (with their current options), and the new answer
    // leaves the bagged one out: it stays.
    let basics = rows(&pod, "options")
        .await
        .into_iter()
        .find(|r| r["store"] == "basics")
        .unwrap();
    update(&pod, "options", json!({"id": basics["id"], "in_bag": true})).await;
    update(&pod, "items", json!({"id": tees, "notes": "Thick cotton"})).await;
    *agent.answers.lock().unwrap() = vec![(
        "V-neck".into(),
        vec![option("tees", 28), option("heavy", 35)],
    )];
    let out = fill::fill(&pod, "options", &filler, false).await;
    assert!(matches!(out, Outcome::Filled { .. }), "{out:?}");
    {
        let asked = agent.asked.lock().unwrap();
        assert_eq!(asked.len(), 3, "only the changed item");
        assert!(asked[2].0.contains("- notes: Thick cotton"));
        assert_eq!(asked[2].1, 2, "its own current options");
    }
    assert_eq!(
        options_of(&pod, &tees).await,
        [
            ("basics".into(), true),
            ("heavy".into(), false),
            ("tees".into(), false)
        ]
    );
    let tee = rows(&pod, "options")
        .await
        .into_iter()
        .find(|r| r["store"] == "tees")
        .unwrap();
    assert_eq!(tee["price"], 28, "a known option keeps its row, new values");

    // The model can't set the link or the filled fields by hand.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/options/update",
        Some(json!({"id": tee["id"], "price": 1})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // A deleted item's options go (the bagged one of a bought item would
    // stay: `keep`).
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/items/delete",
        Some(json!({"id": jeans})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // A bought item isn't researched again, even when it changes.
    update(
        &pod,
        "items",
        json!({"id": tees, "done": true, "notes": "Pima"}),
    )
    .await;
    let out = fill::fill(&pod, "options", &filler, false).await;
    assert!(matches!(out, Outcome::Filled { .. }), "{out:?}");
    assert_eq!(agent.asked.lock().unwrap().len(), 3);
    assert!(options_of(&pod, &jeans).await.is_empty());
    assert_eq!(options_of(&pod, &tees).await.len(), 3);

    // The spec reads back with names.
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "options"}),
    )
    .await;
    let f = &d["definition"]["fillers"][0];
    assert_eq!(f["for_each"]["collection"], "items");
    assert_eq!(f["for_each"]["link"], "item");
    assert_eq!(
        f["for_each"]["where"],
        json!([{"field": "done", "equals": "false"}])
    );
    assert_eq!(f["keep"], json!([{"field": "in_bag", "equals": "true"}]));
    assert_eq!(f["refresh"], "once");
    assert_eq!(f["fields"], json!(["store", "url", "price"]));
}

#[tokio::test]
async fn each_researched_item_counts_toward_the_daily_budget() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent.clone());
    let filler = define(&pod, 2).await;
    for n in ["Lamp", "Rug", "Throw blanket"] {
        create(&pod, "items", json!({"name": n})).await;
    }
    *agent.answers.lock().unwrap() = vec![
        ("Lamp".into(), vec![option("lamps", 40)]),
        ("Rug".into(), vec![option("rugs", 90)]),
        ("Throw".into(), vec![option("throws", 30)]),
    ];
    let out = fill::fill(&pod, "options", &filler, false).await;
    assert!(matches!(out, Outcome::Filled { .. }), "{out:?}");
    assert_eq!(agent.asked.lock().unwrap().len(), 2, "two left today");
    let out = fill::fill(&pod, "options", &filler, false).await;
    assert!(matches!(out, Outcome::Failed(_)), "{out:?}");
    assert_eq!(agent.asked.lock().unwrap().len(), 2);
    assert_eq!(rows(&pod, "options").await.len(), 2);
}

#[tokio::test]
async fn a_for_each_spec_must_name_a_collection_and_a_ref_to_it() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent);
    define(&pod, 4).await;
    for (spec, want) in [
        (
            json!({"collection": "nope", "link": "item"}),
            "no collection 'nope'",
        ),
        (
            json!({"collection": "items", "link": "store"}),
            "must be a ref field to 'items'",
        ),
        (
            json!({"collection": "items", "link": "item", "where": [{"field": "colour", "equals": "x"}]}),
            "has no field 'colour'",
        ),
        (
            json!({"collection": "items", "link": "item", "context": [{"collection": "prefs", "limit": 500}]}),
            "limit is 1..=100",
        ),
    ] {
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "options", "changes": [{"op": "set_filler", "filler": {
                "kind": "agent", "prompt": "Find options.", "fields": ["store", "url"],
                "key": "url", "for_each": spec}}]}),
        )
        .await;
        assert!(err, "{v}");
        assert!(v.to_string().contains(want), "{want}: {v}");
    }
}

#[tokio::test]
async fn a_repeating_alert_notifies_each_time_a_row_comes_back_into_a_match() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent);
    ok(
        &pod,
        "define_collection",
        json!({"name": "watch", "title": "Prices", "fields": [
            {"name": "name", "type": "text"},
            {"name": "price", "type": "number", "optional": true},
            {"name": "price_now", "type": "number", "optional": true},
            {"name": "deal", "type": "text", "optional": true, "compute": {"kind": "expr",
             "expr": "entity.price_now ? (entity.price_now * 100 <= entity.price * 85 ? \"Deal\" : \"\") : \"\""}}],
        "alerts": [{"name": "Deals", "any": ["deal"], "fields": ["deal"], "repeat": true}]}),
    )
    .await;
    ok(
        &pod,
        "create_layout",
        json!({"title": "Prices", "feeds": [["collections", "watch"]],
            "layout": {"type": "core://Workspace", "id": "x", "title": "Prices", "slots": [
                {"slot_id": "list", "provider_uri": "core://custom", "content": {
                    "type": "core://List", "query_key": "collection:watch",
                    "item_template": {"type": "core://Text", "style": "Body",
                        "value_expr": "$item.name"}}}]}}),
    )
    .await;
    let id = create(&pod, "watch", json!({"name": "Jeans", "price": 80})).await;
    // With `repeat`, a row that stops matching loses its (stale)
    // notification, and the next drop notifies again: one notification, the
    // latest.
    let notified = |pod: &Pod| pod.stored_notifications().unwrap().len();
    for (now, want) in [(78, 0), (65, 1), (60, 1), (79, 0), (64, 1)] {
        update(&pod, "watch", json!({"id": id, "price_now": now})).await;
        source_collections::alerts::run(&pod, None).await;
        assert_eq!(notified(&pod), want, "at {now}");
    }
    let d = ok(&pod, "describe_collection", json!({"collection": "watch"})).await;
    assert_eq!(d["definition"]["alerts"][0]["repeat"], true);
}

#[tokio::test]
async fn with_refresh_every_a_row_is_researched_again_only_when_its_interval_has_passed() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent.clone());
    let filler = define(&pod, 10).await;
    let v = ok(
        &pod,
        "describe_collection",
        json!({"collection": "options"}),
    )
    .await;
    let mut spec = v["definition"]["fillers"][0].clone();
    spec["refresh"] = json!("every");
    spec["every_minutes"] = json!(10080);
    ok(
        &pod,
        "apply_change",
        json!({"collection": "options", "changes": [{"op": "set_filler", "filler": spec}]}),
    )
    .await;
    // The fill set_filler starts has nothing to research; wait it out.
    while matches!(
        fill::fill(&pod, "options", &filler, false).await,
        Outcome::Busy
    ) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let lamp = create(&pod, "items", json!({"name": "Lamp"})).await;
    *agent.answers.lock().unwrap() = vec![
        ("Lamp".into(), vec![option("lamps", 40)]),
        ("Rug".into(), vec![option("rugs", 90)]),
    ];
    fill::fill(&pod, "options", &filler, false).await;
    assert_eq!(agent.asked.lock().unwrap().len(), 1);
    // A new row is researched; the lamp, researched just now, isn't.
    create(&pod, "items", json!({"name": "Rug"})).await;
    fill::fill(&pod, "options", &filler, false).await;
    {
        let asked = agent.asked.lock().unwrap();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].0.contains("Rug"));
    }
    // A changed row is researched at once.
    update(&pod, "items", json!({"id": lamp, "notes": "Brass"})).await;
    fill::fill(&pod, "options", &filler, false).await;
    assert_eq!(agent.asked.lock().unwrap().len(), 3);
    // A refresh asks for every row again.
    fill::fill(&pod, "options", &filler, true).await;
    assert_eq!(agent.asked.lock().unwrap().len(), 5);
}

/// A fill that runs past its time still spent the model calls it made:
/// they count toward the filler's `fills_per_day` (and the pod's), so a
/// research that never ends can't be started again without limit.
#[tokio::test(start_paused = true)]
async fn a_fill_that_times_out_counts_its_calls() {
    let agent = Arc::new(Researcher::default());
    let pod = pod(agent.clone());
    let filler = define(&pod, 2).await;
    *agent.answers.lock().unwrap() = vec![("Lamp".into(), vec![option("lamps", 40)])];
    *agent.hang.lock().unwrap() = Some("Rug".into());
    create(&pod, "items", json!({"name": "Lamp"})).await;
    create(&pod, "items", json!({"name": "Rug"})).await;
    // The research the new rows start, or this one: it times out on the rug
    // (the paused clock runs on while nothing else can).
    loop {
        match fill::fill(&pod, "options", &filler, false).await {
            Outcome::Busy => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            _ if agent.asked.lock().unwrap().len() >= 2 => break,
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    // Both calls counted: the next fill researches nothing more today.
    let out = loop {
        match fill::fill(&pod, "options", &filler, false).await {
            Outcome::Busy => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            out => break out,
        }
    };
    let Outcome::Failed(why) = &out else {
        panic!("{out:?}")
    };
    assert!(why.contains("already researched 2 rows today"), "{why}");
    assert_eq!(agent.asked.lock().unwrap().len(), 2, "no row asked again");
}
