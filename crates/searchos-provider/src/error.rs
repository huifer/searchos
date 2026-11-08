//! Provider error taxonomy (RFC-009 9.7).
//!
//! Only retryable errors enter Retry; 429 follows Retry-After; 403 is never
//! retried unboundedly.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Error classes (RFC-009 9.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    AuthenticationError,
    RateLimit,
    Timeout,
    NetworkError,
    InvalidResponse,
    QuotaExhausted,
    ProviderUnavailable,
}

impl ProviderErrorKind {
    /// Stable snake_case code persisted in traces/DB.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthenticationError => "authentication_error",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::NetworkError => "network_error",
            Self::InvalidResponse => "invalid_response",
            Self::QuotaExhausted => "quota_exhausted",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }

    /// Only these classes are retryable (RFC-009 9.7).
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::Timeout | Self::NetworkError | Self::ProviderUnavailable
        )
    }
}

/// A provider failure with its class and optional Retry-After seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderError {
    pub provider_id: String,
    pub kind: ProviderErrorKind,
    pub message: String,
    /// Retry-After seconds when the server sent one (429/503).
    pub retry_after_seconds: Option<u64>,
    /// HTTP status when applicable.
    pub status: Option<u16>,
}

impl ProviderError {
    /// Builds from an HTTP status code.
    pub fn from_status(provider_id: &str, status: u16, body: &str) -> Self {
        let (kind, retry_after) = match status {
            401 | 403 => (ProviderErrorKind::AuthenticationError, None),
            404 => (ProviderErrorKind::InvalidResponse, None),
            429 => (ProviderErrorKind::RateLimit, None),
            408 | 504 => (ProviderErrorKind::Timeout, None),
            503 => (ProviderErrorKind::ProviderUnavailable, None),
            400 | 422 => (ProviderErrorKind::InvalidResponse, None),
            s if (500..600).contains(&s) => (ProviderErrorKind::ProviderUnavailable, None),
            _ => (ProviderErrorKind::InvalidResponse, None),
        };
        Self {
            provider_id: provider_id.into(),
            kind,
            message: truncate(body, 200),
            retry_after_seconds: retry_after,
            status: Some(status),
        }
    }

    /// Attaches a parsed Retry-After value.
    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }

    /// Network-layer failure.
    pub fn network(provider_id: &str, message: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind: ProviderErrorKind::NetworkError,
            message: message.into(),
            retry_after_seconds: None,
            status: None,
        }
    }

    /// Timeout failure.
    pub fn timeout(provider_id: &str, message: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind: ProviderErrorKind::Timeout,
            message: message.into(),
            retry_after_seconds: None,
            status: None,
        }
    }

    /// Invalid/undecodable response.
    pub fn invalid_response(provider_id: &str, message: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind: ProviderErrorKind::InvalidResponse,
            message: message.into(),
            retry_after_seconds: None,
            status: None,
        }
    }

    /// Auth failure.
    pub fn auth(provider_id: &str, message: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind: ProviderErrorKind::AuthenticationError,
            message: message.into(),
            retry_after_seconds: None,
            status: None,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}]: {}",
            self.provider_id,
            self.kind.code(),
            self.message
        )?;
        if let Some(s) = self.status {
            write!(f, " (http {s})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

impl From<ProviderError> for searchos_domain::SearchOsError {
    fn from(e: ProviderError) -> Self {
        searchos_domain::SearchOsError::Provider(e.to_string())
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification() {
        let e = ProviderError::from_status("tavily", 429, "rate limited");
        assert_eq!(e.kind, ProviderErrorKind::RateLimit);
        assert!(e.kind.retryable());

        let e = ProviderError::from_status("tavily", 403, "forbidden");
        assert_eq!(e.kind, ProviderErrorKind::AuthenticationError);
        assert!(!e.kind.retryable(), "403 不进行无限重试");

        let e = ProviderError::from_status("tavily", 503, "unavailable");
        assert_eq!(e.kind, ProviderErrorKind::ProviderUnavailable);
        assert!(e.kind.retryable());
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(ProviderErrorKind::RateLimit.code(), "rate_limit");
        assert_eq!(ProviderErrorKind::QuotaExhausted.code(), "quota_exhausted");
    }
}
