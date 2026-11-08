//! Metrics registry (RFC-015 15.2).
//!
//! Search / quality / cost / reliability counters plus latency histograms
//! with P50/P95/P99 export. In-process implementation for MVP; the JSON
//! snapshot feeds `GET /stats/search`.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;

/// Latency percentiles in ms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Atomic counters block.
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
}

/// Global in-process metrics registry.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    counters: Counters,
    search_latencies: Mutex<Vec<f64>>,
}

impl MetricsRegistry {
    /// Global singleton.
    pub fn global() -> &'static MetricsRegistry {
        static GLOBAL: std::sync::OnceLock<MetricsRegistry> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(MetricsRegistry::default)
    }

    /// Counts one search request completion.
    pub fn record_search(&self, latency_ms: f64, path: &str) {
        self.counters
            .search_requests
            .fetch_add(1, Ordering::Relaxed);
        match path {
            "local" | "local_with_refresh" => {
                self.counters
                    .local_resolutions
                    .fetch_add(1, Ordering::Relaxed);
            }
            "hybrid" | "external" => {
                self.counters
                    .external_searches
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.record_latency(latency_ms);
    }

    /// Latency sample for search.
    pub fn record_latency(&self, latency_ms: f64) {
        self.search_latencies.lock().unwrap().push(latency_ms);
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

    /// Percentiles from recorded latencies.
    pub fn latency_percentiles(&self) -> Percentiles {
        let mut v = self.search_latencies.lock().unwrap().clone();
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

    /// Full JSON snapshot (RFC-015 15.2 metric list).
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_percentiles() {
        let m = MetricsRegistry::default();
        for i in 1..=100 {
            m.record_search(i as f64, if i % 2 == 0 { "local" } else { "external" });
        }
        let snap = m.snapshot();
        assert_eq!(snap.search_requests, 100);
        assert!((snap.local_resolution_rate - 0.5).abs() < 1e-9);
        assert!(snap.search_latency.p50 > 0.0);
        assert!(snap.search_latency.p99 >= snap.search_latency.p95);
        assert!(snap.search_latency.p95 >= snap.search_latency.p50);
    }

    #[test]
    fn empty_snapshot_is_zeroed_not_nan() {
        let m = MetricsRegistry::default();
        let snap = m.snapshot();
        assert_eq!(snap.local_resolution_rate, 0.0);
        assert_eq!(snap.search_latency, Percentiles::default());
    }

    #[test]
    fn quality_and_cost_counters() {
        let m = MetricsRegistry::default();
        m.record_quality("passed");
        m.record_quality("failed");
        m.add_cost_micros(5000);
        m.record_crawler(true);
        m.record_crawler(false);
        m.record_outbox(true, false);
        m.record_outbox(false, true);
        let snap = m.snapshot();
        assert_eq!(snap.quality_pass_count, 1);
        assert_eq!(snap.cost_micros_total, 5000);
        assert!((snap.crawler_success_rate - 0.5).abs() < 1e-9);
        assert_eq!(snap.index_outbox_dead, 1);
    }
}
