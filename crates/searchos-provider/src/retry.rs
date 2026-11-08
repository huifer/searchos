//! Bounded retry with exponential backoff (RFC-009 9.7, RFC-014 14.4).
//!
//! - Only [`ProviderErrorKind::retryable`] errors retry.
//! - 429/503 honor Retry-After when provided.
//! - Total attempts bounded by policy (`max_retries`).

use std::future::Future;
use std::time::Duration;

use crate::error::{ProviderError, ProviderErrorKind};

/// Retry policy bounds.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum attempts including the first (RFC budget max_retries+1).
    pub max_attempts: u32,
    /// Base backoff in ms.
    pub base_backoff_ms: u64,
    /// Max backoff cap in ms.
    pub max_backoff_ms: u64,
    /// Jitter factor 0..=1 applied to backoff (deterministic seed-free).
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_ms: 200,
            max_backoff_ms: 4_000,
            jitter: 0.2,
        }
    }
}

impl RetryPolicy {
    /// Policy bounded by a task budget's max_retries (RFC-009 9.6).
    pub fn bounded(max_retries: u32) -> Self {
        Self {
            max_attempts: (max_retries + 1).clamp(1, 6),
            ..Self::default()
        }
    }

    /// Backoff for attempt n (0-based): base * 2^n, capped, ± jitter.
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        let exp = self.base_backoff_ms.saturating_mul(1u64 << attempt.min(10));
        let capped = exp.min(self.max_backoff_ms) as f64;
        let jittered = capped * (1.0 - self.jitter + 2.0 * self.jitter * pseudo_random(attempt));
        Duration::from_millis(jittered as u64)
    }

    /// Retry-After from the error, if any, as the backoff for next attempt.
    pub fn backoff_with_retry_after(&self, attempt: u32, err: &ProviderError) -> Duration {
        if err.kind == ProviderErrorKind::RateLimit {
            if let Some(secs) = err.retry_after_seconds {
                return Duration::from_secs(secs.min(60));
            }
        }
        self.backoff_for(attempt)
    }
}

/// Deterministic pseudo-random in [0,1) so tests are reproducible.
fn pseudo_random(seed: u32) -> f64 {
    let x = seed.wrapping_mul(2654435761).wrapping_add(1013904223);
    (x >> 8) as f64 / (1 << 24) as f64
}

/// Executes `op` under the policy. `op` must be retry-safe (idempotent
/// search calls; never retry non-idempotent writes).
pub async fn retry_with_policy<T, F, Fut>(
    policy: &RetryPolicy,
    mut op: F,
) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !e.kind.retryable() || attempt + 1 >= policy.max_attempts {
                    return Err(e);
                }
                let backoff = policy.backoff_with_retry_after(attempt, &e);
                tracing::warn!(
                    provider = %e.provider_id,
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "retrying provider call after {:?}: {}",
                    e.kind,
                    e.message
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorKind;

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy::default();
        let b0 = p.backoff_for(0).as_millis();
        let b1 = p.backoff_for(1).as_millis();
        let b5 = p.backoff_for(5).as_millis();
        assert!(b1 >= b0);
        assert!(b5 <= p.max_backoff_ms as u128 + 1);
    }

    #[test]
    fn bounded_policy_respects_budget() {
        let p = RetryPolicy::bounded(2);
        assert_eq!(p.max_attempts, 3);
        let p0 = RetryPolicy::bounded(0);
        assert_eq!(p0.max_attempts, 1);
    }

    #[tokio::test]
    async fn non_retryable_fails_fast() {
        let p = RetryPolicy::default();
        let mut calls = 0;
        let res: Result<(), ProviderError> = retry_with_policy(&p, || {
            calls += 1;
            async {
                Err(ProviderError {
                    provider_id: "x".into(),
                    kind: ProviderErrorKind::AuthenticationError,
                    message: "403".into(),
                    retry_after_seconds: None,
                    status: Some(403),
                })
            }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls, 1, "403 must not retry");
    }

    #[tokio::test]
    async fn retryable_succeeds_on_second_attempt() {
        let p = RetryPolicy {
            base_backoff_ms: 1,
            max_backoff_ms: 2,
            ..Default::default()
        };
        let mut calls = 0;
        let res: Result<&str, ProviderError> = retry_with_policy(&p, || {
            calls += 1;
            async move {
                if calls == 1 {
                    Err(ProviderError::timeout("x", "first fails"))
                } else {
                    Ok("ok")
                }
            }
        })
        .await;
        assert_eq!(res.unwrap(), "ok");
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn attempts_exhausted_returns_last_error() {
        let p = RetryPolicy {
            base_backoff_ms: 1,
            max_backoff_ms: 2,
            ..Default::default()
        };
        let mut calls = 0;
        let res: Result<(), ProviderError> = retry_with_policy(&p, || {
            calls += 1;
            async { Err(ProviderError::timeout("x", "always")) }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls, p.max_attempts as usize);
    }

    #[tokio::test]
    async fn rate_limit_honors_retry_after() {
        let p = RetryPolicy::default();
        let err = ProviderError {
            provider_id: "x".into(),
            kind: ProviderErrorKind::RateLimit,
            message: "slow down".into(),
            retry_after_seconds: Some(30),
            status: Some(429),
        };
        let backoff = p.backoff_with_retry_after(0, &err);
        assert_eq!(backoff.as_secs(), 30);
    }
}
