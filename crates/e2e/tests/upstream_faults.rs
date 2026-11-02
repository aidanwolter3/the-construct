//! Outbox and sign-in behaviour under upstream faults, end to end: client-core engine ->
//! real pod (`server::app`) -> fake Pixelfed with injected faults (docs/DECISIONS.md D14,
//! D16, D17). Always the in-process fake.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use client_core::{CanvasEngine, MemoryStore};
use e2e::{
    Harness, LIKE_ENDPOINT, TIMEOUT, auth, feed, fresh_post, like_intent, post, wait_flushed,
    wait_signed_in, wait_until, wait_upstream,
};
use fake_pixelfed::FaultRule;
use serde_json::Value;

/// Signed in on a fresh harness; returns the engine, a fresh post id and every `is_liked`
/// value that post had in a snapshot from now on.
fn signed_in(h: &Harness) -> (CanvasEngine, String, Arc<Mutex<Vec<Value>>>) {
    let store = Arc::new(MemoryStore::new());
    let engine = h.engine(&store);
    engine.dispatch(&h.login_intent());
    wait_signed_in(&engine);
    let (id, _) = fresh_post(&engine.snapshot());
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (log, pid) = (Arc::clone(&seen), id.clone());
    engine.subscribe(move |s| log.lock().unwrap().push(post(&s, &pid)["is_liked"].clone()));
    (engine, id, seen)
}

/// After the like was dispatched, no snapshot ever showed it undone.
fn assert_never_rolled_back(seen: &Mutex<Vec<Value>>) {
    let seen = seen.lock().unwrap();
    let from = seen
        .iter()
        .position(|v| v == true)
        .expect("the optimistic like");
    assert!(
        seen[from..].iter().all(|v| v == true),
        "the like was rolled back: {seen:?}"
    );
}

/// Statuses the fake answered for `POST /api/v1/statuses/<id>/favourite`.
fn upstream_favourites(h: &Harness, id: &str) -> Vec<(u16, Option<String>)> {
    let path = format!("/api/v1/statuses/{id}/favourite");
    h.upstream
        .fake()
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == path)
        .map(|r| (r.status, r.fault))
        .collect()
}

#[test]
fn upstream_503_twice_gives_502_and_the_client_retries_with_backoff() {
    let h = Harness::fake();
    let (engine, id, seen) = signed_in(&h);
    h.upstream
        .fake()
        .add_faults([FaultRule::status(503).path("/api/v1/statuses").times(2)]);

    engine.dispatch(&like_intent(&id));
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(&h.upstream, &id, (true, false));

    assert_eq!(h.posts_to(LIKE_ENDPOINT), [502, 502, 200]);
    let up = upstream_favourites(&h, &id);
    assert_eq!(
        up.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        [503, 503, 200]
    );
    assert_eq!(up[0].1.as_deref(), Some("status 503 (rule)"));
    // Exponential backoff (fast_config: 50 ms * 2^n): >= 100 ms, then >= 200 ms.
    let at: Vec<_> = h
        .hits()
        .into_iter()
        .filter(|x| x.path == LIKE_ENDPOINT)
        .map(|x| x.at)
        .collect();
    assert!(
        at[1] - at[0] >= Duration::from_millis(90),
        "{:?}",
        at[1] - at[0]
    );
    assert!(
        at[2] - at[1] >= Duration::from_millis(190),
        "{:?}",
        at[2] - at[1]
    );
    assert_never_rolled_back(&seen);
    assert!(h.upstream.fake().faults().is_empty());
}

/// Upstream 429: the pod passes the status through (its rule: upstream 4xx -> same
/// status), and the client treats 429 as transient (D14), so the like is retried, not
/// rolled back. The pod does not forward `Retry-After`; the client uses its own backoff.
#[test]
fn upstream_429_is_retried_not_rolled_back() {
    let h = Harness::fake();
    let (engine, id, seen) = signed_in(&h);
    h.upstream
        .fake()
        .add_faults([FaultRule::rate_limit(2).path("/api/v1/statuses").times(1)]);

    engine.dispatch(&like_intent(&id));
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(&h.upstream, &id, (true, false));

    assert_eq!(h.posts_to(LIKE_ENDPOINT), [429, 200]);
    assert_eq!(
        upstream_favourites(&h, &id),
        [(429, Some("rate_limit 2s (rule)".to_owned())), (200, None)]
    );
    assert_never_rolled_back(&seen);
    assert_eq!(post(&engine.snapshot(), &id)["is_liked"], true);
}

#[test]
fn expired_upstream_token_is_refreshed_transparently() {
    let h = Harness::fake();
    let (engine, id, seen) = signed_in(&h);
    let fake = h.upstream.fake();
    assert_eq!(fake.refresh_token_grants(), 0);

    // Every access token expires: the pod's like gets a 401 upstream, refreshes, retries.
    fake.add_faults([FaultRule::expire_tokens()]);
    engine.dispatch(&like_intent(&id));
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(&h.upstream, &id, (true, false));
    assert_eq!(h.posts_to(LIKE_ENDPOINT), [200]);
    assert_eq!(fake.refresh_token_grants(), 1);
    assert_eq!(
        upstream_favourites(&h, &id)
            .iter()
            .map(|(s, _)| *s)
            .collect::<Vec<_>>(),
        [401, 200]
    );
    assert_never_rolled_back(&seen);

    // A one-off 401 on the feed (token looks expired): refresh again, the client never
    // notices.
    fake.add_faults([FaultRule::status(401).path("/api/v1/timelines").times(1)]);
    let before = h.hits().len();
    engine.refresh();
    wait_until("the refetch", TIMEOUT, || {
        h.hits()[before..]
            .iter()
            .any(|x| x.path == "/v1/workspaces/pixelfed_home")
    });
    let fetch = h.hits()[before..]
        .iter()
        .find(|x| x.path == "/v1/workspaces/pixelfed_home")
        .unwrap()
        .status;
    assert_eq!(fetch, 200);
    assert_eq!(fake.refresh_token_grants(), 2);
    let snap = engine.snapshot();
    assert_eq!(auth(&snap)["state"], "signed_in");
    assert!(!feed(&snap).is_empty());
}

/// Latency past the client's 10 s outbox timeout (and the pod's 10 s upstream timeout):
/// the attempt fails (timeout or 502) and is retried. Takes ~11 s.
#[test]
fn latency_beyond_the_timeouts_is_retried() {
    let h = Harness::fake();
    let (engine, id, seen) = signed_in(&h);
    h.upstream
        .fake()
        .add_faults([FaultRule::latency(10_500).path("/api/v1/statuses").times(1)]);

    engine.dispatch(&like_intent(&id));
    wait_flushed(&engine, Duration::from_secs(30));
    wait_upstream(&h.upstream, &id, (true, false));

    // The pod gave up after 10 s (the fake logs that as 499) and the retry went through.
    assert_eq!(
        upstream_favourites(&h, &id),
        [
            (499, Some("latency 10500ms (rule)".to_owned())),
            (200, None)
        ]
    );
    let pod = h.posts_to(LIKE_ENDPOINT);
    assert_eq!(pod.last(), Some(&200), "{pod:?}");
    assert!(pod.len() <= 2, "{pod:?}");
    assert_never_rolled_back(&seen);
}

/// `reset(keep_auth)` restores the fixtures under a live session; a full reset (as after an
/// instance wipe) drops the pod's upstream token and app: the client is signed out, and a
/// new sign-in re-registers the app.
#[test]
fn upstream_reset_between_runs() {
    let h = Harness::fake();
    let (engine, id, _) = signed_in(&h);
    engine.dispatch(&like_intent(&id));
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(&h.upstream, &id, (true, false));

    h.upstream.fake().reset(true);
    assert_eq!(h.upstream.flags(&id), (false, false));
    engine.refresh();
    wait_until("the refetched, reset feed", TIMEOUT, || {
        post(&engine.snapshot(), &id)["is_liked"] == false
    });
    assert_eq!(auth(&engine.snapshot())["state"], "signed_in");

    h.upstream.reset(); // over HTTP: POST /_admin/reset
    engine.refresh();
    wait_until("signed out after the upstream wipe", TIMEOUT, || {
        let s = engine.snapshot();
        auth(&s)["state"] == "signed_out" && s.workspace_json.contains("Sign in with Pixelfed")
    });
    engine.dispatch(&h.login_intent());
    wait_signed_in(&engine);
    let report = h.upstream.fake().report(0);
    assert_eq!(report["counts"]["apps"], 1, "{report}");
    assert_eq!(report["grants"]["authorization_code"], 1);
}
