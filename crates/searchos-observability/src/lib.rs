//! Observability (RFC-015).
//!
//! - [`metrics`] — in-process metrics: search/quality/cost/reliability
//!   counters, latency percentiles (P50/P95/P99), exportable as JSON for
//!   `GET /stats/search`, as Prometheus text for `GET /metrics`, and as OTLP
//!   metrics when enabled.
//! - [`logging`] — tracing subscriber setup (env filter; JSON optional).
//! - [`otlp`] — opt-in OTLP/HTTP exporter for traces and the same metric
//!   snapshot. Off by default; see `docs/observability.md`.

pub mod logging;
pub mod metrics;
pub mod otlp;

pub use metrics::{
    render_prometheus, MetricsRegistry, MetricsSnapshot, Percentiles, ServerMetrics,
    INVARIANT_METRICS, MAX_LATENCY_SAMPLES,
};
pub use otlp::{OtlpConfig, OtlpGuard, OtlpHandle};
