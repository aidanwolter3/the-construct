//! Sign in with Pixelfed (docs/DECISIONS.md D19) end to end: the engine runs
//! the PKCE flow against the real pod (`server::app`) backed by the fake
//! upstream (`fake_pixelfed`), both in-process on random local ports. The
//! "browser" is a test `UrlOpener` that GETs the authorize URL with
//! `&auto=1` (the fake then approves with a 302 straight to the redirect URI).

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use client_core::auth::session_key;
use client_core::{
    CanvasEngine, EngineConfig, EngineOptions, MemoryStore, RedirectMode, SecureStore,
    StateSnapshot, UrlOpener,
};
use serde_json::{Value, json};
use server::pixelfed::routes::session_hash;
use server::{AppState, PixelfedConfig};

const TIMEOUT: Duration = Duration::from_secs(15);
const SEED: &str = "seed-token";
const CUSTOM_REDIRECT: &str = "canvas://oauth";
const WORKSPACE: &str = "pixelfed_home";
const LIKE_COUNT_EXPR: &str = "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1";

fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(20),
        backoff_cap: Duration::from_millis(200),
    }
}

fn spawn_serve(rt: &tokio::runtime::Runtime, listener: TcpListener, app: axum::Router) {
    listener.set_nonblocking(true).unwrap();
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
}

fn bind() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

/// The fake upstream (no auto-approve: the opener's `auto=1` does it, and
/// a seeded token for inspecting its state) and the pod in front of it.
struct Env {
    fake: String,
    pod: String,
    pod_state: Arc<AppState>,
    rt: Option<tokio::runtime::Runtime>,
}

impl Env {
    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (fl, faddr) = bind();
        let fake = format!("http://{faddr}");
        let fake_state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
            fixtures_dir: fake_pixelfed::default_fixtures_dir(),
            public_url: fake.clone(),
            auto_approve: false,
            seed_token: Some(SEED.to_owned()),
        })
        .unwrap();
        spawn_serve(&rt, fl, fake_pixelfed::app(Arc::new(fake_state)));
        let pod_state =
            Arc::new(AppState::in_memory().with_pixelfed(PixelfedConfig::new(&fake, None)));
        let (pl, paddr) = bind();
        spawn_serve(&rt, pl, server::app(Arc::clone(&pod_state)));
        Self {
            fake,
            pod: format!("http://{paddr}"),
            pod_state,
            rt: Some(rt),
        }
    }

    fn login_intent(&self) -> String {
        json!({ "intent": "action.auth.login", "instance_url": self.fake }).to_string()
    }

    /// The fake's view of a status (via the seeded token).
    fn upstream_status(&self, id: &str) -> Value {
        let list: Vec<Value> = reqwest::blocking::Client::new()
            .get(format!("{}/api/v1/timelines/home", self.fake))
            .bearer_auth(SEED)
            .send()
            .unwrap()
            .json()
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

    fn session_exists(&self, token: &str) -> bool {
        self.pod_state
            .store()
            .session_exists(&session_hash(token))
            .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// A browser that auto-approves: GET `<authorize url>&auto=1` on its own
/// thread (a real browser is asynchronous too), following redirects, which
/// lands on the engine's loopback listener.
#[derive(Default)]
struct AutoApproveBrowser {
    results: Arc<Mutex<Vec<(u16, String)>>>,
    /// Every authorize URL opened.
    urls: Mutex<Vec<String>>,
}

impl UrlOpener for AutoApproveBrowser {
    fn open(&self, url: &str) -> bool {
        self.urls.lock().unwrap().push(url.to_owned());
        let url = format!("{url}&auto=1");
        let results = Arc::clone(&self.results);
        thread::spawn(move || {
            let resp = reqwest::blocking::Client::new().get(&url).send().unwrap();
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            results.lock().unwrap().push((status, body));
        });
        true
    }
}

/// Hands the authorize URL to the test (custom-scheme mode).
struct CapturingOpener(Mutex<mpsc::Sender<String>>);

impl UrlOpener for CapturingOpener {
    fn open(&self, url: &str) -> bool {
        self.0.lock().unwrap().send(url.to_owned()).unwrap();
        true
    }
}

fn engine(
    env_pod: &str,
    store: &Arc<MemoryStore>,
    opener: Option<Arc<dyn UrlOpener>>,
    redirect: RedirectMode,
) -> CanvasEngine {
    CanvasEngine::init_with_options(
        "",
        env_pod,
        WORKSPACE,
        EngineOptions {
            config: fast_config(),
            secure_store: Some(Arc::clone(store) as Arc<dyn SecureStore>),
            url_opener: opener,
            redirect,
            // Random ports: tests run in parallel (the fixed default is covered below).
            loopback_port: 0,
        },
    )
}

fn loopback_engine(env: &Env, store: &Arc<MemoryStore>) -> CanvasEngine {
    engine(
        &env.pod,
        store,
        Some(Arc::new(AutoApproveBrowser::default())),
        RedirectMode::Loopback,
    )
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn auth(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.auth_json).unwrap()
}

fn queries(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.queries_json).unwrap()
}

fn home_query_len(snap: &StateSnapshot) -> usize {
    queries(snap)["timeline:home"]
        .as_array()
        .map_or(0, Vec::len)
}

fn entities(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.entities_json).unwrap()
}

fn wait_signed_in(engine: &CanvasEngine) {
    wait_until("signed in with the home feed", || {
        let s = engine.snapshot();
        auth(&s)["state"] == "signed_in" && home_query_len(&s) > 0
    });
}

fn wait_signed_out_bundle(engine: &CanvasEngine) {
    wait_until("the signed-out bundle", || {
        let s = engine.snapshot();
        s.workspace_json.contains("Sign in with Pixelfed") && home_query_len(&s) == 0
    });
}

fn stored_token(env: &Env, store: &MemoryStore) -> String {
    let raw = store.get(&session_key(&env.pod)).expect("stored session");
    let v: Value = serde_json::from_str(&raw).unwrap();
    v["token"].as_str().unwrap().to_owned()
}

fn like_intent(id: &str) -> String {
    json!({
        "intent": "action.mutation.optimistic",
        "entity_uri": format!("pixelfed://posts/{id}"),
        "field": "is_liked",
        "value_expr": "!entity.is_liked",
        "rollback_value": false,
        "endpoint": "/v1/pixelfed/posts/like",
        "payload": { "id": id, "liked": "$value" },
        "also": [{ "field": "like_count", "value_expr": LIKE_COUNT_EXPR }],
    })
    .to_string()
}

/// A post in the feed that is not liked yet: (id, like_count).
fn unliked_post(snap: &StateSnapshot) -> (String, i64) {
    let ents = entities(snap);
    queries(snap)["timeline:home"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| &ents[u.as_str().unwrap()])
        .find(|p| p["is_liked"] == false)
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["like_count"].as_i64().unwrap(),
            )
        })
        .expect("an unliked post")
}

#[test]
fn loopback_sign_in_stores_session_and_fetches_the_feed() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let browser = Arc::new(AutoApproveBrowser::default());
    let engine = engine(
        &env.pod,
        &store,
        Some(Arc::clone(&browser) as Arc<dyn UrlOpener>),
        RedirectMode::Loopback,
    );
    let states: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&states);
    engine.subscribe(move |s| {
        let st = auth(&s)["state"].as_str().unwrap().to_owned();
        let mut log = log.lock().unwrap();
        if log.last() != Some(&st) {
            log.push(st);
        }
    });
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    wait_signed_out_bundle(&engine);

    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);

    let snap = engine.snapshot();
    let a = auth(&snap);
    assert_eq!(a["username"], "canvasdemo");
    assert_eq!(a["display_name"], "Canvas Demo");
    assert!(
        a["avatar_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/v1/pixelfed/media/", env.pod))
    );
    assert!(snap.workspace_json.contains("FeedList") || snap.workspace_json.contains("feed"));
    let ents = entities(&snap);
    for uri in queries(&snap)["timeline:home"].as_array().unwrap() {
        assert!(ents.get(uri.as_str().unwrap()).is_some(), "{uri} missing");
    }
    assert_eq!(
        *states.lock().unwrap(),
        vec!["signed_out", "pending", "signed_in"]
    );
    let token = stored_token(&env, &store);
    assert!(env.session_exists(&token));
    // The browser landed on the loopback listener's success page.
    let results = browser.results.lock().unwrap().clone();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 200);
    assert!(results[0].1.contains("Signed in"));
}

#[test]
fn custom_scheme_sign_in_rejects_a_wrong_state() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let (tx, rx) = mpsc::channel();
    let engine = engine(
        &env.pod,
        &store,
        Some(Arc::new(CapturingOpener(Mutex::new(tx)))),
        RedirectMode::Custom(CUSTOM_REDIRECT.into()),
    );
    assert!(!engine.handle_auth_redirect("canvas://oauth?code=x&state=y"));
    engine.dispatch(&env.login_intent());
    let url = rx.recv_timeout(TIMEOUT).unwrap();
    let parsed = reqwest::Url::parse(&url).unwrap();
    let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(q["redirect_uri"], CUSTOM_REDIRECT);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["scope"], "read write");
    assert!(url.contains("scope=read%20write"));

    let resp = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{url}&auto=1"))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 302);
    let location = resp.headers()["location"].to_str().unwrap().to_owned();
    assert!(location.starts_with("canvas://oauth?"), "{location}");

    // A forged redirect (wrong state) is rejected and the flow keeps waiting.
    let (head, _) = location
        .split_once("&state=")
        .expect("state in the redirect");
    let forged = format!("{head}&state=forged");
    assert_ne!(forged, location);
    assert!(!engine.handle_auth_redirect(&forged));
    assert!(!engine.handle_auth_redirect("canvas://oauth?nothing=1"));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(auth(&engine.snapshot())["state"], "pending");

    assert!(engine.handle_auth_redirect(&location));
    wait_signed_in(&engine);
    // The flow is over: a replay is ignored.
    assert!(!engine.handle_auth_redirect(&location));
    assert_eq!(auth(&engine.snapshot())["username"], "canvasdemo");
}

#[test]
fn authorization_denied_is_an_error() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let (tx, rx) = mpsc::channel();
    let engine = engine(
        &env.pod,
        &store,
        Some(Arc::new(CapturingOpener(Mutex::new(tx)))),
        RedirectMode::Custom(CUSTOM_REDIRECT.into()),
    );
    engine.dispatch(&env.login_intent());
    let url = rx.recv_timeout(TIMEOUT).unwrap();
    let parsed = reqwest::Url::parse(&url).unwrap();
    let state = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    assert!(
        engine.handle_auth_redirect(&format!("canvas://oauth?error=access_denied&state={state}"))
    );
    wait_until("auth error", || {
        auth(&engine.snapshot())["state"] == "error"
    });
    let a = auth(&engine.snapshot());
    assert!(
        a["error"].as_str().unwrap().contains("access_denied"),
        "{a}"
    );
    assert!(store.get(&session_key(&env.pod)).is_none());
}

#[test]
fn no_browser_reports_an_error_but_keeps_waiting() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = engine(&env.pod, &store, None, RedirectMode::Loopback);
    engine.dispatch(&env.login_intent());
    wait_until("no-browser error", || {
        auth(&engine.snapshot())["error"] == "no browser available"
    });
    // A new sign-in supersedes the waiting one.
    engine.dispatch(&env.login_intent());
    wait_until("no-browser error again", || {
        auth(&engine.snapshot())["error"] == "no browser available"
    });
}

#[test]
fn restart_restores_the_session_from_the_store() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    {
        let engine = loopback_engine(&env, &store);
        engine.dispatch(&env.login_intent());
        wait_signed_in(&engine);
    }
    let engine = engine(&env.pod, &store, None, RedirectMode::Loopback);
    // Signed in before any network round trip...
    let first = auth(&engine.snapshot());
    assert_eq!(first["state"], "signed_in");
    assert_eq!(first["username"], "canvasdemo");
    // ...and the bearer gets the signed-in feed.
    wait_signed_in(&engine);
}

#[test]
fn logout_forgets_the_session_everywhere() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let token = stored_token(&env, &store);

    engine.logout();
    // Immediately signed out and gone from the store (no network wait).
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    assert!(store.get(&session_key(&env.pod)).is_none());
    wait_until("pod session deleted", || !env.session_exists(&token));
    wait_signed_out_bundle(&engine);
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
}

#[test]
fn outbox_like_carries_the_bearer_to_the_upstream() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let (id, count) = unliked_post(&engine.snapshot());
    assert_eq!(env.upstream_status(&id)["favourited"], false);

    engine.dispatch(&like_intent(&id));
    let uri = format!("pixelfed://posts/{id}");
    let e = &entities(&engine.snapshot())[&uri];
    assert_eq!(e["is_liked"], true);
    assert_eq!(e["like_count"], count + 1);
    wait_until("like flushed", || engine.snapshot().pending_actions == 0);
    let up = env.upstream_status(&id);
    assert_eq!(up["favourited"], true);
    assert_eq!(up["favourites_count"], count + 1);
    let e = &entities(&engine.snapshot())[&uri];
    assert_eq!(e["is_liked"], true, "not rolled back");
}

#[test]
fn outbox_401_keeps_the_like_pending_until_the_next_sign_in() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let (id, count) = unliked_post(&engine.snapshot());

    // The pod forgets the session (e.g. revoked elsewhere).
    let token = stored_token(&env, &store);
    let revoked = reqwest::blocking::Client::new()
        .post(format!("{}/v1/auth/logout", env.pod))
        .bearer_auth(&token)
        .send()
        .unwrap();
    assert_eq!(revoked.status().as_u16(), 200);

    engine.dispatch(&like_intent(&id));
    wait_until("signed out by the 401", || {
        auth(&engine.snapshot())["state"] == "signed_out"
    });
    assert!(store.get(&session_key(&env.pod)).is_none());
    // Not rolled back, not dropped, not retried in a loop.
    thread::sleep(Duration::from_millis(400));
    let snap = engine.snapshot();
    assert_eq!(snap.pending_actions, 1);
    let uri = format!("pixelfed://posts/{id}");
    assert_eq!(entities(&snap)[&uri]["is_liked"], true);
    assert_eq!(entities(&snap)[&uri]["like_count"], count + 1);
    assert_eq!(env.upstream_status(&id)["favourited"], false);

    // Signing in again resumes the outbox.
    engine.dispatch(&env.login_intent());
    wait_until("like flushed after sign-in", || {
        let s = engine.snapshot();
        auth(&s)["state"] == "signed_in" && s.pending_actions == 0
    });
    assert_eq!(env.upstream_status(&id)["favourited"], true);
    assert_eq!(env.upstream_status(&id)["favourites_count"], count + 1);
    wait_signed_in(&engine);
    assert_eq!(entities(&engine.snapshot())[&uri]["is_liked"], true);
}

#[test]
fn fetch_401_clears_the_session() {
    // A pod that rejects every workspace fetch with 401.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route(
            "/v1/workspaces/{id}",
            axum::routing::get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "{}") }),
        );
    let (l, addr) = bind();
    spawn_serve(&rt, l, app);
    let base = format!("http://{addr}");
    let store = Arc::new(MemoryStore::new());
    store.set(
        &session_key(&base),
        r#"{"token":"stale","username":"canvasdemo"}"#,
    );
    let engine = engine(&base, &store, None, RedirectMode::Loopback);
    assert_eq!(auth(&engine.snapshot())["state"], "signed_in");
    wait_until("signed out by the fetch 401", || {
        auth(&engine.snapshot())["state"] == "signed_out"
    });
    assert!(store.get(&session_key(&base)).is_none());
    rt.shutdown_background();
}

#[test]
fn stale_session_is_cleared_by_the_pods_401_on_fetch() {
    // The real pod answers a bearer it does not know with 401 (+ the signed-out bundle and
    // `X-Canvas-Session: invalid`): the engine forgets the session and refetches without it.
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    {
        let engine = loopback_engine(&env, &store);
        engine.dispatch(&env.login_intent());
        wait_signed_in(&engine);
    }
    let token = stored_token(&env, &store);
    let revoked = reqwest::blocking::Client::new()
        .post(format!("{}/v1/auth/logout", env.pod))
        .bearer_auth(&token)
        .send()
        .unwrap();
    assert_eq!(revoked.status().as_u16(), 200);

    // Restart with the stale session in the store.
    let engine = engine(&env.pod, &store, None, RedirectMode::Loopback);
    assert_eq!(auth(&engine.snapshot())["state"], "signed_in");
    wait_signed_out_bundle(&engine);
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    assert!(store.get(&session_key(&env.pod)).is_none());
}

fn redirect_uri_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .unwrap()
        .1
        .into_owned()
}

#[test]
fn loopback_reuses_the_preferred_port_across_sign_ins() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let browser = Arc::new(AutoApproveBrowser::default());
    let engine = CanvasEngine::init_with_options(
        "",
        &env.pod,
        WORKSPACE,
        EngineOptions {
            config: fast_config(),
            secure_store: Some(Arc::clone(&store) as Arc<dyn SecureStore>),
            url_opener: Some(Arc::clone(&browser) as Arc<dyn UrlOpener>),
            redirect: RedirectMode::Loopback,
            loopback_port: port,
        },
    );
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    engine.logout();
    wait_signed_out_bundle(&engine);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);

    let urls = browser.urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 2);
    let want = format!("http://127.0.0.1:{port}/callback");
    assert_eq!(redirect_uri_of(&urls[0]), want);
    assert_eq!(redirect_uri_of(&urls[1]), want);
}
