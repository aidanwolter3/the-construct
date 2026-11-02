//! HTTP client for the configured Pixelfed (Mastodon API) instance.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

/// Total timeout for API calls.
pub const API_TIMEOUT: Duration = Duration::from_secs(10);
/// Media downloads may take longer than API calls.
pub const MEDIA_TIMEOUT: Duration = Duration::from_secs(30);

pub const APP_NAME: &str = "Project Canvas";
pub const SCOPES: &str = "read write";

#[derive(Debug)]
pub enum UpstreamError {
    /// Connect error, timeout, or an unreadable / undecodable response.
    Unreachable(String),
    /// A non-2xx response (body kept for error details, truncated).
    Status(StatusCode, String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "upstream unreachable: {e}"),
            Self::Status(s, body) => write!(f, "upstream answered {s}: {body}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisteredApp {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
}

/// Upstream client: API calls go to `instance_url` (server-side reachable).
#[derive(Debug, Clone)]
pub struct Upstream {
    http: reqwest::Client,
    instance_url: String,
}

impl Upstream {
    pub fn new(instance_url: &str) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(API_TIMEOUT)
            .user_agent(concat!("canvas-pod/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            http,
            instance_url: instance_url.trim_end_matches('/').to_owned(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.instance_url)
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        req: reqwest::RequestBuilder,
    ) -> Result<T, UpstreamError> {
        let resp = req
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let mut text = String::from_utf8_lossy(&body).into_owned();
            text.truncate(500);
            return Err(UpstreamError::Status(status, text));
        }
        serde_json::from_slice(&body)
            .map_err(|e| UpstreamError::Unreachable(format!("invalid JSON from upstream: {e}")))
    }

    /// `POST /api/v1/apps`.
    pub async fn register_app(&self, redirect_uri: &str) -> Result<RegisteredApp, UpstreamError> {
        Self::send_json(self.http.post(self.url("/api/v1/apps")).form(&[
            ("client_name", APP_NAME),
            ("redirect_uris", redirect_uri),
            ("scopes", SCOPES),
        ]))
        .await
    }

    /// `POST /oauth/token` (authorization_code + PKCE verifier).
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse, UpstreamError> {
        Self::send_json(self.http.post(self.url("/oauth/token")).form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code_verifier", code_verifier),
            ("scope", SCOPES),
        ]))
        .await
    }

    pub async fn verify_credentials(&self, token: &str) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url("/api/v1/accounts/verify_credentials"))
                .bearer_auth(token),
        )
        .await
    }

    pub async fn home_timeline(
        &self,
        token: &str,
        limit: u32,
    ) -> Result<Vec<Value>, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url("/api/v1/timelines/home"))
                .query(&[("limit", limit)])
                .bearer_auth(token),
        )
        .await
    }

    /// `POST /api/v1/statuses/{id}/{action}` (favourite, unfavourite, bookmark,
    /// unbookmark). `id` must already be validated as path-safe.
    pub async fn status_action(
        &self,
        token: &str,
        id: &str,
        action: &str,
    ) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .post(self.url(&format!("/api/v1/statuses/{id}/{action}")))
                .bearer_auth(token),
        )
        .await
    }

    /// GET a media URL (no credentials are sent: media are public and the URL
    /// may be on another host).
    pub async fn fetch_media(&self, url: &str) -> Result<reqwest::Response, UpstreamError> {
        let resp = self
            .http
            .get(url)
            .timeout(MEDIA_TIMEOUT)
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(UpstreamError::Status(resp.status(), String::new()));
        }
        Ok(resp)
    }
}
