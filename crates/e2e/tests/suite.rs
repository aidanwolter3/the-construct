//! The end-to-end suite, the same steps against either upstream (README "End-to-end
//! tests"): sign in via PKCE with a scripted browser, the home feed, a like, a like made
//! offline and flushed on reconnect, a bookmark. Leaves the upstream as it found it.
//!
//! `cargo test -p e2e` runs it against the in-process fake (or `E2E_FAKE_URL`);
//! `E2E_UPSTREAM=real` (or `scripts/e2e.sh --upstream real`) against a real Pixelfed.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client_core::MemoryStore;
use e2e::{
    BOOKMARK_ENDPOINT, Harness, LIKE_ENDPOINT, TIMEOUT, Upstream, auth, bookmark_intent, entities,
    feed, fresh_post, like_intent, post, wait_flushed, wait_signed_in, wait_until, wait_upstream,
};

/// Real upstream: undo the suite's like/bookmark even if a step fails.
struct Restore<'a> {
    upstream: &'a Upstream,
    id: String,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Upstream::Real { url, token, .. } = self.upstream {
            let http = reqwest::blocking::Client::new();
            for action in ["unfavourite", "unbookmark"] {
                let _ = http
                    .post(format!("{url}/api/v1/statuses/{}/{action}", self.id))
                    .bearer_auth(token)
                    .send();
            }
        }
    }
}

#[test]
fn e2e_suite() {
    let started = Instant::now();
    let h = Harness::from_env();
    let up = &h.upstream;
    eprintln!("[e2e] upstream {} (real: {})", up.url(), up.is_real());
    up.reset();

    // 1. Signed out: the sign-in bundle. Then PKCE through the scripted browser.
    let store = Arc::new(MemoryStore::new());
    let engine = h.engine(&store);
    wait_until("the signed-out bundle", TIMEOUT, || {
        engine
            .snapshot()
            .workspace_json
            .contains("Sign in with Pixelfed")
    });
    engine.dispatch(&h.login_intent());
    wait_signed_in(&engine);
    let snap = engine.snapshot();
    let a = auth(&snap);
    assert_eq!(a["username"], up.expected_username(), "{a}");
    eprintln!(
        "[e2e] signed in as @{} after {:?}",
        a["username"].as_str().unwrap_or(""),
        started.elapsed()
    );

    // 2. The feed: posts with their entities, media through the pod's proxy.
    let uris = feed(&snap);
    let ents = entities(&snap);
    assert!(!uris.is_empty());
    for uri in &uris {
        assert!(ents.get(uri).is_some(), "{uri} has no entity");
    }
    let media_prefix = format!("{}/v1/pixelfed/media/", h.pod_url);
    assert!(
        snap.entities_json.contains(&media_prefix),
        "no proxied media URL"
    );
    if !up.is_real() {
        // The fixture timeline: five entries, one of them a boost.
        assert_eq!(uris.len(), 5);
        assert!(snap.entities_json.contains("reblogged_by_uri"));
    }

    // 3. Like: optimistic at once, then on the upstream.
    let (id, count) = fresh_post(&snap);
    let _restore = Restore {
        upstream: up,
        id: id.clone(),
    };
    assert_eq!(up.flags(&id), (false, false));
    engine.dispatch(&like_intent(&id));
    let p = post(&engine.snapshot(), &id);
    assert_eq!(p["is_liked"], true);
    assert_eq!(p["like_count"], count + 1);
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(up, &id, (true, false));
    assert_eq!(h.posts_to(LIKE_ENDPOINT), [200]);

    // 4. Offline: the unlike is kept (not rolled back) and flushed on reconnect.
    h.set_pod_reachable(false);
    wait_until("offline", TIMEOUT, || !engine.snapshot().is_online);
    engine.dispatch(&like_intent(&id));
    let snap = engine.snapshot();
    assert_eq!(snap.pending_actions, 1);
    assert_eq!(post(&snap, &id)["is_liked"], false);
    thread::sleep(Duration::from_millis(300));
    let snap = engine.snapshot();
    assert_eq!(snap.pending_actions, 1, "the queued unlike must wait");
    assert_eq!(post(&snap, &id)["is_liked"], false);
    assert_eq!(post(&snap, &id)["like_count"], count);
    assert_eq!(up.flags(&id), (true, false));
    h.set_pod_reachable(true);
    wait_until("online", TIMEOUT, || engine.snapshot().is_online);
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(up, &id, (false, false));
    assert_eq!(h.posts_to(LIKE_ENDPOINT), [200, 200]);
    assert_eq!(post(&engine.snapshot(), &id)["is_liked"], false);

    // 5. Bookmark, and un-bookmark (restores the upstream).
    engine.dispatch(&bookmark_intent(&id));
    assert_eq!(post(&engine.snapshot(), &id)["is_bookmarked"], true);
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(up, &id, (false, true));
    engine.dispatch(&bookmark_intent(&id));
    wait_flushed(&engine, TIMEOUT);
    wait_upstream(up, &id, (false, false));
    assert_eq!(h.posts_to(BOOKMARK_ENDPOINT), [200, 200]);

    // 6. Sign out.
    engine.logout();
    wait_until("signed out", TIMEOUT, || {
        auth(&engine.snapshot())["state"] == "signed_out"
    });
    wait_until("the logout to reach the pod", TIMEOUT, || {
        h.hits()
            .iter()
            .any(|x| x.path == "/v1/auth/logout" && x.status == 200)
    });
    eprintln!("[e2e] suite passed in {:?}", started.elapsed());
}
