//! The fake's test controls: embedded fixtures, `/_admin/{state,reset,faults}`, fault rules
//! and the `X-Fake-Fault` header (docs/DECISIONS.md D16).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use fake_pixelfed::{AppState, Config, FaultKind, FaultRule, app, default_fixtures_dir};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "http://fake.test:9999";
const SEED: &str = "seed-token";
const SINGLE: &str = "781233309876543489";
const CAROUSEL: &str = "781254316723458049";

fn state_with(dir: Option<std::path::PathBuf>) -> Arc<AppState> {
    Arc::new(
        AppState::new(Config {
            fixtures_dir: dir.unwrap_or_default(),
            public_url: BASE.to_owned(),
            auto_approve: true,
            seed_token: Some(SEED.to_owned()),
        })
        .unwrap(),
    )
}

fn fake() -> (Arc<AppState>, Router) {
    let state = state_with(None);
    (Arc::clone(&state), app(state))
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
}

async fn send(router: &Router, req: Request<Body>) -> Resp {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    fault: Option<&str>,
    body: Option<Value>,
) -> Resp {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(f) = fault {
        b = b.header("X-Fake-Fault", f);
    }
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    };
    send(router, req.unwrap()).await
}

async fn favourite(router: &Router, id: &str, token: &str) -> Resp {
    call(
        router,
        "POST",
        &format!("/api/v1/statuses/{id}/favourite"),
        Some(token),
        None,
        None,
    )
    .await
}

async fn timeline(router: &Router, fault: Option<&str>) -> Resp {
    call(
        router,
        "GET",
        "/api/v1/timelines/home?include_reblogs=true",
        Some(SEED),
        fault,
        None,
    )
    .await
}

#[tokio::test]
async fn embedded_fixtures_match_the_repository_copies() {
    let names: Vec<&str> = fake_pixelfed::embedded_fixture_names().collect();
    assert!(names.contains(&"timeline_home.json"), "{names:?}");
    assert!(names.contains(&"media/carousel-1.png"), "{names:?}");
    let embedded = app(state_with(None));
    let on_disk = app(state_with(Some(default_fixtures_dir())));
    let a = timeline(&embedded, None).await;
    let b = timeline(&on_disk, None).await;
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(a.json(), b.json());
    for path in ["/storage/carousel-1.png", "/storage/avatar-me.png"] {
        let x = call(&embedded, "GET", path, None, None, None).await;
        let y = call(&on_disk, "GET", path, None, None, None).await;
        assert_eq!(x.status, StatusCode::OK, "{path}");
        assert_eq!(x.headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(x.body, y.body, "{path}");
    }
    let missing = call(&embedded, "GET", "/storage/nope.png", None, None, None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let traversal = call(
        &embedded,
        "GET",
        "/storage/../timeline_home.json",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(traversal.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fault_header_injects_one_failure() {
    let (state, router) = fake();
    let r = timeline(&router, Some("401")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(r.json(), json!({ "error": "The access token is invalid" }));

    let r = timeline(&router, Some("429")).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers[header::RETRY_AFTER], "1");

    for code in [500u16, 503] {
        let r = timeline(&router, Some(&code.to_string())).await;
        assert_eq!(r.status.as_u16(), code);
        assert!(r.json()["error"].is_string());
    }

    let start = Instant::now();
    let r = timeline(&router, Some("latency=200")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(start.elapsed() >= Duration::from_millis(200));

    let start = Instant::now();
    let r = timeline(&router, Some("latency=100, 503")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(start.elapsed() >= Duration::from_millis(100));

    // Only that request: the next one is fine, and no rule was left behind.
    assert_eq!(timeline(&router, None).await.status, StatusCode::OK);
    assert!(state.faults().is_empty());
    let log = state.requests();
    assert_eq!(log.len(), 7);
    assert_eq!(log[0].fault.as_deref(), Some("status 401 (header)"));
    assert_eq!(log[4].fault.as_deref(), Some("latency 200ms (header)"));
    assert_eq!(
        log[5].fault.as_deref(),
        Some("latency 100ms + status 503 (header)")
    );
    assert_eq!(log[6].fault, None);
    assert_eq!(log[6].path, "/api/v1/timelines/home?include_reblogs=true");
}

#[test]
fn fault_header_parsing() {
    assert_eq!(
        fake_pixelfed::parse_fault_header("latency=1500, 503, bogus, 99"),
        vec![
            FaultKind::Latency { ms: 1500 },
            FaultKind::Status { status: 503 }
        ]
    );
}

#[tokio::test]
async fn admin_fault_rules_count_down_and_match_by_path_and_method() {
    let (state, router) = fake();
    let r = call(
        &router,
        "POST",
        "/_admin/faults",
        None,
        None,
        Some(json!({ "rules": [
            { "path_prefix": "/api/v1/statuses", "kind": "status", "status": 503, "times": 2 },
            { "path_prefix": "/api/v1/timelines", "method": "GET", "kind": "rate_limit", "retry_after": 2, "times": 1 },
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["rules"].as_array().unwrap().len(), 2);

    // The timeline rule: one 429 with Retry-After: 2, then normal.
    let r = timeline(&router, None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers[header::RETRY_AFTER], "2");
    assert_eq!(timeline(&router, None).await.status, StatusCode::OK);

    // The statuses rule: two 503s (nothing applied), then the favourite goes through.
    assert_eq!(favourite(&router, SINGLE, SEED).await.status.as_u16(), 503);
    assert_eq!(
        favourite(&router, CAROUSEL, SEED).await.status.as_u16(),
        503
    );
    assert!(state.favourited_ids().iter().all(|id| id != CAROUSEL));
    let ok = favourite(&router, CAROUSEL, SEED).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json()["favourited"], true);
    assert!(state.faults().is_empty(), "{:?}", state.faults());

    // Latency rule without `times` stays until DELETE.
    state.add_faults([FaultRule::latency(150).path("/api/v1/timelines")]);
    for _ in 0..2 {
        let start = Instant::now();
        assert_eq!(timeline(&router, None).await.status, StatusCode::OK);
        assert!(start.elapsed() >= Duration::from_millis(150));
    }
    let r = call(&router, "DELETE", "/_admin/faults", None, None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(state.faults().is_empty());

    let report = call(&router, "GET", "/_admin/state?tail=3", None, None, None)
        .await
        .json();
    let tail = report["requests"].as_array().unwrap();
    assert_eq!(tail.len(), 3);
    assert!(tail[0].get("fault").is_none(), "{report}");
    assert_eq!(
        tail[0]["path"],
        format!("/api/v1/statuses/{CAROUSEL}/favourite")
    );
    assert_eq!(tail[1]["fault"], "latency 150ms (rule)");
    assert_eq!(tail[2]["fault"], "latency 150ms (rule)");
    // Admin requests are not logged.
    assert_eq!(report["counts"]["requests"], 7);
    assert!(
        report["favourited"]
            .as_array()
            .unwrap()
            .contains(&json!(CAROUSEL))
    );

    // Bad bodies are rejected.
    let bad = call(
        &router,
        "POST",
        "/_admin/faults",
        None,
        None,
        Some(json!({ "rules": [{ "kind": "explode" }] })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn expire_tokens_rule_and_status_401_look_like_an_expired_token() {
    let (state, router) = fake();
    state.add_faults([FaultRule::status(401).path("/api/v1/statuses").times(1)]);
    let r = favourite(&router, SINGLE, SEED).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(r.json()["error"], "The access token is invalid");

    let r = call(
        &router,
        "POST",
        "/_admin/faults",
        None,
        None,
        Some(json!({ "rules": [{ "kind": "expire_tokens" }] })),
    )
    .await;
    assert_eq!(r.json()["rules"], json!([]), "expire_tokens is not stored");
    assert_eq!(
        timeline(&router, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A reset brings the seed token back.
    state.reset(false);
    assert_eq!(timeline(&router, None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn reset_restores_fixtures_and_optionally_keeps_auth() {
    let (state, router) = fake();
    // Sign in for a real token (auto-approve) to see it dropped / kept.
    let app_resp = call(
        &router,
        "POST",
        "/api/v1/apps",
        None,
        None,
        Some(json!({ "client_name": "t", "redirect_uris": "construct://oauth", "scopes": "read write" })),
    )
    .await
    .json();
    let client_id = app_resp["client_id"].as_str().unwrap().to_owned();
    let secret = app_resp["client_secret"].as_str().unwrap().to_owned();
    let verifier = "v".repeat(50);
    let q = serde_urlencoded::to_string([
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", "construct://oauth"),
        ("code_challenge", &fake_pixelfed::s256_challenge(&verifier)),
        ("code_challenge_method", "S256"),
    ])
    .unwrap();
    let loc = call(
        &router,
        "GET",
        &format!("/oauth/authorize?{q}"),
        None,
        None,
        None,
    )
    .await;
    let location = loc.headers[header::LOCATION].to_str().unwrap().to_owned();
    let code = location.split("code=").nth(1).unwrap().to_owned();
    let tok = call(
        &router,
        "POST",
        "/oauth/token",
        None,
        None,
        Some(
            json!({ "grant_type": "authorization_code", "code": code, "client_id": client_id,
            "client_secret": secret, "redirect_uri": "construct://oauth", "code_verifier": verifier }),
        ),
    )
    .await
    .json();
    let token = tok["access_token"].as_str().unwrap().to_owned();

    let before = state.favourited_ids();
    let bookmarked_before = state.bookmarked_ids();
    assert!(!bookmarked_before.contains(&SINGLE.to_owned()));
    assert_eq!(
        favourite(&router, CAROUSEL, &token).await.status,
        StatusCode::OK
    );
    call(
        &router,
        "POST",
        &format!("/api/v1/statuses/{SINGLE}/bookmark"),
        Some(&token),
        None,
        None,
    )
    .await;
    state.add_faults([FaultRule::status(500)]);
    let report = state.report(10);
    assert_eq!(report["grants"]["authorization_code"], 1);
    assert_eq!(report["counts"]["apps"], 1);
    assert!(
        report["bookmarked"]
            .as_array()
            .unwrap()
            .contains(&json!(SINGLE))
    );

    // keep_auth: content and faults reset, the token still works.
    let r = call(
        &router,
        "POST",
        "/_admin/reset?keep_auth=1",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(state.favourited_ids(), before);
    assert_eq!(state.bookmarked_ids(), bookmarked_before);
    assert!(state.faults().is_empty());
    assert!(state.requests().is_empty());
    assert_eq!(
        favourite(&router, CAROUSEL, &token).await.status,
        StatusCode::OK
    );

    // Full reset: the app and its token are gone, the seed token stays.
    call(&router, "POST", "/_admin/reset", None, None, None).await;
    assert_eq!(state.favourited_ids(), before);
    assert_eq!(
        favourite(&router, CAROUSEL, &token).await.status,
        StatusCode::UNAUTHORIZED
    );
    let report = state.report(10);
    assert_eq!(report["counts"]["apps"], 0);
    assert_eq!(report["counts"]["access_tokens"], 1);
    assert_eq!(report["grants"]["authorization_code"], 0);
    assert_eq!(timeline(&router, None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn admin_routes_can_be_disabled() {
    let state = Arc::new(
        AppState::new(Config::embedded(BASE))
            .unwrap()
            .with_admin(false),
    );
    let router = app(Arc::clone(&state));
    for (m, p) in [
        ("GET", "/_admin/state"),
        ("POST", "/_admin/reset"),
        ("POST", "/_admin/faults"),
        ("DELETE", "/_admin/faults"),
    ] {
        let r = call(&router, m, p, None, None, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {p}");
    }
    // The in-process controls still work.
    state.add_faults([FaultRule::status(503).times(1)]);
    let r = call(&router, "GET", "/api/v1/timelines/home", None, None, None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}
