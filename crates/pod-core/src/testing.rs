//! Helpers for tests of the pod and its sources: a layout as a device of the
//! construct draws it, read through the pod's own HTTP router (decision
//! `content-addressed-construct`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

async fn call(
    router: &Router,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .expect("a request");
    let resp = router.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn query_value(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The objects `want` reaches, fetched (`POST /v1/objects`).
pub async fn objects(
    router: &Router,
    headers: &HeaderMap,
    want: &[&str],
) -> Result<BTreeMap<String, Arc<Value>>, (StatusCode, Value)> {
    let (s, b) = call(
        router,
        Method::POST,
        "/v1/objects",
        headers,
        Some(json!({ "have": [], "want": want })),
    )
    .await;
    if !s.is_success() {
        return Err((s, b));
    }
    Ok(b["objects"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
        .collect())
}

/// Layout `id` (a layout's id or a blueprint's key) as a device draws it:
/// `{version, workspace, entities, queries}`. Its element by name
/// (`/v1/construct/names/<id>`), the objects it reaches, materialized with
/// what home binds it with, and its data (`/v1/construct/data/<element>`).
/// A refusal on the way is answered as it is.
pub async fn layout(router: &Router, id: &str, headers: &HeaderMap) -> (StatusCode, Value) {
    let (s, b) = call(
        router,
        Method::GET,
        &format!("/v1/construct/names/{id}"),
        headers,
        None,
    )
    .await;
    if !s.is_success() {
        return (s, b);
    }
    let Some(el) = b["el"].as_str().map(str::to_owned) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, b);
    };
    let mut objects = match objects(router, headers, &[&el]).await {
        Ok(o) => o,
        Err(e) => return e,
    };
    // A layout that reads inputs draws what home's use site binds.
    let mut inputs = Map::new();
    let reads = objects
        .get(&el)
        .is_some_and(|v| !construct_elements::template::inputs_of(v).is_empty());
    if reads {
        let (s, root) = call(router, Method::GET, "/v1/construct", headers, None).await;
        if s.is_success()
            && let Some(root) = root["root"].as_str()
            && let Ok(more) = self::objects(router, headers, &[root]).await
        {
            objects.extend(more);
            if let Some(k) = construct_elements::keyed(&objects, crate::HOME_APP, root).remove(id) {
                inputs = k.inputs;
            }
        }
    }
    let m = match construct_elements::materialize_with(&objects, &el, &inputs) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "materialize", "detail": e.to_string() }),
            );
        }
    };
    let mut path = format!("/v1/construct/data/{el}");
    if !inputs.is_empty() {
        path.push_str("?bind=");
        path.push_str(&query_value(&Value::Object(inputs).to_string()));
    }
    let (s, data) = call(router, Method::GET, &path, headers, None).await;
    if !s.is_success() {
        return (s, data);
    }
    (
        StatusCode::OK,
        json!({
            "version": crate::BUNDLE_VERSION,
            "workspace": m.node,
            "entities": data["entities"],
            "queries": data["queries"],
        }),
    )
}

/// [`layout`] with only a bearer and this build's features.
pub async fn layout_for(router: &Router, id: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut h = HeaderMap::new();
    if let Some(b) = bearer {
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {b}").parse().expect("a header value"),
        );
    }
    h.insert(
        protocol::FEATURES_HEADER,
        protocol::features_header().parse().expect("a header value"),
    );
    layout(router, id, &h).await
}

/// Make home bind `feeds` (a test pod's home, made with a blank page when
/// the pod has none), without the checks of a layout write: what a test of
/// a source's data and events needs. Blocking.
pub fn bind_home(pod: &crate::Pod, feeds: &[(&str, &str)]) {
    use crate::construct::{store as cstore, write};
    let feeds: Vec<(String, String)> = feeds
        .iter()
        .map(|(s, f)| ((*s).to_owned(), (*f).to_owned()))
        .collect();
    let moved = pod
        .store()
        .transaction(|tx| {
            let current = cstore::name(tx, crate::HOME_APP)?
                .and_then(|h| cstore::object(tx, &h).ok().flatten())
                .map(|(v, _)| (*v).clone());
            let mut el = current.unwrap_or_else(|| {
                json!({ "kind": "element", "title": "Home", "icon": "dashboard",
                    "node": { "type": "core://Page", "id": crate::HOME_APP, "title": "Home",
                              "root": { "type": "core://Spacer" } } })
            });
            el["bind"] = json!({ "feeds": feeds });
            if let Some(o) = el.as_object_mut() {
                o.remove("needs");
            }
            Ok(write::put(
                tx,
                pod.catalog(),
                crate::HOME_APP,
                el,
                true,
                crate::layouts::AUTHOR_POD,
                "Test: home binds feeds",
            ))
        })
        .expect("the store")
        .expect("home's element");
    write::publish(pod, &moved);
}

/// A new layout of the pod that is the whole of blueprint `blueprint`
/// (`{"type": "core://Use", "ref": "@core/<blueprint>"}`), with the
/// blueprint's feeds added to home's ([`bind_home`]; a pod without a home
/// gets a blank one): what draws a blueprint with its data on the
/// construct. Its id.
pub async fn use_blueprint(pod: &crate::Pod, blueprint: &str) -> String {
    let info = *pod
        .catalog()
        .blueprint(blueprint)
        .unwrap_or_else(|| panic!("no blueprint {blueprint}"));
    let mut feeds: Vec<(&str, &str)> = Vec::new();
    let home = pod.store().with_conn(crate::layouts::home_feeds);
    for (s, f) in &home {
        feeds.push((s, f));
    }
    for (s, f) in info.feeds {
        if !feeds.contains(&(*s, *f)) {
            feeds.push((s, f));
        }
    }
    bind_home(pod, &feeds);
    let made = crate::construct::write::mcp(
        pod,
        "create_layout",
        &json!({ "title": info.title,
                 "layout": { "type": crate::components::USE_TYPE,
                             "ref": crate::construct::blueprint_name(blueprint) } }),
    )
    .await
    .expect("create_layout is a tool")
    .unwrap_or_else(|e| panic!("{blueprint} as a layout: {e}"));
    made["id"].as_str().expect("an id").to_owned()
}
