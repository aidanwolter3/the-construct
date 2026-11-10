//! The follower runs for every pod signed in to a runner, from the moment
//! the pod opens, with no request and no registered device (decision
//! `agents-runs`).

use std::sync::Arc;
use std::time::Duration;

use app_agents::cap::Runner;
use app_agents::{AgentsAdapter, AgentsConfig, db, state::State};
use app_collections::CollectionsAdapter;
use pod_core::store::Store;
use pod_core::{AppAdapter, AppState, Pod};

use crate::runner::{Fake, TOKEN, serve, snapshot_data, until};

fn adapters() -> Vec<Arc<dyn AppAdapter>> {
    vec![
        Arc::new(AgentsAdapter::new(AgentsConfig {
            allow_private_hosts: true,
            tasks_every: Duration::from_millis(100),
            backoff_max: Duration::from_millis(100),
        })),
        Arc::new(CollectionsAdapter::new()),
        app_shell::adapter(),
    ]
}

#[tokio::test]
async fn a_signed_in_pod_follows_its_runner_from_the_start() {
    let fake = Arc::new(Fake::default());
    *fake.snapshot.lock().unwrap() = snapshot_data();
    let url = serve(fake.clone()).await;
    let dir = std::env::temp_dir().join(format!(
        "construct-agents-always-on-{}-{}",
        std::process::id(),
        pod_core::util::now_secs()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.db");

    // A pod signed in before the server stopped (never opened here, so no
    // follower runs for it).
    {
        let pod = AppState::new(Store::open(&path).unwrap(), adapters());
        let runner = Runner {
            url,
            token: TOKEN.into(),
        };
        db::sign_in(pod.store(), &runner, &State::default(), 0).unwrap();
    }

    // The server starts again: the pod says it must be open even without a
    // device, so the host's scheduler opens it at once ...
    let pod: Pod = Arc::new(AppState::new(Store::open(&path).unwrap(), adapters()));
    assert_eq!(pod.poll_due().await, Some(pod_core::notify::KEEP_OPEN_WAKE));
    assert!(fake.streams.lock().unwrap().is_empty());
    // ... and opening it starts the follower, with no request.
    let _app = pod_core::app(pod.clone());
    until(
        "the follower took a snapshot and follows the stream",
        || async { !fake.streams.lock().unwrap().is_empty() },
    )
    .await;
    assert_eq!(fake.sinces()[0], 10);
    let stored = db::state(pod.store()).unwrap().unwrap().0;
    assert!(stored.runs.contains_key("run_work"));

    drop(pod);
    let _ = std::fs::remove_dir_all(dir);
}
