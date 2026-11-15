//! Web search (decision `web-and-agent-modules`): one operation,
//! [`search`], behind the providers the pod's `web_search` jack may name.
//!
//! - **Brave** (`provider: brave`): the Brave Search API with the pod's own
//!   key (`X-Subscription-Token`). Meant for programs.
//! - **SearXNG** (`provider: searxng`): the user's own instance, JSON output.
//! - **DuckDuckGo** (`provider: duckduckgo`): its HTML results page, no key.
//!   It refuses repeated automated searches, so the host policy caps it at
//!   [`crate::hosts::SEARCH_PAGE_CAP`] a day; a last resort.
//!
//! Every request goes through the pod's host policy ([`crate::hosts`]), and
//! answers are cached per provider and query for [`CACHE_TTL`].
//!
//! [`search_images`] finds pictures instead (decision `image-search`): Brave's
//! image search, or SearXNG's `images` category. DuckDuckGo's image results
//! need a token from its pages, so it has none.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

/// How long a search's answer is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Most results asked for.
pub const MAX_RESULTS: usize = 10;
/// Longest query.
pub const MAX_QUERY_CHARS: usize = 300;
/// Brave's endpoint.
pub const BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";
/// Brave's image search endpoint.
pub const BRAVE_IMAGES_URL: &str = "https://api.search.brave.com/res/v1/images/search";
/// DuckDuckGo's HTML results page.
pub const DUCKDUCKGO_URL: &str = "https://html.duckduckgo.com/html/";

/// One result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl SearchResult {
    pub fn to_json(&self) -> Value {
        json!({ "title": self.title, "url": self.url, "snippet": self.snippet })
    }
}

/// What an image result offers a filler's `map` ([`ImageResult::get`]).
pub const IMAGE_FIELDS: &[&str] = &["image", "thumbnail", "page", "title", "source"];

/// One image result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageResult {
    /// What the image shows, as its page titles it.
    pub title: String,
    /// The full-size image's URL.
    pub image: String,
    /// A small copy's URL (the provider's), else the image's.
    pub thumbnail: String,
    /// The page the image is on.
    pub page: String,
    /// The site it is from (a host name).
    pub source: String,
}

impl ImageResult {
    /// One of [`IMAGE_FIELDS`].
    pub fn get(&self, field: &str) -> Option<&str> {
        Some(match field {
            "image" => &self.image,
            "thumbnail" => &self.thumbnail,
            "page" => &self.page,
            "title" => &self.title,
            "source" => &self.source,
            _ => return None,
        })
    }
}

/// An http(s) URL, else empty.
fn web_url(u: &str) -> String {
    let u = u.trim();
    match reqwest::Url::parse(u) {
        Ok(p) if matches!(p.scheme(), "http" | "https") => u.to_owned(),
        _ => String::new(),
    }
}

/// A configured provider (from the `web_search` jack's config).
#[derive(Clone, PartialEq, Eq)]
pub enum Provider {
    Brave { key: String },
    Searxng { base_url: String },
    DuckDuckGo,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.write_str(self.name())
    }
}

impl Provider {
    /// From the `web_search` jack's config (field names as declared by
    /// `pod_core::jacks`).
    pub fn from_config(c: &Map<String, Value>) -> Result<Self, String> {
        let text = |k: &str| {
            c.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        match text("provider") {
            Some("brave") => text("api_key")
                .map(|k| Self::Brave { key: k.to_owned() })
                .ok_or_else(|| "the web_search jack's brave provider has no api_key".to_owned()),
            Some("searxng") => text("base_url")
                .map(|b| Self::Searxng {
                    base_url: b.trim_end_matches('/').to_owned(),
                })
                .ok_or_else(|| "the web_search jack's searxng provider has no base_url".to_owned()),
            Some("duckduckgo") => Ok(Self::DuckDuckGo),
            other => Err(format!(
                "the web_search jack's provider {other:?} is not brave, searxng or duckduckgo"
            )),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Brave { .. } => "brave",
            Self::Searxng { .. } => "searxng",
            Self::DuckDuckGo => "duckduckgo",
        }
    }

    /// The request URL and headers for `query`.
    fn request(
        &self,
        query: &str,
        count: usize,
    ) -> Result<(String, Vec<(String, String)>), String> {
        let n = count.clamp(1, MAX_RESULTS).to_string();
        let url = |base: &str, params: &[(&str, &str)]| {
            reqwest::Url::parse_with_params(base, params)
                .map(|u| u.to_string())
                .map_err(|e| format!("building the search URL: {e}"))
        };
        Ok(match self {
            Self::Brave { key } => (
                url(BRAVE_URL, &[("q", query), ("count", &n)])?,
                vec![
                    ("Accept".into(), "application/json".into()),
                    ("X-Subscription-Token".into(), key.clone()),
                ],
            ),
            Self::Searxng { base_url } => (
                url(
                    &format!("{base_url}/search"),
                    &[("q", query), ("format", "json")],
                )?,
                vec![("Accept".into(), "application/json".into())],
            ),
            Self::DuckDuckGo => (url(DUCKDUCKGO_URL, &[("q", query)])?, Vec::new()),
        })
    }

    /// The request URL and headers of an image search for `query`.
    fn image_request(
        &self,
        query: &str,
        count: usize,
    ) -> Result<(String, Vec<(String, String)>), String> {
        let n = count.clamp(1, MAX_RESULTS).to_string();
        let url = |base: &str, params: &[(&str, &str)]| {
            reqwest::Url::parse_with_params(base, params)
                .map(|u| u.to_string())
                .map_err(|e| format!("building the search URL: {e}"))
        };
        match self {
            Self::Brave { key } => Ok((
                url(
                    BRAVE_IMAGES_URL,
                    &[("q", query), ("count", &n), ("safesearch", "strict")],
                )?,
                vec![
                    ("Accept".into(), "application/json".into()),
                    ("X-Subscription-Token".into(), key.clone()),
                ],
            )),
            Self::Searxng { base_url } => Ok((
                url(
                    &format!("{base_url}/search"),
                    &[("q", query), ("format", "json"), ("categories", "images")],
                )?,
                vec![("Accept".into(), "application/json".into())],
            )),
            Self::DuckDuckGo => Err(NO_IMAGES.into()),
        }
    }

    /// The image results in a provider's answer: those with an http(s)
    /// image.
    pub fn parse_images(&self, body: &str, count: usize) -> Result<Vec<ImageResult>, String> {
        let v: Value = serde_json::from_str(body)
            .map_err(|_| format!("{}'s image answer is not JSON", self.name()))?;
        let list = v["results"].as_array().cloned().unwrap_or_default();
        let mut out: Vec<ImageResult> = list
            .iter()
            .map(|r| {
                let text = |x: &Value| x.as_str().unwrap_or("").to_owned();
                let (image, thumbnail, page, source) = match self {
                    Self::Searxng { .. } => (
                        web_url(&text(&r["img_src"])),
                        web_url(&text(&r["thumbnail_src"])),
                        web_url(&text(&r["url"])),
                        String::new(),
                    ),
                    _ => (
                        web_url(&text(&r["properties"]["url"])),
                        web_url(&text(&r["thumbnail"]["src"])),
                        web_url(&text(&r["url"])),
                        text(&r["source"]),
                    ),
                };
                let source = if source.trim().is_empty() {
                    crate::fetch::host_of(&page)
                } else {
                    source.trim().to_owned()
                };
                ImageResult {
                    title: clean(&text(&r["title"])),
                    thumbnail: if thumbnail.is_empty() {
                        image.clone()
                    } else {
                        thumbnail
                    },
                    image,
                    page,
                    source: source.trim_start_matches("www.").to_owned(),
                }
            })
            .filter(|r| !r.image.is_empty())
            .collect();
        out.truncate(count);
        Ok(out)
    }

    /// The results in a provider's answer.
    pub fn parse(&self, body: &str, count: usize) -> Result<Vec<SearchResult>, String> {
        let mut out = match self {
            Self::Brave { .. } => {
                let v: Value = serde_json::from_str(body)
                    .map_err(|_| "Brave's answer is not JSON".to_owned())?;
                json_results(&v["web"]["results"], "description")
            }
            Self::Searxng { .. } => {
                let v: Value = serde_json::from_str(body).map_err(|_| {
                    "SearXNG's answer is not JSON (is format=json enabled on the instance?)"
                        .to_owned()
                })?;
                json_results(&v["results"], "content")
            }
            Self::DuckDuckGo => duckduckgo_results(body),
        };
        out.retain(|r| !r.url.is_empty());
        out.truncate(count);
        Ok(out)
    }
}

fn clean(s: &str) -> String {
    crate::html::decode_entities(&crate::html::to_text(s))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn json_results(list: &Value, snippet_key: &str) -> Vec<SearchResult> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|r| SearchResult {
            title: clean(r["title"].as_str().unwrap_or("")),
            url: r["url"].as_str().unwrap_or("").trim().to_owned(),
            snippet: clean(r[snippet_key].as_str().unwrap_or("")),
        })
        .collect()
}

/// A DuckDuckGo result link is a redirect (`//duckduckgo.com/l/?uddg=<url>`):
/// the target.
fn ddg_target(href: &str) -> String {
    let full = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    match reqwest::Url::parse(&full) {
        Ok(u) if u.path() == "/l/" => u
            .query_pairs()
            .find(|(k, _)| k == "uddg")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default(),
        Ok(u) if matches!(u.scheme(), "http" | "https") => u.to_string(),
        _ => String::new(),
    }
}

fn duckduckgo_results(body: &str) -> Vec<SearchResult> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(body);
    let (Ok(result), Ok(link), Ok(snippet)) = (
        Selector::parse(".result"),
        Selector::parse(".result__a"),
        Selector::parse(".result__snippet"),
    ) else {
        return Vec::new();
    };
    doc.select(&result)
        .filter(|r| !r.value().attr("class").unwrap_or("").contains("result--ad"))
        .filter_map(|r| {
            let a = r.select(&link).next()?;
            Some(SearchResult {
                title: a
                    .text()
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                url: ddg_target(a.value().attr("href").unwrap_or("")),
                snippet: r
                    .select(&snippet)
                    .next()
                    .map(|s| {
                        s.text()
                            .collect::<String>()
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Why DuckDuckGo can't search for images.
pub const NO_IMAGES: &str =
    "duckduckgo has no image search: plug the web_search jack in with brave or searxng";

/// Answers by `(provider, query)`, each with when it was fetched.
type Answers<T> = HashMap<(String, String), (Instant, Vec<T>)>;

/// The pod's cache of search answers by `(provider, query)`, web and image
/// searches apart (`Pod::local::<SearchCache>()`).
#[derive(Debug, Default)]
pub struct SearchCache {
    web: tokio::sync::Mutex<Answers<SearchResult>>,
    images: tokio::sync::Mutex<Answers<ImageResult>>,
}

const MAX_CACHED: usize = 500;

async fn cached<T: Clone>(
    m: &tokio::sync::Mutex<Answers<T>>,
    provider: &str,
    query: &str,
) -> Option<Vec<T>> {
    let m = m.lock().await;
    m.get(&(provider.to_owned(), query.to_owned()))
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, r)| r.clone())
}

async fn keep<T>(m: &tokio::sync::Mutex<Answers<T>>, provider: &str, query: &str, results: Vec<T>) {
    let mut m = m.lock().await;
    if m.len() >= MAX_CACHED {
        m.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if m.len() >= MAX_CACHED {
            m.clear();
        }
    }
    m.insert(
        (provider.to_owned(), query.to_owned()),
        (Instant::now(), results),
    );
}

impl SearchCache {
    async fn get(&self, provider: &str, query: &str) -> Option<Vec<SearchResult>> {
        cached(&self.web, provider, query).await
    }

    async fn put(&self, provider: &str, query: &str, results: Vec<SearchResult>) {
        keep(&self.web, provider, query, results).await
    }
}

/// The normalized form of a query (the cache's key).
pub fn normalize_query(q: &str) -> String {
    q.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect()
}

/// Search `query` with `provider`: at most `count` results, cached, through
/// the host policy. `Err`: a sentence for a filler's status (a block, a
/// spent cap, a refused key).
pub async fn search(
    hosts: &crate::hosts::Hosts,
    cache: &SearchCache,
    client: &reqwest::Client,
    provider: &Provider,
    query: &str,
    count: usize,
) -> Result<Vec<SearchResult>, String> {
    let q = normalize_query(query);
    if q.is_empty() {
        return Err("the search is empty".into());
    }
    if let Some(hit) = cache.get(provider.name(), &q).await {
        return Ok(hit.into_iter().take(count).collect());
    }
    let (url, headers) = provider.request(&q, count)?;
    let fetched = crate::hosts::fetch(hosts, client, &url, None, None, &headers)
        .await
        .map_err(|e| match provider {
            Provider::Brave { .. } if e.message.contains("401") || e.message.contains("422") => {
                "Brave refused the web_search jack's api_key".to_owned()
            }
            _ => format!("searching with {}: {}", provider.name(), e.message),
        })?;
    let crate::fetch::Fetched::Body { bytes, .. } = fetched else {
        return Err("the search answered nothing".into());
    };
    let results = provider.parse(&String::from_utf8_lossy(&bytes), MAX_RESULTS)?;
    cache.put(provider.name(), &q, results.clone()).await;
    Ok(results.into_iter().take(count).collect())
}

/// Search `query` for images with `provider`: at most `count` results,
/// cached apart from web searches, through the host policy. `Err`: a
/// sentence for a filler's status (DuckDuckGo, a block, a refused key).
pub async fn search_images(
    hosts: &crate::hosts::Hosts,
    cache: &SearchCache,
    client: &reqwest::Client,
    provider: &Provider,
    query: &str,
    count: usize,
) -> Result<Vec<ImageResult>, String> {
    let q = normalize_query(query);
    if q.is_empty() {
        return Err("the search is empty".into());
    }
    if let Some(hit) = cached(&cache.images, provider.name(), &q).await {
        return Ok(hit.into_iter().take(count).collect());
    }
    let (url, headers) = provider.image_request(&q, MAX_RESULTS)?;
    let fetched = crate::hosts::fetch(hosts, client, &url, None, None, &headers)
        .await
        .map_err(|e| match provider {
            Provider::Brave { .. } if e.message.contains("401") || e.message.contains("422") => {
                "Brave refused the web_search jack's api_key".to_owned()
            }
            _ => format!("searching images with {}: {}", provider.name(), e.message),
        })?;
    let crate::fetch::Fetched::Body { bytes, .. } = fetched else {
        return Err("the search answered nothing".into());
    };
    let results = provider.parse_images(&String::from_utf8_lossy(&bytes), MAX_RESULTS)?;
    keep(&cache.images, provider.name(), &q, results.clone()).await;
    Ok(results.into_iter().take(count).collect())
}

/// Results as numbered text for a prompt.
pub fn results_text(results: &[SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {}\n   {}\n   {}", i + 1, r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_from_config() {
        let c = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(
            Provider::from_config(&c(json!({"provider": "brave", "api_key": "k"}))).unwrap(),
            Provider::Brave { key: "k".into() }
        );
        assert!(Provider::from_config(&c(json!({"provider": "brave"}))).is_err());
        assert_eq!(
            Provider::from_config(&c(
                json!({"provider": "searxng", "base_url": "https://s.example/"})
            ))
            .unwrap(),
            Provider::Searxng {
                base_url: "https://s.example".into()
            }
        );
        assert_eq!(
            format!(
                "{:?}",
                Provider::Brave {
                    key: "secret".into()
                }
            ),
            "brave"
        );
    }

    #[test]
    fn brave_and_searxng_answers() {
        let brave = r#"{"web":{"results":[{"title":"AeroPress <strong>Filters</strong>","url":"https://shop.example/f","description":"350 count"},{"title":"x","url":""}]}}"#;
        let r = Provider::Brave { key: "k".into() }.parse(brave, 5).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "AeroPress Filters");
        assert_eq!(r[0].snippet, "350 count");
        let sx = r#"{"results":[{"title":"Soap","url":"https://soap.example/bar","content":"Lavender"}]}"#;
        let r = Provider::Searxng {
            base_url: "https://s.example".into(),
        }
        .parse(sx, 5)
        .unwrap();
        assert_eq!(r[0].url, "https://soap.example/bar");
    }

    #[test]
    fn image_answers() {
        let brave = r#"{"results":[
            {"title":"Oxford <b>shirt</b>","url":"https://www.shop.example/p/oxford","source":"shop.example",
             "thumbnail":{"src":"https://imgs.search.example/t1.jpg"},
             "properties":{"url":"https://cdn.shop.example/oxford.jpg"}},
            {"title":"no image","url":"https://x.example","properties":{"url":"data:image/png;base64,AA"}}]}"#;
        let r = Provider::Brave { key: "k".into() }
            .parse_images(brave, 5)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Oxford shirt");
        assert_eq!(
            r[0].get("image"),
            Some("https://cdn.shop.example/oxford.jpg")
        );
        assert_eq!(
            r[0].get("thumbnail"),
            Some("https://imgs.search.example/t1.jpg")
        );
        assert_eq!(r[0].get("page"), Some("https://www.shop.example/p/oxford"));
        assert_eq!(r[0].get("source"), Some("shop.example"));
        let sx = r#"{"results":[{"title":"Boots","url":"https://www.boots.example/a","img_src":"https://boots.example/a.jpg"}]}"#;
        let r = Provider::Searxng {
            base_url: "https://s.example".into(),
        }
        .parse_images(sx, 5)
        .unwrap();
        assert_eq!(r[0].thumbnail, "https://boots.example/a.jpg");
        assert_eq!(r[0].source, "boots.example");
        for f in IMAGE_FIELDS {
            assert!(r[0].get(f).is_some(), "{f}");
        }
    }

    #[test]
    fn image_requests() {
        let (u, h) = Provider::Brave { key: "k1".into() }
            .image_request("oxford shirt", 5)
            .unwrap();
        assert!(
            u.starts_with(BRAVE_IMAGES_URL) && u.contains("q=oxford+shirt"),
            "{u}"
        );
        assert!(
            h.iter()
                .any(|(k, v)| k == "X-Subscription-Token" && v == "k1")
        );
        let (u, _) = Provider::Searxng {
            base_url: "https://s.example".into(),
        }
        .image_request("boots", 5)
        .unwrap();
        assert!(
            u.contains("categories=images") && u.contains("format=json"),
            "{u}"
        );
        assert_eq!(
            Provider::DuckDuckGo.image_request("x", 5).unwrap_err(),
            NO_IMAGES
        );
    }

    #[test]
    fn duckduckgo_page() {
        let page = r#"<div class="result results_links"><h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fshop.example%2Ffilters&rut=abc">Paper Filters</a></h2><a class="result__snippet">350 count, official</a></div>
        <div class="result result--ad"><a class="result__a" href="https://ads.example">Ad</a></div>"#;
        let r = Provider::DuckDuckGo.parse(page, 5).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://shop.example/filters");
        assert_eq!(r[0].snippet, "350 count, official");
    }

    #[test]
    fn requests_carry_the_key_only_to_brave() {
        let (u, h) = Provider::Brave { key: "k1".into() }
            .request("aeropress filters", 5)
            .unwrap();
        assert!(
            u.starts_with(BRAVE_URL) && u.contains("q=aeropress+filters"),
            "{u}"
        );
        assert!(
            h.iter()
                .any(|(k, v)| k == "X-Subscription-Token" && v == "k1")
        );
        let (u, h) = Provider::DuckDuckGo.request("a b", 5).unwrap();
        assert!(u.starts_with(DUCKDUCKGO_URL));
        assert!(h.is_empty());
    }
}
