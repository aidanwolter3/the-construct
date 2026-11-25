//! The owner's taste history lives in the pod; the service only caches it
//! (decision `taste-history-in-the-pod`).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use pod_core::Pod;
use serde_json::{Value, json};

use crate::fake::{Fake, TOKEN, call, pod, serve, taste_layout, workspace};

/// A service that caches histories, holding `own` as its own, and a pod
/// signed in to it.
async fn service_with(own: Vec<Value>) -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    fake.keeps_states.store(true, Ordering::SeqCst);
    *fake.own.lock().unwrap() = own;
    let url = serve(fake.clone()).await;
    (fake, url)
}

async fn sign_in(pod: &Pod, url: &str) {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/taste/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn react(pod: &Pod, id: &str, liked: bool) -> (StatusCode, Value) {
    let body = if liked {
        json!({"id": id, "liked": true})
    } else {
        json!({"id": id, "skipped": true})
    };
    call(pod, "POST", "/v1/taste/react", Some("phone"), Some(body)).await
}

fn earlier() -> Vec<Value> {
    vec![
        json!({"id": "shop9.example:1", "kind": "like", "t": 1.0}),
        json!({"id": "shop9.example:2", "kind": "skip", "t": 2.0}),
    ]
}

fn last_tag(fake: &Fake) -> String {
    fake.tags
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn the_pod_takes_over_the_services_history_and_names_it() {
    let (fake, url) = service_with(earlier()).await;
    let pod = pod();
    sign_in(&pod, &url).await;
    let app = taste_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["queries"]["taste:feed"].as_array().unwrap().len(),
        6,
        "{ws}"
    );
    // The service didn't hold the pod's history under its name: the pod sent
    // it, the two reactions it took over and what had been served.
    let uploads = fake.uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["events"], json!(earlier()));
    assert_eq!(uploads[0]["served"]["shop0.example:1003"], 2);
    let tag = last_tag(&fake);
    assert!(tag.ends_with("/2"), "{tag}");
    // A reaction is the pod's first and names the version it makes.
    let (status, body) = react(&pod, "shop0.example:1001", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = tag.split('/').next().unwrap().to_owned();
    assert_eq!(last_tag(&fake), format!("{log}/3"));
    assert_eq!(fake.states.lock().unwrap()[&log].len(), 3);
    assert_eq!(fake.uploads.lock().unwrap().len(), 1, "no second upload");
}

#[tokio::test]
async fn a_service_that_lost_the_history_is_sent_it_again() {
    let (fake, url) = service_with(earlier()).await;
    let pod = pod();
    sign_in(&pod, &url).await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    react(&pod, "shop0.example:1001", false).await;
    fake.forget();
    // The next request is told the service doesn't hold it, sends it whole
    // and goes through.
    let (status, body) = react(&pod, "shop0.example:1002", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let uploads = fake.uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 2);
    let events = uploads[1]["events"].as_array().unwrap();
    assert_eq!(events.len(), 4, "the reaction being sent is already in it");
    assert_eq!(events[2]["id"], "shop0.example:1001");
    assert_eq!(events[3]["kind"], "like");
    // The feed's pages count as served.
    assert_eq!(uploads[1]["served"]["shop0.example:1000"], 1);
}

#[tokio::test]
async fn signing_out_keeps_the_history_for_the_next_service() {
    let (first, url) = service_with(earlier()).await;
    let pod = pod();
    sign_in(&pod, &url).await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    react(&pod, "shop0.example:1001", true).await;
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/taste/sign-out",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // A new service with a history of its own: the pod's is the one it gets.
    let (second, url) = service_with(vec![
        json!({"id": "shop8.example:5", "kind": "like", "t": 9.0}),
    ])
    .await;
    sign_in(&pod, &url).await;
    workspace(&pod, &app).await;
    let uploads = second.uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 1);
    let ids: Vec<&str> = uploads[0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["shop9.example:1", "shop9.example:2", "shop0.example:1001"]
    );
    assert_eq!(first.uploads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_reaction_the_service_refuses_leaves_the_history() {
    let (fake, url) = service_with(earlier()).await;
    let pod = pod();
    sign_in(&pod, &url).await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    fake.down.store(true, Ordering::SeqCst);
    let (status, _) = react(&pod, "shop0.example:1001", true).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    fake.down.store(false, Ordering::SeqCst);
    let (status, body) = react(&pod, "shop0.example:1002", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = last_tag(&fake).split('/').next().unwrap().to_owned();
    assert_eq!(
        last_tag(&fake),
        format!("{log}/3"),
        "the refused one was taken back out"
    );
    assert_eq!(fake.uploads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_service_from_before_keeps_its_own_history() {
    // No /api/state: the pod names no history and keeps working.
    let fake = Arc::new(Fake::default());
    let url = serve(fake.clone()).await;
    let pod = pod();
    sign_in(&pod, &url).await;
    let app = taste_layout(&pod).await;
    workspace(&pod, &app).await;
    let (status, body) = react(&pod, "shop0.example:1001", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(fake.tags.lock().unwrap().is_empty());
    assert_eq!(fake.events.lock().unwrap().len(), 1);
}
