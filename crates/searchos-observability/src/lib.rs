//! Observability (RFC-015).
//!
//! - [`metrics`] — in-process metrics: search/quality/cost/reliability
//!   counters, latency percentiles (P50/P95/P99), exportable as JSON for
//!   `GET /stats/search`.
//! - [`logging`] — tracing subscriber setup (env filter; JSON optional).

pub mod logging;
pub mod metrics;

pub use metrics::{
    MetricsRegistry, MetricsSnapshot, Percentiles, INVARIANT_METRICS, MAX_LATENCY_SAMPLES,
};
