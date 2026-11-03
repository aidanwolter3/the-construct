//! Outbound HTTP to hosts a user picked (D45, D47): a typed Pixelfed
//! instance, a device's Web Push endpoint. Such a host must not reach the
//! pod's own network, so names resolve only to public addresses
//! ([`PublicOnlyResolver`]) and redirects go only to public `https://` names.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;

const USER_AGENT: &str = concat!("construct-pod/", env!("CARGO_PKG_VERSION"));
const MAX_REDIRECTS: usize = 5;

/// A public-looking host name: a dotted DNS name (no IP literal, no
/// `localhost`, `.local`, `.internal`, ...). The DNS answer is checked at
/// connect time too ([`PublicOnlyResolver`]).
pub fn is_public_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return false;
    }
    // A numeric last label is an IP address in some spelling (`0x7f.1`).
    let tld = labels[labels.len() - 1];
    if tld.bytes().all(|b| b.is_ascii_digit()) || tld.starts_with("0x") {
        return false;
    }
    !matches!(
        tld,
        "localhost" | "local" | "internal" | "lan" | "home" | "intranet" | "corp" | "arpa"
    )
}

/// An `https://<public host>` URL (default port; no IP literal).
pub fn is_public_https(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.port().is_none() && url.host_str().is_some_and(is_public_host)
}

/// A globally routable unicast address (not private, loopback, link-local,
/// CGNAT, ULA, documentation, multicast, ...).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local (fc00::/7, e.g. Fly's fdaa::)
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64: embeds an IPv4 address
                || s[0] == 0 // ::/16 (IPv4-compatible and other reserved)
                || s[0] == 0x2002) // 6to4: embeds an IPv4 address
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18/15
        || a >= 240) // reserved
}

/// The system resolver, keeping only public addresses: a name a user typed
/// must not reach the pod's own network.
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                tracing::warn!(%host, "refused: the name has no public address");
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// A client builder with the pod's user agent and timeouts.
pub fn builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(USER_AGENT)
}

/// [`builder`] for user-picked hosts: public addresses only, no proxy (a
/// proxy resolves names itself, past [`PublicOnlyResolver`]), redirects only
/// to public `https://` names.
pub fn public_builder(timeout: Duration) -> reqwest::ClientBuilder {
    builder(timeout)
        .no_proxy()
        .dns_resolver(Arc::new(PublicOnlyResolver))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if is_public_https(attempt.url()) {
                attempt.follow()
            } else {
                tracing::warn!(host = ?attempt.url().host_str(), "redirect to a non-public URL refused");
                attempt.stop()
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_hosts() {
        for ok in ["social.example", "a.b.example.com", "push.example.org."] {
            assert!(is_public_host(ok), "{ok}");
        }
        for bad in [
            "localhost",
            "x.local",
            "10.0.0.1",
            "[::1]",
            "host",
            "a.internal",
            "0x7f.1",
            "1.2.3.4",
        ] {
            assert!(!is_public_host(bad), "{bad}");
        }
    }

    #[test]
    fn public_ips() {
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip("fdaa::1".parse().unwrap()));
        assert!(!is_public_ip("::ffff:127.0.0.1".parse().unwrap()));
    }
}
