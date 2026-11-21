//! Caps on open event streams (`text/event-stream` answers), host-wide.
//!
//! A pod caps streams per session token
//! ([`pod_core::AppState::sse_streams_per_token`]), but anyone can make a
//! temporary pod and a browser session, so the host also caps them per
//! client address, per pod, for temporary pods together and in total. The
//! total stays under the proxy's connection limit (Fly's `hard_limit`), and
//! temporary pods get only part of it, so visitors can't take every stream
//! from admin pods.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The caps (`CONSTRUCT_SSE_MAX_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    /// Per client address (an IPv4 address, an IPv6 /64):
    /// `CONSTRUCT_SSE_MAX_PER_IP`, default 24.
    pub per_ip: usize,
    /// Per pod: `CONSTRUCT_SSE_MAX_PER_POD`, default 32.
    pub per_pod: usize,
    /// All temporary pods together: `CONSTRUCT_SSE_MAX_TEMPORARY`, default
    /// 80.
    pub temporary: usize,
    /// All pods together: `CONSTRUCT_SSE_MAX_TOTAL`, default 200 (Fly's
    /// `hard_limit` is 250 connections).
    pub total: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            per_ip: 24,
            per_pod: 32,
            temporary: 80,
            total: 200,
        }
    }
}

impl StreamLimits {
    /// The defaults, overridden by the `CONSTRUCT_SSE_MAX_*` variables.
    pub fn from_env() -> Self {
        let n = |k: &str, default: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| *n >= 1)
                .unwrap_or(default)
        };
        let d = Self::default();
        Self {
            per_ip: n("CONSTRUCT_SSE_MAX_PER_IP", d.per_ip),
            per_pod: n("CONSTRUCT_SSE_MAX_PER_POD", d.per_pod),
            temporary: n("CONSTRUCT_SSE_MAX_TEMPORARY", d.temporary),
            total: n("CONSTRUCT_SSE_MAX_TOTAL", d.total),
        }
    }
}

#[derive(Default)]
struct Open {
    by_ip: HashMap<String, usize>,
    by_pod: HashMap<String, usize>,
    temporary: usize,
    total: usize,
}

/// The open streams, counted against [`StreamLimits`].
pub struct Streams {
    limits: StreamLimits,
    open: Mutex<Open>,
}

/// Which cap refused a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// This client address, or this pod, has its most streams open.
    Caller,
    /// The server (or its temporary pods together) has its most open.
    Server,
}

impl Streams {
    pub fn new(limits: StreamLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            open: Mutex::new(Open::default()),
        })
    }

    /// Take a slot for one stream of `pod` from client `ip`, held until the
    /// slot is dropped.
    pub fn take(
        self: &Arc<Self>,
        ip: &str,
        pod: &str,
        temporary: bool,
    ) -> Result<StreamSlot, Refused> {
        let l = self.limits;
        let mut o = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if o.by_ip.get(ip).copied().unwrap_or(0) >= l.per_ip
            || o.by_pod.get(pod).copied().unwrap_or(0) >= l.per_pod
        {
            return Err(Refused::Caller);
        }
        if o.total >= l.total || (temporary && o.temporary >= l.temporary) {
            return Err(Refused::Server);
        }
        *o.by_ip.entry(ip.to_owned()).or_default() += 1;
        *o.by_pod.entry(pod.to_owned()).or_default() += 1;
        o.total += 1;
        if temporary {
            o.temporary += 1;
        }
        Ok(StreamSlot {
            streams: self.clone(),
            ip: ip.to_owned(),
            pod: pod.to_owned(),
            temporary,
        })
    }

    /// Streams open now.
    pub fn open_count(&self) -> usize {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).total
    }
}

/// One open stream; dropping it frees the slot.
pub struct StreamSlot {
    streams: Arc<Streams>,
    ip: String,
    pod: String,
    temporary: bool,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut o = self.streams.open.lock().unwrap_or_else(|e| e.into_inner());
        let dec = |m: &mut HashMap<String, usize>, k: &str| {
            if let Some(n) = m.get_mut(k) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(k);
                }
            }
        };
        dec(&mut o.by_ip, &self.ip);
        dec(&mut o.by_pod, &self.pod);
        o.total = o.total.saturating_sub(1);
        if self.temporary {
            o.temporary = o.temporary.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(per_ip: usize, per_pod: usize, temporary: usize, total: usize) -> Arc<Streams> {
        Streams::new(StreamLimits {
            per_ip,
            per_pod,
            temporary,
            total,
        })
    }

    #[test]
    fn each_cap_holds_and_a_dropped_slot_frees_its_place() {
        let s = caps(2, 3, 4, 5);
        let a = s.take("ip1", "pod1", false).unwrap();
        let _b = s.take("ip1", "pod1", false).unwrap();
        assert_eq!(s.take("ip1", "pod2", false).err(), Some(Refused::Caller));
        let _c = s.take("ip2", "pod1", false).unwrap();
        assert_eq!(s.take("ip3", "pod1", false).err(), Some(Refused::Caller));
        drop(a);
        let _a = s.take("ip1", "pod2", false).unwrap();
        assert_eq!(s.open_count(), 3);
        let _d = s.take("ip3", "pod3", false).unwrap();
        let _e = s.take("ip4", "pod4", false).unwrap();
        assert_eq!(s.take("ip5", "pod5", false).err(), Some(Refused::Server));
    }

    #[test]
    fn temporary_pods_share_part_of_the_total() {
        let s = caps(10, 10, 2, 4);
        let _a = s.take("ip1", "t1", true).unwrap();
        let _b = s.take("ip2", "t2", true).unwrap();
        assert_eq!(s.take("ip3", "t3", true).err(), Some(Refused::Server));
        // Admin pods still get the rest.
        let _c = s.take("ip3", "admin", false).unwrap();
        let _d = s.take("ip4", "admin", false).unwrap();
        assert_eq!(s.take("ip5", "admin", false).err(), Some(Refused::Server));
    }
}
