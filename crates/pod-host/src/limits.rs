//! Fixed-window rate limits per key (a client address), in memory, with a
//! bounded number of keys.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Keys tracked at most; past that the oldest window is forgotten.
const MAX_KEYS: usize = 10_000;

pub struct RateLimit {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimit {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Count one attempt for `key`; false when over the limit.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if hits.len() >= MAX_KEYS && !hits.contains_key(key) {
            let window = self.window;
            hits.retain(|_, (start, _)| now.duration_since(*start) < window);
            if hits.len() >= MAX_KEYS
                && let Some(oldest) = hits
                    .iter()
                    .min_by_key(|(_, (start, _))| *start)
                    .map(|(k, _)| k.clone())
            {
                hits.remove(&oldest);
            }
        }
        let entry = hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max
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
}
