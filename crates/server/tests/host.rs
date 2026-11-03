//! One server, many pods (D46): the host in front of real pods (every
//! adapter) and the fake Pixelfed, over HTTP on random local ports.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use protocol::{ActionIntent, UiNode, WorkspaceSpec};
use reqwest::{StatusCode, redirect};
use serde_json::{Value, json};
use server::PixelfedConfig;
use server::host::{Host, HostConfig, PodKind, TemporaryPods};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

const REDIRECT: &str = "http://127.0.0.1:9/callback";
const CAROUSEL: &str = "781254316723458049";

struct Server {
    url: String,
    handle: JoinHandle<()>,
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
    Server { url, handle }
}

async fn spawn_fake() -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fake_pixelfed::default_fixtures_dir(),
        public_url: url.clone(),
        auto_approve: true,
        seed_token: Some("seed-token".to_owned()),
    })
    .unwrap();
    let router = fake_pixelfed::app(Arc::new(state));
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { url, handle }
}

struct Env {
    _dir: tempfile::TempDir,
    fake: Server,
    host: Arc<Host>,
    server: Server,
}

async fn env_with(config: HostConfig) -> Env {
    env_custom(config, Vec::new()).await
}

/// [`env_with`] whose Pixelfed adapter also accepts `allowed` instances.
async fn env_custom(config: HostConfig, allowed: Vec<String>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let fake = spawn_fake().await;
    let pixelfed = PixelfedConfig {
        allowed_instances: allowed,
        ..PixelfedConfig::new(&fake.url, None)
    };
    let factory = server::pod_factory(pixelfed, false, 86_400, true);
    let config = HostConfig {
        new_pod_settings: server::new_pod_settings(false),
        temporary_pod_settings: server::temporary_pod_settings(),
        ..config
    };
    let path = dir.path().to_owned();
    let host = tokio::task::spawn_blocking(move || Host::open(&path, factory, config))
        .await
        .unwrap()
        .unwrap();
    let host = Arc::new(host);
    let server = serve(server::host::app(host.clone())).await;
    Env {
        _dir: dir,
        fake,
        host,
        server,
    }
}

async fn env() -> Env {
    env_with(HostConfig::default()).await
}

impl Env {
    async fn create(&self, name: &str, import_recipes: bool) -> String {
        let host = self.host.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            host.create_pod(
                &name,
                PodKind::Admin,
                None,
                &server::new_pod_settings(import_recipes),
            )
            .unwrap()
            .token
        })
        .await
        .unwrap()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.url)
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .build()
        .unwrap()
}

async fn get(url: &str, bearer: Option<&str>) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(url).header("x-construct-schema", "7");
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let session = resp
        .headers()
        .get("x-construct-session")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = resp.json().await.unwrap_or(Value::Null);
    (status, session, body)
}

async fn post(url: &str, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = client().post(url).json(&body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn children(spec: &WorkspaceSpec) -> Vec<UiNode> {
    let UiNode::Workspace { slots, .. } = &spec.workspace else {
        panic!("not a workspace");
    };
    let UiNode::Container { children, .. } = &slots[0].content else {
        panic!("not a Container");
    };
    children.clone()
}

fn session_buttons(spec: &WorkspaceSpec) -> Vec<String> {
    children(spec)
        .into_iter()
        .filter_map(|c| match c {
            UiNode::Button {
                on_click: ActionIntent::OpenSession { path, .. },
                ..
            } => Some(path),
            _ => None,
        })
        .collect()
}

/// Sign the pod of `token` in to the fake Pixelfed through the host (the
/// engine sends the pod token on both auth calls).
async fn sign_in(env: &Env, token: &str) {
    let (status, app) = post(
        &env.url("/v1/auth/pixelfed/app"),
        Some(token),
        json!({ "instance_url": env.fake.url, "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{app}");
    let mut bytes = [0u8; 48];
    getrandom::fill(&mut bytes).unwrap();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let resp = client()
        .get(app["authorize_endpoint"].as_str().unwrap())
        .query(&[
            ("response_type", "code"),
            ("client_id", app["client_id"].as_str().unwrap()),
            ("redirect_uri", REDIRECT),
            ("scope", "read write"),
            ("state", "s"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    let location = reqwest::Url::parse(resp.headers()["location"].to_str().unwrap()).unwrap();
    let code = location
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let (status, body) = post(
        &env.url("/v1/auth/pixelfed/token"),
        Some(token),
        json!({ "instance_url": env.fake.url, "code": code, "code_verifier": verifier,
                "redirect_uri": REDIRECT }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["session_token"], token,
        "the pod token stays the session"
    );
}

#[tokio::test]
async fn a_caller_without_a_pod_token_gets_only_the_open_screen() {
    let env = env().await;
    let resp = client().get(env.url("/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (status, header, body) = get(&env.url("/v1/workspaces/pixelfed_home"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header, None);
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    assert!(
        children(&spec)
            .iter()
            .any(|c| matches!(c, UiNode::TextField { field, .. } if field == "pod_token"))
    );
    assert_eq!(
        session_buttons(&spec),
        ["/v1/pods/open"],
        "no temporary pods here"
    );
    let (status, _, body) = get(&env.url("/v1/apps"), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "apps": [] })));
    let (status, body) = post(&env.url("/v1/pixelfed/posts/like"), None, json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "pod_required");
    // A token the host does not know is told to go away.
    let (status, header, _) = get(&env.url("/v1/workspaces/recipes"), Some("cpod_nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    let (status, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "temporary_pods_disabled");
}

#[tokio::test]
async fn a_pod_token_opens_its_pod_and_recipes_are_imported_only_when_asked() {
    let env = env().await;
    let family = env.create("family", true).await;
    let other = env.create("other", false).await;
    let (status, body) = post(&env.url("/v1/pods/open"), None, json!({ "token": family })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_token"], family);
    for wrong in ["cpod_wrong", "", "family"] {
        let (status, body) = post(&env.url("/v1/pods/open"), None, json!({ "token": wrong })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{wrong}");
        assert_eq!(body["error"], "pod_not_found");
    }
    for (token, recipes) in [(&family, 81), (&other, 0)] {
        let (status, _, body) = get(&env.url("/v1/workspaces/recipes"), Some(token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let n = body["queries"]["recipes:all"]
            .as_array()
            .map_or(0, Vec::len);
        assert_eq!(n, recipes);
        let (_, _, apps) = get(&env.url("/v1/apps"), Some(token)).await;
        assert!(apps["apps"].as_array().unwrap().len() >= 2, "{apps}");
    }
}

#[tokio::test]
async fn pods_keep_their_sign_ins_and_media_apart() {
    let env = env().await;
    let a = env.create("a", false).await;
    let b = env.create("b", false).await;
    sign_in(&env, &a).await;
    let (status, _, feed) = get(&env.url("/v1/workspaces/pixelfed_home"), Some(&a)).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    assert_eq!(
        feed["queries"]["timeline:home"].as_array().unwrap().len(),
        5
    );
    // Pod b is not signed in to anything, and keeps its token (200, no header).
    let (status, header, body) = get(&env.url("/v1/workspaces/pixelfed_home"), Some(&b)).await;
    assert_eq!((status, header), (StatusCode::OK, None));
    assert!(body["queries"]["timeline:home"].is_null());
    let (status, body) = post(
        &env.url("/v1/pixelfed/posts/like"),
        Some(&b),
        json!({ "id": CAROUSEL, "liked": true }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "sign_in_required");
    // Media URLs name pod a and load without a bearer; not through pod b.
    let avatar = feed["entities"]["pixelfed://users/698765432100012345"]["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let (prefix, key) = avatar.rsplit_once('/').unwrap();
    assert!(
        prefix.starts_with(&env.url("/v1/p/")) && prefix.ends_with("/v1/pixelfed/media"),
        "{avatar}"
    );
    let resp = client().get(&avatar).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    let id_a = prefix
        .trim_start_matches(&env.url("/v1/p/"))
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let id_b = env.host.store().pod_by_name("b").unwrap().unwrap().id;
    let other = format!("{}/v1/p/{id_b}/v1/pixelfed/media/{key}", env.server.url);
    let resp = client().get(&other).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Only public paths: the feed itself is not reachable that way.
    for path in [
        format!("/v1/p/{id_a}/v1/workspaces/pixelfed_home"),
        format!("/v1/p/{id_a}/v1/workspaces/recipes"),
        format!("/v1/p/{id_a}/v1/pixelfed/media/../../workspaces/recipes"),
        format!("/v1/p/{}/v1/pixelfed/media/{key}", "0".repeat(32)),
    ] {
        let resp = client().get(env.url(&path)).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn logout_keeps_the_pod_token_and_rotation_replaces_it() {
    let env = env().await;
    let token = env.create("me", false).await;
    sign_in(&env, &token).await;
    let (status, body) = post(&env.url("/v1/auth/logout"), Some(&token), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_kept"], true);
    let (status, header, body) = get(&env.url("/v1/workspaces/pixelfed_home"), Some(&token)).await;
    assert_eq!((status, header), (StatusCode::OK, None), "{body}");
    assert!(
        body["queries"]["timeline:home"].is_null(),
        "signed out of Pixelfed"
    );
    // Rotation: the old token stops working, the new one opens the pod.
    let host = env.host.clone();
    let new = tokio::task::spawn_blocking(move || host.rotate("me").unwrap())
        .await
        .unwrap();
    let (status, header, _) = get(&env.url("/v1/workspaces/recipes"), Some(&token)).await;
    assert_eq!(
        (status, header.as_deref()),
        (StatusCode::UNAUTHORIZED, Some("invalid"))
    );
    let (status, _, _) = get(&env.url("/v1/workspaces/recipes"), Some(&new)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn temporary_pods_are_capped_and_swept() {
    let env = env_with(HostConfig {
        temporary: Some(TemporaryPods {
            hours: 2,
            max_live: 2,
        }),
        ..HostConfig::default()
    })
    .await;
    let (_, _, body) = get(&env.url("/v1/workspaces/recipes"), None).await;
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    assert_eq!(
        session_buttons(&spec),
        ["/v1/pods/open", "/v1/pods/temporary"]
    );
    let mut tokens = Vec::new();
    for _ in 0..2 {
        let (status, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let expires = body["expires_at"].as_i64().unwrap();
        let now = server::util::now_secs();
        assert!((now + 7000..=now + 7300).contains(&expires));
        tokens.push(body["session_token"].as_str().unwrap().to_owned());
    }
    let (status, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "temporary_pods_full");
    // Full features, no family recipes.
    let (status, _, body) = get(&env.url("/v1/workspaces/recipes"), Some(&tokens[0])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["queries"]["recipes:all"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    sign_in(&env, &tokens[0]).await;
    // Expired pods are swept: token dead, files gone.
    let pods = env.host.store().pods().unwrap();
    let first = pods
        .iter()
        .find(|p| p.kind == PodKind::Temporary)
        .unwrap()
        .clone();
    let host = env.host.clone();
    let name = first.name.clone();
    let swept = tokio::task::spawn_blocking(move || {
        host.delete(&name).unwrap();
        host.sweep()
    })
    .await
    .unwrap();
    assert_eq!(swept, 1);
    assert!(env.host.store().pod_by_name(&first.name).unwrap().is_none());
    let mut dead = 0;
    for t in &tokens {
        let (status, _, _) = get(&env.url("/v1/workspaces/recipes"), Some(t)).await;
        if status == StatusCode::UNAUTHORIZED {
            dead += 1;
        }
    }
    assert_eq!(dead, 1);
    // A slot is free again.
    let (status, _) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

/// The rollout: today's single-pod `server.db` (with the family recipes)
/// becomes an admin pod with everything in it.
#[tokio::test]
async fn adopting_a_single_pod_database_keeps_its_data() {
    let env = env().await;
    let old = tempfile::tempdir().unwrap();
    let old_path = old.path().to_owned();
    tokio::task::spawn_blocking(move || {
        let state = server::open_dir(&old_path).unwrap();
        assert_eq!(state.store().adapter_version("recipes").unwrap(), 2);
    })
    .await
    .unwrap();
    let src = old.path().join("server.db");
    let host = env.host.clone();
    let src2 = src.clone();
    let pod = tokio::task::spawn_blocking(move || host.adopt("me", &src2).unwrap())
        .await
        .unwrap();
    assert!(!src.exists(), "the original was renamed");
    assert!(
        old.path()
            .join(format!("server.db.adopted-{}", pod.id))
            .is_file()
    );
    let (status, _, body) = get(&env.url("/v1/workspaces/recipes"), Some(&pod.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queries"]["recipes:all"].as_array().unwrap().len(), 81);
}

#[tokio::test]
async fn the_open_pod_serves_callers_without_a_token() {
    let env = env_with(HostConfig {
        open_pod: Some("dev".into()),
        ..HostConfig::default()
    })
    .await;
    let (status, _, body) = get(&env.url("/v1/workspaces/pixelfed_demo"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workspace"]["id"], "pixelfed_demo");
    // Admin pods still need their own token.
    let token = env.create("me", false).await;
    let (status, _, _) = get(&env.url("/v1/workspaces/recipes"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Security review of D46: anyone can make a temporary pod, so it must not
/// make the server contact hosts of the visitor's choosing. It is held to the
/// default Pixelfed instance; an admin pod may use the other allowed ones.
#[tokio::test]
async fn temporary_pods_stay_on_the_default_instance() {
    let other = spawn_fake().await;
    let env = env_custom(
        HostConfig {
            temporary: Some(TemporaryPods {
                hours: 1,
                max_live: 5,
            }),
            ..HostConfig::default()
        },
        vec![other.url.clone()],
    )
    .await;
    let (_, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    let temp = body["session_token"].as_str().unwrap().to_owned();
    let admin = env.create("me", false).await;
    let register = |token: String, instance: String| {
        let url = env.url("/v1/auth/pixelfed/app");
        async move {
            post(
                &url,
                Some(&token),
                json!({ "instance_url": instance, "redirect_uri": REDIRECT }),
            )
            .await
        }
    };
    let (status, body) = register(temp.clone(), other.url.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "instance_not_allowed");
    let (status, _) = register(temp.clone(), env.fake.url.clone()).await;
    assert_eq!(status, StatusCode::OK, "the default instance works");
    let (status, body) = register(admin, other.url.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Security review of D46: a burst of creations can't overshoot the cap.
#[tokio::test]
async fn concurrent_temporary_pods_respect_the_cap() {
    let env = env_with(HostConfig {
        temporary: Some(TemporaryPods {
            hours: 1,
            max_live: 2,
        }),
        ..HostConfig::default()
    })
    .await;
    let url = env.url("/v1/pods/temporary");
    let tries: Vec<_> = (0..5)
        .map(|_| {
            let url = url.clone();
            tokio::spawn(async move { post(&url, None, json!({})).await.0 })
        })
        .collect();
    let mut made = 0;
    for t in tries {
        if t.await.unwrap() == StatusCode::OK {
            made += 1;
        }
    }
    assert_eq!(made, 2);
    assert_eq!(env.host.store().live_temporary_count().unwrap(), 2);
}
