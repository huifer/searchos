//! Circuit breaker (RFC-014 14.4 Reliability).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// Breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Normal operation.
    Closed,
    /// Failing; requests rejected until cooldown ends.
    Open,
    /// Cooldown ended; allowing a probe request.
    HalfOpen,
}

/// Per-provider circuit breaker (thread-safe counters).
#[derive(Debug)]
pub struct CircuitBreaker {
    state: AtomicU32, // 0 closed, 1 open, 2 half-open
    consecutive_failures: AtomicU32,
    opened_at_ms: AtomicU64,
    failure_threshold: u32,
    cooldown: Duration,
}

const CLOSED: u32 = 0;
const OPEN: u32 = 1;
const HALF_OPEN: u32 = 2;

impl CircuitBreaker {
    /// Breaker with failure threshold and cooldown.
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: AtomicU32::new(CLOSED),
            consecutive_failures: AtomicU32::new(0),
            opened_at_ms: AtomicU64::new(0),
            failure_threshold,
            cooldown,
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Whether a request may proceed now; transitions Open → HalfOpen after
    /// cooldown. Returns the current state.
    pub fn check(&self) -> CircuitState {
        match self.state.load(Ordering::SeqCst) {
            CLOSED => CircuitState::Closed,
            OPEN => {
                let opened = self.opened_at_ms.load(Ordering::SeqCst);
                if Self::now_ms().saturating_sub(opened) >= self.cooldown.as_millis() as u64 {
                    self.state.store(HALF_OPEN, Ordering::SeqCst);
                    CircuitState::HalfOpen
                } else {
                    CircuitState::Open
                }
            }
            _ => CircuitState::HalfOpen,
        }
    }

    /// Records success: resets failures; HalfOpen success closes the breaker.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        if self.state.load(Ordering::SeqCst) == HALF_OPEN {
            self.state.store(CLOSED, Ordering::SeqCst);
        }
    }

    /// Records failure: threshold reached ⇒ Open the breaker.
    pub fn record_failure(&self) {
        let f = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        if f >= self.failure_threshold || self.state.load(Ordering::SeqCst) == HALF_OPEN {
            self.opened_at_ms.store(Self::now_ms(), Ordering::SeqCst);
            self.state.store(OPEN, Ordering::SeqCst);
        }
    }

    /// Current state snapshot.
    pub fn state(&self) -> CircuitState {
        self.check()
    }

    /// Consecutive failure count.
    pub fn failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_after_threshold() {
        let b = CircuitBreaker::new(3, Duration::from_millis(50));
        assert_eq!(b.check(), CircuitState::Closed);
        b.record_failure();
        b.record_failure();
        assert_eq!(b.check(), CircuitState::Closed);
        b.record_failure();
        assert_eq!(b.check(), CircuitState::Open);
    }

    #[test]
    fn half_open_then_close_on_success() {
        let b = CircuitBreaker::new(1, Duration::from_millis(20));
        b.record_failure();
        assert_eq!(b.check(), CircuitState::Open);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(b.check(), CircuitState::HalfOpen);
        b.record_success();
        assert_eq!(b.check(), CircuitState::Closed);
    }

    #[test]
    fn success_resets_counter() {
        let b = CircuitBreaker::new(2, Duration::from_millis(10));
        b.record_failure();
        b.record_success();
        assert_eq!(b.failures(), 0);
        b.record_failure();
        assert_eq!(b.check(), CircuitState::Closed);
    }
}
