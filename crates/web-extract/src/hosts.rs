//! One policy per host, pod-wide (decision `web-and-agent-modules`): every
//! request the pod makes to a website goes through [`Hosts::admit`], whatever
//! asked for it (a filler, a preview, an agent's fetch tool, a search).
//!
//! - **Spacing:** at least [`SPACING`] between two requests to one host.
//! - **A daily cap per host:** [`DAILY_CAP`], lower for search pages scraped
//!   without an API ([`SEARCH_PAGE_CAP`]), so no single feature can hammer a
//!   site.
//! - **A shared backoff:** when a host refuses the pod as an automated
//!   visitor ([`crate::fetch::FetchError::blocked`]), every caller stays away
//!   from it until the backoff ends (an hour, doubling to a day, at least
//!   its `Retry-After`).
//!
//! The state is the pod's own (`Pod::local`), in memory: a restart forgets
//! it, which only means one more try.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Least time between two requests to one host.
pub const SPACING: Duration = Duration::from_secs(1);
/// Requests per host per day.
pub const DAILY_CAP: u32 = 500;
/// Requests per day to a search engine's HTML page (no API: it blocks
/// automated visitors quickly).
pub const SEARCH_PAGE_CAP: u32 = 20;
/// The first backoff after a block; it doubles per block in a row.
pub const FIRST_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// The longest backoff.
pub const MAX_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);

/// Hosts whose pages are search results scraped without an API.
const SEARCH_PAGES: &[&str] = &[
    "html.duckduckgo.com",
    "duckduckgo.com",
    "lite.duckduckgo.com",
];

/// The daily cap for `host`.
pub fn daily_cap(host: &str) -> u32 {
    if SEARCH_PAGES.contains(&host) {
        SEARCH_PAGE_CAP
    } else {
        DAILY_CAP
    }
}

#[derive(Debug, Clone)]
struct HostState {
    last: Instant,
    day_start: Instant,
    count: u32,
    blocked_until: Option<Instant>,
    blocks_in_a_row: u32,
}

/// The pod's host policy state (`Pod::local::<Hosts>()`).
#[derive(Debug, Default)]
pub struct Hosts(tokio::sync::Mutex<HashMap<String, HostState>>);

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

impl Hosts {
    /// Wait for `url`'s host to be due and count the request; `Err` (a
    /// sentence for a filler's status) while the host is backed off or its
    /// daily cap is spent.
    pub async fn admit(&self, url: &str) -> Result<(), String> {
        let host = crate::fetch::host_of(url);
        let wait = {
            let mut m = self.0.lock().await;
            let now = Instant::now();
            let st = m.entry(host.clone()).or_insert(HostState {
                last: now - SPACING,
                day_start: now,
                count: 0,
                blocked_until: None,
                blocks_in_a_row: 0,
            });
            if let Some(until) = st.blocked_until {
                if until > now {
                    let mins = (until - now).as_secs().div_ceil(60);
                    return Err(format!(
                        "{host} refused automated visits; the pod waits {mins} more minutes before trying it again"
                    ));
                }
                st.blocked_until = None;
            }
            if now.duration_since(st.day_start) >= DAY {
                st.day_start = now;
                st.count = 0;
            }
            let cap = daily_cap(&host);
            if st.count >= cap {
                return Err(format!(
                    "the pod already sent {cap} requests to {host} today (its daily limit for that site)"
                ));
            }
            st.count += 1;
            let at = (st.last + SPACING).max(now);
            st.last = at;
            at.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        Ok(())
    }

    /// `url`'s host refused the pod: back off (at least `retry_after`
    /// seconds).
    pub async fn blocked(&self, url: &str, retry_after: Option<i64>) {
        let host = crate::fetch::host_of(url);
        let mut m = self.0.lock().await;
        let now = Instant::now();
        let st = m.entry(host).or_insert(HostState {
            last: now,
            day_start: now,
            count: 0,
            blocked_until: None,
            blocks_in_a_row: 0,
        });
        let backoff = FIRST_BACKOFF
            .saturating_mul(2u32.saturating_pow(st.blocks_in_a_row))
            .min(MAX_BACKOFF)
            .max(Duration::from_secs(retry_after.unwrap_or(0).max(0) as u64));
        st.blocks_in_a_row = st.blocks_in_a_row.saturating_add(1);
        st.blocked_until = Some(now + backoff);
    }

    /// `url`'s host answered: a block in a row ends.
    pub async fn answered(&self, url: &str) {
        let host = crate::fetch::host_of(url);
        if let Some(st) = self.0.lock().await.get_mut(&host) {
            st.blocks_in_a_row = 0;
        }
    }

    /// Whether `url`'s host is backed off now.
    pub async fn is_blocked(&self, url: &str) -> bool {
        let host = crate::fetch::host_of(url);
        self.0
            .lock()
            .await
            .get(&host)
            .and_then(|s| s.blocked_until)
            .is_some_and(|u| u > Instant::now())
    }
}

/// Fetch `url` through the pod's host policy: admitted first, a block
/// recorded after.
pub async fn fetch(
    hosts: &Hosts,
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    headers: &[(String, String)],
) -> Result<crate::fetch::Fetched, crate::fetch::FetchError> {
    hosts
        .admit(url)
        .await
        .map_err(|m| crate::fetch::FetchError {
            message: m,
            retry_after: None,
            blocked: true,
        })?;
    let r = crate::fetch::fetch(client, url, etag, last_modified, headers).await;
    match &r {
        Err(e) if e.blocked => hosts.blocked(url, e.retry_after).await,
        Ok(_) => hosts.answered(url).await,
        Err(_) => {}
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_blocked_host_waits_and_others_do_not() {
        let h = Hosts::default();
        h.admit("https://a.example/1").await.unwrap();
        h.blocked("https://a.example/2", None).await;
        let e = h.admit("https://a.example/3").await.unwrap_err();
        assert!(e.contains("a.example"), "{e}");
        assert!(h.is_blocked("https://a.example/").await);
        h.admit("https://b.example/").await.unwrap();
    }

    #[tokio::test]
    async fn search_pages_have_a_small_daily_cap() {
        let h = Hosts::default();
        {
            let mut m = h.0.lock().await;
            let now = Instant::now();
            m.insert(
                "html.duckduckgo.com".into(),
                HostState {
                    last: now - SPACING,
                    day_start: now,
                    count: SEARCH_PAGE_CAP,
                    blocked_until: None,
                    blocks_in_a_row: 0,
                },
            );
        }
        let e = h
            .admit("https://html.duckduckgo.com/html/?q=x")
            .await
            .unwrap_err();
        assert!(e.contains("daily limit"), "{e}");
    }
}
