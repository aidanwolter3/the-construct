//! The engine in front of a pod host (D46): no token -> "Open your pod";
//! a temporary pod opens a session; Pixelfed sign-in adds to that session;
//! signing out keeps the pod token; leaving the pod forgets it. Settings'
//! list of sign-ins follows along.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client_core::auth::{POD_CODE_REJECTED, session_key};
use client_core::{
    ConstructEngine, EngineConfig, EngineOptions, MemoryStore, RedirectMode, SecureStore,
    StateSnapshot, UrlOpener,
};
use serde_json::{Value, json};
use server::PixelfedConfig;
use server::host::{Host, HostConfig, TemporaryPods};

const TIMEOUT: Duration = Duration::from_secs(15);

fn serve(rt: &tokio::runtime::Runtime, app: axum::Router) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    l.set_nonblocking(true).unwrap();
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(l).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    url
}

/// Approves on the fake's authorize page (`auto=1`), like a user would.
struct Browser;

impl UrlOpener for Browser {
    fn open(&self, url: &str) -> bool {
        let url = format!("{url}&auto=1");
        thread::spawn(move || {
            let _ = reqwest::blocking::Client::new().get(&url).send();
        });
        true
    }
}

fn wait_until(engine: &ConstructEngine, what: &str, cond: impl Fn(&StateSnapshot) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let s = engine.snapshot();
        if cond(&s) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: auth {} workspace {}",
            s.auth_json,
            s.workspace_json.chars().take(300).collect::<String>()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn auth(s: &StateSnapshot) -> Value {
    serde_json::from_str(&s.auth_json).unwrap()
}

fn open_session(path: &str, payload: Value) -> String {
    json!({ "intent": "action.auth.session", "path": path, "payload": payload }).to_string()
}

#[test]
fn a_device_opens_a_pod_signs_in_signs_out_and_leaves() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // The fake needs its own URL in its config, so bind first.
    let fl = TcpListener::bind("127.0.0.1:0").unwrap();
    let fake = format!("http://{}", fl.local_addr().unwrap());
    let fake_state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fake_pixelfed::default_fixtures_dir(),
        public_url: fake.clone(),
        auto_approve: false,
        seed_token: None,
    })
    .unwrap();
    fl.set_nonblocking(true).unwrap();
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(fl).unwrap();
        axum::serve(listener, fake_pixelfed::app(Arc::new(fake_state)))
            .await
            .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let factory = server::pod_factory(PixelfedConfig::new(&fake, None), false, 86_400, false);
    let host = Host::open(
        dir.path(),
        factory,
        HostConfig {
            temporary: Some(TemporaryPods {
                hours: 1,
                max_live: 5,
            }),
            new_pod_settings: server::new_pod_settings(false),
            ..HostConfig::default()
        },
    )
    .unwrap();
    let base = serve(&rt, server::host::app(Arc::new(host)));

    let store = Arc::new(MemoryStore::default());
    let engine = ConstructEngine::init_with_options(
        "",
        &base,
        "pixelfed_home",
        EngineOptions {
            config: EngineConfig {
                poll_interval: Duration::from_millis(100),
                backoff_base: Duration::from_millis(20),
                backoff_cap: Duration::from_millis(200),
            },
            secure_store: Some(Arc::clone(&store) as Arc<dyn SecureStore>),
            url_opener: Some(Arc::new(Browser)),
            redirect: RedirectMode::Loopback,
            loopback_port: 0,
        },
    );
    wait_until(&engine, "the open-your-pod screen", |s| {
        s.workspace_json.contains("Try a temporary pod")
    });
    let sign_ins =
        |e: &ConstructEngine| -> Value { serde_json::from_str(&e.sign_ins().unwrap()).unwrap() };
    assert_eq!(
        sign_ins(&engine),
        json!({ "sign_ins": [] }),
        "no session yet"
    );

    // A wrong code: a short message, no session.
    engine.dispatch(&open_session(
        "/v1/pods/open",
        json!({ "token": "cpod_wrong" }),
    ));
    wait_until(&engine, "the code error", |s| {
        auth(s)["error"] == POD_CODE_REJECTED
    });

    // A temporary pod: a session without an account, the app's sign-in screen.
    engine.dispatch(&open_session("/v1/pods/temporary", json!({})));
    wait_until(&engine, "the pod's Pixelfed screen", |s| {
        auth(s) == json!({ "state": "signed_out", "session": true })
            && s.workspace_json.contains("Sign in with Pixelfed")
    });
    let token = |store: &MemoryStore| -> Option<String> {
        store.get(&session_key(&base)).map(|raw| {
            serde_json::from_str::<Value>(&raw).unwrap()["token"]
                .as_str()
                .unwrap()
                .to_owned()
        })
    };
    let pod_token = token(&store).expect("stored");
    assert!(pod_token.starts_with("cpod_"));

    // Sign in to Pixelfed inside the pod: same token, now with the account.
    engine.dispatch(
        &json!({ "intent": "action.auth.login", "instance_url": fake,
                 "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" })
        .to_string(),
    );
    wait_until(&engine, "signed in with the feed", |s| {
        auth(s)["state"] == "signed_in" && s.queries_json.contains("timeline:home")
    });
    assert_eq!(token(&store).as_deref(), Some(pod_token.as_str()));
    let list = sign_ins(&engine);
    assert_eq!(list["sign_ins"][0]["title"], "Pixelfed", "{list}");
    assert_eq!(list["sign_ins"][0]["server"], fake.as_str());
    assert_eq!(list["sign_ins"][0]["username"], "constructdemo");

    // Sign out of Pixelfed: the pod token stays.
    engine.logout();
    wait_until(&engine, "signed out of Pixelfed, still in the pod", |s| {
        auth(s)["session"] == true
            && auth(s)["state"] == "signed_out"
            && s.workspace_json.contains("Sign in with Pixelfed")
    });
    assert_eq!(token(&store).as_deref(), Some(pod_token.as_str()));
    let listed = sign_ins(&engine);
    assert_eq!(listed["sign_ins"], json!([]));
    // Settings can start the sign-in again.
    assert_eq!(listed["providers"][0]["id"], "pixelfed");
    assert_eq!(listed["providers"][0]["signed_in"], false);
    assert_eq!(
        listed["providers"][0]["intent"]["intent"],
        "action.auth.login"
    );

    // Leave the pod on this device.
    engine.forget_session();
    wait_until(&engine, "the open-your-pod screen again", |s| {
        auth(s)["session"].is_null() && s.workspace_json.contains("Open pod")
    });
    assert_eq!(token(&store), None);
    drop(engine);
    rt.shutdown_background();
}
