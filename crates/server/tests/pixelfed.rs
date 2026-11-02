//! Pixelfed adapter integration tests: the pod and the fake upstream
//! (`crates/fake-pixelfed`) both run on random local ports.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use protocol::{ActionIntent, FieldUpdate, PresentationMode, UiNode, WorkspaceSpec};
use reqwest::{StatusCode, redirect};
use serde_json::{Value, json};
use server::pixelfed::routes::session_hash;
use server::store::PixelfedAccount;
use server::{AppState, PixelfedConfig};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

const SEED: &str = "seed-token";
const REDIRECT: &str = "http://127.0.0.1:9/callback";
const CAROUSEL: &str = "781254316723458049";
const REBLOG_OUTER: &str = "781249870123446273";
const REBLOG_INNER: &str = "781101134560870401";
const TEXT_ONLY: &str = "781220014433221633";

struct Server {
    url: String,
    handle: JoinHandle<()>,
    /// The fake upstream's state (test hooks); None for the pod.
    fake: Option<Arc<fake_pixelfed::AppState>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn serve(router: axum::Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server {
        url,
        handle,
        fake: None,
    }
}

async fn spawn_fake() -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fake_pixelfed::default_fixtures_dir(),
        public_url: url.clone(),
        auto_approve: true,
        seed_token: Some(SEED.to_owned()),
    })
    .unwrap();
    let state = Arc::new(state);
    let router = fake_pixelfed::app(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server {
        url,
        handle,
        fake: Some(state),
    }
}

async fn spawn_pod(state: AppState) -> Server {
    serve(server::app(Arc::new(state))).await
}

/// A URL nothing listens on.
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .build()
        .unwrap()
}

struct Env {
    fake: Server,
    pod: Server,
}

async fn env_with(state: AppState) -> Env {
    let fake = spawn_fake().await;
    let pod = spawn_pod(state.with_pixelfed(PixelfedConfig::new(&fake.url, None))).await;
    Env { fake, pod }
}

async fn env() -> Env {
    env_with(AppState::in_memory()).await
}

async fn post_json(url: &str, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = client().post(url).json(&body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn get_home(pod: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let (status, _, body) = get_home_full(pod, bearer).await;
    (status, body)
}

/// Like [`get_home`], plus the `X-Canvas-Session` response header.
async fn get_home_full(pod: &str, bearer: Option<&str>) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(format!("{pod}/v1/workspaces/pixelfed_home"));
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let session = resp
        .headers()
        .get("x-canvas-session")
        .map(|v| v.to_str().unwrap().to_owned());
    (resp.status(), session, resp.json().await.unwrap())
}

/// A real RFC 7636 verifier (64 unreserved chars) and its S256 challenge.
fn pkce() -> (String, String) {
    let mut bytes = [0u8; 48];
    getrandom::fill(&mut bytes).unwrap();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    assert_eq!(verifier.len(), 64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(challenge, fake_pixelfed::s256_challenge(&verifier));
    (verifier, challenge)
}

/// app -> authorize (auto-approved by the fake) -> returns (code, verifier).
async fn authorize(env: &Env) -> (String, String) {
    let (status, app) = post_json(
        &format!("{}/v1/auth/pixelfed/app", env.pod.url),
        None,
        json!({ "instance_url": env.fake.url, "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{app}");
    assert_eq!(app["scopes"], "read write");
    assert_eq!(
        app["authorize_endpoint"],
        format!("{}/oauth/authorize", env.fake.url)
    );
    let (verifier, challenge) = pkce();
    let resp = client()
        .get(app["authorize_endpoint"].as_str().unwrap())
        .query(&[
            ("response_type", "code"),
            ("client_id", app["client_id"].as_str().unwrap()),
            ("redirect_uri", REDIRECT),
            ("scope", "read write"),
            ("state", "st4te"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = reqwest::Url::parse(resp.headers()["location"].to_str().unwrap()).unwrap();
    assert!(location.as_str().starts_with(REDIRECT));
    let q: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(q["state"], "st4te");
    (q["code"].clone(), verifier)
}

async fn sign_in(env: &Env) -> String {
    let (code, verifier) = authorize(env).await;
    let (status, body) = post_json(
        &format!("{}/v1/auth/pixelfed/token", env.pod.url),
        None,
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["id"], "703641122334455667");
    assert_eq!(body["account"]["username"], "canvasdemo");
    assert_eq!(body["account"]["display_name"], "Canvas Demo");
    assert!(
        body["account"]["avatar_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/v1/pixelfed/media/", env.pod.url))
    );
    body["session_token"].as_str().unwrap().to_owned()
}

/// The fake's view of a status (via the pre-seeded token).
async fn upstream_status(env: &Env, id: &str) -> Value {
    let list: Vec<Value> = client()
        .get(format!(
            "{}/api/v1/timelines/home?include_reblogs=true",
            env.fake.url
        ))
        .bearer_auth(SEED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    list.into_iter()
        .map(|s| {
            if s["reblog"].is_object() {
                s["reblog"].clone()
            } else {
                s
            }
        })
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("status {id} not in the fake timeline"))
}

fn signed_out_button(spec: &WorkspaceSpec) -> (String, ActionIntent) {
    let UiNode::Workspace { id, slots, .. } = &spec.workspace else {
        panic!("not a workspace");
    };
    assert_eq!(id, "pixelfed_home");
    let UiNode::Container { children, .. } = &slots[0].content else {
        panic!("signed-out slot is not a Container");
    };
    assert!(matches!(&children[0], UiNode::Text { value_expr, .. } if value_expr == "Pixelfed"));
    children
        .iter()
        .find_map(|c| match c {
            UiNode::Button { label, on_click } => Some((label.clone(), on_click.clone())),
            _ => None,
        })
        .expect("sign-in button")
}

#[tokio::test]
async fn pixelfed_full_auth_flow_and_signed_in_feed() {
    let env = env().await;
    let session = sign_in(&env).await;

    // The app is cached per redirect_uri.
    let app_url = format!("{}/v1/auth/pixelfed/app", env.pod.url);
    let body = json!({ "instance_url": env.fake.url, "redirect_uri": REDIRECT });
    let (_, a1) = post_json(&app_url, None, body.clone()).await;
    let (_, a2) = post_json(&app_url, None, body).await;
    assert_eq!(a1["client_id"], a2["client_id"]);

    // Feed fetched through a non-local Host (as the emulator does).
    let resp = client()
        .get(format!("{}/v1/workspaces/pixelfed_home", env.pod.url))
        .header("host", "10.0.2.2:8080")
        .bearer_auth(&session)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let spec: WorkspaceSpec = resp.json().await.unwrap();
    assert_eq!(spec.version, "1.0.0");
    let order = &spec.queries["timeline:home"];
    assert_eq!(order.len(), 5);
    assert_eq!(order[0], format!("pixelfed://posts/{CAROUSEL}"));
    assert_eq!(order[1], format!("pixelfed://posts/{REBLOG_INNER}"));
    for uri in order {
        assert!(spec.entities.contains_key(uri), "{uri}");
    }
    let alice = &spec.entities["pixelfed://users/698765432100012345"];
    assert!(
        alice
            .get_str("avatar_url")
            .unwrap()
            .starts_with("http://10.0.2.2:8080/v1/pixelfed/media/"),
        "{alice:?}"
    );

    // Layout matches the contract.
    let UiNode::Workspace { id, slots, .. } = &spec.workspace else {
        panic!("not a workspace");
    };
    assert_eq!(id, "pixelfed_home");
    let UiNode::FeedList {
        query_key,
        item_template,
    } = &slots[0].content
    else {
        panic!("not a FeedList");
    };
    assert_eq!(query_key, "timeline:home");
    let UiNode::PostCard {
        post_ref,
        author_slot,
        media_slot,
        action_slot,
        caption_slot,
    } = item_template.as_ref()
    else {
        panic!("not a PostCard");
    };
    assert_eq!(post_ref, "$item.uri");
    assert!(
        matches!(author_slot.as_ref(), UiNode::AuthorHeader { user_ref, on_click: None } if user_ref == "$item.author_uri")
    );
    assert!(
        matches!(media_slot.as_ref(), UiNode::MediaCarousel { aspect_ratio, .. } if *aspect_ratio == 1.0)
    );
    assert!(
        matches!(caption_slot.as_ref(), UiNode::Text { value_expr, .. } if value_expr == "entity.caption")
    );
    let UiNode::ActionBar {
        like_intent,
        bookmark_intent,
        comment_intent,
    } = action_slot.as_ref()
    else {
        panic!("not an ActionBar");
    };
    assert_eq!(
        *like_intent,
        ActionIntent::OptimisticMutate {
            entity_uri: "$item.uri".into(),
            field: "is_liked".into(),
            value_expr: "!entity.is_liked".into(),
            rollback_value: json!(false),
            endpoint: "/v1/pixelfed/posts/like".into(),
            payload: json!({ "id": "$item.id", "liked": "$value" }),
            also: vec![FieldUpdate {
                field: "like_count".into(),
                value_expr: "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
                    .into(),
            }],
        }
    );
    assert_eq!(
        *bookmark_intent,
        ActionIntent::OptimisticMutate {
            entity_uri: "$item.uri".into(),
            field: "is_bookmarked".into(),
            value_expr: "!entity.is_bookmarked".into(),
            rollback_value: json!(false),
            endpoint: "/v1/pixelfed/posts/bookmark".into(),
            payload: json!({ "id": "$item.id", "bookmarked": "$value" }),
            also: vec![],
        }
    );
    assert_eq!(
        *comment_intent,
        ActionIntent::Navigate {
            target_uri: "pixelfed://posts/$item.id/comments".into(),
            presentation: PresentationMode::Modal,
        }
    );
}

#[tokio::test]
async fn pixelfed_home_signed_out_without_valid_session() {
    let env = env().await;
    // No bearer: 200. A bearer that is not a session: 401 + `X-Canvas-Session: invalid`, so the
    // client learns its session is stale; the body is the same signed-out bundle either way.
    for (bearer, want_status, want_header) in [
        (None, StatusCode::OK, None),
        (
            Some("not-a-session"),
            StatusCode::UNAUTHORIZED,
            Some("invalid"),
        ),
    ] {
        let (status, header, body) = get_home_full(&env.pod.url, bearer).await;
        assert_eq!(status, want_status);
        assert_eq!(header.as_deref(), want_header);
        let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
        assert!(spec.entities.is_empty() && spec.queries.is_empty());
        let (label, intent) = signed_out_button(&spec);
        assert_eq!(label, "Sign in with Pixelfed");
        assert_eq!(
            intent,
            ActionIntent::AuthLogin {
                instance_url: env.fake.url.clone()
            }
        );
    }
}

#[tokio::test]
async fn like_and_unlike_reach_the_upstream() {
    let env = env().await;
    let session = sign_in(&env).await;
    let like_url = format!("{}/v1/pixelfed/posts/like", env.pod.url);

    let (status, body) = post_json(
        &like_url,
        Some(&session),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "ok": true, "id": CAROUSEL, "is_liked": true, "like_count": 28, "is_bookmarked": false })
    );
    let up = upstream_status(&env, CAROUSEL).await;
    assert_eq!(
        (up["favourited"].clone(), up["favourites_count"].clone()),
        (json!(true), json!(28))
    );

    let (status, body) = post_json(
        &like_url,
        Some(&session),
        json!({ "id": CAROUSEL, "liked": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["is_liked"], false);
    assert_eq!(body["like_count"], 27);
    assert_eq!(upstream_status(&env, CAROUSEL).await["favourited"], false);

    // Acting on a reblog wrapper id acts on (and reports) the inner post.
    let (status, body) = post_json(
        &like_url,
        Some(&session),
        json!({ "id": REBLOG_OUTER, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], REBLOG_INNER);
    assert_eq!(body["like_count"], 319);

    // The next feed reflects upstream state.
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    assert_eq!(
        home["entities"][format!("pixelfed://posts/{REBLOG_INNER}")]["is_liked"],
        true
    );

    // Upstream 4xx keeps its status (client rolls back); bad input is 400.
    let (status, body) = post_json(
        &like_url,
        Some(&session),
        json!({ "id": "999", "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "upstream_rejected");
    for bad in [
        json!({ "id": "../apps", "liked": true }),
        json!({ "id": CAROUSEL }),
        json!({ "liked": true }),
    ] {
        let (status, _) = post_json(&like_url, Some(&session), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn bookmark_reaches_the_upstream() {
    let env = env().await;
    let session = sign_in(&env).await;
    let url = format!("{}/v1/pixelfed/posts/bookmark", env.pod.url);
    let (status, body) = post_json(
        &url,
        Some(&session),
        json!({ "id": TEXT_ONLY, "bookmarked": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_bookmarked"], false);
    assert_eq!(body["like_count"], 15);
    assert_eq!(upstream_status(&env, TEXT_ONLY).await["bookmarked"], false);
    let (_, body) = post_json(
        &url,
        Some(&session),
        json!({ "id": CAROUSEL, "bookmarked": true }),
    )
    .await;
    assert_eq!(body["is_bookmarked"], true);
    assert_eq!(upstream_status(&env, CAROUSEL).await["bookmarked"], true);
}

#[tokio::test]
async fn actions_and_logout_require_a_session() {
    let env = env().await;
    for bearer in [None, Some("bogus")] {
        for (path, body) in [
            (
                "/v1/pixelfed/posts/like",
                json!({ "id": CAROUSEL, "liked": true }),
            ),
            (
                "/v1/pixelfed/posts/bookmark",
                json!({ "id": CAROUSEL, "bookmarked": true }),
            ),
            ("/v1/auth/logout", json!({})),
        ] {
            let (status, resp) = post_json(&format!("{}{path}", env.pod.url), bearer, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} {bearer:?}");
            assert_eq!(resp["error"], "unauthorized");
        }
    }
    // Nothing reached the upstream.
    assert_eq!(upstream_status(&env, CAROUSEL).await["favourited"], false);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let env = env().await;
    let session = sign_in(&env).await;
    let logout = format!("{}/v1/auth/logout", env.pod.url);
    let (status, body) = post_json(&logout, Some(&session), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let (status, _) = post_json(&logout, Some(&session), json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, header, home) = get_home_full(&env.pod.url, Some(&session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    let spec: WorkspaceSpec = serde_json::from_value(home).unwrap();
    signed_out_button(&spec);
    // Without the stale bearer: the plain 200 signed-out bundle.
    let (status, home) = get_home(&env.pod.url, None).await;
    assert_eq!(status, StatusCode::OK);
    signed_out_button(&serde_json::from_value(home).unwrap());
}

/// A pod whose store already has a session + upstream account for `instance`.
fn seeded_state(instance: &str, upstream_token: &str) -> AppState {
    let state = AppState::in_memory().with_pixelfed(PixelfedConfig::new(instance, None));
    state.store().create_session(&session_hash("tok")).unwrap();
    state
        .store()
        .save_pixelfed_account(
            instance,
            &PixelfedAccount {
                access_token: upstream_token.into(),
                account_id: "1".into(),
                username: "x".into(),
                ..Default::default()
            },
        )
        .unwrap();
    state
}

#[tokio::test]
async fn upstream_down_is_502() {
    let dead = dead_url().await;
    let pod = spawn_pod(seeded_state(&dead, SEED)).await;
    let (status, body) = get_home(&pod.url, Some("tok")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"], "upstream_unreachable");
    let (status, _) = post_json(
        &format!("{}/v1/pixelfed/posts/like", pod.url),
        Some("tok"),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _) = post_json(
        &format!("{}/v1/auth/pixelfed/app", pod.url),
        None,
        json!({ "instance_url": dead, "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn upstream_401_means_signed_out() {
    let fake = spawn_fake().await;
    let pod = spawn_pod(seeded_state(&fake.url, "revoked-token")).await;
    let (status, header, body) = get_home_full(&pod.url, Some("tok")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    signed_out_button(&spec);
    // The dead upstream token was dropped, so actions are now 401.
    let (status, _) = post_json(
        &format!("{}/v1/pixelfed/posts/like", pod.url),
        Some("tok"),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upstream_seeded_token_serves_feed() {
    let fake = spawn_fake().await;
    let pod = spawn_pod(seeded_state(&fake.url, SEED)).await;
    let (status, body) = get_home(&pod.url, Some("tok")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["queries"]["timeline:home"].as_array().unwrap().len(),
        5
    );
}

#[tokio::test]
async fn media_proxy_streams_png() {
    let env = env().await;
    let session = sign_in(&env).await;
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    let url = home["entities"]["pixelfed://media/781254316501123073"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        url.starts_with(&format!("{}/v1/pixelfed/media/", env.pod.url)),
        "{url}"
    );
    // No auth needed.
    let resp = client().get(&url).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let original =
        std::fs::read(fake_pixelfed::default_fixtures_dir().join("media/carousel-1.png")).unwrap();
    assert_eq!(bytes.as_ref(), original.as_slice());

    for key in [
        "0123456789abcdef0123456789abcdef",
        "nothex",
        "..%2F..%2Fetc",
    ] {
        let resp = client()
            .get(format!("{}/v1/pixelfed/media/{key}", env.pod.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{key}");
    }
}

/// A media URL from upstream content that serves HTML/SVG is passed through
/// as inert bytes, never as a page on the pod's origin.
#[tokio::test]
async fn media_proxy_neutralizes_non_media_content_types() {
    use axum::{http::header, routing::get};
    let origin = serve(
        axum::Router::new()
            .route(
                "/page",
                get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<script>x</script>") }),
            )
            .route(
                "/img.svg",
                get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], "<svg/>") }),
            )
            .route(
                "/img.jpg",
                get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], "jpg") }),
            ),
    )
    .await;
    let state = AppState::in_memory();
    let urls: Vec<(String, String)> = ["page", "img.svg", "img.jpg"]
        .iter()
        .map(|p| {
            let url = format!("{}/{p}", origin.url);
            (server::pixelfed::media_key(&url), url)
        })
        .collect();
    state
        .store()
        .save_media_keys(urls.iter().map(|(k, u)| (k, u)))
        .unwrap();
    let pod = spawn_pod(state).await;
    for ((key, _), want) in urls
        .iter()
        .zip(["application/octet-stream", "application/octet-stream", "image/jpeg"])
    {
        let resp = client()
            .get(format!("{}/v1/pixelfed/media/{key}", pod.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], want);
        assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
        assert!(
            resp.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("sandbox")
        );
    }
}

#[tokio::test]
async fn session_tokens_are_stored_hashed() {
    let dir = tempfile::tempdir().unwrap();
    let env = env_with(AppState::open_dir(dir.path()).unwrap()).await;
    let session = sign_in(&env).await;
    assert!(session.len() >= 43);
    let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
    let hashes: Vec<String> = conn
        .prepare("SELECT token_hash FROM sessions")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: String = Sha256::digest(session.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(hashes, [expected]);
    // The raw session token appears nowhere in the database.
    let dump: String = conn
        .query_row(
            "SELECT group_concat(token_hash || created_at) FROM sessions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!dump.contains(&session));
    let (instance, username): (String, String) = conn
        .query_row("SELECT instance, username FROM pixelfed_account", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (instance.as_str(), username.as_str()),
        (env.fake.url.as_str(), "canvasdemo")
    );
}

#[tokio::test]
async fn foreign_instance_is_rejected() {
    let env = env().await;
    for path in ["/v1/auth/pixelfed/app", "/v1/auth/pixelfed/token"] {
        let (status, body) = post_json(
            &format!("{}{path}", env.pod.url),
            None,
            json!({ "instance_url": "http://evil.example", "redirect_uri": REDIRECT,
                    "code": "c", "code_verifier": "v" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["error"], "instance_not_allowed");
    }
    // A trailing slash on the configured instance is fine.
    let (status, _) = post_json(
        &format!("{}/v1/auth/pixelfed/app", env.pod.url),
        None,
        json!({ "instance_url": format!("{}/", env.fake.url), "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn public_url_is_used_for_authorize_and_accepted() {
    let fake = spawn_fake().await;
    let pod = spawn_pod(AppState::in_memory().with_pixelfed(PixelfedConfig::new(
        &fake.url,
        Some("http://10.0.2.2:8081/"),
    )))
    .await;
    let (status, app) = post_json(
        &format!("{}/v1/auth/pixelfed/app", pod.url),
        None,
        json!({ "instance_url": "http://10.0.2.2:8081", "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{app}");
    assert_eq!(
        app["authorize_endpoint"],
        "http://10.0.2.2:8081/oauth/authorize"
    );
}

#[tokio::test]
async fn bad_code_verifier_is_oauth_exchange_failed() {
    let env = env().await;
    let (code, _verifier) = authorize(&env).await;
    let (wrong, _) = pkce();
    let (status, body) = post_json(
        &format!("{}/v1/auth/pixelfed/token", env.pod.url),
        None,
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": wrong,
                "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "oauth_exchange_failed");
    assert!(body["detail"].as_str().unwrap().contains("code_verifier"));
    // Token exchange for an unregistered redirect_uri.
    let (status, body) = post_json(
        &format!("{}/v1/auth/pixelfed/token", env.pod.url),
        None,
        json!({ "instance_url": env.fake.url, "code": "x", "code_verifier": wrong,
                "redirect_uri": "http://other/cb" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "app_not_registered");
}

#[tokio::test]
async fn demo_ack_accepts_pixelfed_demo_likes() {
    let pod = spawn_pod(AppState::in_memory()).await;
    let demo: WorkspaceSpec = client()
        .get(format!("{}/v1/workspaces/pixelfed_demo", pod.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let s = serde_json::to_string(&demo).unwrap();
    assert!(s.contains("\"endpoint\":\"/v1/demo/ack\""), "{s}");
    let (status, body) = post_json(
        &format!("{}/v1/demo/ack", pod.url),
        None,
        json!({ "id": "101" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn home_feed_asks_upstream_for_reblogs() {
    // Like real Pixelfed, the fake hides boosts unless include_reblogs=true.
    let env = env().await;
    let plain: Vec<Value> = client()
        .get(format!("{}/api/v1/timelines/home", env.fake.url))
        .bearer_auth(SEED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(plain.iter().all(|s| s["reblog"].is_null()));
    assert!(plain.iter().all(|s| s["id"] != REBLOG_OUTER));
    // The pod sends include_reblogs=true, so its feed has the boost (as the inner post).
    let session = sign_in(&env).await;
    let (status, home) = get_home(&env.pod.url, Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    let order = home["queries"]["timeline:home"].as_array().unwrap();
    assert_eq!(order.len(), 5);
    assert_eq!(order[1], format!("pixelfed://posts/{REBLOG_INNER}"));
    assert_eq!(
        home["entities"][format!("pixelfed://posts/{REBLOG_INNER}")]["reblogged_by_uri"],
        "pixelfed://users/699112233445566778"
    );
}

#[tokio::test]
async fn expired_upstream_token_is_refreshed() {
    let dir = tempfile::tempdir().unwrap();
    let env = env_with(AppState::open_dir(dir.path()).unwrap()).await;
    let session = sign_in(&env).await;
    let stored = |dir: &std::path::Path| -> (String, Option<String>, Option<i64>, Option<String>) {
        let conn = rusqlite::Connection::open(dir.join("server.db")).unwrap();
        conn.query_row(
            "SELECT access_token, refresh_token, expires_at, redirect_uri FROM pixelfed_account",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    let (token1, refresh1, expires, redirect) = stored(dir.path());
    assert!(refresh1.is_some(), "the refresh token is stored");
    assert!(expires.unwrap() > 1_700_000_000 + 31_000_000);
    assert_eq!(redirect.as_deref(), Some(REDIRECT));

    // Every upstream access token expires; two concurrent feed fetches both
    // succeed through ONE refresh (a second refresh with the rotated refresh
    // token would fail and sign the user out).
    let fake = env.fake.fake.as_ref().unwrap();
    assert!(fake.expire_access_tokens() >= 1);
    let (a, b) = tokio::join!(
        get_home(&env.pod.url, Some(&session)),
        get_home(&env.pod.url, Some(&session))
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    let (token2, refresh2, _, _) = stored(dir.path());
    assert_ne!(token2, token1);
    assert_ne!(refresh2, refresh1);

    // Actions refresh too.
    fake.expire_access_tokens();
    let (status, body) = post_json(
        &format!("{}/v1/pixelfed/posts/like", env.pod.url),
        Some(&session),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_liked"], true);
    assert_ne!(stored(dir.path()).0, token2);
}
