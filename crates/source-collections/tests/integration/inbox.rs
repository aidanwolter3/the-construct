//! The pod's notifications as collections (decision
//! `notifications-in-a-collection`): rows any layout can bind, pod-wide
//! channel switches that hide what they switch off, `read` and `read_at`,
//! the built-in actions, notifications that go with their subject, the
//! retention, and the hand-over from the core's own tables. The data is
//! made up.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::{
    AppState, AuthPolicy, BlueprintInfo, BoxFuture, FeedSpec, Hydration, NewNotification, Pod,
    RequestCtx, Source, SourceManifest,
};
use protocol::{DataBundle, Importance, NotificationChannel};
use serde_json::{Value, json};
use source_collections::{Caps, CollectionsAdapter};
use tower::ServiceExt;

static FILES: &[LayoutSource] = &[LayoutSource {
    dir: "layouts",
    app_id: "bell",
    name: "0001",
    text: r#"{"note": "First", "schema": 1, "layout": {"type": "core://Workspace", "id": "bell",
        "title": "Bell", "slots": [{"slot_id": "main", "provider_uri": "core://test",
        "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#,
}];

/// A source with two channels: `likes` (on by default) and `other` (off).
struct Bell;

impl Source for Bell {
    fn name(&self) -> &'static str {
        "bell"
    }
    fn blueprints(&self) -> Vec<BlueprintInfo> {
        vec![BlueprintInfo {
            id: "bell",
            title: "Bell",
            icon: "photo",
            dev: false,
            auth: AuthPolicy::Public,
            feeds: &[("bell", "all")],
        }]
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }
    fn manifest(&self) -> SourceManifest {
        SourceManifest {
            feeds: &[FeedSpec {
                name: "all",
                query_keys: &[],
                doc: "",
            }],
            ..SourceManifest::new("Bell")
        }
    }
    fn feed<'a>(&'a self, _: &'a Pod, _: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async { Hydration::Data(DataBundle::default()) })
    }
    fn notification_channels(&self) -> Vec<NotificationChannel> {
        [("likes", true), ("other", false)]
            .into_iter()
            .map(|(id, on)| NotificationChannel {
                layout: "bell".into(),
                id: id.into(),
                title: format!("Bell {id}"),
                description: String::new(),
                default_enabled: on,
                importance: Importance::Default,
            })
            .collect()
    }
}

fn adapters() -> Vec<Arc<dyn Source>> {
    vec![
        Arc::new(Bell),
        Arc::new(CollectionsAdapter::with_caps(Caps::default())),
    ]
}

fn sessions(pod: &Pod) {
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
}

fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(adapters()));
    sessions(&pod);
    pod
}

fn note(key: &str, channel: &str, subject: Option<&str>) -> NewNotification {
    NewNotification {
        layout: "bell".into(),
        channel: channel.into(),
        dedupe_key: key.into(),
        title: format!("Title {key}"),
        body: "Body".into(),
        image_url: None,
        target_uri: Some(format!("bell://items/{key}")),
        group_key: None,
        created_at: 1_700_000_000,
        subject: subject.map(str::to_owned),
        quiet: false,
    }
}

async fn call(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let bearer = if path == "/v1/mcp" { "pod" } else { "phone" };
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header(protocol::DEVICE_HEADER, "device-0001")
        .header("authorization", format!("Bearer {bearer}"));
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = pod_core::app(pod.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ok(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, v) = call(pod, method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {v}");
    v
}

async fn tool(pod: &Pod, name: &str, args: Value) -> Value {
    let v = ok(
        pod,
        "POST",
        "/v1/mcp",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": name, "arguments": args}})),
    )
    .await;
    let r = &v["result"];
    assert!(r["isError"] != true, "{name}: {r}");
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// The live rows of `collection` as entities, in its order.
async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let b = ok(
        pod,
        "GET",
        &format!("/v1/collections/{collection}/rows"),
        None,
    )
    .await;
    b["queries"][format!("collection:{collection}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

async fn register(pod: &Pod) {
    ok(
        pod,
        "PUT",
        "/v1/devices/device-0001",
        Some(json!({"platform": "android"})),
    )
    .await;
}

#[tokio::test]
async fn notifications_are_rows_and_switches_hide_them() {
    let pod = pod();
    let stored = pod
        .notify(vec![note("a", "likes", None), note("b", "other", None)])
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    // Every notification is a row, the switched-off one too.
    let all = rows(&pod, "notifications").await;
    assert_eq!(all.len(), 2);
    let by_title = |t: &str| all.iter().find(|r| r["title"] == t).unwrap().clone();
    assert_eq!(by_title("Title a")["channel_enabled"], true);
    assert_eq!(by_title("Title a")["channel_title"], "Bell likes");
    assert_eq!(by_title("Title a")["layout"], "bell");
    assert_eq!(by_title("Title b")["channel_enabled"], false);
    assert_eq!(by_title("Title a")["id"], json!(stored[0].id));
    // The channels are rows too, with their switches.
    let channels = rows(&pod, "notification_channels").await;
    let keys: Vec<(&str, bool)> = channels
        .iter()
        .map(|c| (c["key"].as_str().unwrap(), c["enabled"].as_bool().unwrap()))
        .collect();
    assert_eq!(keys, [("bell.likes", true), ("bell.other", false)]);
    // The old API leaves out what is switched off, counts included.
    let list = ok(&pod, "GET", "/v1/notifications", None).await;
    assert_eq!(list["unread_total"], 1);
    assert_eq!(list["notifications"].as_array().unwrap().len(), 1);

    // Switching a channel on (for the whole pod) shows its notifications.
    register(&pod).await;
    ok(
        &pod,
        "PUT",
        "/v1/devices/device-0001/channels",
        Some(json!({"bell.other": true})),
    )
    .await;
    let all = rows(&pod, "notifications").await;
    assert!(all.iter().all(|r| r["channel_enabled"] == true), "{all:?}");
    let list = ok(&pod, "GET", "/v1/notifications", None).await;
    assert_eq!(list["unread_total"], 2);
    // So does editing its row, as any layout or Claude can.
    let likes = channels.iter().find(|c| c["key"] == "bell.likes").unwrap();
    ok(
        &pod,
        "POST",
        "/v1/collections/notification_channels/update",
        Some(json!({"id": likes["id"], "enabled": false})),
    )
    .await;
    let chans = ok(&pod, "GET", "/v1/notifications/channels", None).await;
    let likes = chans["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "likes")
        .unwrap()
        .clone();
    assert_eq!(likes["enabled"], false);
    let list = ok(&pod, "GET", "/v1/notifications", None).await;
    assert_eq!(list["unread_total"], 1);
    // Reported again, nothing new.
    assert!(
        pod.notify(vec![note("a", "likes", None)])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn read_is_a_field_and_the_actions_mark_and_clear() {
    let pod = pod();
    pod.notify(vec![
        note("a", "likes", None),
        note("b", "likes", None),
        note("c", "likes", None),
    ])
    .await
    .unwrap();
    let first = rows(&pod, "notifications").await[0].clone();
    assert_eq!(first["read"], false);
    assert!(
        first["read_at"].is_null() || first["read_at"] == "",
        "{first}"
    );
    // A row update marks one read; `read_at` follows.
    ok(
        &pod,
        "POST",
        "/v1/collections/notifications/update",
        Some(json!({"id": first["id"], "read": true})),
    )
    .await;
    let row = rows(&pod, "notifications")
        .await
        .into_iter()
        .find(|r| r["id"] == first["id"])
        .unwrap();
    assert_eq!(row["read"], true);
    assert!(
        row["read_at"].as_str().is_some_and(|t| !t.is_empty()),
        "{row}"
    );
    assert_eq!(
        ok(&pod, "GET", "/v1/notifications", None).await["unread_total"],
        2
    );
    // The old API's mark-read writes the same field.
    let list = ok(&pod, "GET", "/v1/notifications", None).await;
    let unread: Vec<Value> = list["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["read"] == false)
        .map(|n| n["id"].clone())
        .collect();
    let counts = ok(
        &pod,
        "POST",
        "/v1/notifications/read",
        Some(json!({"ids": [unread[0]]})),
    )
    .await;
    assert_eq!(counts["unread_total"], 1);
    // "Mark all read" is the collection's own update action.
    let v = ok(
        &pod,
        "POST",
        "/v1/collections/notifications/actions/mark_all_read",
        Some(json!({})),
    )
    .await;
    assert_eq!(v["updated"], 1, "{v}");
    assert_eq!(
        ok(&pod, "GET", "/v1/notifications", None).await["unread_total"],
        0
    );
    // "Clear read" deletes them.
    let v = ok(
        &pod,
        "POST",
        "/v1/collections/notifications/actions/clear_read",
        Some(json!({})),
    )
    .await;
    assert_eq!(v["deleted"], 3, "{v}");
    assert!(rows(&pod, "notifications").await.is_empty());
    // A deleted one doesn't come back when reported again.
    assert!(
        pod.notify(vec![note("a", "likes", None)])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_notification_goes_with_its_subject() {
    let pod = pod();
    tool(
        &pod,
        "define_collection",
        json!({"name": "shows", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    let mut ids = Vec::new();
    for t in ["Brass Lanterns", "Friday Improv"] {
        let v = ok(
            &pod,
            "POST",
            "/v1/collections/shows/create",
            Some(json!({"title": t})),
        )
        .await;
        ids.push(v["id"].as_str().unwrap().to_owned());
    }
    let uri = |id: &str| format!("collections://shows/{id}");
    pod.notify(vec![
        note("a", "likes", Some(&uri(&ids[0]))),
        note("b", "likes", Some(&uri(&ids[1]))),
        note("c", "likes", None),
    ])
    .await
    .unwrap();
    // Deleting the row takes its notification with it.
    ok(
        &pod,
        "POST",
        "/v1/collections/shows/delete",
        Some(json!({"id": ids[0]})),
    )
    .await;
    let left: Vec<String> = rows(&pod, "notifications")
        .await
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(!left.contains(&"Title a".to_owned()), "{left:?}");

    // A channel can keep them instead.
    let likes = rows(&pod, "notification_channels").await[0].clone();
    assert_eq!(likes["key"], "bell.likes");
    ok(
        &pod,
        "POST",
        "/v1/collections/notification_channels/update",
        Some(json!({"id": likes["id"], "remove_with_subject": false})),
    )
    .await;
    ok(
        &pod,
        "POST",
        "/v1/collections/shows/delete",
        Some(json!({"id": ids[1]})),
    )
    .await;
    assert_eq!(rows(&pod, "notifications").await.len(), 2);

    // Other sources say what is gone through the core.
    ok(
        &pod,
        "POST",
        "/v1/collections/notification_channels/update",
        Some(json!({"id": likes["id"], "remove_with_subject": true})),
    )
    .await;
    pod.notify(vec![note("d", "likes", Some("bell://items/d"))])
        .await
        .unwrap();
    let gone = pod
        .notifications_gone(vec!["bell://items/d".into()])
        .await
        .unwrap();
    assert_eq!(gone, 1);
    assert_eq!(rows(&pod, "notifications").await.len(), 2);
}

#[tokio::test]
async fn the_newest_are_kept() {
    let pod = pod();
    let items: Vec<NewNotification> = (0..505)
        .map(|i| note(&format!("k{i:03}"), "likes", None))
        .collect();
    pod.notify(items).await.unwrap();
    assert_eq!(rows(&pod, "notifications").await.len(), 500);
    // Notifications don't count against the pod's rows.
    let list = tool(&pod, "list_collections", json!({})).await;
    assert!(list.to_string().contains("notifications"), "{list}");
}

/// A pod from before: the core's tables held its notifications and
/// switches; the collections take them over when it opens.
#[tokio::test]
async fn the_core_tables_are_handed_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.db");
    {
        // Without Collections, the core keeps them.
        let pod: Pod = Arc::new(AppState::new(
            pod_core::store::Store::open(&path).unwrap(),
            vec![Arc::new(Bell)],
        ));
        sessions(&pod);
        let mut quiet = note("old", "likes", None);
        quiet.quiet = true;
        pod.notify(vec![
            quiet,
            note("new", "likes", None),
            note("off", "other", None),
        ])
        .await
        .unwrap();
        register(&pod).await;
        ok(
            &pod,
            "PUT",
            "/v1/devices/device-0001/channels",
            Some(json!({"bell.likes": false})),
        )
        .await;
        let (_, list) = call(&pod, "GET", "/v1/notifications", None).await;
        assert_eq!(list["notifications"].as_array().unwrap().len(), 0);
    }
    let pod: Pod = Arc::new(AppState::new(
        pod_core::store::Store::open(&path).unwrap(),
        adapters(),
    ));
    let all = rows(&pod, "notifications").await;
    let mut got: Vec<(String, bool)> = all
        .iter()
        .map(|r| {
            (
                r["title"].as_str().unwrap().to_owned(),
                r["read"].as_bool().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("Title new".to_owned(), false),
            ("Title off".to_owned(), false),
            ("Title old".to_owned(), true)
        ]
    );
    // As a device does when it connects.
    ok(&pod, "GET", "/v1/notifications/channels", None).await;
    let channels = rows(&pod, "notification_channels").await;
    let likes = channels.iter().find(|c| c["key"] == "bell.likes").unwrap();
    assert_eq!(likes["enabled"], false);
    assert_eq!(likes["title"], "Bell likes", "declared titles fill in");
    // The core's tables are empty now.
    let left: i64 = pod
        .store()
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(left, 0);
}

/// A pod layout titled `title` drawing a page with one screen per route.
async fn page(pod: &Pod, title: &str, routes: &[&str]) -> String {
    let screens: Vec<Value> = routes
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({"id": format!("s{i}"), "route": r,
                "root": {"type": "core://Text", "style": "Body", "value_expr": "$route.id"}})
        })
        .collect();
    let made = tool(
        pod,
        "create_layout",
        json!({"title": title, "layout": {"type": "core://Page", "id": "p", "title": title,
            "root": {"type": "core://Text", "style": "Body", "value_expr": title},
            "screens": screens}}),
    )
    .await;
    made["id"].as_str().unwrap().to_owned()
}

fn aimed(key: &str, target: Option<&str>) -> NewNotification {
    NewNotification {
        target_uri: target.map(str::to_owned),
        ..note(key, "likes", None)
    }
}

/// Issue #337: a notification of a blueprint's channel opens the pod's own
/// layout that routes its target (the blueprint's copy, or another layout),
/// else the copy; rows stored without one get it when the pod opens.
#[tokio::test]
async fn notifications_open_the_pods_layout_that_routes_them() {
    let pod = pod();
    // A copy of the Bell blueprint (same title, no reference to it) and an
    // unrelated layout that routes the tasks.
    let copy = page(&pod, "Bell", &["bell://items/{id}"]).await;
    let tasks = page(&pod, "Tasks", &["bell://tasks/{id}"]).await;
    pod.notify(vec![
        aimed("item", Some("bell://items/i1")),
        aimed("task", Some("bell://tasks/t1")),
        aimed("none", None),
        aimed("nowhere", Some("bell://elsewhere/e1")),
    ])
    .await
    .unwrap();
    let all = rows(&pod, "notifications").await;
    let layout_of = |t: &str| {
        all.iter()
            .find(|r| r["title"] == format!("Title {t}"))
            .unwrap_or_else(|| panic!("{t} in {all:?}"))["layout"]
            .clone()
    };
    assert_eq!(layout_of("item"), json!(copy), "the copy routes it");
    assert_eq!(layout_of("task"), json!(tasks), "another layout routes it");
    assert_eq!(layout_of("none"), json!(copy), "no target: the copy");
    assert_eq!(layout_of("nowhere"), json!(copy), "no route: the copy");
    let listed = ok(&pod, "GET", "/v1/notifications", None).await;
    assert!(
        listed["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["layout"] != json!("bell")),
        "{listed}"
    );

    // A row stored before (no layout, or the blueprint's id) is filled in
    // when the pod opens; one already naming a pod layout stays.
    let id = |t: &str| {
        all.iter()
            .find(|r| r["title"] == format!("Title {t}"))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (t, layout) in [("task", Value::Null), ("item", json!("bell"))] {
        ok(
            &pod,
            "POST",
            "/v1/collections/notifications/update",
            Some(json!({"id": id(t), "layout": layout})),
        )
        .await;
    }
    ok(
        &pod,
        "POST",
        "/v1/collections/notifications/update",
        Some(json!({"id": id("none"), "layout": tasks})),
    )
    .await;
    assert_eq!(pod.fill_notification_layouts().unwrap(), 2);
    let all = rows(&pod, "notifications").await;
    let layout_of = |t: &str| {
        all.iter()
            .find(|r| r["title"] == format!("Title {t}"))
            .unwrap()["layout"]
            .clone()
    };
    assert_eq!(layout_of("task"), json!(tasks));
    assert_eq!(layout_of("item"), json!(copy));
    assert_eq!(layout_of("none"), json!(tasks), "a pod layout stays");
    assert_eq!(pod.fill_notification_layouts().unwrap(), 0, "nothing left");
}
