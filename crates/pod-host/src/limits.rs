//! Fixed-window rate limits per key (a client address), in memory, with a
//! bounded number of keys.
//!
//! A key's window is never dropped before it ends: when the table is full of
//! live windows a new key is refused (fails closed) rather than evicting
//! someone's count, so cycling through many addresses can't reset one's own
//! (decision `pod-storage-limits`). Callers pair a narrow key with a wide one
//! ([`TwoLevel`]) so one network can't fill the table alone.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Keys tracked at most.
pub const MAX_KEYS: usize = 10_000;

pub struct RateLimit {
    max: u32,
    window: Duration,
    max_keys: usize,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimit {
    pub fn new(max: u32, window: Duration) -> Self {
        Self::with_capacity(max, window, MAX_KEYS)
    }

    /// [`Self::new`] tracking at most `max_keys` keys (tests).
    pub fn with_capacity(max: u32, window: Duration, max_keys: usize) -> Self {
        Self {
            max,
            window,
            max_keys,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Count one attempt for `key`; false when over the limit, or when `key`
    /// is new and every slot holds a live window.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if hits.len() >= self.max_keys && !hits.contains_key(key) {
            let window = self.window;
            hits.retain(|_, (start, _)| now.duration_since(*start) < window);
            if hits.len() >= self.max_keys {
                return false;
            }
        }
        let entry = hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= self.max
    }
}

/// A limit per narrow key (an IPv4 address, an IPv6 /64) under a looser one
/// per wide key (an IPv6 /48). A request over the wide limit is not counted
/// against the narrow one, so one /48 adds a bounded number of narrow keys.
pub struct TwoLevel {
    pub narrow: RateLimit,
    pub wide: RateLimit,
}

impl TwoLevel {
    /// Pod codes typed (`/v1/pods/open`, the OAuth consent form): 10 a
    /// minute per IPv4 address or IPv6 /64, 50 per /48. No global limit: a
    /// 256-bit code can't be guessed, and a global one lets anyone lock
    /// everyone out of entering their code. The /48 limit keeps one network
    /// from filling the table with /64s, which would do the same.
    pub fn code_tries() -> Self {
        Self {
            narrow: RateLimit::new(10, Duration::from_secs(60)),
            wide: RateLimit::new(50, Duration::from_secs(60)),
        }
    }

    /// OAuth client registrations: 20 an hour per IPv4 address or IPv6
    /// /64, 40 per /48.
    pub fn registrations() -> Self {
        Self {
            narrow: RateLimit::new(20, Duration::from_secs(3600)),
            wide: RateLimit::new(40, Duration::from_secs(3600)),
        }
    }

    /// Temporary pods: 5 an hour per IPv4 address or IPv6 /64, 10 per /48
    /// (security audit #31, M2: a /48 holds 65536 /64s, and a few are
    /// enough to take every temporary pod).
    pub fn temp_creates() -> Self {
        Self {
            narrow: RateLimit::new(5, Duration::from_secs(3600)),
            wide: RateLimit::new(10, Duration::from_secs(3600)),
        }
    }

    pub fn allow(&self, keys: &ClientKeys) -> bool {
        if let Some(wide) = &keys.wide
            && !self.wide.allow(wide)
        {
            return false;
        }
        self.narrow.allow(&keys.narrow)
    }
}

/// The keys of one client address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeys {
    /// The IPv4 address, or the IPv6 /64.
    pub narrow: String,
    /// The IPv6 /48 (none for IPv4: one address is already scarce).
    pub wide: Option<String>,
}

impl ClientKeys {
    pub fn of(ip: Option<std::net::IpAddr>) -> Self {
        match ip {
            Some(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::v4(v4),
                None => {
                    let s = v6.segments();
                    Self {
                        narrow: format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3]),
                        wide: Some(format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])),
                    }
                }
            },
            Some(std::net::IpAddr::V4(v4)) => Self::v4(v4),
            None => Self {
                narrow: "unknown".to_owned(),
                wide: None,
            },
        }
    }

    fn v4(v4: std::net::Ipv4Addr) -> Self {
        Self {
            narrow: v4.to_string(),
            wide: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key_and_window() {
        let l = RateLimit::new(2, Duration::from_millis(50));
        assert!(l.allow("a") && l.allow("a"));
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(l.allow("a"));
    }

    #[test]
    fn the_key_count_is_bounded() {
        let l = RateLimit::new(1, Duration::from_secs(60));
        for i in 0..MAX_KEYS + 50 {
            l.allow(&i.to_string());
        }
        assert!(l.hits.lock().unwrap().len() <= MAX_KEYS);
    }

    /// Security audit #31 (M2): cycling through new keys used to evict the
    /// oldest window, resetting the attacker's own count.
    #[test]
    fn new_keys_cannot_evict_a_live_window() {
        let l = RateLimit::with_capacity(2, Duration::from_secs(60), 3);
        assert!(l.allow("me") && l.allow("me"));
        assert!(!l.allow("me"));
        assert!(l.allow("x1") && l.allow("x2"));
        // Full of live windows: new keys fail closed, "me" stays counted.
        for i in 0..10 {
            assert!(!l.allow(&format!("y{i}")));
        }
        assert!(!l.allow("me"));
        // A known key still gets its own answer.
        assert!(l.allow("x1"));
    }

    #[test]
    fn expired_windows_make_room() {
        let l = RateLimit::with_capacity(1, Duration::from_millis(40), 2);
        assert!(l.allow("a") && l.allow("b"));
        assert!(!l.allow("c"));
        std::thread::sleep(Duration::from_millis(50));
        assert!(l.allow("c"));
        assert!(l.hits.lock().unwrap().len() <= 2);
    }

    #[test]
    fn a_48_is_limited_across_its_64s() {
        let l = TwoLevel {
            narrow: RateLimit::new(2, Duration::from_secs(60)),
            wide: RateLimit::new(4, Duration::from_secs(60)),
        };
        let key = |ip: &str| ClientKeys::of(Some(ip.parse().unwrap()));
        let a = key("2001:db8:1:1::1");
        assert_eq!(a.narrow, "2001:db8:1:1::/64");
        assert_eq!(a.wide.as_deref(), Some("2001:db8:1::/48"));
        assert!(l.allow(&a) && l.allow(&a));
        assert!(!l.allow(&a), "the /64's limit");
        // The /48 counted all three; another /64 of it gets one more.
        assert!(l.allow(&key("2001:db8:1:2::1")));
        assert!(!l.allow(&key("2001:db8:1:3::1")));
        // Another /48 is not affected.
        assert!(l.allow(&key("2001:db8:2:1::1")));
        // IPv4 (also mapped) has no wide key.
        assert_eq!(key("::ffff:198.51.100.1"), key("198.51.100.1"));
        assert_eq!(key("198.51.100.1").wide, None);
    }

    /// One /48 cycling through its 65536 /64s used to fill the pod-code and
    /// registration tables, so every new client was refused.
    #[test]
    fn one_48_cannot_lock_everyone_out() {
        for l in [
            TwoLevel::code_tries(),
            TwoLevel::registrations(),
            TwoLevel::temp_creates(),
        ] {
            for i in 0..=MAX_KEYS {
                let ip = format!("2001:db8:1:{i:x}::1");
                l.allow(&ClientKeys::of(Some(ip.parse().unwrap())));
            }
            assert!(
                l.allow(&ClientKeys::of(Some("198.51.100.1".parse().unwrap()))),
                "a new client is still let in"
            );
            assert!(l.allow(&ClientKeys::of(Some("2001:db8:2::1".parse().unwrap()))));
        }
    }
}
