//! A device watch runs its action with the scope of the credential that made
//! it (issue #435): Claude's MCP credential can't have the pod act for the
//! user upstream (post to Pixelfed, start an agent, plug a jack) by having a
//! watch do it later; it can still change a collection's rows.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{Env, client, env};

/// A request from the user's browser, drawing `features`: the pod records
/// the device and runs the watches it now meets.
async fn ping(env: &Env, bearer: &str, features: &str) {
    let resp = client()
        .get(format!("{}/v1/notifications/channels", env.url))
        .bearer_auth(bearer)
        .header(protocol::DEVICE_HEADER, "webwatchdevice0001")
        .header(protocol::BUILD_HEADER, "web 0.6.0+20261118153000.c41d9e2")
        .header(protocol::DEVICE_NAME_HEADER, "Test browser")
        .header(protocol::FEATURES_HEADER, features)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

fn watch(path: &str, body: Value) -> Value {
    json!({"kind": "web", "until": {"feature": "bars"},
        "then": {"action": {"path": path, "body": body}}})
}

#[tokio::test]
async fn an_mcp_credentials_watch_cant_act_for_the_user_upstream() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // Actions that act upstream or change jacks, sign-ins or agents: refused
    // when Claude asks for the watch.
    for path in [
        "/v1/pixelfed/posts/create",
        "/v1/agents/tasks/start",
        "/v1/agents/sign-in",
        "/v1/jacks/plug",
    ] {
        let (is_error, r) = env
            .tool(
                &access,
                "watch_device",
                watch(path, json!({"status": "hi"})),
            )
            .await;
        assert!(is_error, "{path}: {r}");
        assert_eq!(r["error"], "mcp_credential_scope", "{path}: {r}");
    }

    // A collection's row: what add_row already does, so the watch is made.
    let (is_error, r) = env
        .tool(
            &access,
            "watch_device",
            watch(
                "/v1/collections/dishes/create",
                json!({"title": "Watched Waffles"}),
            ),
        )
        .await;
    assert!(!is_error, "{r}");
    assert_eq!(r["watch"]["created_by"], "MCP credential", "{r}");
    let (_, listed) = env.tool(&access, "list_devices", json!({})).await;
    assert_eq!(listed["watches"][0]["created_by"], "MCP credential");

    // The pod token's own watch may run any source action.
    let (is_error, r) = env
        .tool(
            &pod_code,
            "watch_device",
            watch("/v1/pixelfed/posts/create", json!({"status": "hi"})),
        )
        .await;
    assert!(!is_error, "{r}");
    assert_eq!(r["watch"]["created_by"], "pod token", "{r}");

    // The browser updates: the MCP credential's watch adds its row.
    ping(&env, &pod_code, "bars,list,text").await;
    let mut titles = Vec::new();
    for _ in 0..300 {
        let (_, export) = env
            .tool(
                &access,
                "export_collection",
                json!({"collection": "dishes"}),
            )
            .await;
        titles = export["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["values"]["title"].as_str().map(str::to_owned))
            .collect();
        if titles.iter().any(|t| t == "Watched Waffles") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(titles.iter().any(|t| t == "Watched Waffles"), "{titles:?}");
}
