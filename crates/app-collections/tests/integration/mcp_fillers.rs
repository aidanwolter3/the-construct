//! MCP servers and `mcp` fillers (decision `mcp-jack`) against a local fake
//! MCP server behind OAuth: `mcp_connect` finds its authorization server,
//! registers and answers a sign-in URL (PKCE); the browser's trip back to
//! the pod's public callback connects it; tools are listed and called (an
//! event-stream answer); an `mcp` filler fills a collection with dated
//! arguments; an expired access token refreshes (rotating the refresh
//! token); a refused refresh asks for a new sign-in; a token server
//! connects with its token; and the `mcp` jack switches it all off.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use super::fillers::{ok, rows, settle, tool};

/// The fake server's state.
#[derive(Default)]
struct Fake {
    base: Mutex<String>,
    /// The access token `/mcp` takes now.
    access: Mutex<String>,
    /// The refresh token `/token` takes now (empty: none).
    refresh: Mutex<String>,
    /// The PKCE challenge and redirect URI of the last authorize.
    pending: Mutex<Option<(String, String)>>,
    registered: Mutex<usize>,
    refreshes: Mutex<usize>,
    /// Each tools/call's arguments.
    calls: Mutex<Vec<Value>>,
}

fn form(body: &Bytes) -> Map<String, Value> {
    let u = reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(body))).unwrap();
    u.query_pairs()
        .map(|(k, v)| (k.into_owned(), json!(v.into_owned())))
        .collect()
}

async fn mcp(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let want = format!("Bearer {}", f.access.lock().unwrap());
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if auth != want {
        let base = f.base.lock().unwrap().clone();
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                format!(
                    "Bearer error=\"invalid_token\", resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
                ),
            )],
        )
            .into_response();
    }
    let msg: Value = serde_json::from_slice(&body).unwrap();
    let method = msg["method"].as_str().unwrap_or("");
    if method != "initialize" {
        assert_eq!(
            headers.get("mcp-session-id").and_then(|v| v.to_str().ok()),
            Some("s-1"),
            "{method} carries the session"
        );
    }
    let id = msg["id"].clone();
    let answer = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
    match method {
        "initialize" => (
            [("mcp-session-id", "s-1")],
            axum::Json(answer(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake-calendar", "version": "1" },
            }))),
        )
            .into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => axum::Json(answer(json!({ "tools": [{
            "name": "calendar_list_events",
            "description": "Events between two dates",
            "inputSchema": { "type": "object", "properties": {
                "from": { "type": "string" }, "to": { "type": "string" } } },
        }] })))
        .into_response(),
        "tools/call" => {
            f.calls
                .lock()
                .unwrap()
                .push(msg["params"]["arguments"].clone());
            let events = json!({ "count": 2, "events": [
                { "uid": "e1@cal.example", "title": "Dinner at Juniper", "start": "2026-11-18T18:00:00-07:00" },
                { "uid": "e2@cal.example", "title": "Hike with Robin", "start": "2026-11-22" },
            ] });
            // Answered as an event stream, after a progress notification.
            let stream = format!(
                "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {} }),
                answer(json!({ "content": [{ "type": "text", "text": events.to_string() }] })),
            );
            ([(header::CONTENT_TYPE, "text/event-stream")], stream).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn resource_meta(State(f): State<Arc<Fake>>) -> Response {
    let base = f.base.lock().unwrap().clone();
    axum::Json(json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] }))
        .into_response()
}

async fn auth_meta(State(f): State<Arc<Fake>>) -> Response {
    let base = f.base.lock().unwrap().clone();
    axum::Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
    .into_response()
}

async fn register(State(f): State<Arc<Fake>>, body: Bytes) -> Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["token_endpoint_auth_method"], "none");
    *f.registered.lock().unwrap() += 1;
    (
        StatusCode::CREATED,
        axum::Json(json!({ "client_id": "pod-client", "redirect_uris": req["redirect_uris"] })),
    )
        .into_response()
}

/// The user signs in at once: back to the redirect URI with a code.
async fn authorize(State(f): State<Arc<Fake>>, Query(q): Query<Map<String, Value>>) -> Response {
    let base = f.base.lock().unwrap().clone();
    let get = |k: &str| q[k].as_str().unwrap_or("").to_owned();
    assert_eq!(get("client_id"), "pod-client");
    assert_eq!(get("code_challenge_method"), "S256");
    assert_eq!(get("resource"), format!("{base}/mcp"));
    let redirect = get("redirect_uri");
    *f.pending.lock().unwrap() = Some((get("code_challenge"), redirect.clone()));
    let to = format!("{redirect}?code=code-1&state={}", get("state"));
    (StatusCode::FOUND, [(header::LOCATION, to)]).into_response()
}

async fn token(State(f): State<Arc<Fake>>, body: Bytes) -> Response {
    let q = form(&body);
    let get = |k: &str| q.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    assert_eq!(get("client_id"), "pod-client");
    let refused = || {
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "invalid_grant" })),
        )
            .into_response()
    };
    match get("grant_type").as_str() {
        "authorization_code" => {
            let Some((challenge, redirect)) = f.pending.lock().unwrap().take() else {
                return refused();
            };
            let made = URL_SAFE_NO_PAD.encode(Sha256::digest(get("code_verifier").as_bytes()));
            if get("code") != "code-1" || made != challenge || get("redirect_uri") != redirect {
                return refused();
            }
            *f.access.lock().unwrap() = "access-1".into();
            *f.refresh.lock().unwrap() = "refresh-1".into();
            axum::Json(
                json!({ "access_token": "access-1", "refresh_token": "refresh-1",
                "token_type": "Bearer", "expires_in": 3600 }),
            )
            .into_response()
        }
        "refresh_token" => {
            let current = f.refresh.lock().unwrap().clone();
            if current.is_empty() || get("refresh_token") != current {
                return refused();
            }
            let n = {
                let mut r = f.refreshes.lock().unwrap();
                *r += 1;
                *r + 1
            };
            let (a, r) = (format!("access-{n}"), format!("refresh-{n}"));
            *f.access.lock().unwrap() = a.clone();
            *f.refresh.lock().unwrap() = r.clone();
            axum::Json(json!({ "access_token": a, "refresh_token": r, "expires_in": 3600 }))
                .into_response()
        }
        _ => refused(),
    }
}

async fn serve(f: Arc<Fake>) -> String {
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_meta),
        )
        .route("/.well-known/oauth-authorization-server", get(auth_meta))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(f.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    *f.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn pod() -> Pod {
    let adapter = app_collections::CollectionsAdapter::new().with_private_hosts(true);
    let pod: Pod =
        Arc::new(AppState::in_memory(vec![Arc::new(adapter)]).with_mcp_private_hosts(true));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

/// A GET to the pod without a bearer: `(status, body text)`.
async fn public_get(pod: &Pod, path_and_query: &str) -> (StatusCode, String) {
    let resp = pod_core::app(pod.clone())
        .oneshot(
            Request::builder()
                .uri(path_and_query)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Open `sign_in_url` as a browser would and follow the authorization
/// server back to the pod's callback.
async fn sign_in(pod: &Pod, sign_in_url: &str) -> (StatusCode, String) {
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = browser.get(sign_in_url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FOUND);
    let to = reqwest::Url::parse(r.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(to.path(), pod_core::mcp_servers::CALLBACK_PATH);
    public_get(pod, &format!("{}?{}", to.path(), to.query().unwrap_or(""))).await
}

#[tokio::test]
async fn an_oauth_mcp_server_fills_a_collection_and_refreshes_its_token() {
    let fake = Arc::new(Fake::default());
    *fake.access.lock().unwrap() = "never-given".into();
    let base = serve(fake.clone()).await;
    let pod = pod();

    // Connecting meets a 401: the pod registers and answers a sign-in URL.
    let c = ok(
        &pod,
        "mcp_connect",
        json!({"name": "cal", "url": format!("{base}/mcp")}),
    )
    .await;
    assert_eq!(c["state"], "needs_sign_in", "{c}");
    let link = c["sign_in_url"].as_str().unwrap().to_owned();
    assert!(link.starts_with(&format!("{base}/authorize?")), "{link}");
    assert!(
        c["redirect_uri"]
            .as_str()
            .unwrap()
            .ends_with("/v1/mcp-servers/callback"),
        "{c}"
    );
    assert_eq!(*fake.registered.lock().unwrap(), 1);
    let listed = ok(&pod, "list_jacks", json!({})).await;
    let jack = listed["jacks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == "mcp")
        .unwrap()
        .clone();
    assert_eq!(jack["state"], "plugged");
    assert_eq!(jack["servers"][0]["state"], "needs_sign_in", "{jack}");

    // A made-up state is refused; the real trip back connects it.
    let (s, _) = public_get(&pod, "/v1/mcp-servers/callback?code=x&state=forged").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, page) = sign_in(&pod, &link).await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert!(page.contains("cal is connected"), "{page}");
    let (s, _) = sign_in_again_is_spent(&pod, &link).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a state works once");

    let srv = ok(&pod, "mcp_servers", json!({"name": "cal"})).await;
    assert_eq!(srv["server"]["state"], "connected", "{srv}");
    assert_eq!(srv["server"]["auth"], "oauth");
    assert_eq!(srv["server"]["tools"][0]["name"], "calendar_list_events");
    assert!(srv["server"]["tools"][0]["input_schema"].is_object());
    assert!(
        !srv.to_string().contains("access-1") && !srv.to_string().contains("refresh-1"),
        "tokens are never shown back: {srv}"
    );

    // A tool call (answered as an event stream).
    let r = ok(
        &pod,
        "mcp_call_tool",
        json!({"server": "cal", "tool": "calendar_list_events", "arguments": {"from": "2026-11-15"}}),
    )
    .await;
    assert_eq!(
        r["result"]["events"][0]["title"], "Dinner at Juniper",
        "{r}"
    );

    // An mcp filler: dated arguments, items and map.
    ok(
        &pod,
        "define_collection",
        json!({"name": "plans", "fields": [
            {"name": "uid", "type": "text", "optional": true},
            {"name": "title", "type": "text", "optional": true},
            {"name": "start", "type": "datetime", "optional": true}],
            "fillers": [{"kind": "mcp", "server": "cal", "tool": "calendar_list_events",
                "arguments": {"from": "{today}", "to": "{today+180}"}, "items": "events",
                "zone": "America/Denver", "key": "uid",
                "map": {"uid": "uid", "title": "title", "start": "start"}}]}),
    )
    .await;
    settle(&pod, "plans").await;
    let r = rows(&pod, "plans").await;
    let titles: Vec<&str> = r.iter().filter_map(|x| x["title"].as_str()).collect();
    assert_eq!(titles, ["Dinner at Juniper", "Hike with Robin"], "{r:?}");
    assert_eq!(r[0]["filler_title"], "cal · calendar_list_events");
    assert_eq!(
        (r[0]["start_day"].clone(), r[0]["start_time"].clone()),
        (json!("Wed Nov 18"), json!("6 PM"))
    );
    let today = app_collections::fill::today(pod.store(), pod_core::util::now_secs());
    let last = fake.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["from"], json!(today));
    let to = last["to"].as_str().unwrap();
    assert!(to.len() == 10 && to > today.as_str(), "{last}");
    let d = ok(&pod, "describe_collection", json!({"collection": "plans"})).await;
    let filler = d["definition"]["fillers"][0].clone();
    assert_eq!(filler["kind"], "mcp", "{filler}");
    assert_eq!(filler["arguments"]["to"], "{today+180}");
    let p = ok(&pod, "collections_preview", json!({"collection": "plans"})).await;
    assert_eq!(p["items"], 2, "{p}");
    assert_eq!(p["rows"][1]["title"], "Hike with Robin");

    // The access token stops working: the pod refreshes it (rotating the
    // refresh token) and the call goes through.
    *fake.access.lock().unwrap() = "access-2".into();
    let r = ok(
        &pod,
        "mcp_call_tool",
        json!({"server": "cal", "tool": "calendar_list_events"}),
    )
    .await;
    assert_eq!(r["result"]["count"], 2, "{r}");
    assert_eq!(*fake.refreshes.lock().unwrap(), 1);
    assert_eq!(*fake.refresh.lock().unwrap(), "refresh-2");

    // A refused refresh means signing in again.
    *fake.access.lock().unwrap() = "rotated-elsewhere".into();
    fake.refresh.lock().unwrap().clear();
    let (err, e) = tool(
        &pod,
        "mcp_call_tool",
        json!({"server": "cal", "tool": "calendar_list_events"}),
    )
    .await;
    assert!(err && e["error"] == "needs_sign_in", "{e}");
    let st = ok(&pod, "mcp_servers", json!({})).await;
    assert_eq!(st["servers"][0]["state"], "needs_sign_in", "{st}");
    let again = ok(&pod, "mcp_connect", json!({"name": "cal"})).await;
    assert_eq!(again["state"], "needs_sign_in");
    assert_eq!(
        *fake.registered.lock().unwrap(),
        1,
        "the client registration is kept"
    );
    let (s, _) = sign_in(&pod, again["sign_in_url"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::OK);

    // Unplugging the jack stops calls and fillers.
    ok(&pod, "unplug_jack", json!({"id": "mcp"})).await;
    let (err, e) = tool(
        &pod,
        "mcp_call_tool",
        json!({"server": "cal", "tool": "calendar_list_events"}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("mcp jack"),
        "{e}"
    );
    let st = ok(&pod, "collections_status", json!({"collection": "plans"})).await;
    assert_eq!(st["fillers"][0]["state"], "off", "{st}");
}

/// The callback of a sign-in that already finished.
async fn sign_in_again_is_spent(pod: &Pod, link: &str) -> (StatusCode, String) {
    let state = reqwest::Url::parse(link)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    public_get(
        pod,
        &format!("/v1/mcp-servers/callback?code=code-1&state={state}"),
    )
    .await
}

#[tokio::test]
async fn a_token_server_connects_with_its_token() {
    let fake = Arc::new(Fake::default());
    *fake.access.lock().unwrap() = "api-key-1".into();
    let base = serve(fake.clone()).await;
    let pod = pod();
    let (err, e) = tool(
        &pod,
        "mcp_connect",
        json!({"name": "keyed", "url": format!("{base}/mcp"), "token": "wrong"}),
    )
    .await;
    assert!(err && e["error"] == "unauthorized", "{e}");
    let c = ok(
        &pod,
        "mcp_connect",
        json!({"name": "keyed", "url": format!("{base}/mcp"), "token": "api-key-1"}),
    )
    .await;
    assert_eq!(c["server"]["state"], "connected", "{c}");
    assert_eq!(c["server"]["token_set"], true);
    assert!(
        c.to_string().find("api-key-1").is_none(),
        "never shown back"
    );
    let r = ok(
        &pod,
        "mcp_call_tool",
        json!({"server": "keyed", "tool": "calendar_list_events"}),
    )
    .await;
    assert_eq!(r["result"]["count"], 2);
    // A private URL is refused where private hosts aren't allowed.
    let strict: Pod = Arc::new(AppState::in_memory(vec![Arc::new(
        app_collections::CollectionsAdapter::new(),
    )]));
    strict
        .store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    let (err, e) = tool(
        &strict,
        "mcp_connect",
        json!({"name": "x", "url": format!("{base}/mcp")}),
    )
    .await;
    assert!(err && e["error"] == "invalid_url", "{e}");
    ok(&pod, "mcp_disconnect", json!({"name": "keyed"})).await;
    let st = ok(&pod, "mcp_servers", json!({})).await;
    assert_eq!(st["servers"], json!([]));
}
