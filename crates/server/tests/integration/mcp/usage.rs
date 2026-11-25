//! Usage trails (decision `usage-trails`), through the real host: the pod
//! keeps nothing until the owner plugs the `usage` jack; then the owner's
//! own devices' events are kept and read back as totals; an MCP credential
//! can't post usage.

use serde_json::{Value, json};

use super::{client, env};

async fn post(env: &super::Env, bearer: &str, events: Value) -> reqwest::StatusCode {
    client()
        .post(format!("{}/v1/usage", env.url))
        .bearer_auth(bearer)
        .header(protocol::DEVICE_HEADER, "test-phone-1")
        .json(&json!({ "events": events }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn usage_is_kept_only_with_the_jack_and_only_from_the_owners_devices() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    let now = protocol::usage::now_secs();
    let events = json!([
        { "at": now - 60, "kind": "screen", "layout": "c-recipes", "from": "home" },
        { "at": now - 50, "kind": "tap", "layout": "c-recipes", "node": "0/1/i/2", "intent": "action.mutation.optimistic",
          "target": "collections/actions", "collection": "groceries", "effect": "sent" },
        { "at": now - 49, "kind": "result", "layout": "", "target": "collections/actions", "collection": "groceries", "effect": "ok" },
        { "at": now - 40, "kind": "back", "layout": "c-recipes", "dwell_ms": 1200 },
        { "at": now - 30, "kind": "nonsense", "layout": "x" }
    ]);

    // Off by default: accepted, nothing kept.
    assert_eq!(
        post(&env, &pod_code, events.clone()).await,
        reqwest::StatusCode::NO_CONTENT
    );
    let (err, out) = env
        .tool(&owner, "plug_jack", json!({ "id": "usage" }))
        .await;
    assert!(!err, "{out}");
    assert!(post(&env, &pod_code, events.clone()).await.is_success());
    // An MCP credential isn't a device of the owner's.
    assert!(!post(&env, &owner, events).await.is_success());

    let (err, r) = env
        .tool(&owner, "usage_report", json!({ "layout": "c-recipes" }))
        .await;
    assert!(!err, "{r}");
    assert_eq!(r["opens"], 1, "{r}");
    assert_eq!(r["backtracks"], 1, "{r}");
    assert_eq!(r["taps"][0]["taps"], 1, "{r}");
    let (err, j) = env
        .tool(
            &owner,
            "usage_report",
            json!({ "start_layout": "c-recipes", "done_collection": "groceries" }),
        )
        .await;
    assert!(!err, "{j}");
    assert_eq!(j["starts"], 1, "{j}");
    assert_eq!(j["completions"], 1, "{j}");

    // A read connection can't see it.
    let read = env.connect_with(&pod_code, "read").await;
    let (err, out) = env
        .tool(&read, "usage_report", json!({ "layout": "c-recipes" }))
        .await;
    assert!(err && out["error"] == "access_too_low", "{out}");
}
