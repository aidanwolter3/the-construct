//! One server, many pods (D46): the host in front of real pods (every
//! adapter) and the fake Pixelfed, over HTTP on random local ports.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use protocol::{ActionIntent, UiNode};
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
    async fn create(&self, name: &str) -> String {
        let host = self.host.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            host.create_pod(&name, PodKind::Admin, None, &[])
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

/// Home's element for `bearer` as a device fetches it: the root
/// (`GET /v1/construct`) and its object (`POST /v1/objects`).
async fn home(env: &Env, bearer: Option<&str>) -> Value {
    let (status, _, root) = get(&env.url("/v1/construct"), bearer).await;
    assert_eq!(status, StatusCode::OK, "{root}");
    let root = root["root"].as_str().unwrap().to_owned();
    let mut req = client()
        .post(env.url("/v1/objects"))
        .json(&json!({ "have": [], "want": [root] }));
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let body: Value = req.send().await.unwrap().json().await.unwrap();
    body["objects"][&root].clone()
}

/// The children of the "Open your pod" page a caller without a pod gets as
/// its home.
async fn open_screen(env: &Env) -> Vec<UiNode> {
    let el = home(env, None).await;
    let page: UiNode = serde_json::from_value(el["node"].clone()).unwrap();
    let UiNode::Page { root, .. } = page else {
        panic!("not a page: {el}");
    };
    let UiNode::Container { children, .. } = *root else {
        panic!("not a Container: {el}");
    };
    children
}

fn session_buttons(children: &[UiNode]) -> Vec<String> {
    children
        .iter()
        .filter_map(|c| match c {
            UiNode::Button {
                on_click: ActionIntent::OpenSession { path, .. },
                ..
            } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// A tool call on the pod of `token` (`POST /v1/mcp`): its result.
async fn tool(env: &Env, token: &str, name: &str, args: Value) -> Value {
    let resp = client()
        .post(env.url("/v1/mcp"))
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let r = &body["result"];
    assert_ne!(r["isError"], true, "{name}: {body}");
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// The data a layout of blueprint `blueprint` on the pod of `token` draws
/// (`/v1/construct/data/<element>`; the layout is made with the
/// blueprint's feeds): status, `X-Construct-Session` and body.
async fn blueprint_data(
    env: &Env,
    token: &str,
    blueprint: &str,
) -> (StatusCode, Option<String>, Value) {
    let info = *server::catalog().blueprint(blueprint).unwrap();
    let feeds: Vec<[&str; 2]> = info.feeds.iter().map(|(s, f)| [*s, *f]).collect();
    let made = tool(
        env,
        token,
        "create_layout",
        json!({ "title": info.title, "feeds": feeds,
                "layout": { "type": "core://Use", "ref": format!("@core/{blueprint}") } }),
    )
    .await;
    let id = made["id"].as_str().unwrap();
    let (status, _, name) = get(&env.url(&format!("/v1/construct/names/{id}")), Some(token)).await;
    assert_eq!(status, StatusCode::OK, "{name}");
    let el = name["el"].as_str().unwrap();
    get(&env.url(&format!("/v1/construct/data/{el}")), Some(token)).await
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
    let (code, verifier) = authorize(&app).await;
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

/// The fake Pixelfed's authorize page for `app` (an answer of
/// `/v1/auth/pixelfed/app`) -> the code it redirects with and its verifier.
async fn authorize(app: &Value) -> (String, String) {
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
    (code, verifier)
}

#[tokio::test]
async fn a_caller_without_a_pod_token_gets_only_the_open_screen() {
    let env = env().await;
    let resp = client().get(env.url("/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Home is the "Open your pod" screen, a construct of one element.
    let (status, header, root) = get(&env.url("/v1/construct"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header, None);
    let children = open_screen(&env).await;
    assert!(
        children
            .iter()
            .any(|c| matches!(c, UiNode::TextField { field, .. } if field == "pod_token"))
    );
    assert_eq!(
        session_buttons(&children),
        ["/v1/pods/open"],
        "no temporary pods here"
    );
    let (status, _, name) = get(&env.url("/v1/construct/names/home"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(name["el"], root["root"]);
    let el = root["root"].as_str().unwrap();
    let (status, _, data) = get(&env.url(&format!("/v1/construct/data/{el}")), None).await;
    assert_eq!(
        (status, data),
        (StatusCode::OK, json!({ "entities": {}, "queries": {} }))
    );
    let (status, _, body) = get(&env.url("/v1/construct/events"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "pod_required");
    let (status, body) = post(&env.url("/v1/pixelfed/posts/like"), None, json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "pod_required");
    // A token the host does not know is told to go away.
    let (status, header, _) = get(&env.url("/v1/construct"), Some("cpod_nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header.as_deref(), Some("invalid"));
    let (status, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "temporary_pods_disabled");
}

#[tokio::test]
async fn a_pod_token_opens_its_pod() {
    let env = env().await;
    let first = env.create("first").await;
    let other = env.create("other").await;
    let (status, body) = post(&env.url("/v1/pods/open"), None, json!({ "token": first })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_token"], first);
    for wrong in ["cpod_wrong", "", "first"] {
        let (status, body) = post(&env.url("/v1/pods/open"), None, json!({ "token": wrong })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{wrong}");
        assert_eq!(body["error"], "pod_not_found");
    }
    for token in [&first, &other] {
        // A new pod has only its home (decision `blueprints-and-home`).
        let layouts = tool(&env, token, "list_layouts", json!({})).await;
        let ids: Vec<&str> = layouts["layouts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["id"].as_str())
            .collect();
        assert_eq!(ids, ["home"], "{layouts}");
    }
}

#[tokio::test]
async fn pods_keep_their_sign_ins_and_media_apart() {
    let env = env().await;
    let a = env.create("a").await;
    let b = env.create("b").await;
    sign_in(&env, &a).await;
    let (status, _, feed) = blueprint_data(&env, &a, "pixelfed").await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    assert_eq!(
        feed["queries"]["timeline:home"].as_array().unwrap().len(),
        5
    );
    // Pod b is not signed in to anything, and keeps its token (200, no header).
    let (status, header, body) = blueprint_data(&env, &b, "pixelfed").await;
    assert_eq!((status, header), (StatusCode::OK, None));
    assert_eq!(
        body["entities"]["core://sources/pixelfed"]["signed_in"],
        false
    );
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
        format!("/v1/p/{id_a}/v1/construct"),
        format!("/v1/p/{id_a}/v1/construct/names/home"),
        format!("/v1/p/{id_a}/v1/pixelfed/media/../../construct"),
        format!("/v1/p/{}/v1/pixelfed/media/{key}", "0".repeat(32)),
    ] {
        let resp = client().get(env.url(&path)).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn logout_keeps_the_pod_token_and_rotation_replaces_it() {
    let env = env().await;
    let token = env.create("me").await;
    sign_in(&env, &token).await;
    let (_, _, body) = get(&env.url("/v1/auth/sign-ins"), Some(&token)).await;
    assert_eq!(body["sign_ins"][0]["id"], "pixelfed", "{body}");
    assert_eq!(body["sign_ins"][0]["username"], "constructdemo");
    let (status, body) = post(&env.url("/v1/auth/logout"), Some(&token), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_kept"], true);
    // Signed out of Pixelfed, still in the pod: no sign-ins left.
    let (status, _, body) = get(&env.url("/v1/auth/sign-ins"), Some(&token)).await;
    assert_eq!((status, &body["sign_ins"]), (StatusCode::OK, &json!([])));
    let (status, header, body) = blueprint_data(&env, &token, "pixelfed").await;
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
    let (status, header, _) = get(&env.url("/v1/construct"), Some(&token)).await;
    assert_eq!(
        (status, header.as_deref()),
        (StatusCode::UNAUTHORIZED, Some("invalid"))
    );
    let (status, _, _) = get(&env.url("/v1/construct"), Some(&new)).await;
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
    assert_eq!(
        session_buttons(&open_screen(&env).await),
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
    // Full features.
    let (status, _, _) = blueprint_data(&env, &tokens[0], "events").await;
    assert_eq!(status, StatusCode::OK);
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
        let (status, _, _) = get(&env.url("/v1/construct"), Some(t)).await;
        if status == StatusCode::UNAUTHORIZED {
            dead += 1;
        }
    }
    assert_eq!(dead, 1);
    // A slot is free again.
    let (status, _) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

/// Who made a temporary pod is recorded (address, the caller's label, its
/// User-Agent), and the pod can delete itself with its own token, freeing
/// its slot and its files at once; an admin pod can't be deleted this way.
#[tokio::test]
async fn temporary_pods_record_their_creator_and_delete_themselves() {
    let env = env_with(HostConfig {
        temporary: Some(TemporaryPods {
            hours: 2,
            max_live: 1,
        }),
        // As on staging: the address is the one Fly's proxy reports.
        trust_fly_client_ip: true,
        ..HostConfig::default()
    })
    .await;
    let made = |label: &'static str| {
        client()
            .post(env.url("/v1/pods/temporary"))
            .header("fly-client-ip", "198.51.100.7")
            .header("x-construct-client", label)
            .header("user-agent", "smoke/1.0")
            .send()
    };
    let resp = made("ci-smoke-test \"run 7\"").await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let token = body["session_token"].as_str().unwrap().to_owned();
    let name = body["pod"].as_str().unwrap().to_owned();
    let pod = env.host.store().pod_by_name(&name).unwrap().unwrap();
    assert_eq!(
        env.host.store().created_by(&pod.id).unwrap().as_deref(),
        Some(r#"ip 198.51.100.7, client "ci-smoke-test run 7", agent "smoke/1.0""#)
    );
    // Full: the cap is one.
    assert_eq!(
        made("again").await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let delete = |bearer: Option<String>| {
        let mut req = client().delete(env.url("/v1/pods/temporary"));
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        req.send()
    };
    assert_eq!(
        delete(None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let admin = env.create("me").await;
    let resp = delete(Some(admin.clone())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let (status, _, _) = get(&env.url("/v1/construct"), Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "the admin pod is untouched");

    let resp = delete(Some(token.clone())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], name.as_str());
    // Gone now, not at the next sweep: token, row and files.
    let (status, _, _) = get(&env.url("/v1/construct"), Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(env.host.store().pod_by_name(&name).unwrap().is_none());
    assert_eq!(
        delete(Some(token)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    // Its slot is free again.
    assert_eq!(made("next").await.unwrap().status(), StatusCode::OK);
}

/// The rollout: a single-pod `server.db` from before D46 (here with the
/// made-up test dishes) becomes an admin pod with everything in it.
#[tokio::test]
async fn adopting_a_single_pod_database_keeps_its_data() {
    let env = env().await;
    let old = tempfile::tempdir().unwrap();
    let old_path = old.path().to_owned();
    tokio::task::spawn_blocking(move || {
        let state = server::open_dir(&old_path).unwrap();
        state
            .store()
            .with_conn(source_collections::test_seed::seed_test_dishes)
            .unwrap();
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
    let (status, _, body) = get(&env.url("/v1/collections/dishes/rows"), Some(&pod.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["queries"]["collection:dishes"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    // The renamed original can go once the pod works; a lookalike for a pod
    // that doesn't exist stays.
    let stranger = old.path().join(format!("x.adopted-{}", "0".repeat(32)));
    std::fs::write(&stranger, "keep").unwrap();
    let host = env.host.clone();
    let dir = old.path().to_owned();
    let removed = tokio::task::spawn_blocking(move || host.remove_adopted(&dir).unwrap())
        .await
        .unwrap();
    assert_eq!(
        removed,
        [old.path().join(format!("server.db.adopted-{}", pod.id))]
    );
    assert!(stranger.is_file());
    let (status, _, _) = get(&env.url("/v1/construct"), Some(&pod.token)).await;
    assert_eq!(status, StatusCode::OK, "the pod is unaffected");
}

#[tokio::test]
async fn the_open_pod_serves_callers_without_a_token() {
    let env = env_with(HostConfig {
        open_pod: Some("dev".into()),
        ..HostConfig::default()
    })
    .await;
    // The open pod's own home, not the "Open your pod" screen.
    let el = home(&env, None).await;
    assert_eq!(el["title"], "Home", "{el}");
    assert!(!el.to_string().contains("pod_token"), "{el}");
    // Admin pods still need their own token.
    let token = env.create("me").await;
    let (status, _, _) = get(&env.url("/v1/construct"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Security audit #31 (L1): a DNS-rebinding page reaches the loopback
/// server under its own domain name, so the open pod refuses that Host.
/// Pod tokens work with any Host.
#[tokio::test]
async fn the_open_pod_refuses_foreign_host_names() {
    let env = env_with(HostConfig {
        open_pod: Some("dev".into()),
        ..HostConfig::default()
    })
    .await;
    let client = reqwest::Client::new();
    let port = env.server.url.rsplit(':').next().unwrap().to_owned();
    let with_host = |host: String, token: Option<String>| {
        let mut req = client.get(env.url("/v1/construct")).header("host", host);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        async move { req.send().await.unwrap() }
    };
    for ok in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("10.0.2.2:{port}"),
    ] {
        let resp = with_host(ok.clone(), None).await;
        assert_eq!(resp.status(), StatusCode::OK, "{ok}");
    }
    let resp = with_host(format!("rebind.evil.example:{port}"), None).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "host_not_allowed");
    let token = env.create("me").await;
    let resp = with_host("pod.example".to_owned(), Some(token)).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a pod token works with any Host"
    );
}

/// Security audit #31 (Info): every response carries the basic security
/// headers; HSTS only when the host is behind HTTPS.
#[tokio::test]
async fn responses_carry_security_headers() {
    for hsts in [false, true] {
        let env = env_with(HostConfig {
            hsts,
            ..HostConfig::default()
        })
        .await;
        for path in ["/healthz", "/v1/construct", "/v1/nowhere"] {
            let resp = reqwest::get(env.url(path)).await.unwrap();
            let h = resp.headers();
            assert_eq!(h["x-content-type-options"], "nosniff", "{path}");
            assert_eq!(h["referrer-policy"], "no-referrer", "{path}");
            assert_eq!(h["x-frame-options"], "DENY", "{path}");
            assert_eq!(
                h["content-security-policy"], "default-src 'none'; frame-ancestors 'none'",
                "{path}"
            );
            assert_eq!(h.get("strict-transport-security").is_some(), hsts, "{path}");
        }
    }
}

/// Security audit #31 (Info, custom scheme): the Android sign-in redirect is
/// an App Link on the host. `assetlinks.json` lists the configured app (404
/// with no fingerprint), and `/oauth/android`, which the browser shows when
/// it keeps the redirect, sends the query on to that package only. Neither
/// needs a pod token.
#[tokio::test]
async fn android_app_link_routes() {
    let env = env().await;
    let resp = reqwest::get(env.url("/.well-known/assetlinks.json"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let release = "AA:".repeat(31) + "AA";
    let env = env_with(HostConfig {
        android: server::host::app_links::AndroidAppLinks {
            package: "org.example.app".into(),
            cert_sha256: vec![release.clone()],
        },
        ..HostConfig::default()
    })
    .await;
    let resp = client()
        .get(env.url("/.well-known/assetlinks.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        json!([{
            "relation": ["delegate_permission/common.handle_all_urls"],
            "target": {
                "namespace": "android_app",
                "package_name": "org.example.app",
                "sha256_cert_fingerprints": [release],
            },
        }])
    );

    let resp = client()
        .get(env.url("/oauth/android?code=a%2Fb&state=xyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert!(h["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(
        !h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script")
    );
    let page = resp.text().await.unwrap();
    assert!(
        page.contains(
            "href=\"intent://oauth?code=a%2Fb&amp;state=xyz#Intent;scheme=construct;package=org.example.app;end\""
        ),
        "{page}"
    );

    for bad in [
        "/oauth/android",
        "/oauth/android?code=a{b}&state=x",
        "/oauth/android?code=a;package=evil",
    ] {
        let resp = client().get(env.url(bad)).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
        let page = resp.text().await.unwrap();
        assert!(!page.contains("intent:"), "{bad}: {page}");
    }
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
    let admin = env.create("me").await;
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

/// Temporary pods can't fetch websites or spend the server's model key
/// (security audit: collection `model` computes on temporary pods).
#[test]
fn temporary_pods_fetch_nothing_and_ask_no_model() {
    let settings = server::temporary_pod_settings();
    for jack in [
        pod_core::jacks::MODEL,
        pod_core::jacks::WEB,
        pod_core::jacks::WEB_SEARCH,
    ] {
        let key = format!("jack.{jack}");
        assert!(
            settings
                .iter()
                .any(|(k, v)| *k == key && v == pod_core::jacks::LOCKED),
            "{key}: {settings:?}"
        );
    }
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

/// Uploads (D49) pass through the host (a body far over the 16 KB the host
/// reads itself), stay in their pod, and load without a bearer only from
/// their pod's public prefix.
#[tokio::test]
async fn uploads_go_to_their_pod_and_load_from_its_public_prefix() {
    let env = env().await;
    let a = env.create("a").await;
    let b = env.create("b").await;
    let mut photo = vec![0xFF, 0xD8, 0xFF, 0xE0];
    photo.resize(3 * 1024 * 1024, 7);
    let resp = client()
        .post(env.url("/v1/uploads"))
        .bearer_auth(&a)
        .header("content-type", "image/jpeg")
        .header("idempotency-key", "photo-1")
        .body(photo.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let id = resp.json::<Value>().await.unwrap()["upload_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id_a = env.host.store().pod_by_name("a").unwrap().unwrap().id;
    let id_b = env.host.store().pod_by_name("b").unwrap().unwrap().id;
    let resp = client()
        .get(env.url(&format!("/v1/p/{id_a}/v1/uploads/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().len(), photo.len());
    for (url, bearer) in [
        (format!("/v1/p/{id_b}/v1/uploads/{id}"), None),
        (format!("/v1/uploads/{id}"), Some(&b)),
    ] {
        let mut req = client().get(env.url(&url));
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        assert_eq!(
            req.send().await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{url}"
        );
    }
}

/// D52: the host's scheduler opens a pod with a registered device, polls its
/// Pixelfed account (first poll quiet), then stores a new like as unread.
#[tokio::test]
async fn the_scheduler_polls_pods_with_devices() {
    let env = env().await;
    let token = env.create("alice").await;
    sign_in(&env, &token).await;
    // No device yet: nothing is polled.
    env.host.run_due_polls().await;
    let (status, _, list) = get(&env.url("/v1/notifications?limit=0"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["unread_total"], 0);

    let resp = client()
        .put(env.url("/v1/devices/device-0001"))
        .bearer_auth(&token)
        .json(&json!({"platform": "desktop"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    env.host.run_due_polls().await; // first poll: backlog stored quietly
    let (_, _, list) = get(&env.url("/v1/notifications"), Some(&token)).await;
    assert_eq!(list["unread_total"], 0);
    assert!(
        !list["notifications"].as_array().unwrap().is_empty(),
        "{list}"
    );

    let (status, made) = post(
        &format!("{}/_admin/notifications", env.fake.url),
        None,
        json!({"type": "favourite", "from": "carol"}),
    )
    .await;
    assert!(status.is_success(), "{made}");
    // Not due for two minutes; make it due as a restart would find it.
    env.host.run_due_polls().await;
    let (_, _, list) = get(&env.url("/v1/notifications?limit=0"), Some(&token)).await;
    assert_eq!(list["unread_total"], 0, "the next poll is not due yet");
    let dir = env._dir.path().join("pods");
    let pod_dir = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    rusqlite::Connection::open(pod_dir.join("server.db"))
        .unwrap()
        .execute("UPDATE notification_polls SET next_at = 0", [])
        .unwrap();
    env.host.run_due_polls().await;
    let (_, _, list) = get(&env.url("/v1/notifications"), Some(&token)).await;
    assert_eq!(list["unread_total"], 1, "{list}");
    let first = &list["notifications"][0];
    assert_eq!(first["channel"], "likes");
    assert!(
        first["title"].as_str().unwrap().contains("liked your post"),
        "{first}"
    );
    assert!(
        first["target_uri"]
            .as_str()
            .unwrap()
            .starts_with("pixelfed://posts/"),
        "{first}"
    );
}

#[tokio::test]
async fn a_local_pod_keeps_the_codes_it_was_given() {
    let env = env().await;
    let ensure = |code: &'static str, key: Option<&'static str>| {
        let host = env.host.clone();
        async move {
            tokio::task::spawn_blocking(move || host.ensure_local_pod("local", code, key))
                .await
                .unwrap()
        }
    };
    ensure("cpod_localtest123", Some("cmcp_localtest123"))
        .await
        .unwrap();
    // Again, as on every restart: nothing changes.
    ensure("cpod_localtest123", Some("cmcp_localtest123"))
        .await
        .unwrap();
    let el = home(&env, Some("cpod_localtest123")).await;
    assert_eq!(el["title"], "Home", "{el}");
    let resp = client()
        .post(env.url("/v1/mcp"))
        .bearer_auth("cmcp_localtest123")
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // A new code replaces the old one.
    ensure("cpod_othertest456", None).await.unwrap();
    let (status, _, _) = get(&env.url("/v1/construct"), Some("cpod_othertest456")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = get(&env.url("/v1/construct"), Some("cpod_localtest123")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Codes that don't look like codes, or belong to another pod, are refused.
    assert!(ensure("nope", None).await.is_err());
    assert!(ensure("cpod_localtest123", Some("cmcp_x")).await.is_err());
    let other = env.create("other").await;
    let other: &'static str = Box::leak(other.into_boxed_str());
    assert!(ensure(other, None).await.is_err());
}

/// The web client's routes and cookie (decision `web-client`).
mod web {
    use super::*;
    use server::host::web::WebConfig;

    /// A server with the web client on, serving a page from a directory.
    async fn web_env() -> (Env, tempfile::TempDir) {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(
            files.path().join("index.html"),
            "<!doctype html><title>t</title>",
        )
        .unwrap();
        std::fs::create_dir(files.path().join("fonts")).unwrap();
        std::fs::write(files.path().join("fonts/Face.ttf"), b"font").unwrap();
        std::fs::write(files.path().join("app.0123456789abcdef.js"), b"app").unwrap();
        std::fs::write(files.path().join("app.js"), b"app").unwrap();
        std::fs::write(files.path().join("favicon.0123456789abcdef.ico"), b"ico").unwrap();
        std::fs::write(files.path().join("icon.0123456789abcdef.png"), b"png").unwrap();
        std::fs::write(files.path().join("secret.txt"), b"no").unwrap();
        let env = env_with(HostConfig {
            web: Some(WebConfig {
                dir: files.path().to_owned(),
            }),
            ..HostConfig::default()
        })
        .await;
        (env, files)
    }

    /// `POST /web/session` as the page sends it: the cookie, if any.
    async fn open(env: &Env, code: &str, client_header: bool) -> (StatusCode, Option<String>) {
        let mut req = client()
            .post(env.url("/web/session"))
            .json(&json!({ "code": code }));
        if client_header {
            req = req.header("x-construct-client", "web");
        }
        let resp = req.send().await.unwrap();
        let cookie = resp
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_owned());
        (resp.status(), cookie)
    }

    /// A request with the cookie: its status.
    async fn with_cookie(
        env: &Env,
        path: &str,
        cookie: &str,
        extra: &[(&str, &str)],
    ) -> StatusCode {
        let mut req = client().get(env.url(path)).header("cookie", cookie);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        req.send().await.unwrap().status()
    }

    const WEB: (&str, &str) = ("x-construct-client", "web");

    #[tokio::test]
    async fn without_the_switch_there_is_no_web_client() {
        let env = env().await;
        let code = env.create("off").await;
        // No page: the path is the pod's, which wants a pod.
        assert_eq!(
            client().get(env.url("/web")).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let (status, _) = open(&env, &code, true).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "no route: the pod answers pod_required"
        );
        // A cookie means nothing.
        assert_eq!(
            with_cookie(
                &env,
                "/v1/auth/sign-ins",
                &format!("construct_web={code}"),
                &[WEB]
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_page_and_its_files_are_served_and_nothing_else() {
        let (env, _files) = web_env().await;
        let resp = client().get(env.url("/web")).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let csp = resp.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
            "{csp}"
        );
        let status = |p: &'static str| {
            let url = env.url(p);
            async move { client().get(url).send().await.unwrap().status() }
        };
        assert_eq!(status("/web/fonts/Face.ttf").await, StatusCode::OK);
        assert_eq!(status("/web/secret.txt").await, StatusCode::NOT_FOUND);
        assert_eq!(
            status("/web/fonts/..%2Fsecret.txt").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(status("/web/.hidden.js").await, StatusCode::NOT_FOUND);
    }

    /// Decision `web-client-cache`: a hashed name (or a font with the build's
    /// `?v=`) is kept for good, everything else is revalidated with its ETag.
    #[tokio::test]
    async fn hashed_files_are_kept_and_the_page_is_revalidated() {
        let (env, _files) = web_env().await;
        let get = |p: &'static str, tag: Option<String>| {
            let mut req = client().get(env.url(p));
            if let Some(t) = tag {
                req = req.header("if-none-match", t);
            }
            async move { req.send().await.unwrap() }
        };
        let header = |r: &reqwest::Response, h: &str| {
            r.headers()
                .get(h)
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default()
        };
        for p in [
            "/web/app.0123456789abcdef.js",
            "/web/fonts/Face.ttf?v=0123abcd",
            "/web/favicon.0123456789abcdef.ico",
            "/web/icon.0123456789abcdef.png",
        ] {
            let r = get(p, None).await;
            assert_eq!(r.status(), StatusCode::OK, "{p}");
            assert_eq!(
                header(&r, "cache-control"),
                "public, max-age=31536000, immutable",
                "{p}"
            );
        }
        for p in ["/web", "/web/app.js", "/web/fonts/Face.ttf"] {
            let r = get(p, None).await;
            assert_eq!(r.status(), StatusCode::OK, "{p}");
            assert_eq!(header(&r, "cache-control"), "no-cache", "{p}");
            let tag = header(&r, "etag");
            assert!(tag.starts_with('"') && tag.len() > 2, "{p}: {tag}");
            let again = get(p, Some(tag.clone())).await;
            assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{p}");
            assert_eq!(header(&again, "etag"), tag, "{p}");
            assert!(again.bytes().await.unwrap().is_empty(), "{p}");
            let other = get(p, Some("\"stale\"".into())).await;
            assert_eq!(other.status(), StatusCode::OK, "{p}");
        }
    }

    #[tokio::test]
    async fn the_icon_is_served_as_an_image() {
        let (env, _files) = web_env().await;
        for (p, kind) in [
            ("/web/favicon.0123456789abcdef.ico", "image/x-icon"),
            ("/web/icon.0123456789abcdef.png", "image/png"),
        ] {
            let r = client().get(env.url(p)).send().await.unwrap();
            assert_eq!(r.status(), StatusCode::OK, "{p}");
            assert_eq!(r.headers()["content-type"], kind, "{p}");
        }
    }

    #[tokio::test]
    async fn a_pod_code_becomes_a_session_cookie_that_reaches_only_the_pod() {
        let (env, _files) = web_env().await;
        let code = env.create("browser").await;
        // Only the web client may ask, and only with a real code.
        assert_eq!(open(&env, &code, false).await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            open(&env, "cpod_wrong", true).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, set) = open(&env, &code, true).await;
        assert_eq!(status, StatusCode::OK);
        let set = set.expect("a cookie");
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Strict"),
            "{set}"
        );
        let cookie = set.split(';').next().unwrap().to_owned();
        assert!(!cookie.contains(&code), "the cookie is not the pod code");

        assert_eq!(
            with_cookie(&env, "/v1/auth/sign-ins", &cookie, &[WEB]).await,
            StatusCode::OK
        );
        // Not without the web client's header, nor from another site.
        assert_eq!(
            with_cookie(&env, "/v1/auth/sign-ins", &cookie, &[]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            with_cookie(
                &env,
                "/v1/auth/sign-ins",
                &cookie,
                &[WEB, ("sec-fetch-site", "cross-site")]
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        // Never the pod's MCP endpoint.
        let mcp = client()
            .post(env.url("/v1/mcp"))
            .header("cookie", &cookie)
            .header("x-construct-client", "web")
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .send()
            .await
            .unwrap();
        assert_eq!(mcp.status(), StatusCode::FORBIDDEN);
        // A pod code put in the cookie is not taken.
        assert_eq!(
            with_cookie(
                &env,
                "/v1/auth/sign-ins",
                &format!("construct_web={code}"),
                &[WEB]
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        // Logging out ends this browser's session; the pod code still opens the pod.
        let out = client()
            .post(env.url("/web/logout"))
            .header("cookie", &cookie)
            .header("x-construct-client", "web")
            .send()
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::OK);
        assert!(
            out.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert_eq!(
            with_cookie(&env, "/v1/auth/sign-ins", &cookie, &[WEB]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&env.url("/v1/auth/sign-ins"), Some(&code)).await.0,
            StatusCode::OK
        );
    }

    /// The cookie is `HttpOnly`: a Pixelfed sign-in made through it answers
    /// the page without the session's token, and still signs the browser in.
    #[tokio::test]
    async fn a_sign_in_through_the_cookie_answers_no_session_token() {
        let (env, _files) = web_env().await;
        let code = env.create("cookie").await;
        let set = open(&env, &code, true).await.1.expect("a cookie");
        let cookie = set.split(';').next().unwrap().to_owned();
        let token = cookie.strip_prefix("construct_web=").unwrap().to_owned();
        let web_post = |path: &str, body: Value| {
            client()
                .post(env.url(path))
                .header("cookie", &cookie)
                .header("x-construct-client", "web")
                .json(&body)
                .send()
        };
        let app: Value = web_post(
            "/v1/auth/pixelfed/app",
            json!({ "instance_url": env.fake.url, "redirect_uri": REDIRECT }),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let (upstream, verifier) = authorize(&app).await;
        let resp = web_post(
            "/v1/auth/pixelfed/token",
            json!({ "instance_url": env.fake.url, "code": upstream, "code_verifier": verifier,
                    "redirect_uri": REDIRECT }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().await.unwrap();
        assert!(
            !text.contains(&token),
            "the cookie's value reached the page: {text}"
        );
        let body: Value = serde_json::from_str(&text).unwrap();
        assert!(body.get("session_token").is_none(), "{body}");
        assert!(body["account"]["id"].is_string(), "{body}");
        let sign_ins: Value = client()
            .get(env.url("/v1/auth/sign-ins"))
            .header("cookie", &cookie)
            .header("x-construct-client", "web")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(sign_ins.to_string().contains("pixelfed"), "{sign_ins}");
    }

    /// Decision `web-sessions-share-sign-ins`: typing the pod code in a
    /// browser starts it signed in where the code is, on the same server;
    /// logging the browser out leaves the code's sign-in alone.
    #[tokio::test]
    async fn a_browser_starts_with_the_pod_codes_sign_ins() {
        let (env, _files) = web_env().await;
        let code = env.create("shared").await;
        sign_in(&env, &code).await;
        let cookie = open(&env, &code, true).await.1.expect("a cookie");
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let sign_ins = client()
            .get(env.url("/v1/auth/sign-ins"))
            .header("cookie", &cookie)
            .header("x-construct-client", "web")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let pixelfed = &sign_ins["sign_ins"][0];
        assert_eq!(pixelfed["id"], "pixelfed", "{sign_ins}");
        assert_eq!(
            pixelfed["server"].as_str().map(|s| s.trim_end_matches('/')),
            Some(env.fake.url.trim_end_matches('/')),
            "the server the code signed in to: {sign_ins}"
        );

        client()
            .post(env.url("/web/logout"))
            .header("cookie", &cookie)
            .header("x-construct-client", "web")
            .send()
            .await
            .unwrap();
        let (_, _, own) = get(&env.url("/v1/auth/sign-ins"), Some(&code)).await;
        assert_eq!(own["sign_ins"][0]["id"], "pixelfed", "{own}");
    }

    #[tokio::test]
    async fn the_image_proxy_needs_a_session_and_a_public_url() {
        let (env, _files) = web_env().await;
        let code = env.create("images").await;
        let cookie = open(&env, &code, true).await.1.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let img = |q: &str, c: Option<&str>| {
            let mut req = client()
                .get(env.url(&format!("/web/image?url={q}")))
                .header("x-construct-client", "web");
            if let Some(c) = c {
                req = req.header("cookie", c);
            }
            async move { req.send().await.unwrap().status() }
        };
        assert_eq!(
            img("https%3A%2F%2Fexample.org%2Fa.png", None).await,
            StatusCode::UNAUTHORIZED
        );
        // The pod's own network is out of reach.
        assert_eq!(
            img("http%3A%2F%2F127.0.0.1%3A8080%2Fhealthz", Some(&cookie)).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            img("file%3A%2F%2F%2Fetc%2Fpasswd", Some(&cookie)).await,
            StatusCode::BAD_REQUEST
        );
    }

    /// Calls a pod MCP tool with an MCP credential: its JSON answer.
    async fn tool(env: &Env, key: &str, name: &str, args: Value) -> Value {
        let resp: Value = client()
            .post(env.url("/v1/mcp"))
            .bearer_auth(key)
            .header("accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("null");
        serde_json::from_str(text).unwrap_or(resp)
    }

    async fn mcp_key(env: &Env, pod: &str) -> String {
        let (host, pod) = (env.host.clone(), pod.to_owned());
        tokio::task::spawn_blocking(move || host.issue_mcp(&pod, "test").unwrap())
            .await
            .unwrap()
    }

    /// Decision `public-web-links`: a reviewed copy of a layout and the
    /// Collections rows it shows, read at `/s/<link>` by anyone, without the
    /// fields it doesn't show or the controls that write; revoked at once.
    #[tokio::test]
    async fn a_public_link_shows_a_reviewed_copy_until_it_is_revoked() {
        // Off: no tool, no page.
        let (env, _files) = web_env().await;
        env.create("off").await;
        let key = mcp_key(&env, "off").await;
        let off = tool(&env, &key, "share_publicly", json!({ "layout": "home" })).await;
        assert_eq!(off["error"], "public_links_off", "{off}");

        let files = tempfile::tempdir().unwrap();
        std::fs::write(
            files.path().join("index.html"),
            "<title>The Construct</title>",
        )
        .unwrap();
        let env = env_with(HostConfig {
            web: Some(WebConfig {
                dir: files.path().to_owned(),
            }),
            public_web: true,
            ..HostConfig::default()
        })
        .await;
        env.create("cook").await;
        let key = mcp_key(&env, "cook").await;
        let def = tool(&env, &key, "define_collection", json!({ "name": "recipes", "fields": [
            { "name": "title", "type": "text" }, { "name": "notes", "type": "text", "optional": true },
            { "name": "photo", "type": "text", "optional": true } ] })).await;
        assert!(def.get("error").is_none(), "{def}");
        for (t, n) in [
            ("Lentil soup", "for Dana's birthday"),
            ("Shakshuka", "too spicy"),
        ] {
            tool(
                &env,
                &key,
                "add_row",
                json!({ "collection": "recipes",
                "values": { "title": t, "notes": n, "photo": "https://img.example/soup.png" } }),
            )
            .await;
        }
        let made = tool(&env, &key, "create_layout", json!({ "title": "Recipes",
            "feeds": [["collections", "recipes"]],
            "layout": { "type": "core://Page", "id": "recipes", "title": "Recipes <b>", "root":
              { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Title", "value_expr": "Our recipes" },
                { "type": "core://Button", "label": "Add one", "on_click": { "intent": "action.mutation.create",
                  "query_key": "collection:recipes", "entity_uri": "collections://recipes/$nonce", "entity": {},
                  "endpoint": "/v1/collections/recipes/create", "payload": {} } },
                { "type": "core://List", "query_key": "collection:recipes", "item_template":
                  { "type": "core://Column", "children": [
                    { "type": "core://Text", "style": "Body", "value_expr": "$item.title" },
                    { "type": "core://Image", "url": "$item.photo" } ] } } ] } } })).await;
        let layout = made["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{made}"))
            .to_owned();
        tool(
            &env,
            &key,
            "update_layout",
            json!({ "id": "home", "layout": { "type": "core://Page", "id": "home",
            "title": "Home", "root": { "type": "core://Layout", "ref": layout } } }),
        )
        .await;

        // The review says what goes, and nothing is shared yet.
        let review = tool(&env, &key, "share_publicly", json!({ "layout": layout })).await;
        let reads = &review["anyone_with_the_link_reads"];
        assert_eq!(reads["rows"], 2, "{review}");
        assert!(
            !reads["fields"]
                .as_array()
                .unwrap()
                .contains(&json!("notes")),
            "{review}"
        );
        assert!(
            reads["texts"]
                .as_array()
                .unwrap()
                .contains(&json!("Our recipes")),
            "{review}"
        );
        assert_eq!(
            review["taken_out"]["controls_that_write_or_go_elsewhere"],
            1
        );
        assert_eq!(
            tool(&env, &key, "list_public_links", json!({})).await["links"],
            json!([])
        );
        // A wrong review id shares nothing.
        let again = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": layout, "confirm": "review-nope" }),
        )
        .await;
        assert!(again["review_id"].is_string());
        let shared = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": layout, "confirm": review["review_id"] }),
        )
        .await;
        let link = shared["link"]
            .as_str()
            .unwrap_or_else(|| panic!("{shared}"))
            .to_owned();

        // Anyone reads it: no cookie, no header.
        let view: Value = client()
            .get(env.url(&format!("/s/{link}/view")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let text = view.to_string();
        assert!(
            text.contains("Lentil soup") && text.contains("Our recipes"),
            "{text}"
        );
        assert!(
            !text.contains("birthday") && !text.contains("Add one"),
            "{text}"
        );
        let page = client()
            .get(env.url(&format!("/s/{link}")))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            page.contains("<title>Recipes &lt;b&gt;</title>"),
            "the title is escaped: {page}"
        );
        // Its image proxy passes only what the copy names.
        let img = |u: &str| {
            client()
                .get(env.url(&format!("/s/{link}/image?url={u}")))
                .send()
        };
        assert_eq!(
            img("https%3A%2F%2Fother.example%2Fx.png")
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        // An unknown or malformed link is nothing.
        let unknown = client()
            .get(env.url("/s/pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/view"))
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        // New rows go out without a review; a layout edited since doesn't.
        let view = || async {
            client()
                .get(env.url(&format!("/s/{link}/view")))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        };
        let add = |t: &'static str| {
            tool(
                &env,
                &key,
                "add_row",
                json!({ "collection": "recipes", "values": { "title": t, "notes": "for Dana's birthday" } }),
            )
        };
        add("Pho").await;
        let mut shown = false;
        for _ in 0..100 {
            if view().await.contains("Pho") {
                shown = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(shown, "a new row reaches the page: {}", view().await);
        let row_shows_notes = json!({ "type": "core://Page", "id": "recipes", "title": "Recipes <b>", "root":
            { "type": "core://List", "query_key": "collection:recipes", "item_template":
              { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Body", "value_expr": "$item.title" },
                { "type": "core://Text", "style": "Body", "value_expr": "$item.notes" } ] } } });
        let edited = tool(
            &env,
            &key,
            "update_layout",
            json!({ "id": layout, "layout": row_shows_notes }),
        )
        .await;
        assert!(edited.get("error").is_none(), "{edited}");
        add("Dal").await;
        // Past the refresh's settling time: the page keeps its reviewed copy.
        tokio::time::sleep(Duration::from_secs(4)).await;
        let held = view().await;
        assert!(
            !held.contains("birthday") && !held.contains("Dal"),
            "an unreviewed layout went out: {held}"
        );
        // Shared again with a new review, the same link shows the edit.
        let review = tool(&env, &key, "share_publicly", json!({ "layout": layout })).await;
        let again = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": layout, "confirm": review["review_id"] }),
        )
        .await;
        assert_eq!(again["link"], link.as_str(), "{again}");
        assert!(view().await.contains("birthday"));

        // Revoked: gone at once.
        let gone = tool(&env, &key, "unshare_publicly", json!({ "link": link })).await;
        assert_eq!(gone["ok"], true, "{gone}");
        let after = client()
            .get(env.url(&format!("/s/{link}/view")))
            .send()
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::NOT_FOUND);
    }

    /// A public link of a collection's sites carries each site's alerts
    /// (`collection:<c>:alerts`, filtered by `filler_id`), and follows
    /// alert edits as it follows rows.
    #[tokio::test]
    async fn a_public_link_carries_each_sites_alerts_and_follows_their_edits() {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(
            files.path().join("index.html"),
            "<title>The Construct</title>",
        )
        .unwrap();
        let env = env_with(HostConfig {
            web: Some(WebConfig {
                dir: files.path().to_owned(),
            }),
            public_web: true,
            ..HostConfig::default()
        })
        .await;
        env.create("town").await;
        let key = mcp_key(&env, "town").await;
        let filler = |url: &str, title: &str| {
            json!({ "kind": "fetch", "url": url, "extract": "json", "items": "data",
                "map": { "title": "title" }, "title": title })
        };
        let def = tool(
            &env,
            &key,
            "define_collection",
            json!({ "name": "gigs",
            "fields": [{ "name": "title", "type": "text", "optional": true }],
            "fillers": [filler("https://hall.example/feed.json", "Town hall"),
                        filler("https://club.example/feed.json", "Chess club")] }),
        )
        .await;
        assert!(def.get("error").is_none(), "{def}");
        let d = tool(
            &env,
            &key,
            "describe_collection",
            json!({ "collection": "gigs" }),
        )
        .await;
        let ids: Vec<String> = d["definition"]["fillers"]
            .as_array()
            .unwrap_or_else(|| panic!("{d}"))
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_owned())
            .collect();
        let added = tool(
            &env,
            &key,
            "apply_change",
            json!({ "collection": "gigs", "changes": [
            { "op": "add_alert", "name": "Budget", "any": ["budget cuts"],
              "none": ["class", "workshop"], "filler": ids[0] },
            { "op": "add_alert", "name": "Openings", "any": ["open night"], "filler": ids[1] }] }),
        )
        .await;
        assert!(added.get("error").is_none(), "{added}");
        let site = tool(
            &env,
            &key,
            "create_layout",
            json!({ "title": "Site",
            "feeds": [["collections", "gigs"]],
            "layout": { "type": "core://Page", "id": "site", "title": "Site", "root":
              { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Title",
                  "value_expr": "${collections://gigs/fillers/$route.id#title}" },
                { "type": "core://Text", "style": "Body",
                  "value_expr": "${collections://gigs/fillers/$route.id#alerts_text}" },
                { "type": "core://List", "query_key": "collection:gigs:alerts",
                  "where": [{ "field": "filler_id", "equals": "$route.id" }],
                  "item_template": { "type": "core://Text", "style": "Body",
                    "value_expr": "$item.name: $item.summary" } } ] } } }),
        )
        .await;
        let site = site["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{site}"))
            .to_owned();
        let sites = tool(&env, &key, "create_layout", json!({ "title": "Sites",
            "feeds": [["collections", "gigs"]],
            "layout": { "type": "core://Page", "id": "sites", "title": "Sites", "root":
              { "type": "core://List", "query_key": "collection:gigs:fillers", "item_template":
                { "type": "core://Tappable", "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.title" },
                  "on_click": { "intent": "action.navigation.open", "ref": site,
                    "params": { "id": "$item.id" }, "presentation": "Push" } } } } })).await;
        let sites = sites["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{sites}"))
            .to_owned();
        tool(
            &env,
            &key,
            "update_layout",
            json!({ "id": "home", "layout": { "type": "core://Page",
            "id": "home", "title": "Home", "root": { "type": "core://Layout", "ref": sites } } }),
        )
        .await;

        let review = tool(&env, &key, "share_publicly", json!({ "layout": sites })).await;
        let shared = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": sites, "confirm": review["review_id"] }),
        )
        .await;
        let link = shared["link"]
            .as_str()
            .unwrap_or_else(|| panic!("{shared}"))
            .to_owned();
        let view = || async {
            let v: Value = client()
                .get(env.url(&format!("/s/{link}/view")))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v
        };
        let first = view().await;
        assert_eq!(
            first["data"]["queries"]["collection:gigs:alerts"]
                .as_array()
                .map(Vec::len),
            Some(2),
            "both sites' alerts go: {first}"
        );
        let text = first.to_string();
        assert!(
            text.contains("budget cuts")
                && text.contains("workshop")
                && text.contains("open night"),
            "{text}"
        );
        assert!(
            text.contains(&ids[0]),
            "the filler_id goes for the where: {text}"
        );

        // An edited alert reaches the page without a review, as rows do.
        let edited = tool(&env, &key, "apply_change", json!({ "collection": "gigs", "changes": [
            { "op": "update_alert", "alert": first_alert(&first, "Budget"), "any": ["hiring freeze"] }] })).await;
        assert!(edited.get("error").is_none(), "{edited}");
        let mut seen = String::new();
        for _ in 0..100 {
            seen = view().await.to_string();
            if seen.contains("hiring freeze") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            seen.contains("hiring freeze") && !seen.contains("budget cuts"),
            "{seen}"
        );
        // A removed one goes from the page.
        let removed = tool(
            &env,
            &key,
            "apply_change",
            json!({ "collection": "gigs", "changes": [
            { "op": "remove_alert", "alert": first_alert(&first, "Openings") }] }),
        )
        .await;
        assert!(removed.get("error").is_none(), "{removed}");
        for _ in 0..100 {
            seen = view().await.to_string();
            if !seen.contains("open night") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!seen.contains("open night"), "{seen}");
        assert!(seen.contains("hiring freeze"), "{seen}");
    }

    /// The id of the alert named `name` in a public copy's view.
    fn first_alert(view: &Value, name: &str) -> String {
        view["data"]["entities"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(uri, e)| uri.starts_with("collections://gigs/alerts/") && e["name"] == name)
            .map(|(uri, _)| uri.rsplit('/').next().unwrap().to_owned())
            .unwrap_or_else(|| panic!("no alert {name}: {view}"))
    }

    /// Decision `public-links-share-the-closure`: a list whose rows push
    /// their recipe screen is shared whole; each recipe carries only its
    /// rows; a screen reading `$route.id` shares with `params`; a short
    /// path answers the same copy; with public pages off, links answer that
    /// sharing is paused.
    #[tokio::test]
    async fn a_public_link_carries_the_screens_its_rows_open() {
        // Off: a link answers that sharing is paused, not a 404.
        let (off, _off_files) = web_env().await;
        let paused = client()
            .get(off.url("/s/pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .send()
            .await
            .unwrap();
        assert_eq!(paused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(paused.text().await.unwrap().contains("Sharing is paused"));

        let files = tempfile::tempdir().unwrap();
        std::fs::write(
            files.path().join("index.html"),
            "<title>The Construct</title>",
        )
        .unwrap();
        let env = env_with(HostConfig {
            web: Some(WebConfig {
                dir: files.path().to_owned(),
            }),
            public_web: true,
            ..HostConfig::default()
        })
        .await;
        env.create("cook").await;
        let key = mcp_key(&env, "cook").await;
        for (name, fields) in [
            (
                "meals",
                json!([{ "name": "name", "type": "text" }, { "name": "notes", "type": "text", "optional": true }]),
            ),
            (
                "lines",
                json!([{ "name": "meal_id", "type": "text" }, { "name": "thing", "type": "text" }]),
            ),
        ] {
            let def = tool(
                &env,
                &key,
                "define_collection",
                json!({ "name": name, "fields": fields }),
            )
            .await;
            assert!(def.get("error").is_none(), "{def}");
        }
        let mut meals = Vec::new();
        for (meal, things) in [
            ("Lentil soup", ["Red lentils", "Cumin"]),
            ("Shakshuka", ["Eggs", "Tomatoes"]),
        ] {
            let row = tool(
                &env,
                &key,
                "add_row",
                json!({ "collection": "meals",
                "values": { "name": meal, "notes": "only for me" } }),
            )
            .await;
            let id = row["id"]
                .as_str()
                .unwrap_or_else(|| panic!("{row}"))
                .to_owned();
            for t in things {
                tool(
                    &env,
                    &key,
                    "add_row",
                    json!({ "collection": "lines",
                    "values": { "meal_id": id, "thing": t } }),
                )
                .await;
            }
            meals.push(id);
        }
        let recipe = tool(&env, &key, "create_layout", json!({ "title": "Recipe",
            "feeds": [["collections", "meals"], ["collections", "lines"]],
            "layout": { "type": "core://Page", "id": "recipe", "title": "Recipe", "root":
              { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Title", "value_expr": "${collections://meals/$route.id#name}" },
                { "type": "core://List", "query_key": "collection:lines",
                  "where": [{ "field": "meal_id", "equals": "$route.id" }],
                  "empty_text": "No ingredients yet",
                  "item_template": { "type": "core://Text", "style": "Body", "value_expr": "$item.thing" } } ] } } })).await;
        let recipe = recipe["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{recipe}"))
            .to_owned();
        let list = tool(&env, &key, "create_layout", json!({ "title": "Meals",
            "feeds": [["collections", "meals"], ["collections", "lines"]],
            "layout": { "type": "core://Page", "id": "meals", "title": "Meals", "root":
              { "type": "core://List", "query_key": "collection:meals", "item_template":
                { "type": "core://Tappable", "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.name" },
                  "on_click": { "intent": "action.navigation.open", "ref": recipe,
                    "params": { "id": "$item.id" }, "presentation": "Push" } } } } })).await;
        let list = list["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{list}"))
            .to_owned();
        tool(
            &env,
            &key,
            "update_layout",
            json!({ "id": "home", "layout": { "type": "core://Page",
            "id": "home", "title": "Home", "root": { "type": "core://Layout", "ref": list } } }),
        )
        .await;

        // The recipe screen alone, with params: one meal and its lines.
        let review = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": recipe, "params": { "id": meals[0] } }),
        )
        .await;
        assert_eq!(review["anyone_with_the_link_reads"]["rows"], 3, "{review}");
        let shared = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": recipe,
            "params": { "id": meals[0] }, "confirm": review["review_id"] }),
        )
        .await;
        let link = shared["link"]
            .as_str()
            .unwrap_or_else(|| panic!("{shared}"))
            .to_owned();
        let view = client()
            .get(env.url(&format!("/s/{link}/view")))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            view.contains("Cumin") && !view.contains("Eggs") && !view.contains("Shakshuka"),
            "{view}"
        );
        // Other params are another link.
        let other = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": recipe, "params": { "id": meals[1] } }),
        )
        .await;
        assert_ne!(other["review_id"], review["review_id"]);

        // The list: its rows push the recipe, which comes along.
        let missing = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": list, "slug": "our-meals" }),
        )
        .await;
        assert_eq!(missing["error"], "no_handle", "{missing}");
        tool(&env, &key, "set_handle", json!({ "handle": "cook" })).await;
        let review = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": list, "slug": "our-meals" }),
        )
        .await;
        let layouts = review["anyone_with_the_link_reads"]["layouts"]
            .as_array()
            .unwrap_or_else(|| panic!("{review}"));
        assert_eq!(layouts.len(), 2, "{review}");
        assert!(
            !review["anyone_with_the_link_reads"]["fields"]
                .as_array()
                .unwrap()
                .contains(&json!("notes"))
        );
        let shared = tool(
            &env,
            &key,
            "share_publicly",
            json!({ "layout": list, "slug": "our-meals",
            "confirm": review["review_id"] }),
        )
        .await;
        assert!(
            shared["short_url"]
                .as_str()
                .is_some_and(|u| u.ends_with("/cook/our-meals")),
            "{shared}"
        );
        let link = shared["link"].as_str().unwrap().to_owned();
        let view: Value = client()
            .get(env.url(&format!("/s/{link}/view")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(view["format"], 2);
        assert!(view["layouts"][&recipe].is_object(), "{view}");
        assert!(
            view["node"].to_string().contains(&recipe),
            "the push stays: {view}"
        );
        let text = view.to_string();
        assert!(
            text.contains("Eggs") && text.contains("Cumin") && !text.contains("only for me"),
            "{text}"
        );
        let short: Value = client()
            .get(env.url("/cook/our-meals/view"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(short, view);
        // A page already open at the first form (`/@…`) still reads its copy.
        let first: Value = client()
            .get(env.url("/@cook/our-meals/view"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(first, view);
        let page = client()
            .get(env.url("/cook/our-meals"))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        // One row's page, by its name (decision `public-row-links`): titled
        // with the row; a name no row has is the list's page.
        let row = client()
            .get(env.url("/cook/our-meals/shakshuka"))
            .send()
            .await
            .unwrap();
        assert_eq!(row.status(), StatusCode::OK);
        let row = row.text().await.unwrap();
        assert!(row.contains("<title>Shakshuka · "), "{row}");
        let gone = client()
            .get(env.url("/cook/our-meals/lentil-stew"))
            .send()
            .await
            .unwrap();
        assert_eq!(gone.status(), StatusCode::OK);
        assert!(!gone.text().await.unwrap().contains("Shakshuka"));
        assert_eq!(
            client()
                .get(env.url("/cook/nope/view"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        // The link as apps write it (a trailing slash, `@` as `%40`, other
        // case) redirects to the page's own address.
        let no_redirects = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for (from, to) in [
            // The first form, with `@`, as sent before.
            ("/@cook/our-meals".to_owned(), "/cook/our-meals".to_owned()),
            ("/@cook/our-meals/".to_owned(), "/cook/our-meals".to_owned()),
            (
                "/%40cook/our-meals".to_owned(),
                "/cook/our-meals".to_owned(),
            ),
            (
                "/%40Cook/Our-Meals/".to_owned(),
                "/cook/our-meals".to_owned(),
            ),
            ("/@Cook/our-meals".to_owned(), "/cook/our-meals".to_owned()),
            ("/cook/our-meals/".to_owned(), "/cook/our-meals".to_owned()),
            ("/Cook/Our-Meals".to_owned(), "/cook/our-meals".to_owned()),
            (
                "/web/@cook/our-meals".to_owned(),
                "/cook/our-meals".to_owned(),
            ),
            (format!("/web/s/{link}"), format!("/s/{link}")),
            (format!("/s/{link}/"), format!("/s/{link}")),
        ] {
            let r = no_redirects.get(env.url(&from)).send().await.unwrap();
            assert_eq!(r.status(), StatusCode::PERMANENT_REDIRECT, "{from}");
            assert_eq!(r.headers()["location"], to.as_str(), "{from}");
        }
        // The pod's own paths with two segments still reach it: a handle
        // can't be one of their first segments.
        let pod = client()
            .get(env.url("/v1/construct"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        // (The test's key only reaches /v1/mcp: the host's own answer for
        // a pod path, not a short path's 404.)
        assert_eq!(pod.status(), StatusCode::FORBIDDEN, "/v1/construct");
        // The short path holds the handle.
        let renamed = tool(&env, &key, "set_handle", json!({ "handle": "chef" })).await;
        assert_eq!(renamed["error"], "handle_in_use", "{renamed}");
    }

    /// A temporary pod (anyone can make one) can't share a public page,
    /// even where the server serves them.
    #[tokio::test]
    async fn a_temporary_pod_cannot_share_publicly() {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(
            files.path().join("index.html"),
            "<title>The Construct</title>",
        )
        .unwrap();
        let env = env_with(HostConfig {
            web: Some(WebConfig {
                dir: files.path().to_owned(),
            }),
            public_web: true,
            temporary: Some(TemporaryPods {
                hours: 1,
                max_live: 2,
            }),
            ..HostConfig::default()
        })
        .await;
        let (status, body) = post(&env.url("/v1/pods/temporary"), None, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = body["session_token"].as_str().unwrap().to_owned();
        let answer = tool(&env, &token, "share_publicly", json!({ "layout": "home" })).await;
        assert_eq!(answer["error"], "public_links_off", "{answer}");
    }
}

/// Decision `ical-feeds`.
mod calendar_feeds;
