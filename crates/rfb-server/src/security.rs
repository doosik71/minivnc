//! Security types and the brute-force blocker, following TurboVNC `auth.c`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use rfb_proto::security::{Method, SecurityType, Tls, parse_security_types};

/// TurboVNC's blocking of hosts that keep failing VNC authentication:
/// after `max_fails` failures, a host is refused for `timeout`, doubled for
/// each further failure, until it authenticates successfully.
pub(crate) struct Blocker {
    max_fails: u32,
    timeout: Duration,
    hosts: Mutex<HashMap<IpAddr, Failures>>,
}

struct Failures {
    count: u32,
    blocked_until: Option<Instant>,
}

impl Blocker {
    pub fn new(max_fails: u32, timeout: Duration) -> Self {
        Self {
            max_fails,
            timeout,
            hosts: Mutex::default(),
        }
    }

    fn hosts(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Failures>> {
        self.hosts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `rfbAuthIsBlocked`
    pub fn is_blocked(&self, host: IpAddr, now: Instant) -> bool {
        self.max_fails > 0
            && self
                .hosts()
                .get(&host)
                .and_then(|f| f.blocked_until)
                .is_some_and(|until| now < until)
    }

    /// `rfbAuthConsiderBlocking`: counts a failure; returns whether the host
    /// is now blocked.
    pub fn failed(&self, host: IpAddr, now: Instant) -> bool {
        if self.max_fails == 0 {
            return false;
        }
        let mut hosts = self.hosts();
        let f = hosts.entry(host).or_insert(Failures {
            count: 0,
            blocked_until: None,
        });
        f.count += 1;
        if f.count < self.max_fails {
            return false;
        }
        let doublings = (f.count - self.max_fails).min(16);
        f.blocked_until = Some(now + self.timeout * 2u32.pow(doublings));
        true
    }

    /// `rfbAuthUnblock`: a successful authentication forgets the failures.
    pub fn succeeded(&self, host: IpAddr) {
        self.hosts().remove(&host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_doubles() {
        let b = Blocker::new(5, Duration::from_secs(10));
        let host: IpAddr = [10, 0, 0, 1].into();
        let t0 = Instant::now();
        for _ in 0..4 {
            assert!(!b.failed(host, t0));
        }
        assert!(!b.is_blocked(host, t0));
        assert!(b.failed(host, t0));
        assert!(b.is_blocked(host, t0 + Duration::from_secs(9)));
        assert!(!b.is_blocked(host, t0 + Duration::from_secs(10)));
        // The sixth failure blocks for 20 s.
        let t1 = t0 + Duration::from_secs(11);
        assert!(b.failed(host, t1));
        assert!(b.is_blocked(host, t1 + Duration::from_secs(19)));
        assert!(!b.is_blocked(host, t1 + Duration::from_secs(20)));
        // Other hosts are not affected; success forgets the failures.
        assert!(!b.is_blocked([10, 0, 0, 2].into(), t1));
        b.succeeded(host);
        assert!(!b.is_blocked(host, t1));
        assert!(!b.failed(host, t1));
    }
}
