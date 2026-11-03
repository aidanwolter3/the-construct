//! Storage limits through the host (decision `pod-storage-limits`, security
//! audit #31 M2): upload quotas per pod kind and the low-disk guard.

use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use server::PixelfedConfig;
use server::host::{Host, HostConfig, PodKind, StorageLimits, TemporaryPods};
use tokio::task::JoinHandle;

struct Env {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    url: String,
    handle: JoinHandle<()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn env(storage: StorageLimits) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // No upstream is contacted: uploads and pod creation are the core's.
    let factory = server::pod_factory(
        PixelfedConfig::new("http://127.0.0.1:9", None),
        false,
        86_400,
        false,
    );
    let config = HostConfig {
        temporary: Some(TemporaryPods {
            hours: 1,
            max_live: 5,
        }),
        new_pod_settings: server::new_pod_settings(false),
        temporary_pod_settings: server::temporary_pod_settings(),
        storage,
        ..HostConfig::default()
    };
    let path = dir.path().to_owned();
    let host = tokio::task::spawn_blocking(move || Host::open(&path, factory, config))
        .await
        .unwrap()
        .unwrap();
    let host = Arc::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::host::app(host.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Env {
        _dir: dir,
        host,
        url,
        handle,
    }
}

impl Env {
    async fn temporary(&self) -> (StatusCode, Value) {
        let resp = reqwest::Client::new()
            .post(format!("{}/v1/pods/temporary", self.url))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        (resp.status(), resp.json().await.unwrap_or(Value::Null))
    }

    async fn admin(&self, name: &str) -> String {
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

    async fn upload(&self, token: &str, key: &str, bytes: usize) -> (StatusCode, Value) {
        let mut photo = vec![0xFF, 0xD8, 0xFF, 0xE0];
        photo.resize(bytes, 7);
        let resp = reqwest::Client::new()
            .post(format!("{}/v1/uploads", self.url))
            .bearer_auth(token)
            .header("content-type", "image/jpeg")
            .header("idempotency-key", key)
            .body(photo)
            .send()
            .await
            .unwrap();
        (resp.status(), resp.json().await.unwrap_or(Value::Null))
    }
}

const KIB: usize = 1024;

#[tokio::test]
async fn temporary_pods_get_the_smaller_upload_quota() {
    let env = env(StorageLimits {
        pod_upload_quota_bytes: 1024 * KIB as u64,
        temporary_upload_quota_bytes: 300 * KIB as u64,
        min_free_disk_bytes: 0,
    })
    .await;
    let (status, body) = env.temporary().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let temp = body["session_token"].as_str().unwrap().to_owned();
    let admin = env.admin("me").await;

    assert_eq!(env.upload(&temp, "1", 200 * KIB).await.0, StatusCode::OK);
    let (status, body) = env.upload(&temp, "2", 200 * KIB).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"], "upload_quota_exceeded");

    // The admin pod has its own, larger quota.
    for key in ["1", "2", "3"] {
        assert_eq!(env.upload(&admin, key, 200 * KIB).await.0, StatusCode::OK);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_nearly_full_disk_stops_uploads_and_new_temporary_pods() {
    let env = env(StorageLimits {
        min_free_disk_bytes: u64::MAX,
        ..StorageLimits::default()
    })
    .await;
    let (status, body) = env.temporary().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "low_disk_space");
    let admin = env.admin("me").await;
    let (status, body) = env.upload(&admin, "1", 10 * KIB).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"], "low_disk_space");
}
