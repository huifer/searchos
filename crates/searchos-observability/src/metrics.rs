//! Metrics registry (RFC-015 15.2, Part 30 §30.15).
//!
//! Search / quality / cost / reliability counters plus bounded latency
//! percentiles. Invariant counters are exported through `/stats/*`, so an
//! execution-correctness violation is observable rather than merely fixed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;

/// Maximum retained latency samples (bounded ring, INV-30-21).
pub const MAX_LATENCY_SAMPLES: usize = 4096;

/// Every Part 30 invariant has at least one exported counter or gauge.
pub const INVARIANT_METRICS: &[&str] = &[
    "search.evidence.stale_or_excluded_served_total",
    "search.idempotency.claim_conflict_total",
    "budget.reservation.leaked_total",
    "budget.reservation.reserved_age_seconds",
    "cache.cross_mode_hit_total",
    "cache.hit_without_claim_total",
    "cache.evicted_total",
    "singleflight.follower_timeout_total",
    "singleflight.leader_abandoned_total",
    "singleflight.duplicate_execution_total",
    "cost.attribution_mismatch_total",
    "index.rebuild.provenance_mismatch_total",
    "index.delete.noop_total",
    "storage.migration.failure_total",
    "storage.schema.version",
    "auth.workspace_cross_read_total",
    "fetch.ssrf.address_mismatch_total",
    "extensions.load_failed_total",
    "extensions.quarantined",
    "cost.pricing.parse_error_total",
    "cost.estimate.units_missing_total",
    "trace.invalid_field_total",
    "document.canonical_divergence_total",
    "config.unconsumed_field_total",
    "fetch.limit_exceeded_total",
    "fetch.oversize_buffered_bytes",
    "fetch.robots_disallowed_total",
    "fetch.crawl_delay_waited_seconds",
    "transport.parity_violation_total",
    "quality.conflict_declared_total",
    "metrics.sample_overflow_total",
    "ratelimit.windows_evicted_total",
];

/// Latency percentiles in ms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug, Default)]
struct Counters {
    search_requests: AtomicU64,
    local_resolutions: AtomicU64,
    external_searches: AtomicU64,
    cache_hits: AtomicU64,
    semantic_reuse: AtomicU64,
    singleflight_merges: AtomicU64,
    quality_pass: AtomicU64,
    quality_partial: AtomicU64,
    quality_failed: AtomicU64,
    quality_conflict: AtomicU64,
    freshness_violations: AtomicU64,
    coverage_pass: AtomicU64,
    provider_failures: AtomicU64,
    crawler_successes: AtomicU64,
    crawler_failures: AtomicU64,
    cost_micros: AtomicI64,
    index_outbox_processed: AtomicU64,
    index_outbox_dead: AtomicU64,
    external_calls: AtomicU64,
    external_avoided: AtomicU64,
    valid_local_resolutions: AtomicU64,
}

#[derive(Debug, Default)]
struct StatusCounts {
    inner: Mutex<BTreeMap<String, u64>>,
}

#[derive(Debug, Default)]
struct LatencyRing {
    samples: Vec<f64>,
    write: usize,
}

impl LatencyRing {
    fn push(&mut self, sample: f64, overflow_counter: &mut dyn FnMut()) {
        if self.samples.len() < MAX_LATENCY_SAMPLES {
            self.samples.push(sample);
        } else {
            self.samples[self.write] = sample;
            overflow_counter();
        }
        if self.samples.len() == MAX_LATENCY_SAMPLES {
            self.write = (self.write + 1) % MAX_LATENCY_SAMPLES;
        }
    }

    fn snapshot(&self) -> Vec<f64> {
        self.samples.clone()
    }
}

/// Global in-process metrics registry.
#[derive(Debug)]
pub struct MetricsRegistry {
    counters: Counters,
    latencies: Mutex<LatencyRing>,
    statuses: StatusCounts,
    /// Low-cardinality invariant counters / gauges; names are static strings.
    invariant: Mutex<BTreeMap<String, u64>>,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsRegistry {
    /// New registry with every invariant metric present at zero.
    pub fn new() -> Self {
        let mut invariant = BTreeMap::new();
        for name in INVARIANT_METRICS {
            invariant.insert((*name).to_string(), 0);
        }
        Self {
            counters: Counters::default(),
            latencies: Mutex::new(LatencyRing::default()),
            statuses: StatusCounts::default(),
            invariant: Mutex::new(invariant),
        }
    }

    /// Global singleton.
    pub fn global() -> &'static MetricsRegistry {
        static GLOBAL: std::sync::OnceLock<MetricsRegistry> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(MetricsRegistry::new)
    }

    /// Increments a named invariant counter.
    pub fn inc_counter(&self, name: &str) {
        let mut map = self.invariant.lock().unwrap();
        *map.entry(name.to_string()).or_insert(0) += 1;
    }

    /// Sets a named invariant gauge.
    pub fn set_gauge(&self, name: &str, value: u64) {
        self.invariant
            .lock()
            .unwrap()
            .insert(name.to_string(), value);
    }

    /// Reads one invariant counter / gauge.
    pub fn counter(&self, name: &str) -> u64 {
        self.invariant
            .lock()
            .unwrap()
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Counts one search request completion.
    pub fn record_search(&self, latency_ms: f64, path: &str) {
        self.counters
            .search_requests
            .fetch_add(1, Ordering::Relaxed);
        match path {
            "local" | "local_with_refresh" => self
                .counters
                .local_resolutions
                .fetch_add(1, Ordering::Relaxed),
            "hybrid" | "external" => self
                .counters
                .external_searches
                .fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
        self.record_latency(latency_ms);
    }

    /// Latency sample, bounded to `MAX_LATENCY_SAMPLES`.
    pub fn record_latency(&self, latency_ms: f64) {
        let mut overflowed = false;
        {
            let mut ring = self.latencies.lock().unwrap();
            ring.push(latency_ms, &mut || overflowed = true);
        }
        if overflowed {
            self.inc_counter("metrics.sample_overflow_total");
        }
    }

    /// Current bounded sample count (diagnostics / tests).
    pub fn latency_sample_count(&self) -> usize {
        self.latencies.lock().unwrap().samples.len()
    }

    pub fn inc_cache_hit(&self) {
        self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_semantic_reuse(&self) {
        self.counters.semantic_reuse.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_singleflight(&self) {
        self.counters
            .singleflight_merges
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_quality(&self, status: &str) {
        let counter = match status {
            "passed" => &self.counters.quality_pass,
            "partial" => &self.counters.quality_partial,
            "failed" => &self.counters.quality_failed,
            "conflict" => &self.counters.quality_conflict,
            _ => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_freshness_violation(&self) {
        self.counters
            .freshness_violations
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_coverage_pass(&self) {
        self.counters.coverage_pass.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_provider_failure(&self) {
        self.counters
            .provider_failures
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_crawler(&self, ok: bool) {
        if ok {
            self.counters
                .crawler_successes
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters
                .crawler_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn add_cost_micros(&self, micros: i64) {
        self.counters
            .cost_micros
            .fetch_add(micros, Ordering::Relaxed);
    }
    pub fn record_status(&self, status: &str) {
        *self
            .statuses
            .inner
            .lock()
            .unwrap()
            .entry(status.to_string())
            .or_insert(0) += 1;
    }
    pub fn record_external_call(&self) {
        self.counters.external_calls.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_external_avoided(&self) {
        self.counters
            .external_avoided
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_valid_local_resolution(&self) {
        self.counters
            .valid_local_resolutions
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_outbox(&self, processed: bool, dead: bool) {
        if processed {
            self.counters
                .index_outbox_processed
                .fetch_add(1, Ordering::Relaxed);
        }
        if dead {
            self.counters
                .index_outbox_dead
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Percentiles from the bounded latency ring.
    pub fn latency_percentiles(&self) -> Percentiles {
        let mut v = self.latencies.lock().unwrap().snapshot();
        if v.is_empty() {
            return Percentiles::default();
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pick = |p: f64| -> f64 {
            let idx = ((v.len() as f64 * p).ceil() as usize)
                .saturating_sub(1)
                .min(v.len() - 1);
            v[idx]
        };
        Percentiles {
            p50: pick(0.50),
            p95: pick(0.95),
            p99: pick(0.99),
        }
    }

    /// Full JSON snapshot (RFC-015 15.2 + Part 30 §30.15).
    pub fn snapshot(&self) -> MetricsSnapshot {
        let c = &self.counters;
        let requests = c.search_requests.load(Ordering::Relaxed);
        let locals = c.local_resolutions.load(Ordering::Relaxed);
        let externals = c.external_searches.load(Ordering::Relaxed);
        let hits = c.cache_hits.load(Ordering::Relaxed);
        MetricsSnapshot {
            search_requests: requests,
            local_resolution_rate: rate(locals, requests),
            external_search_rate: rate(externals, requests),
            cache_hit_rate: rate(hits, requests),
            semantic_reuse_rate: rate(c.semantic_reuse.load(Ordering::Relaxed), requests),
            singleflight_reuse_rate: rate(c.singleflight_merges.load(Ordering::Relaxed), requests),
            search_latency: self.latency_percentiles(),
            quality_pass_count: c.quality_pass.load(Ordering::Relaxed),
            coverage_pass_rate: rate(c.coverage_pass.load(Ordering::Relaxed), requests),
            freshness_violation_rate: rate(
                c.freshness_violations.load(Ordering::Relaxed),
                requests,
            ),
            conflict_count: c.quality_conflict.load(Ordering::Relaxed),
            quality_pass_rate: rate(c.quality_pass.load(Ordering::Relaxed), requests),
            cost_micros_total: c.cost_micros.load(Ordering::Relaxed),
            provider_failure_count: c.provider_failures.load(Ordering::Relaxed),
            crawler_success_rate: {
                let ok = c.crawler_successes.load(Ordering::Relaxed);
                let fail = c.crawler_failures.load(Ordering::Relaxed);
                rate(ok, ok + fail)
            },
            index_outbox_processed: c.index_outbox_processed.load(Ordering::Relaxed),
            index_outbox_dead: c.index_outbox_dead.load(Ordering::Relaxed),
            status_counts: self.statuses.inner.lock().unwrap().clone(),
            external_avoidance_rate: rate(c.external_avoided.load(Ordering::Relaxed), requests),
            valid_local_resolution_rate: rate(
                c.valid_local_resolutions.load(Ordering::Relaxed),
                requests,
            ),
            cost_per_quality_passing_answer_micros: {
                let passing = c.quality_pass.load(Ordering::Relaxed);
                if passing > 0 {
                    c.cost_micros.load(Ordering::Relaxed) / passing as i64
                } else {
                    0
                }
            },
            invariant_metrics: self.invariant.lock().unwrap().clone(),
        }
    }
}

fn rate(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

/// JSON-exportable snapshot (feeds /stats/search).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub search_requests: u64,
    pub local_resolution_rate: f64,
    pub external_search_rate: f64,
    pub cache_hit_rate: f64,
    pub semantic_reuse_rate: f64,
    pub singleflight_reuse_rate: f64,
    pub search_latency: Percentiles,
    pub quality_pass_count: u64,
    pub quality_pass_rate: f64,
    pub coverage_pass_rate: f64,
    pub freshness_violation_rate: f64,
    pub conflict_count: u64,
    pub cost_micros_total: i64,
    pub provider_failure_count: u64,
    pub crawler_success_rate: f64,
    pub index_outbox_processed: u64,
    pub index_outbox_dead: u64,
    pub status_counts: BTreeMap<String, u64>,
    pub external_avoidance_rate: f64,
    pub valid_local_resolution_rate: f64,
    pub cost_per_quality_passing_answer_micros: i64,
    /// Part 30 invariant counters / gauges, always low-cardinality.
    pub invariant_metrics: BTreeMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_percentiles() {
        let m = MetricsRegistry::new();
        for i in 1..=100 {
            m.record_search(i as f64, if i % 2 == 0 { "local" } else { "external" });
        }
        let snap = m.snapshot();
        assert_eq!(snap.search_requests, 100);
        assert!((snap.local_resolution_rate - 0.5).abs() < 1e-9);
        assert!(snap.search_latency.p50 > 0.0);
        assert_eq!(snap.status_counts.len(), 0);
    }

    /// Part 30 §30.11: latency samples cannot grow without bound.
    #[test]
    fn latency_samples_are_bounded() {
        let m = MetricsRegistry::new();
        for i in 0..(MAX_LATENCY_SAMPLES as u64 * 3) {
            m.record_latency(i as f64);
        }
        assert_eq!(m.latency_sample_count(), MAX_LATENCY_SAMPLES);
        assert!(m.counter("metrics.sample_overflow_total") > 0);
        assert!(m.snapshot().search_latency.p99 > 0.0);
    }

    #[test]
    fn all_invariant_metrics_are_exported() {
        let snapshot = MetricsRegistry::new().snapshot();
        for name in INVARIANT_METRICS {
            assert!(
                snapshot.invariant_metrics.contains_key(*name),
                "missing {name}"
            );
        }
    }
}
