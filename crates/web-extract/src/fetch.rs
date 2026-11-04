//! A conditional `GET` of a web page, with a size cap, and the check that
//! keeps fetches on public websites.

use std::time::Duration;

use reqwest::{StatusCode, header};

/// Largest body read.
pub const MAX_BODY: usize = 3 * 1024 * 1024;
/// One fetch's timeout.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest URL.
pub const MAX_URL: usize = 2_000;

/// A fetch that went wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchError {
    pub message: String,
    /// The server asked to wait this long (`Retry-After`, seconds).
    pub retry_after: Option<i64>,
}

fn fail(message: impl Into<String>) -> FetchError {
    FetchError {
        message: message.into(),
        retry_after: None,
    }
}

/// What a fetch answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// The `Accept` header a fetch sends unless the caller gives one.
pub const DEFAULT_ACCEPT: &str = "text/html,application/xhtml+xml,text/calendar,application/ld+json;q=0.9,application/json;q=0.9,application/rss+xml;q=0.9,*/*;q=0.8";

/// GET `url`, sending the validators of the last fetch and `headers`
/// (which may replace `Accept`).
pub async fn fetch(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    headers: &[(String, String)],
) -> Result<Fetched, FetchError> {
    let mut req = client.get(url);
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
    {
        req = req.header(header::ACCEPT, DEFAULT_ACCEPT);
    }
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(e) = etag {
        req = req.header(header::IF_NONE_MATCH, e);
    }
    if let Some(m) = last_modified {
        req = req.header(header::IF_MODIFIED_SINCE, m);
    }
    let mut resp = req.send().await.map_err(|e| {
        let e = if e.is_timeout() {
            "timed out".to_owned()
        } else if e.is_connect() {
            "could not connect".to_owned()
        } else if e.is_builder() {
            "the request could not be built (a header name or value is not valid)".to_owned()
        } else {
            e.to_string()
        };
        fail(e)
    })?;
    let status = resp.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|s| *s > 0);
        return Err(FetchError {
            message: format!("the site answered {status}"),
            retry_after,
        });
    }
    let header_of = |h: header::HeaderName| {
        resp.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header_of(header::ETAG);
    let last_modified = header_of(header::LAST_MODIFIED);
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| fail(e.to_string()))? {
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err(fail(format!("the page is over {} MiB", MAX_BODY >> 20)));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
    })
}

/// `url` normalized if the pod may fetch it: `http(s)`, at most
/// [`MAX_URL`] characters, and (unless `allow_private`) a public host name,
/// not an IP literal or a private name ([`pod_core::net::is_public_host`];
/// the DNS answer is checked again at connect time).
pub fn check_url(url: &str, allow_private: bool) -> Result<String, String> {
    let url = url.trim();
    if url.len() > MAX_URL {
        return Err(format!("the URL is at most {MAX_URL} characters"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("\"{url}\" is not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("the URL must start with http:// or https://".into());
    }
    let host = parsed.host_str().unwrap_or("");
    if !allow_private && !pod_core::net::is_public_host(host) {
        return Err(format!("\"{host}\" is not a public website"));
    }
    Ok(parsed.to_string())
}

/// The host of `url` (lowercase), for spacing requests to one site.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// Make `url` absolute against `base` (pages often give paths); empty when
/// it isn't an `http(s)` URL.
pub fn absolute(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    match reqwest::Url::parse(base).and_then(|b| b.join(url)) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_websites() {
        for bad in [
            "http://127.0.0.1:8080/v1/apps",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://localhost:8080/",
            "http://pod.internal/",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(check_url(bad, false).is_err(), "{bad}");
        }
        assert_eq!(
            check_url(" https://Example.com/a?b=1 ", false).as_deref(),
            Ok("https://example.com/a?b=1")
        );
        assert!(check_url("http://127.0.0.1:8080/x", true).is_ok());
        assert_eq!(host_of("https://Shop.Example.com/p/1"), "shop.example.com");
        assert_eq!(
            absolute("https://a.example/x/", "../y"),
            "https://a.example/y"
        );
        assert_eq!(absolute("https://a.example/", "javascript:alert(1)"), "");
    }
}
