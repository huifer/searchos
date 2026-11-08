//! Crawler status & failure isolation (RFC-010 10.6/10.7).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Crawler lifecycle/health status (RFC-010 10.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerStatus {
    Healthy,
    Degraded,
    Unstable,
    Quarantined,
    Disabled,
    Updating,
}

/// Error taxonomy (RFC-010 10.6 错误类型).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerErrorKind {
    DnsError,
    NetworkError,
    Timeout,
    Http403,
    Http404,
    Http429,
    Http5xx,
    RobotsDisallowed,
    SelectorNotFound,
    ContentEmpty,
    ParserError,
    BrowserRenderError,
}

impl CrawlerErrorKind {
    /// From an HTTP status code.
    pub fn from_http_status(status: u16) -> Option<Self> {
        Some(match status {
            403 => Self::Http403,
            404 => Self::Http404,
            429 => Self::Http429,
            s if (500..600).contains(&s) => Self::Http5xx,
            _ => return None,
        })
    }

    /// Whether this error counts toward quarantine.
    pub fn counts_toward_quarantine(&self) -> bool {
        // Selector issues and 4xx content errors signal rule rot; robots and
        // 404 are normal outcomes, not failures (RFC-010 10.7).
        matches!(
            self,
            Self::DnsError
                | Self::NetworkError
                | Self::Timeout
                | Self::Http5xx
                | Self::SelectorNotFound
                | Self::ContentEmpty
                | Self::ParserError
                | Self::BrowserRenderError
        )
    }
}

/// Failure record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlerFailure {
    pub crawler_id: String,
    pub kind: CrawlerErrorKind,
    pub url: String,
    pub detail: String,
    pub at_ms: u64,
}

/// Tracks failures per crawler and applies quarantine thresholds
/// (RFC-010 10.7: 连续失败达到阈值进入 Quarantine).
pub struct FailureTracker {
    state: RwLock<HashMap<String, CrawlerState>>,
    threshold: u32,
    quarantine_duration: Duration,
}

#[derive(Debug, Clone)]
struct CrawlerState {
    status: CrawlerStatus,
    consecutive_failures: u32,
    quarantined_until: Option<Instant>,
    failures: Vec<CrawlerFailure>,
}

impl Default for CrawlerState {
    fn default() -> Self {
        Self {
            status: CrawlerStatus::Healthy,
            consecutive_failures: 0,
            quarantined_until: None,
            failures: Vec::new(),
        }
    }
}

impl FailureTracker {
    /// Tracker with quarantine threshold + duration.
    pub fn new(threshold: u32, quarantine_duration: Duration) -> Self {
        Self {
            state: RwLock::new(HashMap::new()),
            threshold,
            quarantine_duration,
        }
    }

    fn state_of<'a>(
        map: &'a mut HashMap<String, CrawlerState>,
        id: &'a str,
    ) -> &'a mut CrawlerState {
        map.entry(id.to_string()).or_default()
    }

    /// Records a failure; escalates status and quarantines at threshold.
    pub fn record_failure(&self, failure: CrawlerFailure) {
        let mut map = self.state.write().unwrap();
        let st = Self::state_of(&mut map, &failure.crawler_id);
        st.failures.push(failure.clone());
        if failure.kind.counts_toward_quarantine() {
            st.consecutive_failures += 1;
            st.status = match st.consecutive_failures {
                n if n >= self.threshold => {
                    st.quarantined_until = Some(Instant::now() + self.quarantine_duration);
                    CrawlerStatus::Quarantined
                }
                1 => CrawlerStatus::Degraded,
                _ => CrawlerStatus::Unstable,
            };
        }
    }

    /// Records success: resets consecutive counter, clears quarantine when
    /// expired, restores Healthy.
    pub fn record_success(&self, crawler_id: &str) {
        let mut map = self.state.write().unwrap();
        let st = Self::state_of(&mut map, crawler_id);
        st.consecutive_failures = 0;
        if st
            .quarantined_until
            .map(|t| Instant::now() >= t)
            .unwrap_or(true)
        {
            st.quarantined_until = None;
            if st.status == CrawlerStatus::Quarantined || st.status == CrawlerStatus::Unstable {
                st.status = CrawlerStatus::Healthy;
            }
        }
    }

    /// Current status of a crawler.
    pub fn status(&self, crawler_id: &str) -> CrawlerStatus {
        let mut map = self.state.write().unwrap();
        let st = Self::state_of(&mut map, crawler_id);
        // Expired quarantine auto-releases to Unstable (probe), per
        // RFC-011 11.3 failure states.
        if st.status == CrawlerStatus::Quarantined
            && st
                .quarantined_until
                .map(|t| Instant::now() >= t)
                .unwrap_or(false)
        {
            st.status = CrawlerStatus::Unstable;
            st.consecutive_failures = self.threshold - 1;
        }
        st.status
    }

    /// Whether the crawler may run now (not quarantined/disabled).
    pub fn may_run(&self, crawler_id: &str) -> bool {
        matches!(
            self.status(crawler_id),
            CrawlerStatus::Healthy
                | CrawlerStatus::Degraded
                | CrawlerStatus::Unstable
                | CrawlerStatus::Updating
        )
    }

    /// Recent failures for `crawler failures` (RFC-010 10.8).
    pub fn recent_failures(&self, crawler_id: Option<&str>) -> Vec<CrawlerFailure> {
        let map = self.state.read().unwrap();
        match crawler_id {
            Some(id) => map.get(id).map(|s| s.failures.clone()).unwrap_or_default(),
            None => map.values().flat_map(|s| s.failures.clone()).collect(),
        }
    }

    /// Disables a crawler manually (RFC-011 11.3 Disabled).
    pub fn disable(&self, crawler_id: &str) {
        let mut map = self.state.write().unwrap();
        Self::state_of(&mut map, crawler_id).status = CrawlerStatus::Disabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(id: &str, kind: CrawlerErrorKind) -> CrawlerFailure {
        CrawlerFailure {
            crawler_id: id.into(),
            kind,
            url: "https://x.example.com/p".into(),
            detail: "test".into(),
            at_ms: 0,
        }
    }

    #[test]
    fn escalates_and_quarantines() {
        let t = FailureTracker::new(5, Duration::from_millis(10));
        assert_eq!(t.status("c1"), CrawlerStatus::Healthy);
        t.record_failure(failure("c1", CrawlerErrorKind::Timeout));
        assert_eq!(t.status("c1"), CrawlerStatus::Degraded);
        for _ in 0..3 {
            t.record_failure(failure("c1", CrawlerErrorKind::Timeout));
        }
        assert_eq!(t.status("c1"), CrawlerStatus::Unstable);
        t.record_failure(failure("c1", CrawlerErrorKind::Timeout));
        assert_eq!(t.status("c1"), CrawlerStatus::Quarantined);
        assert!(!t.may_run("c1"));
    }

    #[test]
    fn non_counting_errors_do_not_escalate() {
        let t = FailureTracker::new(3, Duration::from_millis(5));
        t.record_failure(failure("c2", CrawlerErrorKind::RobotsDisallowed));
        t.record_failure(failure("c2", CrawlerErrorKind::Http404));
        assert_eq!(t.status("c2"), CrawlerStatus::Healthy);
    }

    #[test]
    fn success_resets_and_releases() {
        let t = FailureTracker::new(2, Duration::from_millis(5));
        t.record_failure(failure("c3", CrawlerErrorKind::NetworkError));
        t.record_failure(failure("c3", CrawlerErrorKind::NetworkError));
        assert_eq!(t.status("c3"), CrawlerStatus::Quarantined);
        std::thread::sleep(Duration::from_millis(8));
        assert_eq!(
            t.status("c3"),
            CrawlerStatus::Unstable,
            "expired quarantine probes"
        );
        t.record_success("c3");
        assert_eq!(t.status("c3"), CrawlerStatus::Healthy);
    }

    #[test]
    fn isolation_is_per_crawler() {
        let t = FailureTracker::new(2, Duration::from_secs(60));
        t.record_failure(failure("a", CrawlerErrorKind::Timeout));
        t.record_failure(failure("a", CrawlerErrorKind::Timeout));
        assert_eq!(t.status("a"), CrawlerStatus::Quarantined);
        // Single-site failure must not affect others (RFC-010 10.7).
        assert_eq!(t.status("b"), CrawlerStatus::Healthy);
        assert!(t.may_run("b"));
    }

    #[test]
    fn failure_listing() {
        let t = FailureTracker::new(10, Duration::from_secs(1));
        t.record_failure(failure("a", CrawlerErrorKind::Http5xx));
        t.record_failure(failure("b", CrawlerErrorKind::ParserError));
        assert_eq!(t.recent_failures(Some("a")).len(), 1);
        assert_eq!(t.recent_failures(None).len(), 2);
    }
}
