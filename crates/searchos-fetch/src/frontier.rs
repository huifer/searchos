//! RFC-051: crawl frontier — polite scheduling of many fetches across hosts.
//!
//! The frontier is a bounded, per-host FIFO with cross-host fairness:
//! - Every host has its own queue, so one slow or rate-limited host can
//!   never starve the others (host fairness).
//! - Within a host, URLs leave in the order they arrived (deterministic).
//! - [`FetchFrontier::note_fetch`] records the last fetch per host and the
//!   interval to honour next time (robots `Crawl-delay` when known, else the
//!   configured default), so repeated bursts against one host pace out.
//!
//! `pop` takes an explicit `now_ms` so scheduling decisions are testable
//! without sleeping. When nothing is ready, `pop` returns `None` and
//! [`FetchFrontier::next_ready_ms`] reports when to wake up — callers sleep
//! between ticks instead of busy-looping.

use std::collections::{BTreeMap, VecDeque};

/// Default minimum interval between two fetches of the same host when no
/// robots Crawl-delay is known (250 ms).
pub const DEFAULT_HOST_INTERVAL_MS: u64 = 250;

/// Maximum URLs retained per host. Pushing beyond the cap drops the NEWEST
/// urls and reports it, so a burst against one host cannot exhaust memory.
pub const DEFAULT_MAX_PER_HOST: usize = 256;
/// Global queue bound, independent of the number of hosts.
pub const DEFAULT_MAX_QUEUED: usize = 4_096;
/// Host-state bound, including hosts whose queues have drained.
pub const DEFAULT_MAX_HOSTS: usize = 1_024;

/// A queued url plus its enqueue sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedUrl {
    seq: u64,
    input_index: usize,
    url: String,
    priority: u8,
}

/// Per-host scheduling state.
#[derive(Debug, Default)]
struct HostState {
    queue: VecDeque<QueuedUrl>,
    /// Monotonic ms of the last accepted fetch for this host.
    last_fetch_ms: Option<u64>,
    /// Interval to honour after the last fetch.
    interval_ms: u64,
    /// URLs dropped because the per-host cap was hit.
    dropped: u64,
}

/// Politeness-first, bounded, cross-host fetch frontier (RFC-051).
#[derive(Debug)]
pub struct FetchFrontier {
    hosts: BTreeMap<String, HostState>,
    seen: std::collections::HashSet<String>,
    next_seq: u64,
    max_per_host: usize,
    max_queued: usize,
    max_hosts: usize,
    default_interval_ms: u64,
}

/// One url ready for fetching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierPop {
    pub url: String,
    /// Original position supplied by a batch caller.
    pub input_index: usize,
    /// Host key the url was queued under.
    pub host: String,
    pub priority: u8,
}

/// Why a `pop` returned nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierIdle {
    /// Nothing queued anywhere.
    Empty,
    /// Queued urls exist; `wait_ms` is the earliest time one becomes ready.
    NotYetReady { wait_ms: u64 },
}

/// Result of a `pop` attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontierNext {
    Ready(FrontierPop),
    Idle(FrontierIdle),
}

/// Outcome of a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierPush {
    Accepted,
    /// Already queued or fetched earlier — deduplicated.
    Duplicate,
    /// Dropped: the per-host cap was hit.
    OverCap,
    /// URL has no valid HTTP(S) authority.
    InvalidUrl,
}

/// Failure of `host_of`: an unusable url (no scheme, no host).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUrl;

impl FetchFrontier {
    pub fn new() -> Self {
        Self::with_config(DEFAULT_HOST_INTERVAL_MS, DEFAULT_MAX_PER_HOST)
    }

    pub fn with_config(default_interval_ms: u64, max_per_host: usize) -> Self {
        Self::with_limits(
            default_interval_ms,
            max_per_host,
            DEFAULT_MAX_QUEUED,
            DEFAULT_MAX_HOSTS,
        )
    }

    pub fn with_limits(
        default_interval_ms: u64,
        max_per_host: usize,
        max_queued: usize,
        max_hosts: usize,
    ) -> Self {
        Self {
            hosts: BTreeMap::new(),
            seen: std::collections::HashSet::new(),
            next_seq: 0,
            max_per_host: max_per_host.max(1),
            max_queued: max_queued.max(1),
            max_hosts: max_hosts.max(1),
            default_interval_ms: default_interval_ms.max(1),
        }
    }

    /// Host key of a url: lowercase `scheme://host` authority without
    /// credentials, port, path or query. Returns [`InvalidUrl`] when the url
    /// has no usable authority.
    pub fn host_of(url: &str) -> Result<String, InvalidUrl> {
        let parsed = url::Url::parse(url).map_err(|_| InvalidUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(InvalidUrl);
        }
        parsed
            .host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .ok_or(InvalidUrl)
    }

    fn host_state(&mut self, host: &str) -> &mut HostState {
        self.hosts.entry(host.to_string()).or_default()
    }

    /// Queue a url. The per-host cap drops the NEWEST urls (oldest-first
    /// service), and exact duplicates are skipped.
    pub fn push(&mut self, url: &str, priority: u8) -> FrontierPush {
        self.push_indexed(url, priority, self.next_seq as usize)
    }

    /// Queue a URL while retaining its caller input position.
    pub fn push_indexed(&mut self, url: &str, priority: u8, input_index: usize) -> FrontierPush {
        let host = match Self::host_of(url) {
            Ok(h) => h,
            Err(_) => return FrontierPush::InvalidUrl,
        };
        if self.seen.contains(url) {
            return FrontierPush::Duplicate;
        }
        if self.total_queued() >= self.max_queued
            || (!self.hosts.contains_key(&host) && self.hosts.len() >= self.max_hosts)
        {
            return FrontierPush::OverCap;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let cap = self.max_per_host;
        let state = self.host_state(&host);
        if state.queue.len() >= cap {
            state.dropped += 1;
            self.seen.remove(url);
            return FrontierPush::OverCap;
        }
        state.queue.push_back(QueuedUrl {
            seq,
            input_index,
            url: url.to_string(),
            priority,
        });
        self.seen.insert(url.to_string());
        FrontierPush::Accepted
    }

    /// Is this host's cooldown over at `now_ms`?
    fn host_ready(state: &HostState, now_ms: u64) -> bool {
        match state.last_fetch_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= state.interval_ms,
        }
    }

    /// Earliest ms offset (>= 0) until ANY queued url becomes ready.
    pub fn next_ready_ms(&self, now_ms: u64) -> Option<u64> {
        let mut earliest: Option<u64> = None;
        for (host, state) in &self.hosts {
            if state.queue.is_empty() {
                continue;
            }
            let wait = match state.last_fetch_ms {
                None => 0,
                Some(last) => state
                    .interval_ms
                    .saturating_sub(now_ms.saturating_sub(last)),
            };
            let ready_at = now_ms.saturating_add(wait);
            let _ = host;
            earliest = Some(match earliest {
                None => ready_at,
                Some(e) => e.min(ready_at),
            });
        }
        earliest.map(|at| at.saturating_sub(now_ms))
    }

    /// Take the next url: among hosts that are READY at `now_ms`, the host
    /// whose head has the oldest sequence number wins. This is deterministic
    /// FIFO service across hosts; priority is returned for downstream policy.
    pub fn pop(&mut self, now_ms: u64) -> FrontierNext {
        let mut best: Option<(String, u64, u8)> = None;
        for (host, state) in &self.hosts {
            let Some(head) = state.queue.front() else {
                continue;
            };
            if !Self::host_ready(state, now_ms) {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, best_seq, _)) => head.seq < best_seq,
            };
            if better {
                best = Some((host.clone(), head.seq, head.priority));
            }
        }
        let Some((host, _, _)) = best else {
            let idle = if self.is_empty() {
                FrontierIdle::Empty
            } else {
                FrontierIdle::NotYetReady {
                    wait_ms: self.next_ready_ms(now_ms).unwrap_or(0),
                }
            };
            return FrontierNext::Idle(idle);
        };
        let state = self.hosts.get_mut(&host).expect("host exists");
        let queued = state.queue.pop_front().expect("front exists");
        self.seen.remove(&queued.url);
        FrontierNext::Ready(FrontierPop {
            url: queued.url,
            input_index: queued.input_index,
            host,
            priority: queued.priority,
        })
    }

    /// Record a completed (or attempted) fetch: sets the host's last-fetch
    /// time and the interval to honour (robots Crawl-delay when known).
    pub fn note_fetch(&mut self, url: &str, now_ms: u64, interval_ms: Option<u64>) {
        let host = match Self::host_of(url) {
            Ok(h) => h,
            Err(_) => return,
        };
        let default_interval = self.default_interval_ms;
        let state = self.host_state(&host);
        state.last_fetch_ms = Some(now_ms);
        if let Some(ms) = interval_ms {
            state.interval_ms = ms.max(1);
        } else if state.interval_ms == 0 {
            state.interval_ms = default_interval;
        }
    }

    /// Declare a crawl-delay for a host up front (e.g. learned from robots
    /// before the first fetch). Clamped to >= 1 ms.
    pub fn set_host_interval(&mut self, host: &str, interval_ms: u64) {
        let state = self.host_state(host);
        state.interval_ms = interval_ms.max(1);
    }

    /// Number of urls currently queued for a host.
    pub fn queued_for_host(&self, host: &str) -> usize {
        self.hosts.get(host).map(|s| s.queue.len()).unwrap_or(0)
    }

    /// URLs dropped because the per-host cap was hit.
    pub fn dropped_for_host(&self, host: &str) -> u64 {
        self.hosts.get(host).map(|s| s.dropped).unwrap_or(0)
    }

    /// Total queued across all hosts.
    pub fn total_queued(&self) -> usize {
        self.hosts.values().map(|s| s.queue.len()).sum()
    }

    /// True when nothing is queued anywhere.
    pub fn is_empty(&self) -> bool {
        self.hosts.values().all(|s| s.queue.is_empty())
    }
}

impl Default for FetchFrontier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_is_lowercased_and_handles_authorities() {
        assert_eq!(
            FetchFrontier::host_of("HTTPS://Example.COM:8443/a/b?q=1#f"),
            Ok("example.com".into())
        );
        assert_eq!(
            FetchFrontier::host_of("https://user:pw@host.example/x"),
            Ok("host.example".into())
        );
        assert_eq!(
            FetchFrontier::host_of("http://[::1]:8080/x"),
            Ok("[::1]".into())
        );
        assert_eq!(FetchFrontier::host_of("notaurl"), Err(InvalidUrl));
        assert_eq!(FetchFrontier::host_of("https://"), Err(InvalidUrl));
        assert_eq!(
            FetchFrontier::host_of("file:///etc/passwd"),
            Err(InvalidUrl)
        );
    }

    #[test]
    fn same_host_urls_leave_in_arrival_order() {
        let mut f = FetchFrontier::with_config(0, 16);
        f.push("https://a.example/1", 0);
        f.push("https://a.example/2", 0);
        f.push("https://a.example/3", 0);
        assert_eq!(f.queued_for_host("a.example"), 3);
        let first = f.pop(0);
        assert_eq!(
            first,
            FrontierNext::Ready(FrontierPop {
                url: "https://a.example/1".into(),
                input_index: 0,
                host: "a.example".into(),
                priority: 0,
            })
        );
    }

    #[test]
    fn one_cooling_host_never_blocks_the_other() {
        let mut f = FetchFrontier::with_config(1_000, 16);
        f.set_host_interval("slow.example", 1_000);
        f.set_host_interval("fast.example", 0);
        f.push("https://slow.example/1", 0);
        f.push("https://fast.example/1", 0);
        // slow is ready at t=0 (never fetched).
        let first = match f.pop(0) {
            FrontierNext::Ready(p) => p,
            other => panic!("expected ready, got {other:?}"),
        };
        assert_eq!(first.host, "slow.example");
        f.note_fetch(&first.url, 0, Some(1_000));
        // fast must still be served while slow cools down.
        let second = match f.pop(0) {
            FrontierNext::Ready(p) => p,
            FrontierNext::Idle(other) => panic!("fast host starved by slow host: {other:?}"),
        };
        assert_eq!(second.host, "fast.example");
        f.note_fetch(&second.url, 0, None);
        // Both hosts cooling now, but slow has another queued url -> idle
        // with a wake time (not Empty).
        f.push("https://slow.example/2", 0);
        assert_eq!(
            f.pop(0),
            FrontierNext::Idle(FrontierIdle::NotYetReady { wait_ms: 1_000 })
        );
        // At t=1_000 the slow host's cooldown has elapsed.
        assert!(matches!(f.pop(1_000), FrontierNext::Ready(_)));
    }

    #[test]
    fn crawl_delay_paces_repeated_bursts_against_one_host() {
        let mut f = FetchFrontier::with_config(250, 16);
        f.push("https://a.example/1", 0);
        f.push("https://a.example/2", 0);
        let pop = match f.pop(0) {
            FrontierNext::Ready(p) => p,
            other => panic!("expected ready, got {other:?}"),
        };
        f.note_fetch(&pop.url, 0, Some(500));
        assert_eq!(
            f.pop(0),
            FrontierNext::Idle(FrontierIdle::NotYetReady { wait_ms: 500 })
        );
        // At t=499 still too soon; at t=500 the second url is due.
        assert_eq!(
            f.pop(499),
            FrontierNext::Idle(FrontierIdle::NotYetReady { wait_ms: 1 })
        );
        assert!(matches!(f.pop(500), FrontierNext::Ready(_)));
    }

    #[test]
    fn default_interval_applies_when_robots_delay_is_unknown() {
        let mut f = FetchFrontier::with_config(250, 16);
        f.push("https://a.example/1", 0);
        f.push("https://a.example/2", 0);
        let pop = match f.pop(10) {
            FrontierNext::Ready(p) => p,
            other => panic!("expected ready, got {other:?}"),
        };
        f.note_fetch(&pop.url, 10, None);
        assert_eq!(
            f.pop(10),
            FrontierNext::Idle(FrontierIdle::NotYetReady { wait_ms: 250 })
        );
    }

    #[test]
    fn duplicates_are_deduplicated_even_after_per_host_cap_drops() {
        let mut f = FetchFrontier::with_config(0, 2);
        assert_eq!(f.push("https://a.example/1", 0), FrontierPush::Accepted);
        assert_eq!(f.push("https://a.example/1", 0), FrontierPush::Duplicate);
        assert_eq!(f.push("https://a.example/2", 0), FrontierPush::Accepted);
        assert_eq!(f.push("https://a.example/3", 0), FrontierPush::OverCap);
        assert_eq!(f.push("file:///etc/passwd", 0), FrontierPush::InvalidUrl);
        assert_eq!(f.dropped_for_host("a.example"), 1);
        assert_eq!(f.total_queued(), 2);
    }

    #[test]
    fn older_ready_url_wins_without_priority_starvation() {
        let mut f = FetchFrontier::with_config(0, 16);
        f.push("https://a.example/older", 0);
        f.push("https://b.example/newer", 9);
        let pop = match f.pop(0) {
            FrontierNext::Ready(p) => p,
            other => panic!("expected ready, got {other:?}"),
        };
        assert_eq!(pop.url, "https://a.example/older");
    }

    #[test]
    fn global_queue_and_host_state_are_bounded() {
        let mut f = FetchFrontier::with_limits(1, 2, 2, 1);
        assert_eq!(f.push("https://a.example/1", 0), FrontierPush::Accepted);
        assert_eq!(f.push("https://a.example/2", 0), FrontierPush::Accepted);
        assert_eq!(f.push("https://b.example/1", 0), FrontierPush::OverCap);
        assert_eq!(f.push("https://a.example/3", 0), FrontierPush::OverCap);
        assert_eq!(f.total_queued(), 2);
    }

    #[test]
    fn empty_frontier_reports_empty_not_not_ready() {
        let mut f = FetchFrontier::new();
        assert_eq!(f.pop(0), FrontierNext::Idle(FrontierIdle::Empty));
        f.push("https://a.example/1", 0);
        f.pop(0);
        assert!(f.is_empty());
        assert_eq!(
            f.push("https://a.example/1", 0),
            FrontierPush::Accepted,
            "the frontier deduplicates pending URLs, not historical crawl state"
        );
    }
}
