//! Sign in with Pixelfed (docs/DECISIONS.md D19) end to end: the engine runs
//! the PKCE flow against the real pod (`server::app`) backed by the fake
//! upstream (`fake_pixelfed`), both in-process on random local ports. The
//! "browser" is a test `UrlOpener` that GETs the authorize URL with
//! `&auto=1` (the fake then approves with a 302 straight to the redirect URI).

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use client_core::auth::{
    PendingAuth, SIGN_IN_DENIED, SIGN_IN_NOT_OWNER, SIGN_IN_POD_UNREACHABLE, SIGN_IN_UPSTREAM_DOWN,
    pending_auth_key, session_key,
};
use client_core::store::{Store, StoreOps};
use client_core::{
    ConstructEngine, EngineConfig, EngineOptions, LOADING_FEED_MESSAGE, MemoryStore, RedirectMode,
    SIGN_IN_INTERRUPTED, SecureStore, StateSnapshot, UrlOpener,
};
use serde_json::{Value, json};
use server::session::session_hash;
use server::{AppState, PixelfedConfig, WithPixelfed};

const TIMEOUT: Duration = Duration::from_secs(15);
const SEED: &str = "seed-token";
const CUSTOM_REDIRECT: &str = "construct://oauth";
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
    /// While true, the pod holds `POST /v1/pixelfed/posts/comment` requests
    /// (so a test can check the optimistic state before the answer lands).
    hold_comments: Arc<AtomicBool>,
    rt: Option<tokio::runtime::Runtime>,
}

impl Env {
    fn start() -> Self {
        Self::start_with(Vec::new())
    }

    /// [`Env::start`] whose pod also accepts these exact redirect URIs
    /// (`PIXELFED_EXTRA_REDIRECTS`).
    fn start_with(extra_redirects: Vec<String>) -> Self {
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
        let pixelfed = PixelfedConfig {
            extra_redirects,
            ..PixelfedConfig::new(&fake, None)
        };
        let pod_state = Arc::new(server::state_in_memory().with_pixelfed(pixelfed));
        let (pl, paddr) = bind();
        let hold_comments = Arc::new(AtomicBool::new(false));
        let hold = Arc::clone(&hold_comments);
        let pod_app = server::app(Arc::clone(&pod_state)).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let hold = Arc::clone(&hold);
                async move {
                    if req.uri().path() == "/v1/pixelfed/posts/comment" {
                        while hold.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                    next.run(req).await
                }
            },
        ));
        spawn_serve(&rt, pl, pod_app);
        Self {
            fake,
            pod: format!("http://{paddr}"),
            pod_state,
            hold_comments,
            rt: Some(rt),
        }
    }

    fn login_intent(&self) -> String {
        json!({ "intent": "action.auth.login", "instance_url": self.fake, "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }).to_string()
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
) -> ConstructEngine {
    engine_in(
        "",
        env_pod,
        Arc::clone(store) as Arc<dyn SecureStore>,
        opener,
        redirect,
    )
}

fn engine_in(
    storage: &str,
    env_pod: &str,
    store: Arc<dyn SecureStore>,
    opener: Option<Arc<dyn UrlOpener>>,
    redirect: RedirectMode,
) -> ConstructEngine {
    ConstructEngine::init_with_options(
        storage,
        env_pod,
        WORKSPACE,
        EngineOptions {
            config: fast_config(),
            secure_store: Some(store),
            url_opener: opener,
            redirect,
            // Random ports: tests run in parallel (the fixed default is covered below).
            loopback_port: 0,
        },
    )
}

fn loopback_engine(env: &Env, store: &Arc<MemoryStore>) -> ConstructEngine {
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

fn wait_signed_in(engine: &ConstructEngine) {
    wait_until("signed in with the home feed", || {
        let s = engine.snapshot();
        auth(&s)["state"] == "signed_in" && home_query_len(&s) > 0
    });
}

fn wait_signed_out_bundle(engine: &ConstructEngine) {
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
    assert_eq!(a["username"], "constructdemo");
    assert_eq!(a["display_name"], "Construct Demo");
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
    // The observer runs on an engine thread and may lag behind snapshot().
    wait_until("the observer to see signed_in", || {
        states.lock().unwrap().last().map(String::as_str) == Some("signed_in")
    });
    let seen = states.lock().unwrap().clone();
    assert_eq!(seen, vec!["signed_out", "pending", "signed_in"]);
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
    // No sign-in at all (in memory or persisted): taken, then reported.
    assert!(engine.handle_auth_redirect("construct://oauth?code=x&state=y"));
    wait_until("interrupted error", || {
        auth(&engine.snapshot())["error"] == SIGN_IN_INTERRUPTED
    });
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
    assert!(location.starts_with("construct://oauth?"), "{location}");

    // A forged redirect (wrong state) is rejected and the flow keeps waiting.
    let (head, _) = location
        .split_once("&state=")
        .expect("state in the redirect");
    let forged = format!("{head}&state=forged");
    assert_ne!(forged, location);
    assert!(!engine.handle_auth_redirect(&forged));
    assert!(!engine.handle_auth_redirect("construct://oauth?nothing=1"));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(auth(&engine.snapshot())["state"], "pending");

    assert!(engine.handle_auth_redirect(&location));
    wait_signed_in(&engine);
    assert!(store.get(&pending_auth_key(&env.pod)).is_none());
    // The flow is over: a replay changes nothing (signed in, no error).
    engine.handle_auth_redirect(&location);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(auth(&engine.snapshot())["state"], "signed_in");
    assert_eq!(auth(&engine.snapshot())["username"], "constructdemo");
}

/// Android App Link redirect (decision `android-app-link-sign-in-redirect`):
/// the engine registers and exchanges the https URI. When the browser keeps
/// the redirect, the pod's fallback page hands the same query to the app as
/// `construct://oauth?…`; the engine matches it by `state` alone, so that
/// completes the sign-in too (the token call still names the https URI).
#[test]
fn app_link_sign_in_completes_from_the_fallback_page() {
    const APP_LINK: &str = "https://pod.example/oauth/android";
    let env = Env::start_with(vec![APP_LINK.into()]);
    let store = Arc::new(MemoryStore::new());
    let (tx, rx) = mpsc::channel();
    let engine = engine(
        &env.pod,
        &store,
        Some(Arc::new(CapturingOpener(Mutex::new(tx)))),
        RedirectMode::Custom(APP_LINK.into()),
    );
    engine.dispatch(&env.login_intent());
    let url = rx.recv_timeout(TIMEOUT).unwrap();
    let parsed = reqwest::Url::parse(&url).unwrap();
    let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(q["redirect_uri"], APP_LINK);
    let resp = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{url}&auto=1"))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 302);
    let location = resp.headers()["location"].to_str().unwrap().to_owned();
    let query = location
        .strip_prefix(&format!("{APP_LINK}?"))
        .unwrap_or_else(|| panic!("redirect to the App Link: {location}"));
    assert!(engine.handle_auth_redirect(&format!("construct://oauth?{query}")));
    wait_signed_in(&engine);
    assert_eq!(auth(&engine.snapshot())["username"], "constructdemo");
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
    assert!(engine.handle_auth_redirect(&format!(
        "construct://oauth?error=access_denied&state={state}"
    )));
    wait_until("auth error", || {
        auth(&engine.snapshot())["state"] == "error"
    });
    // A short message for the status line; "access_denied" is only logged.
    assert_eq!(auth(&engine.snapshot())["error"], SIGN_IN_DENIED);
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
    assert_eq!(first["username"], "constructdemo");
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
    // Immediately signed out; gone from the store soon after (the store is
    // written on a background thread, not on the caller's).
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    wait_until("session deleted from the store", || {
        store.get(&session_key(&env.pod)).is_none()
    });
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
    // Revoke only once the events stream runs with the session: a stream
    // that connects after the revocation would sign out first (its own
    // 401), before the like this test is about is queued.
    let device = engine.device_id();
    wait_until("the events stream open with the session", || {
        env.pod_state.device_is_live(&device)
    });

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
    // The store is written on a background thread, so the delete can land
    // just after the snapshot says signed out.
    wait_until("session deleted from the store", || {
        store.get(&session_key(&env.pod)).is_none()
    });
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
        r#"{"token":"stale","username":"constructdemo"}"#,
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
    // `X-Construct-Session: invalid`): the engine forgets the session and refetches without it.
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
    let engine = ConstructEngine::init_with_options(
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

/// GET `<authorize url>&auto=1` without following redirects: the fake's
/// approval redirect (`construct://oauth?code=..&state=..`).
fn approve(url: &str) -> String {
    let resp = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{url}&auto=1"))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 302);
    resp.headers()["location"].to_str().unwrap().to_owned()
}

fn custom_engine(env: &Env, store: &Arc<MemoryStore>) -> (ConstructEngine, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    let engine = engine(
        &env.pod,
        store,
        Some(Arc::new(CapturingOpener(Mutex::new(tx)))),
        RedirectMode::Custom(CUSTOM_REDIRECT.into()),
    );
    (engine, rx)
}

#[test]
fn custom_scheme_sign_in_survives_a_process_restart() {
    // Android: the process dies while the browser is in front; the
    // construct://oauth redirect cold-starts a new engine on the same store.
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let location = {
        let (a, rx) = custom_engine(&env, &store);
        a.dispatch(&env.login_intent());
        let url = rx.recv_timeout(TIMEOUT).unwrap();
        let pending = PendingAuth::parse(&store.get(&pending_auth_key(&env.pod)).unwrap()).unwrap();
        assert_eq!(pending.redirect_uri, CUSTOM_REDIRECT);
        assert_eq!(pending.instance_url, env.fake);
        assert!(url.contains(&format!("state={}", pending.state)));
        approve(&url)
        // `a` is dropped here, before the redirect arrives.
    };
    thread::sleep(Duration::from_millis(300));
    assert!(
        store.get(&pending_auth_key(&env.pod)).is_some(),
        "kept across the drop"
    );

    let (b, _rx) = custom_engine(&env, &store);
    assert_eq!(auth(&b.snapshot())["state"], "signed_out");
    assert!(b.handle_auth_redirect(&location));
    wait_signed_in(&b);
    assert_eq!(auth(&b.snapshot())["username"], "constructdemo");
    assert!(env.session_exists(&stored_token(&env, &store)));
    assert!(
        store.get(&pending_auth_key(&env.pod)).is_none(),
        "deleted on success"
    );
    // A replay (the code is spent, the record gone) changes nothing.
    assert!(b.handle_auth_redirect(&location));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(auth(&b.snapshot())["state"], "signed_in");
}

#[test]
fn unmatched_or_expired_persisted_sign_in_reports_interrupted() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let stale = PendingAuth {
        state: "old".into(),
        code_verifier: "v".repeat(64),
        redirect_uri: CUSTOM_REDIRECT.into(),
        instance_url: env.fake.clone(),
        token_path: "/v1/auth/pixelfed/token".into(),
        created_at: 1,
    };
    store.set(&pending_auth_key(&env.pod), &stale.to_json());
    let (engine, _rx) = custom_engine(&env, &store);
    // Matching state but expired: deleted, reported.
    assert!(engine.handle_auth_redirect("construct://oauth?code=c&state=old"));
    wait_until("interrupted error", || {
        auth(&engine.snapshot())["error"] == SIGN_IN_INTERRUPTED
    });
    assert!(store.get(&pending_auth_key(&env.pod)).is_none());

    // A fresh record with another state is not used (and kept), but logout drops it.
    let fresh = PendingAuth {
        state: "other".into(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        ..stale
    };
    store.set(&pending_auth_key(&env.pod), &fresh.to_json());
    assert!(engine.handle_auth_redirect("construct://oauth?code=c&state=nope"));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(auth(&engine.snapshot())["error"], SIGN_IN_INTERRUPTED);
    assert!(
        store.get(&pending_auth_key(&env.pod)).is_some(),
        "another flow's record is kept"
    );
    engine.logout();
    wait_until("logout deleted the pending sign-in", || {
        store.get(&pending_auth_key(&env.pod)).is_none()
    });
}

#[test]
fn cancelled_custom_sign_in_deletes_the_persisted_flow() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let (engine, rx) = custom_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    rx.recv_timeout(TIMEOUT).unwrap();
    assert!(store.get(&pending_auth_key(&env.pod)).is_some());
    engine.logout();
    wait_until("pending sign-in deleted", || {
        store.get(&pending_auth_key(&env.pod)).is_none()
    });
}

fn temp_storage(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "client-core-auth-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// (pixelfed entities, outbox rows, has the workspace row) in cache.db.
fn cached(dir: &std::path::Path) -> (i64, i64, bool) {
    let db = Store::open(&dir.join(client_core::CACHE_DB_FILE)).unwrap();
    let count = |sql: &str| {
        db.conn()
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    (
        count("SELECT COUNT(*) FROM entities WHERE domain = 'pixelfed'"),
        count("SELECT COUNT(*) FROM action_outbox"),
        db.load_workspace(WORKSPACE).unwrap().is_some_and(|w| {
            w.entity_uris
                .iter()
                .chain(w.queries.values().flatten())
                .any(|u| u.starts_with("pixelfed://"))
        }),
    )
}

#[test]
fn sign_out_purges_the_private_feed_from_the_cache() {
    let env = Env::start();
    let dir = temp_storage("purge");
    let store = Arc::new(MemoryStore::new());
    let engine = engine_in(
        dir.to_str().unwrap(),
        &env.pod,
        Arc::clone(&store) as Arc<dyn SecureStore>,
        Some(Arc::new(AutoApproveBrowser::default())),
        RedirectMode::Loopback,
    );
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let (ents, _, ws) = cached(&dir);
    assert!(ents > 1 && ws, "feed cached: {ents} {ws}");

    // 401 sign-out (session revoked elsewhere) with a like queued: the liked
    // post stays (its row resumes after the next sign-in), the rest goes.
    let (id, _) = unliked_post(&engine.snapshot());
    let token = stored_token(&env, &store);
    reqwest::blocking::Client::new()
        .post(format!("{}/v1/auth/logout", env.pod))
        .bearer_auth(&token)
        .send()
        .unwrap();
    engine.dispatch(&like_intent(&id));
    wait_until("signed out by the 401", || {
        auth(&engine.snapshot())["state"] == "signed_out"
    });
    wait_signed_out_bundle(&engine);
    assert_eq!(cached(&dir), (1, 1, false));
    let snap = engine.snapshot();
    assert_eq!(snap.pending_actions, 1);
    let ents = entities(&snap);
    let private: Vec<&String> = ents
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("pixelfed://"))
        .collect();
    assert_eq!(private, vec![&format!("pixelfed://posts/{id}")]);

    // Explicit logout: the like can never be sent as anyone; dropped too.
    engine.logout();
    assert_eq!(cached(&dir), (0, 0, false));
    let snap = engine.snapshot();
    assert_eq!(snap.pending_actions, 0);
    assert!(!snap.entities_json.contains("pixelfed://posts/"));
    wait_signed_out_bundle(&engine);
    assert_eq!(cached(&dir).0, 0);
    drop(engine);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `MemoryStore` whose session writes are slow (a Keychain prompt, a
/// slow Keystore) and that records the threads touching the session key.
struct SlowStore {
    inner: MemoryStore,
    set_started: Mutex<mpsc::Sender<()>>,
    threads: Mutex<Vec<String>>,
}

impl SlowStore {
    fn record(&self, key: &str) {
        if key.starts_with("pod_session:") {
            let name = thread::current().name().unwrap_or("?").to_owned();
            self.threads.lock().unwrap().push(name);
        }
    }
}

impl SecureStore for SlowStore {
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) {
        self.record(key);
        if key.starts_with("pod_session:") {
            let _ = self.set_started.lock().unwrap().send(());
            thread::sleep(Duration::from_millis(800));
        }
        self.inner.set(key, value);
    }
    fn delete(&self, key: &str) {
        self.record(key);
        self.inner.delete(key);
    }
}

#[test]
fn secure_store_io_stays_off_the_ui_thread_and_logout_wins_the_race() {
    let env = Env::start();
    let (tx, started) = mpsc::channel();
    let store = Arc::new(SlowStore {
        inner: MemoryStore::new(),
        set_started: Mutex::new(tx),
        threads: Mutex::default(),
    });
    let engine = engine_in(
        "",
        &env.pod,
        Arc::clone(&store) as Arc<dyn SecureStore>,
        Some(Arc::new(AutoApproveBrowser::default())),
        RedirectMode::Loopback,
    );
    engine.dispatch(&env.login_intent());
    // The session is being written (slowly): the UI thread is not blocked
    // by it, neither for logout nor for a new sign-in.
    started.recv_timeout(TIMEOUT).unwrap();
    let t = Instant::now();
    engine.logout();
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "logout blocked {:?}",
        t.elapsed()
    );
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    // Logout came after the sign-in: the store must end up empty.
    thread::sleep(Duration::from_millis(1200));
    assert!(store.inner.get(&session_key(&env.pod)).is_none());
    assert_eq!(auth(&engine.snapshot())["state"], "signed_out");
    let threads = store.threads.lock().unwrap().clone();
    let me = thread::current().name().unwrap_or("?").to_owned();
    assert!(!threads.is_empty());
    assert!(
        !threads.contains(&me),
        "store touched on the caller's thread: {threads:?}"
    );
}

/// A stub pod for the sign-in UX tests: canned `/v1/auth/pixelfed/app` and
/// `/token` answers, and a workspace fetch that, with a bearer, is held
/// until the test opens the gate.
#[derive(Default)]
struct StubPod {
    app_calls: std::sync::atomic::AtomicUsize,
    /// (status, body) of `/app` and `/token`; `None` = success.
    app_reply: Mutex<Option<(u16, Value)>>,
    token_reply: Mutex<Option<(u16, Value)>>,
    gate_open: std::sync::atomic::AtomicBool,
}

fn stub_reply(r: &Mutex<Option<(u16, Value)>>, ok: Value) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (code, body) = r.lock().unwrap().clone().unwrap_or((200, ok));
    (
        axum::http::StatusCode::from_u16(code).unwrap(),
        axum::Json(body),
    )
        .into_response()
}

fn stub_bundle(title: &str) -> Value {
    json!({
        "version": "1.0.0",
        "workspace": { "type": "core://Workspace", "id": "home", "title": title, "slots": [] },
        "entities": {},
    })
}

fn start_stub_pod(rt: &tokio::runtime::Runtime) -> (String, Arc<StubPod>) {
    use axum::routing::{get, post};
    let pod = Arc::new(StubPod::default());
    let (p1, p2, p3) = (Arc::clone(&pod), Arc::clone(&pod), Arc::clone(&pod));
    let app = axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/v1/auth/pixelfed/app",
            post(move || {
                let pod = Arc::clone(&p1);
                async move {
                    pod.app_calls.fetch_add(1, Ordering::SeqCst);
                    stub_reply(
                        &pod.app_reply,
                        json!({ "client_id": "1", "authorize_endpoint": "http://stub/oauth/authorize",
                                "scopes": "read write" }),
                    )
                }
            }),
        )
        .route(
            "/v1/auth/pixelfed/token",
            post(move || {
                let pod = Arc::clone(&p2);
                async move {
                    stub_reply(
                        &pod.token_reply,
                        json!({ "session_token": "stub-session", "account": { "username": "stubuser" } }),
                    )
                }
            }),
        )
        .route(
            "/v1/workspaces/home",
            get(move |headers: axum::http::HeaderMap| {
                let pod = Arc::clone(&p3);
                async move {
                    if !headers.contains_key("authorization") {
                        return axum::Json(stub_bundle("Signed out"));
                    }
                    while !pod.gate_open.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    axum::Json(stub_bundle("Feed"))
                }
            }),
        );
    let (l, addr) = bind();
    spawn_serve(rt, l, app);
    (format!("http://{addr}"), pod)
}

fn stub_engine(base: &str) -> (ConstructEngine, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    let engine = ConstructEngine::init_with_options(
        "",
        base,
        "home",
        EngineOptions {
            config: fast_config(),
            secure_store: Some(Arc::new(MemoryStore::new())),
            url_opener: Some(Arc::new(CapturingOpener(Mutex::new(tx)))),
            redirect: RedirectMode::Custom(CUSTOM_REDIRECT.into()),
            loopback_port: 0,
        },
    );
    (engine, rx)
}

fn state_param(url: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned()
}

/// Start a sign-in and deliver the approval redirect; returns once the
/// redirect was accepted.
fn stub_sign_in(engine: &ConstructEngine, rx: &mpsc::Receiver<String>) {
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": "http://stub", "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }).to_string(),
    );
    let url = rx.recv_timeout(TIMEOUT).unwrap();
    assert!(engine.handle_auth_redirect(&format!(
        "construct://oauth?code=c&state={}",
        state_param(&url)
    )));
}

fn workspace_title(snap: &StateSnapshot) -> String {
    let ws: Value = serde_json::from_str(&snap.workspace_json).unwrap();
    ws["title"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn auth_stays_pending_until_the_first_feed_and_ignores_a_second_tap() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (base, pod) = start_stub_pod(&rt);
    let (engine, rx) = stub_engine(&base);
    wait_until("signed-out screen", || {
        workspace_title(&engine.snapshot()) == "Signed out"
    });

    stub_sign_in(&engine, &rx);
    // Session in place, feed fetch held: still pending, with the loading message.
    wait_until("loading the feed", || {
        auth(&engine.snapshot()) == json!({ "state": "pending", "message": LOADING_FEED_MESSAGE })
    });
    // A second tap on "Sign in" (the signed-out screen is still shown) is ignored.
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": "http://stub", "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }).to_string(),
    );
    thread::sleep(Duration::from_millis(300));
    assert!(rx.try_recv().is_err(), "a second browser flow was started");
    assert_eq!(pod.app_calls.load(Ordering::SeqCst), 1);
    let snap = engine.snapshot();
    assert_eq!(auth(&snap)["state"], "pending");
    assert_eq!(workspace_title(&snap), "Signed out");

    pod.gate_open.store(true, Ordering::SeqCst);
    wait_until("signed in", || {
        auth(&engine.snapshot())["state"] == "signed_in"
    });
    let snap = engine.snapshot();
    assert_eq!(auth(&snap)["username"], "stubuser");
    assert_eq!(
        workspace_title(&snap),
        "Feed",
        "signed_in only after the feed landed"
    );

    // The flow is over: sign-in works again (after a logout).
    engine.logout();
    stub_sign_in(&engine, &rx);
    wait_until("signed in again", || {
        auth(&engine.snapshot())["state"] == "signed_in"
    });
    assert_eq!(pod.app_calls.load(Ordering::SeqCst), 2);
    drop(engine);
    rt.shutdown_background();
}

#[test]
fn sign_in_errors_are_short_human_messages() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (base, pod) = start_stub_pod(&rt);
    let (engine, rx) = stub_engine(&base);
    let error = |engine: &ConstructEngine| {
        wait_until("auth error", || {
            auth(&engine.snapshot())["state"] == "error"
        });
        auth(&engine.snapshot())["error"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    *pod.token_reply.lock().unwrap() = Some((
        403,
        json!({ "error": "account_not_owner", "detail": "this pod is pinned to another Pixelfed account" }),
    ));
    stub_sign_in(&engine, &rx);
    assert_eq!(error(&engine), SIGN_IN_NOT_OWNER);

    *pod.app_reply.lock().unwrap() = Some((
        502,
        json!({ "error": "upstream_unreachable", "detail": "connection refused" }),
    ));
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": "http://stub", "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }).to_string(),
    );
    wait_until("upstream error", || {
        auth(&engine.snapshot())["error"] == SIGN_IN_UPSTREAM_DOWN
    });
    drop(engine);
    rt.shutdown_background();

    // No pod at all.
    let dead = {
        let (l, addr) = bind();
        drop(l);
        format!("http://{addr}")
    };
    let (engine, _rx) = stub_engine(&dead);
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": "http://stub", "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }).to_string(),
    );
    assert_eq!(error(&engine), SIGN_IN_POD_UNREACHABLE);
}

// ---------- data loads and creates (docs/DECISIONS.md D34) ----------

const CAROUSEL_ID: &str = "781254316723458049";

fn comments_path(id: &str) -> String {
    format!("/v1/pixelfed/screens/comments?post_id={id}")
}

fn load_intent(path: &str) -> String {
    json!({ "intent": "action.data.load", "path": path }).to_string()
}

fn loads(snap: &StateSnapshot) -> Value {
    serde_json::from_str(&snap.loads_json).unwrap()
}

/// The create intent the comments screen dispatches (as the client binds it).
fn create_comment_intent(post: &str, text: &str, nonce: &str) -> String {
    json!({
        "intent": "action.mutation.create",
        "query_key": format!("comments:{post}"),
        "entity_uri": format!("pixelfed://comments/pending-{nonce}"),
        "entity": { "author_name": "You", "text": text, "meta": "Sending…", "depth": 0 },
        "endpoint": "/v1/pixelfed/posts/comment",
        "payload": { "id": post, "text": text, "client_id": nonce },
        "refresh_workspace": true,
    })
    .to_string()
}

fn comment_texts(snap: &StateSnapshot, post: &str) -> Vec<String> {
    let ents = entities(snap);
    queries(snap)[format!("comments:{post}")]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|u| {
                    ents[u.as_str().unwrap()]["text"]
                        .as_str()
                        .unwrap_or("?")
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_engine_fetches_the_newest_layout_its_schema_draws() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let snap = engine.snapshot();
    let layout: Value = serde_json::from_str(&snap.layout_json).unwrap();
    // 0010 needs feature `title_scrolls`, which this engine sends.
    assert_eq!(layout["version"], 10);
    let ws: Value = serde_json::from_str(&snap.workspace_json).unwrap();
    let routes: Vec<&str> = ws["screens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["route"].as_str().unwrap())
        .collect();
    assert_eq!(
        routes,
        [
            "pixelfed://posts/{id}",
            "pixelfed://posts/{id}/comments",
            "pixelfed://posts/{id}/comments/{comment}",
            "pixelfed://compose"
        ]
    );
    // Generic nodes only.
    assert!(
        !snap.workspace_json.contains("\"type\":\"pixelfed://"),
        "{}",
        snap.workspace_json
    );
}

#[test]
fn a_sign_in_button_without_pod_paths_reports_an_error() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine
        .dispatch(&json!({ "intent": "action.auth.login", "instance_url": env.fake }).to_string());
    let a = auth(&engine.snapshot());
    assert_eq!(a["state"], "error");
    assert_eq!(a["error"], client_core::SIGN_IN_BUTTON_OUTDATED);
    // Paths to another host are refused too.
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": env.fake,
                 "app_path": "//evil.example/app", "token_path": "/v1/x" })
        .to_string(),
    );
    assert_eq!(
        auth(&engine.snapshot())["error"],
        client_core::SIGN_IN_BUTTON_OUTDATED
    );
}

#[test]
fn a_data_load_merges_its_entities_and_queries_and_is_cached() {
    let env = Env::start();
    let dir = std::env::temp_dir().join(format!(
        "construct-data-load-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = dir.to_str().unwrap().to_owned();
    let store = Arc::new(MemoryStore::new());
    let path = comments_path(CAROUSEL_ID);
    {
        let engine = engine_in(
            &storage,
            &env.pod,
            Arc::clone(&store) as Arc<dyn SecureStore>,
            Some(Arc::new(AutoApproveBrowser::default())),
            RedirectMode::Loopback,
        );
        engine.dispatch(&env.login_intent());
        wait_signed_in(&engine);
        engine.dispatch(&load_intent(&path));
        wait_until("comments loaded", || {
            loads(&engine.snapshot())[&path]["state"] == "ready"
        });
        let snap = engine.snapshot();
        let texts = comment_texts(&snap, CAROUSEL_ID);
        assert_eq!(texts.len(), 7);
        assert_eq!(texts[0], "That second frame is unreal 😍");
        // The feed's own query is still there.
        assert_eq!(home_query_len(&snap), 5);
        // A workspace refetch keeps the loaded data.
        engine.refresh();
        assert_eq!(comment_texts(&engine.snapshot(), CAROUSEL_ID).len(), 7);
    }
    // Cached: a new engine on the same storage shows it before any network.
    let engine = engine_in(
        &storage,
        "http://127.0.0.1:9",
        Arc::clone(&store) as Arc<dyn SecureStore>,
        None,
        RedirectMode::Loopback,
    );
    assert_eq!(comment_texts(&engine.snapshot(), CAROUSEL_ID).len(), 7);
    // Loading offline reports the error and keeps the cached list.
    engine.dispatch(&load_intent(&path));
    wait_until("load error", || {
        loads(&engine.snapshot())[&path]["state"] == "error"
    });
    let snap = engine.snapshot();
    assert_eq!(loads(&snap)[&path]["error"], client_core::LOAD_OFFLINE);
    assert_eq!(comment_texts(&snap, CAROUSEL_ID).len(), 7);
}

#[test]
fn a_create_is_listed_at_once_then_replaced_by_the_pods_answer() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let path = comments_path(CAROUSEL_ID);
    engine.dispatch(&load_intent(&path));
    wait_until("comments loaded", || {
        loads(&engine.snapshot())[&path]["state"] == "ready"
    });

    // Hold the pod's answer so the check below sees the optimistic entry.
    env.hold_comments.store(true, Ordering::SeqCst);
    engine.dispatch(&create_comment_intent(
        CAROUSEL_ID,
        "Golden light!",
        "nonce00000001",
    ));
    // Listed right away (from the dispatch's own snapshot).
    let texts = comment_texts(&engine.snapshot(), CAROUSEL_ID);
    assert_eq!(texts.last().map(String::as_str), Some("Golden light!"));
    assert_eq!(texts.len(), 8);
    assert!(
        queries(&engine.snapshot())[format!("comments:{CAROUSEL_ID}")]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u.as_str().unwrap().contains("pending-nonce00000001"))
    );
    env.hold_comments.store(false, Ordering::SeqCst);
    wait_until("flushed and reloaded", || {
        let s = engine.snapshot();
        s.pending_actions == 0
            && queries(&s)[format!("comments:{CAROUSEL_ID}")]
                .as_array()
                .unwrap()
                .iter()
                .all(|u| !u.as_str().unwrap().contains("pending-"))
            && comment_texts(&s, CAROUSEL_ID).len() == 8
    });
    let snap = engine.snapshot();
    let ents = entities(&snap);
    let last = queries(&snap)[format!("comments:{CAROUSEL_ID}")]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(ents[&last]["text"], "Golden light!");
    assert_eq!(ents[&last]["is_own"], true);
    assert!(
        ents.get("pixelfed://comments/pending-nonce00000001")
            .is_none()
    );
    // refresh_workspace: the feed's count comes from the pod.
    wait_until("reply_count refreshed", || {
        entities(&engine.snapshot())[format!("pixelfed://posts/{CAROUSEL_ID}")]["reply_count"] == 5
    });
    assert_eq!(env.upstream_status(CAROUSEL_ID)["replies_count"], 5);
}

#[test]
fn a_rejected_create_disappears() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    // Hold the pod's answer so the rejection can't roll the comment back
    // before the optimistic check below.
    env.hold_comments.store(true, Ordering::SeqCst);
    engine.dispatch(&create_comment_intent(
        "999",
        "nobody home",
        "nonce00000002",
    ));
    assert_eq!(comment_texts(&engine.snapshot(), "999"), ["nobody home"]);
    env.hold_comments.store(false, Ordering::SeqCst);
    wait_until("rejected", || engine.snapshot().pending_actions == 0);
    let snap = engine.snapshot();
    assert!(comment_texts(&snap, "999").is_empty());
    assert!(
        entities(&snap)
            .get("pixelfed://comments/pending-nonce00000002")
            .is_none()
    );
    // An entity that is not an object, or a reused URI, is ignored.
    engine.dispatch(
        &json!({ "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://a",
                 "entity": 5, "endpoint": "/v1/x", "payload": {} })
        .to_string(),
    );
    assert_eq!(engine.snapshot().pending_actions, 0);
}

#[test]
fn a_sequence_creates_and_sets_local_view_state() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    // The marked entity comes from a screen's data load (like a recipe's
    // ingredients); the create goes to another post, whose comments are not
    // loaded, so nothing reloads the marked entity.
    let path = comments_path(CAROUSEL_ID);
    engine.dispatch(&load_intent(&path));
    wait_until("comments loaded", || {
        loads(&engine.snapshot())[&path]["state"] == "ready"
    });
    let snap = engine.snapshot();
    let target = queries(&snap)[format!("comments:{CAROUSEL_ID}")][0]
        .as_str()
        .unwrap()
        .to_owned();
    let likes = entities(&snap)[&target]["like_count"].as_i64().unwrap();
    let post = queries(&snap)["timeline:home"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            entities(&snap)[u.as_str().unwrap()]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .find(|id| id != CAROUSEL_ID)
        .expect("another post");
    let mut create: Value =
        serde_json::from_str(&create_comment_intent(&post, "hi", "nonce00000010")).unwrap();
    create["refresh_workspace"] = json!(false);
    let set = |uri: &str, expr: &str| {
        json!({ "intent": "action.local.set", "entity_uri": uri, "field": "added",
                "value_expr": expr,
                "also": [{ "field": "taps", "value_expr": "entity.like_count + 1" }] })
    };

    // One tap: the create and the local mark, in order; the unloaded entity
    // and the failing expression change nothing.
    engine.dispatch(
        &json!({ "intent": "action.sequence", "steps": [
            create,
            set(&target, "true"),
            set("pixelfed://comments/not-loaded", "true"),
            { "intent": "action.local.set", "entity_uri": target, "field": "taps",
              "value_expr": "entity.missing +" },
        ]})
        .to_string(),
    );
    let snap = engine.snapshot();
    assert_eq!(
        comment_texts(&snap, &post).last().map(String::as_str),
        Some("hi")
    );
    assert_eq!(entities(&snap)[&target]["added"], true);
    assert_eq!(entities(&snap)[&target]["taps"], likes + 1);
    assert!(
        entities(&snap)
            .get("pixelfed://comments/not-loaded")
            .is_none()
    );
    // Only the create is sent; the local set stays.
    wait_until("sent", || engine.snapshot().pending_actions == 0);
    assert_eq!(entities(&engine.snapshot())[&target]["added"], true);

    // Reloading the entity from the pod replaces the local view state.
    engine.dispatch(&load_intent(&path));
    wait_until("reloaded without the mark", || {
        entities(&engine.snapshot())[&target].get("added").is_none()
    });
}

#[test]
fn a_data_load_401_signs_out() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    engine.dispatch(&env.login_intent());
    wait_signed_in(&engine);
    let token = stored_token(&env, &store);
    let revoked = reqwest::blocking::Client::new()
        .post(format!("{}/v1/auth/logout", env.pod))
        .bearer_auth(&token)
        .send()
        .unwrap();
    assert_eq!(revoked.status().as_u16(), 200);
    let path = comments_path(CAROUSEL_ID);
    engine.dispatch(&load_intent(&path));
    wait_until("signed out", || {
        auth(&engine.snapshot())["state"] == "signed_out"
    });
    wait_signed_out_bundle(&engine);
    // Sign-out forgets load states. The events stream may notice the revoked
    // session first: the load then runs signed out and says so, once its
    // request (still in flight here) comes back.
    wait_until("load settled", || {
        loads(&engine.snapshot())[&path]["state"] != "loading"
    });
    let loads = loads(&engine.snapshot());
    assert!(
        loads == json!({})
            || loads
                == json!({ path.clone(): { "state": "error", "error": client_core::LOAD_SIGNED_OUT } }),
        "{loads}"
    );
}

#[test]
fn bad_data_paths_are_refused_without_a_request() {
    let env = Env::start();
    let store = Arc::new(MemoryStore::new());
    let engine = loopback_engine(&env, &store);
    for bad in ["https://evil.example/x", "//evil.example/x", "/v1/x y"] {
        engine.dispatch(&load_intent(bad));
        assert_eq!(loads(&engine.snapshot())[bad]["state"], "error", "{bad}");
    }
}
