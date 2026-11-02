//! End-to-end tests of the fake Pixelfed upstream through its router.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use fake_pixelfed::{AppState, Config, app, default_fixtures_dir, s256_challenge};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "http://fake.test:9999";
const SEED: &str = "seed-token";
const REDIRECT: &str = "canvas://oauth";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk_extra-chars~.";

const CAROUSEL: &str = "781254316723458049";
const REBLOG_OUTER: &str = "781249870123446273";
const REBLOG_INNER: &str = "781101134560870401";
const SINGLE: &str = "781233309876543489";
const TEXT_ONLY: &str = "781220014433221633";
const CW: &str = "781198765432109057";

fn router_with(auto_approve: bool) -> Router {
    app(Arc::new(
        AppState::new(Config {
            fixtures_dir: default_fixtures_dir(),
            public_url: format!("{BASE}/"),
            auto_approve,
            seed_token: Some(SEED.to_owned()),
        })
        .expect("fixtures load"),
    ))
}

fn router() -> Router {
    router_with(false)
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
    fn location(&self) -> String {
        self.headers[header::LOCATION].to_str().unwrap().to_owned()
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

async fn get(router: &Router, uri: &str, token: Option<&str>) -> Resp {
    let mut b = Request::get(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    send(router, b.body(Body::empty()).unwrap()).await
}

async fn post_empty(router: &Router, uri: &str, token: Option<&str>) -> Resp {
    let mut b = Request::post(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    send(router, b.body(Body::empty()).unwrap()).await
}

async fn post_form(router: &Router, uri: &str, pairs: &[(&str, &str)]) -> Resp {
    let body = serde_urlencoded::to_string(pairs).unwrap();
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    send(router, req).await
}

async fn post_json(router: &Router, uri: &str, body: Value) -> Resp {
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(router, req).await
}

fn query_of(location: &str) -> HashMap<String, String> {
    let q = location.split_once('?').map_or("", |(_, q)| q);
    serde_urlencoded::from_str(q).unwrap()
}

/// Registers an app (form body) and returns (client_id, client_secret).
async fn register(router: &Router) -> (String, String) {
    let r = post_form(
        router,
        "/api/v1/apps",
        &[
            ("client_name", "Canvas"),
            ("redirect_uris", REDIRECT),
            ("scopes", "read write"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    (
        j["client_id"].as_str().unwrap().to_owned(),
        j["client_secret"].as_str().unwrap().to_owned(),
    )
}

/// Runs authorize with `auto=1` and returns the code.
async fn authorize(router: &Router, client_id: &str, verifier: &str) -> String {
    let q = serde_urlencoded::to_string([
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", REDIRECT),
        ("scope", "read write"),
        ("state", "xyz 123"),
        ("code_challenge", &s256_challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("auto", "1"),
    ])
    .unwrap();
    let r = get(router, &format!("/oauth/authorize?{q}"), None).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let loc = r.location();
    assert!(loc.starts_with("canvas://oauth?"), "{loc}");
    let params = query_of(&loc);
    assert_eq!(params["state"], "xyz 123");
    params["code"].clone()
}

async fn exchange(
    router: &Router,
    client_id: &str,
    secret: &str,
    code: &str,
    verifier: &str,
) -> Resp {
    post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("client_id", client_id),
            ("client_secret", secret),
            ("code_verifier", verifier),
        ],
    )
    .await
}

#[test]
fn s256_matches_rfc7636_example() {
    assert_eq!(
        s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[tokio::test]
async fn full_oauth_flow_issues_a_working_token() {
    let router = router();
    let (client_id, secret) = register(&router).await;
    let code = authorize(&router, &client_id, VERIFIER).await;
    let r = exchange(&router, &client_id, &secret, &code, VERIFIER).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let j = r.json();
    assert_eq!(j["token_type"], "Bearer");
    assert_eq!(j["scope"], "read write");
    assert!(j["created_at"].as_u64().unwrap() > 1_700_000_000);
    let token = j["access_token"].as_str().unwrap();

    let me = get(&router, "/api/v1/accounts/verify_credentials", Some(token)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["username"], "canvasdemo");
}

#[tokio::test]
async fn json_bodies_are_accepted_for_apps_and_token() {
    let router = router();
    let r = post_json(
        &router,
        "/api/v1/apps",
        json!({"client_name": "Canvas", "redirect_uris": [REDIRECT, "http://127.0.0.1:1/cb"], "scopes": "read write", "website": "https://example.com"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let app = r.json();
    for key in [
        "id",
        "name",
        "client_id",
        "client_secret",
        "redirect_uri",
        "vapid_key",
    ] {
        assert!(app[key].is_string(), "missing {key}: {app}");
    }
    let client_id = app["client_id"].as_str().unwrap();
    let code = authorize(&router, client_id, VERIFIER).await;
    let r = post_json(
        &router,
        "/oauth/token",
        json!({"grant_type": "authorization_code", "code": code, "redirect_uri": REDIRECT,
               "client_id": client_id, "client_secret": app["client_secret"], "code_verifier": VERIFIER}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

#[tokio::test]
async fn authorize_page_and_button_post() {
    let router = router();
    let (client_id, secret) = register(&router).await;
    let challenge = s256_challenge(VERIFIER);
    let fields = [
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", REDIRECT),
        ("scope", "read write"),
        ("state", "s1"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    let q = serde_urlencoded::to_string(fields).unwrap();
    let page = get(&router, &format!("/oauth/authorize?{q}"), None).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = String::from_utf8(page.body).unwrap();
    assert!(html.contains(">Authorize</button>"), "{html}");
    assert!(
        html.contains(&format!("action=\"{BASE}/oauth/authorize\"")),
        "{html}"
    );

    let mut submit = fields.to_vec();
    submit.push(("decision", "approve"));
    let r = post_form(&router, "/oauth/authorize", &submit).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let params = query_of(&r.location());
    assert_eq!(params["state"], "s1");
    let ok = exchange(&router, &client_id, &secret, &params["code"], VERIFIER).await;
    assert_eq!(ok.status, StatusCode::OK);
}

#[tokio::test]
async fn auto_approve_config_redirects_without_query_flag() {
    let router = router_with(true);
    let (client_id, _) = register(&router).await;
    let q = serde_urlencoded::to_string([
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", REDIRECT),
        ("code_challenge", &s256_challenge(VERIFIER)),
        ("code_challenge_method", "S256"),
    ])
    .unwrap();
    let r = get(&router, &format!("/oauth/authorize?{q}"), None).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert!(query_of(&r.location()).contains_key("code"));
}

#[tokio::test]
async fn authorize_rejects_unregistered_redirect_and_missing_pkce() {
    let router = router();
    let (client_id, _) = register(&router).await;
    let bad_redirect = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fevil.test&code_challenge=abc&code_challenge_method=S256&auto=1"
    );
    assert_eq!(
        get(&router, &bad_redirect, None).await.status,
        StatusCode::BAD_REQUEST
    );
    let no_pkce = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=canvas%3A%2F%2Foauth&auto=1"
    );
    assert_eq!(
        get(&router, &no_pkce, None).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn wrong_verifier_is_invalid_grant() {
    let router = router();
    let (client_id, secret) = register(&router).await;
    let code = authorize(&router, &client_id, VERIFIER).await;
    let wrong = "x".repeat(50);
    let r = exchange(&router, &client_id, &secret, &code, &wrong).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn code_is_single_use() {
    let router = router();
    let (client_id, secret) = register(&router).await;
    let code = authorize(&router, &client_id, VERIFIER).await;
    assert_eq!(
        exchange(&router, &client_id, &secret, &code, VERIFIER)
            .await
            .status,
        StatusCode::OK
    );
    let again = exchange(&router, &client_id, &secret, &code, VERIFIER).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(again.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn wrong_client_secret_is_invalid_client() {
    let router = router();
    let (client_id, _) = register(&router).await;
    let code = authorize(&router, &client_id, VERIFIER).await;
    let r = exchange(&router, &client_id, "nope", &code, VERIFIER).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"], "invalid_client");
}

#[tokio::test]
async fn api_requires_a_valid_bearer() {
    let router = router();
    for uri in [
        "/api/v1/timelines/home",
        "/api/v1/accounts/verify_credentials",
    ] {
        for token in [None, Some("bogus")] {
            let r = get(&router, uri, token).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri} {token:?}");
            assert_eq!(r.json(), json!({"error": "The access token is invalid"}));
        }
    }
    let r = post_empty(
        &router,
        &format!("/api/v1/statuses/{CAROUSEL}/favourite"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The seeded token works.
    assert_eq!(
        get(&router, "/api/v1/timelines/home", Some(SEED))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn favourite_is_idempotent_and_unfavourite_reverts() {
    let router = router();
    let uri = |action: &str| format!("/api/v1/statuses/{CAROUSEL}/{action}");
    let first = post_empty(&router, &uri("favourite"), Some(SEED)).await;
    assert_eq!(first.status, StatusCode::OK);
    let j = first.json();
    assert_eq!(j["id"], CAROUSEL);
    assert_eq!(j["favourited"], true);
    assert_eq!(j["favourites_count"], 28);

    let second = post_empty(&router, &uri("favourite"), Some(SEED))
        .await
        .json();
    assert_eq!(second["favourites_count"], 28);

    let un = post_empty(&router, &uri("unfavourite"), Some(SEED))
        .await
        .json();
    assert_eq!(un["favourited"], false);
    assert_eq!(un["favourites_count"], 27);
    let un2 = post_empty(&router, &uri("unfavourite"), Some(SEED))
        .await
        .json();
    assert_eq!(un2["favourites_count"], 27);
}

#[tokio::test]
async fn unfavourite_a_prefavourited_status_and_timeline_reflects_it() {
    let router = router();
    let r = post_empty(
        &router,
        &format!("/api/v1/statuses/{SINGLE}/unfavourite"),
        Some(SEED),
    )
    .await;
    assert_eq!(r.json()["favourites_count"], 7);
    let tl = get(&router, "/api/v1/timelines/home", Some(SEED))
        .await
        .json();
    let s = tl
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == SINGLE)
        .unwrap();
    assert_eq!(s["favourited"], false);
    assert_eq!(s["favourites_count"], 7);
}

#[tokio::test]
async fn bookmark_and_unbookmark() {
    let router = router();
    let b = post_empty(
        &router,
        &format!("/api/v1/statuses/{CW}/bookmark"),
        Some(SEED),
    )
    .await
    .json();
    assert_eq!(b["bookmarked"], true);
    let tl = get(&router, "/api/v1/timelines/home", Some(SEED))
        .await
        .json();
    assert_eq!(
        tl.as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == CW)
            .unwrap()["bookmarked"],
        true
    );
    let u = post_empty(
        &router,
        &format!("/api/v1/statuses/{TEXT_ONLY}/unbookmark"),
        Some(SEED),
    )
    .await
    .json();
    assert_eq!(u["bookmarked"], false);
}

#[tokio::test]
async fn favouriting_a_reblog_targets_the_reblogged_status() {
    let router = router();
    let r = post_empty(
        &router,
        &format!("/api/v1/statuses/{REBLOG_OUTER}/favourite"),
        Some(SEED),
    )
    .await
    .json();
    assert_eq!(r["id"], REBLOG_INNER);
    assert_eq!(r["favourites_count"], 319);
    let tl = get(&router, "/api/v1/timelines/home", Some(SEED))
        .await
        .json();
    let outer = tl
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == REBLOG_OUTER)
        .unwrap();
    assert_eq!(outer["reblog"]["favourited"], true);
    assert_eq!(outer["reblog"]["favourites_count"], 319);
    let direct = post_empty(
        &router,
        &format!("/api/v1/statuses/{REBLOG_INNER}/favourite"),
        Some(SEED),
    )
    .await;
    assert_eq!(direct.json()["favourites_count"], 319);
}

#[tokio::test]
async fn unknown_status_is_404() {
    let router = router();
    let r = post_empty(&router, "/api/v1/statuses/123/favourite", Some(SEED)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json(), json!({"error": "Record not found"}));
}

#[tokio::test]
async fn timeline_has_the_expected_shapes() {
    let router = router();
    let r = get(&router, "/api/v1/timelines/home", Some(SEED)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&r.body).contains("{{BASE}}"));
    let tl = r.json();
    let items = tl.as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [CAROUSEL, REBLOG_OUTER, SINGLE, TEXT_ONLY, CW]);
    for s in items {
        for key in [
            "created_at",
            "content",
            "url",
            "uri",
            "visibility",
            "spoiler_text",
        ] {
            assert!(s[key].is_string(), "{key} in {}", s["id"]);
        }
        assert!(
            s["account"]["avatar"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{BASE}/storage/"))
        );
        assert!(s["favourites_count"].is_u64());
    }
    let by_id = |id: &str| items.iter().find(|s| s["id"] == id).unwrap();

    let carousel = by_id(CAROUSEL);
    let media = carousel["media_attachments"].as_array().unwrap();
    assert_eq!(media.len(), 3);
    for m in media {
        assert_eq!(m["type"], "image");
        assert!(
            m["url"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{BASE}/storage/"))
        );
        assert!(m["meta"]["original"]["aspect"].is_number());
        assert!(m["description"].is_string());
    }

    let single = by_id(SINGLE);
    let m = &single["media_attachments"][0];
    assert!(m["description"].is_null());
    assert_ne!(m["meta"]["original"]["aspect"].as_f64().unwrap(), 1.0);

    let text = by_id(TEXT_ONLY);
    assert_eq!(text["media_attachments"], json!([]));
    assert!(text["content"].as_str().unwrap().contains("&amp;"));

    let cw = by_id(CW);
    assert_eq!(cw["sensitive"], true);
    assert_eq!(cw["spoiler_text"], "Snake photo");
    assert_eq!(cw["media_attachments"].as_array().unwrap().len(), 1);

    let reblog = by_id(REBLOG_OUTER);
    assert_eq!(reblog["account"]["username"], "bob");
    assert_eq!(reblog["reblog"]["id"], REBLOG_INNER);
    assert_eq!(reblog["reblog"]["account"]["username"], "carol");
    assert_eq!(
        reblog["reblog"]["media_attachments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn timeline_paginates_with_limit_and_max_id() {
    let router = router();
    let r = get(&router, "/api/v1/timelines/home?limit=2", Some(SEED)).await;
    let ids: Vec<String> = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, [CAROUSEL, REBLOG_OUTER]);
    let link = r.headers[header::LINK].to_str().unwrap();
    assert!(link.contains(&format!("max_id={REBLOG_OUTER}")), "{link}");
    let next = get(
        &router,
        &format!("/api/v1/timelines/home?max_id={REBLOG_OUTER}"),
        Some(SEED),
    )
    .await;
    let ids: Vec<String> = next
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, [SINGLE, TEXT_ONLY, CW]);
}

#[tokio::test]
async fn media_is_served_for_every_fixture_url() {
    let router = router();
    let tl = get(&router, "/api/v1/timelines/home", Some(SEED)).await;
    let text = String::from_utf8(tl.body).unwrap();
    // Every {BASE}/storage/... URL in the timeline resolves to an image.
    let mut urls: Vec<&str> = text
        .match_indices(&format!("{BASE}/storage/"))
        .map(|(i, _)| {
            let rest = &text[i..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect();
    urls.sort_unstable();
    urls.dedup();
    assert!(urls.len() >= 10, "{urls:?}");
    for url in urls {
        let path = url.strip_prefix(BASE).unwrap();
        let r = get(&router, path, None).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert_eq!(r.headers[header::CONTENT_TYPE], "image/png", "{path}");
        assert!(r.body.starts_with(b"\x89PNG"), "{path}");
    }
    assert_eq!(
        get(&router, "/storage/nope.png", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&router, "/storage/../timeline_home.json", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&router, "/storage/%2e%2e/timeline_home.json", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}
