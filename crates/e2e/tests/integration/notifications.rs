//! Notifications end to end (D52), against the fake only: sign in through the engine,
//! register a device, let the pod poll (its quiet first poll), inject a like on the fake
//! through `POST /_admin/notifications`, poll again and read it from
//! `GET /v1/notifications`. Polls are driven directly (`AppState::poll_due`), not by the
//! host's scheduler, so nothing waits on a timer.

use std::sync::Arc;

use client_core::{MemoryStore, SecureStore};
use e2e::{Harness, TIMEOUT, wait_signed_in, wait_until};
use serde_json::{Value, json};

const CAROUSEL: &str = "781254316723458049";
const DEVICE: &str = "e2e-device-0001";

#[test]
fn a_like_on_the_upstream_becomes_an_unread_notification() {
    let h = Harness::from_env();
    let up = &h.upstream;
    if up.is_real() {
        eprintln!("[e2e] notifications: skipped (needs the fake's admin routes)");
        return;
    }
    up.reset();

    let store = Arc::new(MemoryStore::new());
    let engine = h.engine(&store);
    wait_until("the signed-out bundle", TIMEOUT, || {
        engine
            .snapshot()
            .workspace_json
            .contains("Sign in with Pixelfed")
    });
    engine.dispatch(&h.login_intent());
    wait_signed_in(&engine);
    let raw = store
        .get(&client_core::auth::session_key(&h.pod_url))
        .expect("a stored pod session");
    let token = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v["token"].as_str().map(str::to_owned))
        .unwrap_or(raw);

    let http = reqwest::blocking::Client::new();
    let pod = |method: reqwest::Method, path: &str, body: Option<Value>| -> Value {
        let mut req = http
            .request(method, format!("{}{path}", h.pod_url))
            .bearer_auth(&token)
            .header("x-construct-device", DEVICE);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().unwrap();
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        resp.json().unwrap_or(Value::Null)
    };
    pod(
        reqwest::Method::PUT,
        &format!("/v1/devices/{DEVICE}"),
        Some(json!({"platform": "e2e"})),
    );

    // The first poll: the fixture backlog as read history.
    assert!(h.block_on(h.pod_state.poll_due()).is_some());
    let list = pod(reqwest::Method::GET, "/v1/notifications", None);
    assert!(
        !list["notifications"].as_array().unwrap().is_empty(),
        "{list}"
    );
    assert_eq!(list["unread_total"], 0, "{list}");

    // A like on the upstream; the next poll is made due now.
    let injected: Value = http
        .post(format!("{}/_admin/notifications", up.url()))
        .json(&json!({"type": "favourite", "from": "alice", "status_id": CAROUSEL}))
        .send()
        .and_then(|r| r.error_for_status())
        .expect("POST /_admin/notifications (is FAKE_PIXELFED_ADMIN on?)")
        .json()
        .unwrap();
    assert_eq!(injected["type"], "favourite");
    h.pod_state
        .store()
        .with_conn(|c| c.execute("UPDATE notification_polls SET next_at = 0", []))
        .unwrap();
    h.block_on(h.pod_state.poll_due());

    let list = pod(reqwest::Method::GET, "/v1/notifications", None);
    assert_eq!(list["unread_total"], 1, "{list}");
    let n = &list["notifications"][0];
    assert_eq!(n["app_id"], "pixelfed");
    assert_eq!(n["channel"], "likes");
    assert_eq!(n["title"], "Alice Lumen liked your post");
    assert_eq!(n["target_uri"], format!("pixelfed://posts/{CAROUSEL}"));
    assert_eq!(n["read"], false);
    let image = n["image_url"].as_str().unwrap();
    assert!(image.starts_with("/v1/pixelfed/media/"), "{image}");
    // The avatar loads through the pod (no bearer needed).
    let avatar = http.get(format!("{}{image}", h.pod_url)).send().unwrap();
    assert!(avatar.status().is_success(), "{}", avatar.status());
}
