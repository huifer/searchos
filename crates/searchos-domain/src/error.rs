//! Unified error taxonomy (RFC-002 2.6).
//!
//! Every error carries a stable error code plus flags describing whether it is
//! retryable, degradable, quality-affecting and cost-bearing, and an optional
//! associated trace id.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable transport error-code table shared by CLI, HTTP and MCP
/// (Part 30 INV-30-19). All codes are upper snake case; JSON-RPC numeric
/// codes remain in their own field.
pub const TRANSPORT_ERROR_CODES: &[&str] = &[
    "VALIDATION_FAILED",
    "POLICY_VIOLATION",
    "BUDGET_EXCEEDED",
    "PROVIDER_FAILURE",
    "RETRIEVAL_FAILURE",
    "STORAGE_FAILURE",
    "EXTENSION_FAILURE",
    "QUALITY_GATE_FAILED",
    "SECURITY_BLOCKED",
    "TIMEOUT",
    "INTERNAL_ERROR",
    "UNAUTHORIZED",
    "WORKSPACE_ACCESS_DENIED",
    "RATE_LIMITED",
    "NOT_FOUND",
    "INVALID_MULTIPART",
    "EMPTY_CONTENT",
    "CONTENT_TOO_LARGE",
    "INVALID_ENCODING",
    "UNSUPPORTED_CONTENT_TYPE",
    "SOURCE_KEY_CONFLICT",
];

/// Static classification attached to every [`SearchOsError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorClass {
    /// Stable machine-readable code, e.g. `PROVIDER_RATE_LIMIT`.
    pub code: &'static str,
    /// Safe to retry with backoff.
    pub retryable: bool,
    /// Task may continue in degraded mode.
    pub degradable: bool,
    /// Error can reduce task quality.
    pub quality_affecting: bool,
    /// Error may have incurred cost.
    pub cost_bearing: bool,
}

/// Validation detail.
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
#[error("{field}: {message}")]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

/// The unified SearchOS error (RFC-002 2.6).
#[derive(Debug, Error)]
pub enum SearchOsError {
    #[error("validation error: {0}")]
    Validation(#[source] ValidationError),
    #[error("policy error: {0}")]
    Policy(String),
    #[error("budget exceeded: {0}")]
    Budget(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("retrieval error: {0}")]
    Retrieval(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("extension error: {0}")]
    Extension(String),
    #[error("quality error: {0}")]
    Quality(String),
    #[error("security error: {0}")]
    Security(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl SearchOsError {
    /// Classification for this error variant (stable codes, RFC-002 2.6).
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Validation(_) => ErrorClass {
                code: "VALIDATION_FAILED",
                retryable: false,
                degradable: false,
                quality_affecting: false,
                cost_bearing: false,
            },
            Self::Policy(_) => ErrorClass {
                code: "POLICY_VIOLATION",
                retryable: false,
                degradable: false,
                quality_affecting: false,
                cost_bearing: false,
            },
            Self::Budget(_) => ErrorClass {
                code: "BUDGET_EXCEEDED",
                retryable: false,
                degradable: true,
                quality_affecting: true,
                cost_bearing: true,
            },
            Self::Provider(_) => ErrorClass {
                code: "PROVIDER_FAILURE",
                retryable: true,
                degradable: true,
                quality_affecting: true,
                cost_bearing: true,
            },
            Self::Retrieval(_) => ErrorClass {
                code: "RETRIEVAL_FAILURE",
                retryable: true,
                degradable: true,
                quality_affecting: true,
                cost_bearing: false,
            },
            Self::Storage(_) => ErrorClass {
                code: "STORAGE_FAILURE",
                retryable: true,
                degradable: false,
                quality_affecting: false,
                cost_bearing: false,
            },
            Self::Extension(_) => ErrorClass {
                code: "EXTENSION_FAILURE",
                retryable: true,
                degradable: true,
                quality_affecting: true,
                cost_bearing: false,
            },
            Self::Quality(_) => ErrorClass {
                code: "QUALITY_GATE_FAILED",
                retryable: false,
                degradable: true,
                quality_affecting: true,
                cost_bearing: false,
            },
            Self::Security(_) => ErrorClass {
                code: "SECURITY_BLOCKED",
                retryable: false,
                degradable: false,
                quality_affecting: false,
                cost_bearing: false,
            },
            Self::Timeout(_) => ErrorClass {
                code: "TIMEOUT",
                retryable: true,
                degradable: true,
                quality_affecting: true,
                cost_bearing: true,
            },
            Self::Internal(_) => ErrorClass {
                code: "INTERNAL_ERROR",
                retryable: false,
                degradable: false,
                quality_affecting: false,
                cost_bearing: false,
            },
        }
    }

    /// Convenience constructor for validation errors.
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation(ValidationError {
            field: field.into(),
            message: message.into(),
        })
    }

    /// Attaches a trace id, returning the same error (wrapper for callers).
    pub fn with_trace(self, _trace_id: &crate::TraceId) -> Self {
        // Trace ids are propagated by the tracing layer; kept for API stability.
        self
    }
}

impl From<serde_json::Error> for SearchOsError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(format!("json: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_code_table_is_consistent() {
        let mut seen = std::collections::BTreeSet::new();
        for code in TRANSPORT_ERROR_CODES {
            assert!(seen.insert(*code), "duplicate transport code {code}");
            assert_eq!(
                code.to_ascii_uppercase(),
                *code,
                "transport codes are upper snake case: {code}"
            );
            assert!(!code.contains('-'));
        }
        for code in [
            SearchOsError::validation("x", "y").class().code,
            SearchOsError::Budget("x".into()).class().code,
            SearchOsError::Provider("x".into()).class().code,
            SearchOsError::Internal("x".into()).class().code,
        ] {
            assert!(TRANSPORT_ERROR_CODES.contains(&code), "missing {code}");
        }
    }

    #[test]
    fn error_classes_have_stable_codes() {
        let e = SearchOsError::Budget("over 0.10 USD".into());
        assert_eq!(e.class().code, "BUDGET_EXCEEDED");
        assert!(!e.class().retryable);
        assert!(e.class().quality_affecting);

        let e = SearchOsError::Provider("rate limited".into());
        assert_eq!(e.class().code, "PROVIDER_FAILURE");
        assert!(e.class().retryable);
        assert!(e.class().cost_bearing);
    }
}
