//! Pixelfed notifications (D52): the adapter's polls against the fake upstream
//! (`crates/fake-pixelfed`) on a random local port: the quiet first poll and
//! its cursor, new notifications, paging, a revoked token, upstream failures,
//! the pod setting that limits instances, and the core's scheduler path.

use std::sync::Arc;
use std::time::Duration;

use app_pixelfed::db::{PixelfedAccount, PixelfedStore};
use app_pixelfed::{OTHER_INSTANCES_SETTING, PixelfedAdapter, PixelfedConfig, SIGN_IN};
use fake_pixelfed::FaultRule;
use pod_core::session::session_hash;
use pod_core::{AppAdapter, AppState, Pod, PollCtx, PollOutcome, PollStatus};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

const SEED: &str = "seed-token";
const ME: &str = "703641122334455667";
const CAROUSEL: &str = "781254316723458049";

struct Fake {
    url: String,
    state: Arc<fake_pixelfed::AppState>,
    handle: JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn_fake() -> Fake {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(
        fake_pixelfed::AppState::new(fake_pixelfed::Config {
            fixtures_dir: fake_pixelfed::default_fixtures_dir(),
            public_url: url.clone(),
            auto_approve: true,
            seed_token: Some(SEED.to_owned()),
        })
        .unwrap(),
    );
    let router = fake_pixelfed::app(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fake { url, state, handle }
}

fn account(token: &str) -> PixelfedAccount {
    PixelfedAccount {
        access_token: token.into(),
        account_id: ME.into(),
        username: "constructdemo".into(),
        ..Default::default()
    }
}

/// A pod whose Pixelfed adapter uses `config`, with `token` stored for its
/// default instance.
fn pod_with(config: PixelfedConfig, token: &str) -> (Pod, Arc<PixelfedAdapter>) {
    let instance = config.instance_url.clone();
    let adapter = Arc::new(PixelfedAdapter::new(config));
    let state = AppState::in_memory(vec![adapter.clone()]);
    state
        .store()
        .save_pixelfed_account(&instance, &account(token))
        .unwrap();
    (Arc::new(state), adapter)
}

fn pod_for(fake: &Fake, token: &str) -> (Pod, Arc<PixelfedAdapter>) {
    pod_with(PixelfedConfig::new(&fake.url, None), token)
}

async fn poll(pod: &Pod, adapter: &PixelfedAdapter) -> PollOutcome {
    adapter.poll_notifications(pod, &PollCtx::default()).await
}

/// The paths (with queries) of the fake's notification requests.
fn notification_requests(fake: &Fake) -> Vec<String> {
    fake.state
        .requests()
        .into_iter()
        .filter(|r| r.path.starts_with("/api/v1/notifications"))
        .map(|r| r.path)
        .collect()
}

#[tokio::test]
async fn first_poll_is_quiet_then_new_ones_alert() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);

    // First poll: the fixture backlog (two replies to our comments, a like
    // of one), stored quietly; the cursor is the newest id.
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.next_in, None);
    let channels: Vec<&str> = out.new.iter().map(|n| n.channel.as_str()).collect();
    assert_eq!(channels, ["comments", "likes", "comments"], "oldest first");
    assert!(out.new.iter().all(|n| n.quiet));
    assert_eq!(out.new[2].title, "Carol Ortiz commented on your post");
    assert_eq!(
        out.new[2].body,
        "@constructdemo an intervalometer, 30 s exposures."
    );
    assert_eq!(
        pod.store().notify_cursor(&fake.url).unwrap(),
        Some(Some("3".to_owned()))
    );
    assert_eq!(
        notification_requests(&fake),
        ["/api/v1/notifications?limit=20"]
    );

    // Nothing new: nothing reported.
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert!(out.new.is_empty());

    // A like of one of our posts.
    let injected = fake
        .state
        .add_notification("favourite", "alice", Some(CAROUSEL), None)
        .unwrap();
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.new.len(), 1);
    let n = &out.new[0];
    assert_eq!(n.app_id, "pixelfed_home");
    assert_eq!(n.channel, "likes");
    assert!(!n.quiet);
    assert_eq!(n.title, "Alice Lumen liked your post");
    assert!(n.body.starts_with("Sunrise over Boulder"), "{}", n.body);
    assert_eq!(
        n.target_uri.as_deref(),
        Some(format!("pixelfed://posts/{CAROUSEL}/comments").as_str())
    );
    assert_eq!(
        n.group_key.as_deref(),
        Some(format!("{CAROUSEL}:likes").as_str())
    );
    assert_eq!(
        n.dedupe_key,
        format!("{}:{}", fake.url, injected["id"].as_str().unwrap())
    );
    // The avatar through the media proxy, as a pod path whose key is stored.
    let image = n.image_url.clone().unwrap();
    let key = image.strip_prefix("/v1/pixelfed/media/").unwrap();
    assert_eq!(
        pod.store().media_url(key).unwrap(),
        Some(format!("{}/storage/avatar-alice.png", fake.url))
    );
    assert_eq!(
        notification_requests(&fake).last().unwrap(),
        "/api/v1/notifications?limit=40&min_id=3"
    );

    // The core stores it unread.
    let stored = pod.notify(out.new).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].read);
    assert_eq!(
        pod.store().notify_cursor(&fake.url).unwrap(),
        Some(Some("4".to_owned()))
    );
}

#[tokio::test]
async fn a_public_prefix_goes_in_front_of_image_paths() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);
    let ctx = PollCtx {
        public_prefix: "/p/abc".into(),
    };
    let out = pf.poll_notifications(&pod, &ctx).await;
    let image = out.new[0].image_url.as_deref().unwrap();
    assert!(image.starts_with("/p/abc/v1/pixelfed/media/"), "{image}");
}

#[tokio::test]
async fn a_backlog_is_read_three_pages_per_poll() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);
    poll(&pod, &pf).await;
    for _ in 0..130 {
        fake.state
            .add_notification("follow", "bob", None, None)
            .unwrap();
    }
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.new.len(), 120);
    assert_eq!(out.next_in, Some(Duration::from_secs(30)), "catch up soon");
    assert!(out.new.iter().all(|n| n.channel == "follows" && !n.quiet));
    assert_eq!(out.new[0].title, "Bob 🚲 followed you");
    assert_eq!(out.new[0].target_uri, None);
    let out = poll(&pod, &pf).await;
    assert_eq!(out.new.len(), 10);
    assert_eq!(out.next_in, None);
    assert_eq!(
        pod.store().notify_cursor(&fake.url).unwrap(),
        Some(Some("133".to_owned()))
    );
}

#[tokio::test]
async fn a_revoked_token_is_sign_in_gone() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, "revoked-token");
    pod.store()
        .save_notify_cursor(&fake.url, Some("1"))
        .unwrap();
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::SignInGone(SIGN_IN.to_owned()));
    assert!(out.new.is_empty());
    // As for a stale home feed: the dead token is forgotten (and with it the
    // cursor, so the next sign-in starts with a quiet first poll).
    assert_eq!(pod.store().pixelfed_account(&fake.url).unwrap(), None);
    assert_eq!(pod.store().notify_cursor(&fake.url).unwrap(), None);
    // No account at all: still gone.
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::SignInGone(SIGN_IN.to_owned()));
}

#[tokio::test]
async fn upstream_failures_are_transient() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);
    fake.state.add_faults([FaultRule::status(503)
        .path("/api/v1/notifications")
        .times(1)]);
    let out = poll(&pod, &pf).await;
    assert!(
        matches!(out.status, PollStatus::Transient(_)),
        "{:?}",
        out.status
    );
    assert!(out.new.is_empty());
    assert_eq!(pod.store().notify_cursor(&fake.url).unwrap(), None);
    // The next poll works (and is still the quiet first one).
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert!(out.new.iter().all(|n| n.quiet));

    // Unreachable.
    let dead = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let (pod, pf) = pod_with(PixelfedConfig::new(&dead, None), SEED);
    let out = poll(&pod, &pf).await;
    assert!(
        matches!(out.status, PollStatus::Transient(_)),
        "{:?}",
        out.status
    );
}

/// A real token pair from the fake's OAuth flow (PKCE, auto-approved), with
/// its app stored on the pod, so a refresh can work.
async fn sign_in_for_real(pod: &Pod, fake: &Fake) -> PixelfedAccount {
    const REDIRECT: &str = "http://127.0.0.1:9/callback";
    let instance = app_pixelfed::Instance {
        url: fake.url.clone(),
        trusted: true,
    };
    let api = app_pixelfed::upstream::Clients::new(Default::default()).api(&instance);
    let app = api.register_app(REDIRECT).await.unwrap();
    let verifier = "v".repeat(64);
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = http
        .get(format!("{}/oauth/authorize", fake.url))
        .query(&[
            ("response_type", "code"),
            ("client_id", app.client_id.as_str()),
            ("redirect_uri", REDIRECT),
            ("scope", "read write"),
            ("state", "s"),
            ("code_challenge", &fake_pixelfed::s256_challenge(&verifier)),
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
    let token = api
        .exchange_code(
            &app.client_id,
            &app.client_secret,
            &code,
            &verifier,
            REDIRECT,
        )
        .await
        .unwrap();
    pod.store()
        .save_pixelfed_app(
            &fake.url,
            REDIRECT,
            &app_pixelfed::db::PixelfedApp {
                client_id: app.client_id,
                client_secret: app.client_secret,
            },
        )
        .unwrap();
    let account = PixelfedAccount {
        refresh_token: token.refresh_token,
        redirect_uri: Some(REDIRECT.to_owned()),
        ..account(&token.access_token)
    };
    pod.store()
        .save_pixelfed_account(&fake.url, &account)
        .unwrap();
    account
}

#[tokio::test]
async fn an_expired_token_is_refreshed() {
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);
    let signed_in = sign_in_for_real(&pod, &fake).await;
    poll(&pod, &pf).await;
    fake.state.expire_access_tokens();
    fake.state
        .add_notification("reblog", "carol", Some(CAROUSEL), None)
        .unwrap();
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.new.len(), 1);
    assert_eq!(out.new[0].channel, "boosts");
    assert_eq!(out.new[0].title, "Carol Ortiz boosted your post");
    assert_eq!(fake.state.refresh_token_grants(), 1);
    let stored = pod.store().pixelfed_account(&fake.url).unwrap().unwrap();
    assert_ne!(stored.access_token, signed_in.access_token);

    // The seed token has no refresh token: once it expires, signed out.
    let fake = spawn_fake().await;
    let (pod, pf) = pod_for(&fake, SEED);
    poll(&pod, &pf).await;
    fake.state.expire_access_tokens();
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::SignInGone(SIGN_IN.to_owned()));
}

#[tokio::test]
async fn temporary_pods_poll_only_the_default_instance() {
    let fake = spawn_fake().await;
    let other = spawn_fake().await;
    let mut config = PixelfedConfig::new(&fake.url, None);
    config.allowed_instances = vec![other.url.clone()];
    let (pod, pf) = pod_with(config, SEED);
    pod.store()
        .save_pixelfed_account(&other.url, &account(SEED))
        .unwrap();

    // Both instances are polled; each has its own cursor and dedupe keys.
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.new.len(), 6);
    assert!(out.new.iter().any(|n| n.dedupe_key.starts_with(&other.url)));
    assert_eq!(notification_requests(&other).len(), 1);

    // A pod limited to the default instance (D46) skips the other one.
    let (pod, pf) = {
        let mut config = PixelfedConfig::new(&fake.url, None);
        config.allowed_instances = vec![other.url.clone()];
        pod_with(config, SEED)
    };
    pod.store()
        .save_pixelfed_account(&other.url, &account(SEED))
        .unwrap();
    pod.store()
        .set_pod_setting(OTHER_INSTANCES_SETTING, "0")
        .unwrap();
    let out = poll(&pod, &pf).await;
    assert_eq!(out.status, PollStatus::Ok);
    assert_eq!(out.new.len(), 3);
    assert!(out.new.iter().all(|n| n.dedupe_key.starts_with(&fake.url)));
    assert_eq!(notification_requests(&other).len(), 1, "not polled again");
}

async fn pod_call(pod_url: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let http = reqwest::Client::new();
    let mut req = http
        .request(method.parse().unwrap(), format!("{pod_url}{path}"))
        .bearer_auth("tok")
        .header("x-construct-device", "device-0001");
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.unwrap();
    assert!(
        resp.status().is_success(),
        "{method} {path}: {}",
        resp.status()
    );
    resp.json().await.unwrap_or(Value::Null)
}

#[tokio::test]
async fn the_scheduler_path_stores_and_serves_them() {
    let fake = spawn_fake().await;
    let (pod, _) = pod_for(&fake, SEED);
    pod.store().create_session(&session_hash("tok")).unwrap();
    pod.store()
        .attach_sign_in(&session_hash("tok"), SIGN_IN)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pod_url = format!("http://{}", listener.local_addr().unwrap());
    let router = pod_core::app(pod.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    // No device: nothing polls.
    assert_eq!(pod.poll_due().await, None);
    assert!(notification_requests(&fake).is_empty());

    // The channels, then a device.
    let channels = pod_call(&pod_url, "GET", "/v1/notifications/channels", None).await;
    assert!(
        channels
            .to_string()
            .contains("When someone likes your post"),
        "{channels}"
    );
    pod_call(
        &pod_url,
        "PUT",
        "/v1/devices/device-0001",
        Some(json!({"platform": "test"})),
    )
    .await;
    assert!(pod.poll_due().await.is_some());
    let list = pod_call(&pod_url, "GET", "/v1/notifications", None).await;
    assert_eq!(list["notifications"].as_array().unwrap().len(), 3);
    assert_eq!(list["unread_total"], 0, "the first poll is history: {list}");

    // A comment on one of our posts; the next poll is due at once.
    fake.state
        .add_notification(
            "mention",
            "bob",
            Some("781300000000000504"),
            Some("@constructdemo glad too"),
        )
        .unwrap();
    pod.store()
        .with_conn(|c| c.execute("UPDATE notification_polls SET next_at = 0", []))
        .unwrap();
    pod.poll_due().await;
    let list = pod_call(&pod_url, "GET", "/v1/notifications", None).await;
    assert_eq!(list["unread_total"], 1, "{list}");
    assert_eq!(list["unread_by_app"]["pixelfed_home"], 1);
    let newest = &list["notifications"][0];
    assert_eq!(newest["channel"], "comments");
    assert_eq!(newest["title"], "Bob 🚲 commented on your post");
    assert_eq!(newest["body"], "@constructdemo glad too");
    assert_eq!(
        newest["target_uri"],
        "pixelfed://posts/781300000000000504/comments"
    );
    assert_eq!(newest["read"], false);
    server.abort();
}
