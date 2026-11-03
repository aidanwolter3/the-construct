//! Pixelfed adapter integration tests: the pod and the fake upstream
//! (`crates/fake-pixelfed`) both run on random local ports.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use protocol::{ActionIntent, FieldUpdate, PresentationMode, UiNode, WorkspaceSpec};
use reqwest::{StatusCode, redirect};
use serde_json::{Value, json};
use server::WithPixelfed;
use server::pixelfed::db::{PixelfedAccount, PixelfedStore};
use server::session::session_hash;
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
    spawn_fake_with(false).await
}

/// `mastodon_threads`: `/context` answers with the whole thread (Mastodon), not one level
/// (Pixelfed).
async fn spawn_fake_with(mastodon_threads: bool) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fake_pixelfed::default_fixtures_dir(),
        public_url: url.clone(),
        auto_approve: true,
        seed_token: Some(SEED.to_owned()),
    })
    .unwrap()
    .with_mastodon_threads(mastodon_threads);
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
    env_with(server::state_in_memory()).await
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

/// Like [`get_home`], plus the `X-Construct-Session` response header.
async fn get_home_full(pod: &str, bearer: Option<&str>) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(format!("{pod}/v1/workspaces/pixelfed_home"));
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let session = resp
        .headers()
        .get("x-construct-session")
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
    authorize_at(&env.pod.url, &env.fake.url).await
}

/// [`authorize`] on pod `pod` for the fake at `instance`.
async fn authorize_at(pod: &str, instance: &str) -> (String, String) {
    authorize_with(pod, instance, REDIRECT).await
}

/// [`authorize_at`] with the client's `redirect` URI.
async fn authorize_with(pod: &str, instance: &str, redirect: &str) -> (String, String) {
    let (status, app) = post_json(
        &format!("{pod}/v1/auth/pixelfed/app"),
        None,
        json!({ "instance_url": instance, "redirect_uri": redirect }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{app}");
    assert_eq!(app["scopes"], "read write");
    assert_eq!(
        app["authorize_endpoint"],
        format!("{instance}/oauth/authorize")
    );
    let (verifier, challenge) = pkce();
    let resp = client()
        .get(app["authorize_endpoint"].as_str().unwrap())
        .query(&[
            ("response_type", "code"),
            ("client_id", app["client_id"].as_str().unwrap()),
            ("redirect_uri", redirect),
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
    assert!(
        location.as_str().starts_with(&format!("{redirect}?")),
        "{location}"
    );
    let q: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(q["state"], "st4te");
    (q["code"].clone(), verifier)
}

async fn sign_in(env: &Env) -> String {
    sign_in_at(&env.pod.url, &env.fake.url).await
}

/// [`sign_in`] on pod `pod` with the fake at `instance`.
async fn sign_in_at(pod: &str, instance: &str) -> String {
    let (code, verifier) = authorize_at(pod, instance).await;
    let (status, body) = post_json(
        &format!("{pod}/v1/auth/pixelfed/token"),
        None,
        json!({ "instance_url": instance, "code": code, "code_verifier": verifier,
                "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["instance_url"], instance);
    assert_eq!(body["account"]["id"], "703641122334455667");
    assert_eq!(body["account"]["username"], "constructdemo");
    assert_eq!(body["account"]["display_name"], "Construct Demo");
    assert!(
        body["account"]["avatar_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{pod}/v1/pixelfed/media/"))
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
    let UiNode::Workspace {
        id, title, slots, ..
    } = &spec.workspace
    else {
        panic!("not a workspace");
    };
    assert_eq!(id, "pixelfed_home");
    assert_eq!(title, "Pixelfed");
    let UiNode::Container { children, .. } = &slots[0].content else {
        panic!("signed-out slot is not a Container");
    };
    // The workspace title is the only "Pixelfed" heading (no duplicate Text).
    assert!(
        !children
            .iter()
            .any(|c| matches!(c, UiNode::Text { value_expr, .. } if value_expr == "Pixelfed")),
        "duplicate Pixelfed title Text"
    );
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
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
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

    // Posts carry their author's display fields (D36).
    let carousel = &spec.entities[&format!("pixelfed://posts/{CAROUSEL}")];
    assert_eq!(carousel.get_str("author_name"), Some("Alice Lumen"));
    assert_eq!(carousel.get_str("author_handle"), Some("@alice"));
    assert!(
        carousel
            .get_str("author_avatar_url")
            .unwrap()
            .starts_with("http://10.0.2.2:8080/v1/pixelfed/media/")
    );
    let boosted = &spec.entities[&format!("pixelfed://posts/{REBLOG_INNER}")];
    assert_eq!(boosted.get_str("reblogged_by_name"), Some("Bob 🚲"));
    let cw = spec
        .entities
        .values()
        .find(|e| e.get_str("content_warning").is_some())
        .unwrap();
    assert_eq!(cw.get_str("content_warning_label"), Some("CW: Snake photo"));

    // The layout is generic: core://Lists of posts (one per row, and the
    // two-column cards, D48) whose buttons hold the actions.
    let UiNode::Workspace { id, .. } = &spec.workspace else {
        panic!("not a workspace");
    };
    assert_eq!(id, "pixelfed_home");
    let feeds: Vec<u16> = spec
        .workspace
        .walk()
        .into_iter()
        .filter_map(|n| match n {
            UiNode::List {
                query_key, columns, ..
            } if query_key == "timeline:home" => Some(*columns),
            _ => None,
        })
        .collect();
    assert_eq!(feeds, [0, 2]);
    assert!(
        spec.workspace
            .walk()
            .iter()
            .all(|n| !matches!(n, UiNode::Unknown(_)))
    );
    let button = |tag: &str| {
        spec.workspace
            .walk()
            .into_iter()
            .find_map(|n| match n {
                UiNode::IconButton {
                    tag: Some(t),
                    on_click,
                    ..
                } if t == tag => Some(on_click.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button {tag}"))
    };
    assert_eq!(
        button("like:$item.uri"),
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
        button("bookmark:$item.uri"),
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
        button("comment:$item.uri"),
        ActionIntent::Navigate {
            target_uri: "pixelfed://posts/$item.id".into(),
            presentation: PresentationMode::Push,
        }
    );
}

#[tokio::test]
async fn pixelfed_home_signed_out_without_valid_session() {
    let env = env().await;
    // No bearer: 200. A bearer that is not a session: 401 + `X-Construct-Session: invalid`, so the
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
                instance_url: env.fake.url.clone(),
                app_path: "/v1/auth/pixelfed/app".into(),
                token_path: "/v1/auth/pixelfed/token".into(),
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

#[tokio::test]
async fn sign_ins_lists_the_pixelfed_account_with_its_server() {
    let env = env().await;
    let list = |bearer: Option<String>| {
        let url = format!("{}/v1/auth/sign-ins", env.pod.url);
        async move {
            let mut req = client().get(url);
            if let Some(b) = bearer {
                req = req.bearer_auth(b);
            }
            let resp = req.send().await.unwrap();
            let status = resp.status();
            (status, resp.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (status, body) = list(None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "sign_ins": [] }));
    let (status, _) = list(Some("stale".into())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let session = sign_in(&env).await;
    let (status, body) = list(Some(session.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "sign_ins": [{
            "id": "pixelfed",
            "title": "Pixelfed",
            "server": env.fake.url,
            "username": "constructdemo",
            "apps": [{ "id": "pixelfed_home", "title": "Pixelfed" }],
        }] })
    );
    let logout = format!("{}/v1/auth/logout", env.pod.url);
    post_json(&logout, Some(&session), json!({})).await;
    let (status, _) = list(Some(session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the session ended");
}

/// A pod whose store already has a session (signed in to Pixelfed) + upstream
/// account for `instance`.
fn seeded_state(instance: &str, upstream_token: &str) -> AppState {
    let state = server::state_in_memory().with_pixelfed(PixelfedConfig::new(instance, None));
    state.store().create_session(&session_hash("tok")).unwrap();
    state
        .store()
        .attach_sign_in(&session_hash("tok"), server::pixelfed::SIGN_IN)
        .unwrap();
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
    // The dead upstream token was dropped, so actions now need a sign-in:
    // 403 for the (still valid) pod session, which the client keeps (D46).
    let (status, body) = post_json(
        &format!("{}/v1/pixelfed/posts/like", pod.url),
        Some("tok"),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "sign_in_required");
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
async fn signed_in_feed_layout_is_versioned_signed_out_is_not() {
    let fake = spawn_fake().await;
    let state = Arc::new(seeded_state(&fake.url, SEED));
    let pod = serve(server::app(state.clone())).await;
    let version = |resp: &reqwest::Response| {
        resp.headers()
            .get("x-construct-layout-version")
            .map(|v| v.to_str().unwrap().to_owned())
    };
    let url = format!("{}/v1/workspaces/pixelfed_home", pod.url);
    let resp = client().get(&url).bearer_auth("tok").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // No schema header: a client from before screens gets the newest version it can
    // draw (0001; 0002 adds the comments screen, 0003 the generic nodes, 0004
    // the cards view and the post screen).
    assert_eq!(version(&resp).as_deref(), Some("1"));
    let resp = client()
        .get(&url)
        .bearer_auth("tok")
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(version(&resp).as_deref(), Some("8"));
    let spec: WorkspaceSpec = resp.json().await.unwrap();
    assert!(matches!(&spec.workspace, UiNode::Workspace { screens, .. } if screens.len() == 4));
    // A newer version replaces the feed layout; entities still come upstream.
    let mut node = server::layouts::builtin(&server::catalog(), "pixelfed_home").unwrap();
    if let UiNode::Workspace { title, .. } = &mut node {
        *title = "Custom".into();
    }
    server::layouts::append_version(state.store(), "pixelfed_home", &node, None).unwrap();
    let resp = client().get(&url).bearer_auth("tok").send().await.unwrap();
    assert_eq!(version(&resp).as_deref(), Some("9"));
    let spec: WorkspaceSpec = resp.json().await.unwrap();
    assert_eq!(spec.workspace, node);
    assert_eq!(spec.queries["timeline:home"].len(), 5);
    // Signed out (no bearer, or a stale one): unversioned.
    let resp = client().get(&url).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(version(&resp), None);
    let resp = client()
        .get(&url)
        .bearer_auth("stale")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(version(&resp), None);
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
    let state = server::state_in_memory().with_pixelfed(PixelfedConfig {
        media_hosts: vec![origin.url.trim_start_matches("http://").to_owned()],
        ..PixelfedConfig::default()
    });
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
    for ((key, _), want) in urls.iter().zip([
        "application/octet-stream",
        "application/octet-stream",
        "image/jpeg",
    ]) {
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
    let env = env_with(server::open_dir(dir.path()).unwrap()).await;
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
        (env.fake.url.as_str(), "constructdemo")
    );
}

/// Staging's upgrade (D40): a database from before sign-ins existed (v10, a
/// signed-in session) keeps its session signed in to Pixelfed after v11.
#[tokio::test]
async fn a_session_from_before_v11_stays_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let env = env_with(server::open_dir(dir.path()).unwrap()).await;
    let session = sign_in(&env).await;
    env.pod.handle.abort();
    {
        // Back to the v10 shape: no sign-in rows, no adapter versions, and
        // none of the tables adapters added later.
        let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
        conn.execute_batch(
            "DROP TABLE session_sign_ins; DROP TABLE adapter_migrations;
             ALTER TABLE sessions DROP COLUMN pinned; ALTER TABLE app_versions DROP COLUMN features;
             PRAGMA user_version = 10;",
        )
        .unwrap();
        let later: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND (name LIKE 'recipes%' OR name IN ('pixelfed_session_instance', 'pixelfed_pending_posts'))
                 OR (type = 'table' AND name IN ('pod_settings', 'uploads', 'notifications',
                     'notification_polls', 'devices', 'device_channel_prefs',
                     'pixelfed_notify_cursor'))",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        for t in later {
            conn.execute_batch(&format!("DROP TABLE {t}")).unwrap();
        }
    }
    let state = server::open_dir(dir.path())
        .unwrap()
        .with_pixelfed(PixelfedConfig::new(&env.fake.url, None));
    assert_eq!(
        state
            .store()
            .session_sign_ins(&server::session::session_hash(&session))
            .unwrap(),
        [server::pixelfed::SIGN_IN]
    );
    let pod = spawn_pod(state).await;
    let (status, body) = get_home(&pod.url, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["queries"]["timeline:home"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Staging's upgrade to sealed credentials: a sign-in stored in plaintext
/// (no `CONSTRUCT_SECRETS_KEY`) is sealed when the pod opens with a key and
/// keeps working; new sign-ins are sealed; the file is private.
#[tokio::test]
async fn plaintext_credentials_are_sealed_when_a_key_is_set() {
    use server::secrets::Secrets;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.db");
    let plain = server::state_new(
        server::store::Store::open(&path)
            .unwrap()
            .with_secrets(Secrets::none()),
    );
    let env = env_with(plain).await;
    let session = sign_in(&env).await;
    env.pod.handle.abort();
    let raw = |what: &str| -> String {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.query_row(&format!("SELECT {what} FROM pixelfed_account"), [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let token = raw("access_token");
    assert!(!Secrets::is_sealed(&token));

    let key = Secrets::from_keys(&[[42; 32]]);
    let state = server::state_new(server::store::Store::open(&path).unwrap().with_secrets(key))
        .with_pixelfed(PixelfedConfig::new(&env.fake.url, None));
    for col in ["access_token", "refresh_token"] {
        let v = raw(col);
        assert!(Secrets::is_sealed(&v) && !v.contains(&token), "{col}: {v}");
    }
    let secret: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT client_secret FROM pixelfed_apps", [], |r| r.get(0))
        .unwrap();
    assert!(Secrets::is_sealed(&secret));
    let pod = spawn_pod(state).await;
    let (status, body) = get_home(&pod.url, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["queries"]["timeline:home"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A new sign-in (registered app reused, its secret opened) is sealed too.
    sign_in_at(&pod.url, &env.fake.url).await;
    assert!(Secrets::is_sealed(&raw("access_token")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

/// Signing in again with a valid session keeps that session (one session
/// collects every sign-in, D40); without one, a new session is made.
#[tokio::test]
async fn signing_in_with_a_session_keeps_it() {
    let env = env().await;
    let first = sign_in(&env).await;
    let (code, verifier) = authorize(&env).await;
    let (status, body) = post_json(
        &format!("{}/v1/auth/pixelfed/token", env.pod.url),
        Some(&first),
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session_token"], first.as_str());
    let second = sign_in(&env).await;
    assert_ne!(second, first);
    for s in [&first, &second] {
        assert_eq!(get_home(&env.pod.url, Some(s)).await.0, StatusCode::OK);
    }
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
    let pod = spawn_pod(server::state_in_memory().with_pixelfed(PixelfedConfig::new(
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
    // The upstream's error body stays in the pod's log (L4, #31).
    assert!(body.get("detail").is_none(), "{body}");
    assert!(!body.to_string().contains("code_verifier"), "{body}");
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
    let pod = spawn_pod(server::state_in_memory()).await;
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
    let env = env_with(server::open_dir(dir.path()).unwrap()).await;
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

// ---------- M6 review fixes ----------

const FAKE_ACCOUNT: &str = "703641122334455667";

/// Fake + pod with a tweaked config; `seed` runs on the pod state (with the
/// fake's URL as the instance) before the pod starts.
async fn env_custom(
    state: AppState,
    cfg: impl FnOnce(&mut PixelfedConfig),
    seed: impl FnOnce(&AppState, &str),
) -> Env {
    let fake = spawn_fake().await;
    let mut config = PixelfedConfig::new(&fake.url, None);
    cfg(&mut config);
    let state = state.with_pixelfed(config);
    seed(&state, &fake.url);
    let pod = spawn_pod(state).await;
    Env { fake, pod }
}

/// Pin another account (id 999, upstream token SEED) as the owner, with pod
/// session "tok".
fn seed_other_owner(state: &AppState, instance: &str) {
    let other = PixelfedAccount {
        access_token: SEED.into(),
        account_id: "999".into(),
        username: "owner".into(),
        ..Default::default()
    };
    let r = state
        .store()
        .sign_in_pixelfed(instance, &other, &session_hash("tok"), false, 86_400)
        .unwrap();
    assert_eq!(r, server::pixelfed::db::SignIn::Ok);
}

async fn exchange(env: &Env) -> (StatusCode, Value) {
    let (code, verifier) = authorize(env).await;
    post_json(
        &format!("{}/v1/auth/pixelfed/token", env.pod.url),
        None,
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": REDIRECT }),
    )
    .await
}

#[tokio::test]
async fn another_account_cannot_take_over_the_pod() {
    let env = env_custom(server::state_in_memory(), |_| {}, seed_other_owner).await;
    let (status, body) = exchange(&env).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "account_not_owner");
    assert!(body.get("session_token").is_none());
    // The owner's stored account (and session) still work.
    let (status, home) = get_home(&env.pod.url, Some("tok")).await;
    assert_eq!(status, StatusCode::OK, "{home}");
    assert_eq!(
        home["queries"]["timeline:home"].as_array().unwrap().len(),
        5
    );
}

#[tokio::test]
async fn the_owner_may_sign_in_again() {
    let env = env().await;
    let s1 = sign_in(&env).await; // first sign-in pins the owner
    let s2 = sign_in(&env).await; // same account: fine
    assert_ne!(s1, s2);
    for s in [&s1, &s2] {
        assert_eq!(get_home(&env.pod.url, Some(s)).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn explicit_owner_and_allow_change_overrides() {
    // PIXELFED_OWNER_ACCOUNT_ID names someone else: 403 even on a fresh pod.
    let env = env_custom(
        server::state_in_memory(),
        |c| c.owner_account_id = Some("999".into()),
        |_, _| {},
    )
    .await;
    let (status, body) = exchange(&env).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "account_not_owner");

    // PIXELFED_OWNER_ACCOUNT_ID names the signing-in account: it replaces a pinned owner.
    let env = env_custom(
        server::state_in_memory(),
        |c| c.owner_account_id = Some(FAKE_ACCOUNT.into()),
        seed_other_owner,
    )
    .await;
    sign_in(&env).await;

    // PIXELFED_ALLOW_ACCOUNT_CHANGE=1: any account replaces the stored one.
    let dir = tempfile::tempdir().unwrap();
    let env = env_custom(
        server::open_dir(dir.path()).unwrap(),
        |c| c.allow_account_change = true,
        seed_other_owner,
    )
    .await;
    sign_in(&env).await;
    let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
    let ids: (String, String) = conn
        .query_row(
            "SELECT a.account_id, o.account_id FROM pixelfed_account a JOIN pixelfed_owner o USING (instance)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ids, (FAKE_ACCOUNT.to_owned(), FAKE_ACCOUNT.to_owned()));
}

#[tokio::test]
async fn sessions_expire_after_the_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let env = env_with(server::open_dir(dir.path()).unwrap()).await;
    let session = sign_in(&env).await;
    assert_eq!(
        get_home(&env.pod.url, Some(&session)).await.0,
        StatusCode::OK
    );
    // Age the session past the default 90 days.
    let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
    conn.execute(
        "UPDATE sessions SET created_at = created_at - 91 * 86400",
        [],
    )
    .unwrap();
    let (status, header, _) = get_home_full(&env.pod.url, Some(&session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    let (status, _) = post_json(
        &format!("{}/v1/pixelfed/posts/like", env.pod.url),
        Some(&session),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The next sign-in prunes it.
    sign_in(&env).await;
    let n: i64 = conn
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn app_registration_only_for_allowed_redirect_uris() {
    let env = env_custom(
        server::state_in_memory(),
        |c| {
            c.extra_redirects = vec![
                "myapp://signed-in".into(),
                "https://pod.example/oauth/android".into(),
            ]
        },
        |_, _| {},
    )
    .await;
    let app_url = format!("{}/v1/auth/pixelfed/app", env.pod.url);
    for bad in [
        "http://evil.example/callback",
        "http://127.0.0.1:9/cb",
        "http://127.0.0.1:0/callback",
        "http://127.0.0.1:99999/callback",
        "http://127.0.0.1:9/callback?x=1",
        "http://127.0.0.1:9/callback/",
        "https://127.0.0.1:9/callback",
        "http://127.0.0.1.evil.example:9/callback",
        "http://localhost:9@evil.example/callback",
        "construct://oauth/x",
        "myapp://signed-in/x",
        "https://pod.example/oauth/android/x",
        "https://pod.example/oauth/android?x=1",
        "https://evil.example/oauth/android",
        "http://pod.example/oauth/android",
    ] {
        let (status, body) = post_json(
            &app_url,
            None,
            json!({ "instance_url": env.fake.url, "redirect_uri": bad }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], "redirect_uri_not_allowed", "{bad}");
    }
    for ok in [
        "http://127.0.0.1:47823/callback",
        "http://localhost:65535/callback",
        "construct://oauth",
        "myapp://signed-in",
        "https://pod.example/oauth/android",
    ] {
        let (status, body) = post_json(
            &app_url,
            None,
            json!({ "instance_url": env.fake.url, "redirect_uri": ok }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ok}: {body}");
    }
}

/// The Android app's App Link redirect (decision
/// `android-app-link-sign-in-redirect`): an https URI the deployment lists in
/// `PIXELFED_EXTRA_REDIRECTS` signs in end to end, and the code is bound to
/// it (the token call must name the same redirect).
#[tokio::test]
async fn sign_in_with_an_app_link_redirect() {
    const APP_LINK: &str = "https://pod.example/oauth/android";
    let env = env_custom(
        server::state_in_memory(),
        |c| c.extra_redirects = vec![APP_LINK.into()],
        |_, _| {},
    )
    .await;
    let token_url = format!("{}/v1/auth/pixelfed/token", env.pod.url);
    let (code, verifier) = authorize_with(&env.pod.url, &env.fake.url, APP_LINK).await;
    let (status, body) = post_json(
        &token_url,
        None,
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": "construct://oauth" }),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let (code, verifier) = authorize_with(&env.pod.url, &env.fake.url, APP_LINK).await;
    let (status, body) = post_json(
        &token_url,
        None,
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": APP_LINK }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["username"], "constructdemo");
}

#[tokio::test]
async fn stale_cached_app_is_re_registered() {
    let env = env().await;
    let app_url = format!("{}/v1/auth/pixelfed/app", env.pod.url);
    let body = json!({ "instance_url": env.fake.url, "redirect_uri": REDIRECT });
    let (_, a1) = post_json(&app_url, None, body.clone()).await;
    let fake = env.fake.fake.as_ref().unwrap();
    // A cached app the upstream still knows is reused (validated).
    let (_, a2) = post_json(&app_url, None, body.clone()).await;
    assert_eq!(a1["client_id"], a2["client_id"]);
    assert_eq!(fake.client_credentials_grants(), 1);
    // The instance forgets it: the next /app call registers a new one, and
    // the whole sign-in works with it.
    assert_eq!(fake.forget_apps(), 1);
    let (status, a3) = post_json(&app_url, None, body).await;
    assert_eq!(status, StatusCode::OK, "{a3}");
    assert_ne!(a3["client_id"], a1["client_id"]);
    let session = sign_in(&env).await;
    assert_eq!(
        get_home(&env.pod.url, Some(&session)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn cached_app_is_kept_when_validation_is_inconclusive() {
    let dead = dead_url().await;
    let state = seeded_state(&dead, SEED);
    let app = server::pixelfed::db::PixelfedApp {
        client_id: "cached-1".into(),
        client_secret: "s".into(),
    };
    state
        .store()
        .save_pixelfed_app(&dead, REDIRECT, &app)
        .unwrap();
    let pod = spawn_pod(state).await;
    let (status, body) = post_json(
        &format!("{}/v1/auth/pixelfed/app", pod.url),
        None,
        json!({ "instance_url": dead, "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["client_id"], "cached-1");
}

#[tokio::test]
async fn media_proxy_ssrf_guards() {
    use axum::{body::Body, http::header, response::Redirect, routing::get};
    const BIG: usize = server::pixelfed::routes::MAX_MEDIA_BYTES as usize + 1;
    let outside = serve(axum::Router::new().route(
        "/img.jpg",
        get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], "secret") }),
    ))
    .await;
    let out_url = format!("{}/img.jpg", outside.url);
    let redirect_out = out_url.clone();
    let origin = serve(
        axum::Router::new()
            .route(
                "/img.jpg",
                get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], "jpg") }),
            )
            .route(
                "/redir-ok",
                get(|| async { Redirect::temporary("/img.jpg") }),
            )
            .route(
                "/redir-out",
                get(move || {
                    let to = redirect_out.clone();
                    async move { Redirect::temporary(&to) }
                }),
            )
            .route(
                "/big",
                get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], vec![0u8; BIG]) }),
            )
            .route(
                "/big-stream",
                get(|| async {
                    // Chunked (no Content-Length): 26 x 1 MiB.
                    let chunks = (0..26).map(|_| Ok::<_, std::io::Error>(vec![0u8; 1 << 20]));
                    (
                        [(header::CONTENT_TYPE, "image/jpeg")],
                        Body::from_stream(futures_util::stream::iter(chunks)),
                    )
                }),
            ),
    )
    .await;
    let state = server::state_in_memory().with_pixelfed(PixelfedConfig {
        media_hosts: vec![origin.url.trim_start_matches("http://").to_owned()],
        ..PixelfedConfig::default()
    });
    let mut urls: Vec<(String, String)> = ["img.jpg", "redir-ok", "redir-out", "big", "big-stream"]
        .iter()
        .map(|p| format!("{}/{p}", origin.url))
        .chain([out_url.clone()])
        .map(|u| (server::pixelfed::media_key(&u), u))
        .collect();
    // Keys stored for a disallowed URL (e.g. by an older pod) are refused too.
    state
        .store()
        .save_media_keys(urls.iter().map(|(k, u)| (k, u)))
        .unwrap();
    let pod = spawn_pod(state).await;
    let fetch = |key: String| {
        let pod = pod.url.clone();
        async move {
            client()
                .get(format!("{pod}/v1/pixelfed/media/{key}"))
                .send()
                .await
                .unwrap()
        }
    };
    let (out_key, _) = urls.pop().unwrap();
    let resp = fetch(out_key).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"],
        "media_host_not_allowed"
    );

    let key = |i: usize| urls[i].0.clone();
    let resp = fetch(key(0)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "jpg");
    // A redirect within the allowlist is followed; one leaving it is not.
    let resp = fetch(key(1)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "jpg");
    let resp = fetch(key(2)).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(!resp.text().await.unwrap().contains("secret"));
    // Too large: refused up front by Content-Length, or cut off while streaming.
    let resp = fetch(key(3)).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"],
        "media_too_large"
    );
    let resp = fetch(key(4)).await;
    // Aborted mid-body (Err) is expected; a complete body must be under the cap.
    if let Ok(b) = resp.bytes().await {
        assert!(b.len() < BIG, "streamed {} bytes past the cap", b.len());
    }
}

/// A 401 that survives refresh only deletes the stored account if it still
/// holds the token that failed (compare-and-delete).
#[tokio::test]
async fn surviving_401_does_not_delete_a_replaced_account() {
    use axum::{extract::State, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_url = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(seeded_state(&upstream_url, "tok-0"));
    // Each timeline call stores a newer token (as a concurrent sign-in would)
    // and then rejects the request.
    #[derive(Clone)]
    struct Mock(Arc<AppState>, String, Arc<AtomicUsize>);
    let mock = Mock(
        state.clone(),
        upstream_url.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let router = axum::Router::new()
        .route(
            "/api/v1/timelines/home",
            get(|State(Mock(pod, instance, n)): State<Mock>| async move {
                let next = n.fetch_add(1, Ordering::SeqCst) + 1;
                pod.store()
                    .save_pixelfed_account(
                        &instance,
                        &PixelfedAccount {
                            access_token: format!("tok-{next}"),
                            account_id: "1".into(),
                            username: "x".into(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                (StatusCode::UNAUTHORIZED, "{\"error\":\"Unauthenticated.\"}")
            }),
        )
        .with_state(mock.clone());
    let _upstream = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let pod = serve(server::app(state.clone())).await;
    let (status, header, _) = get_home_full(&pod.url, Some("tok")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    // tok-0 failed, the retry with tok-1 failed, but tok-2 was stored meanwhile: kept.
    assert_eq!(mock.2.load(Ordering::SeqCst), 2);
    let stored = state.store().pixelfed_account(&upstream_url).unwrap();
    assert_eq!(stored.map(|a| a.access_token).as_deref(), Some("tok-2"));
}

async fn get_comments_screen(pod: &str, bearer: Option<&str>, id: &str) -> (StatusCode, Value) {
    let mut req = client().get(format!("{pod}/v1/pixelfed/screens/comments?post_id={id}"));
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn comments_screen_data_is_a_bundle_in_thread_order() {
    let env = env().await;
    let session = sign_in(&env).await;
    let (status, body) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle: protocol::DataBundle = serde_json::from_value(body).unwrap();
    let order = &bundle.queries[&format!("comments:{CAROUSEL}")];
    let shape: Vec<(String, i64)> = order
        .iter()
        .map(|u| (u.clone(), bundle.entities[u].get_i64("depth").unwrap()))
        .collect();
    let want = [
        ("101", 0),
        ("102", 1),
        ("103", 2),
        ("104", 0),
        ("105", 1),
        ("106", 0),
        ("107", 0),
    ]
    .map(|(id, d)| (format!("pixelfed://comments/781300000000000{id}"), d));
    assert_eq!(shape, want);
    // The comments, plus the post, its author and its media (shown above them).
    let comments = bundle
        .entities
        .keys()
        .filter(|u| u.starts_with("pixelfed://comments/"))
        .count();
    assert_eq!(comments, 7);
    let first = &bundle.entities[&order[0]];
    assert_eq!(
        first.get_str("text"),
        Some("That second frame is unreal 😍")
    );
    assert_eq!(first.get_str("post_id"), Some(CAROUSEL));
    assert_eq!(first.get_str("in_reply_to_id"), Some(CAROUSEL));
    assert_eq!(first.get_str("author_name"), Some("Bob 🚲"));
    assert_eq!(first.get_str("author_handle"), Some("@bob"));
    assert!(first.get_str("meta").unwrap().starts_with("@bob · "));
    assert_eq!(first.get_i64("like_count"), Some(3));
    assert_eq!(first.get_bool("is_own"), Some(false));
    assert!(
        first
            .get_str("avatar_url")
            .unwrap()
            .starts_with(&format!("{}/v1/pixelfed/media/", env.pod.url))
    );
    assert_eq!(bundle.entities[&order[5]].get_bool("is_own"), Some(true));
    assert_eq!(
        bundle.entities[&order[6]].get_str("text"),
        Some("Number 3 for me.\nThat blue!")
    );
    // A boost's id gives the boosted post's thread.
    let (_, boosted) = get_comments_screen(&env.pod.url, Some(&session), REBLOG_OUTER).await;
    assert_eq!(
        boosted["queries"][format!("comments:{REBLOG_OUTER}")]
            .as_array()
            .unwrap()
            .len(),
        13
    );
    // Unknown post: 404; bad or missing id: 400; no session: 401.
    let (status, body) = get_comments_screen(&env.pod.url, Some(&session), "999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "post_not_found");
    for bad in ["a.b", ""] {
        let (status, _) = get_comments_screen(&env.pod.url, Some(&session), bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    for bearer in [None, Some("bogus")] {
        let (status, _) = get_comments_screen(&env.pod.url, bearer, CAROUSEL).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

/// The comments screen shows its post; asked for a comment (an older
/// notification's link), it shows the whole thread of the comment's post.
#[tokio::test]
async fn comments_screen_data_has_the_post_even_from_a_comment() {
    let env = env().await;
    let session = sign_in(&env).await;
    let post_uri = format!("pixelfed://posts/{CAROUSEL}");
    let (status, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(
        data["queries"][format!("post:{CAROUSEL}")],
        json!([post_uri])
    );
    assert!(
        data["entities"][&post_uri]["caption"]
            .as_str()
            .unwrap()
            .starts_with("Sunrise over Boulder"),
        "{data}"
    );
    let whole = thread_shape(&data, CAROUSEL);
    assert_eq!(whole.len(), 7);

    // 103 replies to 102, which replies to 101, a comment on the post.
    let reply = "781300000000000103";
    let (status, data) = get_comments_screen(&env.pod.url, Some(&session), reply).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(data["queries"][format!("post:{reply}")], json!([post_uri]));
    assert_eq!(thread_shape(&data, reply), whole);
    let first = data["queries"][format!("comments:{reply}")][0]
        .as_str()
        .unwrap();
    assert_eq!(data["entities"][first]["post_id"], CAROUSEL);

    // Liking a comment uses the posts' like endpoint.
    let (status, body) = post_json(
        &format!("{}/v1/pixelfed/posts/like", env.pod.url),
        Some(&session),
        json!({ "id": "781300000000000101", "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (&body["is_liked"], &body["like_count"]),
        (&json!(true), &json!(4))
    );
    let (_, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    let liked = &data["entities"]["pixelfed://comments/781300000000000101"];
    assert_eq!(
        (&liked["is_liked"], &liked["like_count"]),
        (&json!(true), &json!(4))
    );
}

/// The `/context` paths the fake was asked for, sorted.
fn context_calls(fake: &fake_pixelfed::AppState) -> Vec<String> {
    let mut calls: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/context"))
        .map(|r| r.path)
        .collect();
    calls.sort();
    calls
}

/// (comment id suffix, depth) of the comments screen's list.
fn thread_shape(data: &Value, post: &str) -> Vec<(String, i64)> {
    data["queries"][format!("comments:{post}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let u = u.as_str().unwrap();
            let id = u.rsplit('/').next().unwrap();
            let depth = data["entities"][u]["depth"].as_i64().unwrap();
            (id.trim_start_matches("781300000000000").to_owned(), depth)
        })
        .collect()
}

#[tokio::test]
async fn replies_to_comments_are_fetched_one_level_at_a_time() {
    let want: Vec<(String, i64)> = [
        ("101", 0),
        ("102", 1),
        ("103", 2),
        ("104", 0),
        ("105", 1),
        ("106", 0),
        ("107", 0),
    ]
    .map(|(id, d)| (id.to_owned(), d))
    .to_vec();
    let path = |id: &str| format!("/api/v1/statuses/{id}/context");
    for mastodon in [false, true] {
        let fake = spawn_fake_with(mastodon).await;
        let pod = spawn_pod(
            server::state_in_memory().with_pixelfed(PixelfedConfig::new(&fake.url, None)),
        )
        .await;
        let env = Env { fake, pod };
        let session = sign_in(&env).await;
        let fake = env.fake.fake.as_ref().unwrap();
        let (status, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        assert_eq!(thread_shape(&data, CAROUSEL), want, "mastodon: {mastodon}");
        let calls = context_calls(fake);
        if mastodon {
            // The whole thread came at once: no extra calls.
            assert_eq!(calls, [path(CAROUSEL)]);
        } else {
            // The post, then each comment with replies (101 -> 102 -> 103; 104 -> 105).
            let mut expected = vec![
                path(CAROUSEL),
                path("781300000000000101"),
                path("781300000000000102"),
                path("781300000000000104"),
            ];
            expected.sort();
            assert_eq!(calls, expected);
        }
    }

    // A reply to a reply shows up under it; a failed call loses only that branch.
    let env = env().await;
    let session = sign_in(&env).await;
    let fake = env.fake.fake.as_ref().unwrap();
    let (status, reply) = post_json(
        &format!("{}/v1/pixelfed/posts/comment", env.pod.url),
        Some(&session),
        json!({ "id": "781300000000000105", "text": "Agreed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let new_id = reply["comment"]["id"].as_str().unwrap();
    let new_id = new_id.trim_start_matches("781300000000000").to_owned();
    let (_, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    let shape = thread_shape(&data, CAROUSEL);
    let pos = shape.iter().position(|(id, _)| id == "105").unwrap();
    assert_eq!(shape[pos + 1], (new_id.clone(), 2));
    fake.add_faults([fake_pixelfed::FaultRule::status(503).path(&path("781300000000000101"))]);
    let (status, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    let ids: Vec<String> = thread_shape(&data, CAROUSEL)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, ["101", "104", "105", new_id.as_str(), "106", "107"]);
}

#[tokio::test]
async fn posting_a_comment_reaches_the_upstream() {
    let env = env().await;
    let session = sign_in(&env).await;
    let fake = env.fake.fake.as_ref().unwrap();
    let url = format!("{}/v1/pixelfed/posts/comment", env.pod.url);
    let (status, body) = post_json(
        &url,
        Some(&session),
        json!({ "id": CAROUSEL, "text": "  Lovely <light>  ", "client_id": "nonce-0001" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["id"], CAROUSEL);
    assert_eq!(body["comment"]["text"], "Lovely <light>");
    assert_eq!(body["comment"]["is_own"], true);
    assert_eq!(body["comment"]["author_handle"], "@constructdemo");
    let new_id = body["comment"]["id"].as_str().unwrap().to_owned();
    assert_eq!(fake.created_ids(), std::slice::from_ref(&new_id));
    assert_eq!(upstream_status(&env, CAROUSEL).await["replies_count"], 5);

    // A retry with the same client_id does not post twice.
    let (status, again) = post_json(
        &url,
        Some(&session),
        json!({ "id": CAROUSEL, "text": "Lovely <light>", "client_id": "nonce-0001" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["comment"]["id"], new_id.as_str());
    assert_eq!(fake.created_ids().len(), 1);

    // It is in the screen data; the feed's reply_count follows.
    let (_, data) = get_comments_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    let order = data["queries"][format!("comments:{CAROUSEL}")]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        order.last().unwrap(),
        &json!(format!("pixelfed://comments/{new_id}"))
    );
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    assert_eq!(
        home["entities"][format!("pixelfed://posts/{CAROUSEL}")]["reply_count"],
        5
    );

    // A reply to a comment, without a client_id.
    let (status, reply) = post_json(
        &url,
        Some(&session),
        json!({ "id": "781300000000000104", "text": "Same question" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["comment"]["in_reply_to_id"], "781300000000000104");

    // Rejections: unknown post 404 (the client rolls back), bad input 400, no session 401.
    let (status, body) = post_json(
        &url,
        Some(&session),
        json!({ "id": "999", "text": "hello" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "post_not_found");
    for bad in [
        json!({ "id": CAROUSEL, "text": "   " }),
        json!({ "id": CAROUSEL, "text": "x".repeat(501) }),
        json!({ "id": CAROUSEL }),
        json!({ "text": "hi" }),
        json!({ "id": "../x", "text": "hi" }),
        json!({ "id": CAROUSEL, "text": "hi", "client_id": "short" }),
        json!({ "id": CAROUSEL, "text": "hi", "client_id": "bad key with spaces" }),
    ] {
        let (status, _) = post_json(&url, Some(&session), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = post_json(&url, None, json!({ "id": CAROUSEL, "text": "hi" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(fake.created_ids().len(), 2);
}

#[tokio::test]
async fn comments_with_the_upstream_down_are_502() {
    let dead = dead_url().await;
    let pod = spawn_pod(seeded_state(&dead, SEED)).await;
    let (status, _) = get_comments_screen(&pod.url, Some("tok"), CAROUSEL).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _) = post_json(
        &format!("{}/v1/pixelfed/posts/comment", pod.url),
        Some("tok"),
        json!({ "id": CAROUSEL, "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

/// The `url` of the carousel post in a signed-in feed (it names the instance).
fn carousel_url(feed: &Value) -> String {
    feed["entities"][format!("pixelfed://posts/{CAROUSEL}")]["url"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Whether the fake at `url` has the carousel post favourited.
async fn carousel_favourited(url: &str) -> Value {
    let list: Vec<Value> = client()
        .get(format!("{url}/api/v1/timelines/home"))
        .bearer_auth(SEED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    list.into_iter()
        .find(|s| s["id"] == CAROUSEL)
        .map(|s| s["favourited"].clone())
        .unwrap()
}

/// D45: two sessions on one pod, each on its own instance (the default and a
/// listed one): feeds and likes stay apart.
#[tokio::test]
async fn sessions_use_the_instance_they_signed_in_to() {
    let (a, b) = (spawn_fake().await, spawn_fake().await);
    let pod = spawn_pod(server::state_in_memory().with_pixelfed(PixelfedConfig {
        allowed_instances: vec![b.url.clone()],
        ..PixelfedConfig::new(&a.url, None)
    }))
    .await;
    let on_a = sign_in_at(&pod.url, &a.url).await;
    let on_b = sign_in_at(&pod.url, &b.url).await;
    for (session, fake) in [(&on_a, &a), (&on_b, &b)] {
        let (status, feed) = get_home(&pod.url, Some(session)).await;
        assert_eq!(status, StatusCode::OK, "{feed}");
        let url = carousel_url(&feed);
        assert!(url.starts_with(&fake.url), "{url} is not from {}", fake.url);
    }
    // A like through the session on B reaches B only.
    let (status, body) = post_json(
        &format!("{}/v1/pixelfed/posts/like", pod.url),
        Some(&on_b),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(carousel_favourited(&b.url).await, true);
    assert_eq!(carousel_favourited(&a.url).await, false);
}

#[tokio::test]
async fn signed_out_screen_offers_a_server_box_only_when_allowed() {
    let fake = spawn_fake().await;
    let children = |spec: &WorkspaceSpec| -> Vec<UiNode> {
        let UiNode::Workspace { slots, .. } = &spec.workspace else {
            panic!("not a workspace");
        };
        let UiNode::Container { children, .. } = &slots[0].content else {
            panic!("signed-out slot is not a Container");
        };
        children.clone()
    };
    let closed =
        spawn_pod(server::state_in_memory().with_pixelfed(PixelfedConfig::new(&fake.url, None)))
            .await;
    let (_, body) = get_home(&closed.url, None).await;
    let nodes = children(&serde_json::from_value(body).unwrap());
    assert!(
        !nodes.iter().any(|c| matches!(c, UiNode::TextField { .. })),
        "a pod for one instance has no server box"
    );
    let open = spawn_pod(server::state_in_memory().with_pixelfed(PixelfedConfig {
        any_instance: true,
        ..PixelfedConfig::new(&fake.url, None)
    }))
    .await;
    let (_, body) = get_home(&open.url, None).await;
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    // The default button still comes first.
    let (label, _) = signed_out_button(&spec);
    assert_eq!(label, "Sign in with Pixelfed");
    let nodes = children(&spec);
    assert!(nodes.iter().any(|c| matches!(c,
        UiNode::TextField { field, placeholder, .. }
            if field == "instance" && placeholder.contains("pixelfed.social"))));
    assert!(nodes.iter().any(|c| matches!(c,
        UiNode::Button { on_click: ActionIntent::AuthLogin { instance_url, .. }, .. }
            if instance_url == "$form.instance")));
}

/// D45: under `*` the pod accepts only public HTTPS names; private hosts,
/// IP literals and plain HTTP are refused before any upstream call.
#[tokio::test]
async fn any_instance_refuses_private_and_plain_http_hosts() {
    let fake = spawn_fake().await;
    let pod = spawn_pod(server::state_in_memory().with_pixelfed(PixelfedConfig {
        any_instance: true,
        ..PixelfedConfig::new(&fake.url, None)
    }))
    .await;
    for instance in [
        "localhost:8090",
        "https://127.0.0.1",
        "https://10.0.0.2",
        "https://[::1]",
        "http://pixelfed.social",
        "https://metadata.google.internal",
        "https://pixelfed.social/some/path",
        "not a host",
    ] {
        let (status, body) = post_json(
            &format!("{}/v1/auth/pixelfed/app", pod.url),
            None,
            json!({ "instance_url": instance, "redirect_uri": REDIRECT }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{instance}: {body}");
        assert_eq!(body["error"], "instance_not_allowed", "{instance}");
    }
    // The default instance still works.
    sign_in_at(&pod.url, &fake.url).await;
}

async fn get_post_screen(pod: &str, bearer: Option<&str>, id: &str) -> (StatusCode, Value) {
    let mut req = client().get(format!("{pod}/v1/pixelfed/screens/post?id={id}"));
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn post_screen_data_is_the_post_with_its_cover() {
    let env = env().await;
    let session = sign_in(&env).await;
    let (status, body) = get_post_screen(&env.pod.url, Some(&session), CAROUSEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle: protocol::DataBundle = serde_json::from_value(body).unwrap();
    let uri = format!("pixelfed://posts/{CAROUSEL}");
    assert_eq!(
        bundle.queries[&format!("post:{CAROUSEL}")],
        vec![uri.clone()]
    );
    let post = &bundle.entities[&uri];
    let media = post.get_str_list("media_uris").unwrap();
    assert!(media.len() > 1);
    assert_eq!(post.get_i64("media_count"), Some(media.len() as i64));
    // The cover is the first media's preview (else its image), proxied.
    let first = &bundle.entities[media[0]];
    let want = first
        .get_str("preview_url")
        .or(first.get_str("url"))
        .unwrap();
    assert_eq!(post.get_str("cover_url"), Some(want));
    assert!(want.starts_with(&format!("{}/v1/pixelfed/media/", env.pod.url)));
    assert!(
        bundle
            .entities
            .contains_key(post.get_str("author_uri").unwrap())
    );
    // A boost's id gives the boosted post, listed under the asked id.
    let (status, boosted) = get_post_screen(&env.pod.url, Some(&session), REBLOG_OUTER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        boosted["queries"][format!("post:{REBLOG_OUTER}")][0],
        format!("pixelfed://posts/{REBLOG_INNER}")
    );
    // Unknown post: 404; bad id: 400; no session: 401.
    let (status, body) = get_post_screen(&env.pod.url, Some(&session), "999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "post_not_found");
    for bad in ["a.b", ""] {
        let (status, _) = get_post_screen(&env.pod.url, Some(&session), bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    for bearer in [None, Some("bogus")] {
        let (status, _) = get_post_screen(&env.pod.url, bearer, CAROUSEL).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

// ---------- posting (D49, D50) ----------

/// The first bytes of a 3x2 PNG (all the pod and the fake look at).
fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend_from_slice(&3u32.to_be_bytes());
    b.extend_from_slice(&2u32.to_be_bytes());
    b.extend_from_slice(&[8, 2, 0, 0, 0]);
    b
}

async fn upload_png(pod: &str, session: &str, key: &str) -> String {
    let resp = client()
        .post(format!("{pod}/v1/uploads"))
        .bearer_auth(session)
        .header("content-type", "image/png")
        .header("idempotency-key", key)
        .body(png())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json::<Value>().await.unwrap()["upload_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn wait_for(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !f().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The workspace's SSE stream, read in the background into a shared string.
fn listen(pod: &str, session: &str) -> Arc<std::sync::Mutex<String>> {
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let (url, session, out) = (
        format!("{pod}/v1/workspaces/pixelfed_home/events"),
        session.to_owned(),
        seen.clone(),
    );
    tokio::spawn(async move {
        let mut resp = client().get(url).bearer_auth(session).send().await.unwrap();
        while let Ok(Some(chunk)) = resp.chunk().await {
            out.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&chunk));
        }
    });
    seen
}

#[tokio::test]
async fn posting_photos_reaches_pixelfed_and_the_feed() {
    let env = env().await;
    let session = sign_in(&env).await;
    let fake = env.fake.fake.as_ref().unwrap();
    fake.set_media_processing_polls(2);
    let events = listen(&env.pod.url, &session);
    let a = upload_png(&env.pod.url, &session, "photo-a").await;
    let b = upload_png(&env.pod.url, &session, "photo-b").await;
    let url = format!("{}/v1/pixelfed/posts/create", env.pod.url);
    let body =
        json!({ "caption": "  Hello there ", "media": [a, b], "client_id": "post-nonce-01" });
    let (status, answer) = post_json(&url, Some(&session), body.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    assert_eq!(answer["id"], "pixelfed://posts/pending-post-nonce-01");

    // Right away the feed starts with the placeholder, its photos served by the pod.
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    let first = home["queries"]["timeline:home"][0]
        .as_str()
        .unwrap()
        .to_owned();
    if first == "pixelfed://posts/pending-post-nonce-01" {
        let p = &home["entities"][&first];
        assert_eq!(p["pending_label"], "Posting…");
        assert_eq!(p["caption"], "Hello there");
        assert!(p.get("id").is_none(), "placeholders have no id");
        let m = p["media_uris"][0].as_str().unwrap();
        let photo = home["entities"][m]["url"].as_str().unwrap();
        assert!(photo.ends_with(&format!("/v1/uploads/{a}")), "{photo}");
        let resp = client().get(photo).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // The job uploads, waits for processing, posts once; the session hears of it.
    wait_for("the post", async || !fake.created_ids().is_empty()).await;
    wait_for("the data event", async || {
        events.lock().unwrap().contains("event: data")
    })
    .await;
    let id = fake.created_ids()[0].clone();
    let upstream = upstream_status(&env, &id).await;
    assert_eq!(upstream["content_text"], "Hello there");
    assert_eq!(upstream["media_attachments"].as_array().unwrap().len(), 2);
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    let order = home["queries"]["timeline:home"].as_array().unwrap();
    assert_eq!(order[0], json!(format!("pixelfed://posts/{id}")));
    assert!(
        !order
            .iter()
            .any(|u| u.as_str().unwrap().contains("pending-"))
    );
    let post = &home["entities"][format!("pixelfed://posts/{id}")];
    assert_eq!(post["caption"], "Hello there");
    assert_eq!(post["media_uris"].as_array().unwrap().len(), 2);
    // The photos are released; the same client_id doesn't post again.
    let resp = client()
        .get(format!("{}/v1/uploads/{a}", env.pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let (status, _) = post_json(&url, Some(&session), body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(fake.created_ids().len(), 1);
}

#[tokio::test]
async fn refused_posts_can_be_retried_or_discarded() {
    let env = env().await;
    let session = sign_in(&env).await;
    let other = sign_in(&env).await;
    let fake = env.fake.fake.as_ref().unwrap();
    fake.set_media_processing_polls(0);
    let url = format!("{}/v1/pixelfed/posts/create", env.pod.url);
    let mine = upload_png(&env.pod.url, &session, "p1").await;
    let theirs = upload_png(&env.pod.url, &other, "p1").await;
    // Bad input 400, someone else's or unknown uploads 422, no session 401.
    for bad in [
        json!({ "media": [], "client_id": "nonce-0000" }),
        json!({ "media": [mine, mine], "client_id": "nonce-0000" }),
        json!({ "media": ["a", "b", "c", "d", "e"], "client_id": "nonce-0000" }),
        json!({ "media": [mine], "client_id": "x" }),
        json!({ "media": [mine], "caption": "x".repeat(501), "client_id": "nonce-0000" }),
    ] {
        let (status, _) = post_json(&url, Some(&session), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    for media in [json!([theirs]), json!(["0".repeat(32)])] {
        let (status, body) = post_json(
            &url,
            Some(&session),
            json!({ "media": media, "client_id": "nonce-0000" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, _) = post_json(
        &url,
        None,
        json!({ "media": [mine], "client_id": "nonce-0000" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Pixelfed refuses the post once: the placeholder says so.
    fake.add_faults([fake_pixelfed::FaultRule::status(422)
        .path("/api/v1/statuses")
        .method("POST")
        .times(1)]);
    let (status, _) = post_json(
        &url,
        Some(&session),
        json!({ "media": [mine], "client_id": "nonce-0001" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pending = "pixelfed://posts/pending-nonce-0001";
    wait_for("the refusal", async || {
        let (_, home) = get_home(&env.pod.url, Some(&session)).await;
        home["entities"][pending]["failed"] == true
    })
    .await;
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    assert!(
        home["entities"][pending]["error"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't post")
    );
    assert!(home["entities"][pending].get("pending_label").is_none());
    // Not in the other session's feed.
    let (_, theirs_home) = get_home(&env.pod.url, Some(&other)).await;
    assert!(theirs_home["entities"].get(pending).is_none());

    // Retry: posted (Pixelfed kept the uploaded media).
    let retry = format!("{}/v1/pixelfed/posts/pending/retry", env.pod.url);
    let (status, _) = post_json(&retry, Some(&session), json!({ "client_id": "nonce-0001" })).await;
    assert_eq!(status, StatusCode::OK);
    wait_for("the retried post", async || fake.created_ids().len() == 1).await;
    assert_eq!(fake.media_ids().len(), 1, "the photo was uploaded once");

    // Discard a refused one: gone, with its photos.
    fake.add_faults([fake_pixelfed::FaultRule::status(422)
        .path("/api/v1/statuses")
        .method("POST")
        .times(1)]);
    let again = upload_png(&env.pod.url, &session, "p2").await;
    post_json(
        &url,
        Some(&session),
        json!({ "media": [again], "client_id": "nonce-0002" }),
    )
    .await;
    let pending = "pixelfed://posts/pending-nonce-0002";
    wait_for("the second refusal", async || {
        let (_, home) = get_home(&env.pod.url, Some(&session)).await;
        home["entities"][pending]["failed"] == true
    })
    .await;
    let discard = format!("{}/v1/pixelfed/posts/pending/discard", env.pod.url);
    let (status, _) = post_json(
        &discard,
        Some(&session),
        json!({ "client_id": "nonce-0002" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, home) = get_home(&env.pod.url, Some(&session)).await;
    assert!(home["entities"].get(pending).is_none());
    let resp = client()
        .get(format!("{}/v1/uploads/{again}", env.pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let (status, _) = post_json(
        &discard,
        Some(&session),
        json!({ "client_id": "nonce-0002" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
