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
            local_resolutions: locals,
            external_searches: externals,
            cache_hits: hits,
            semantic_reuse: c.semantic_reuse.load(Ordering::Relaxed),
            singleflight_merges: c.singleflight_merges.load(Ordering::Relaxed),
            quality_partial: c.quality_partial.load(Ordering::Relaxed),
            quality_failed: c.quality_failed.load(Ordering::Relaxed),
            freshness_violations: c.freshness_violations.load(Ordering::Relaxed),
            coverage_pass: c.coverage_pass.load(Ordering::Relaxed),
            external_calls: c.external_calls.load(Ordering::Relaxed),
            external_avoided: c.external_avoided.load(Ordering::Relaxed),
            valid_local_resolutions: c.valid_local_resolutions.load(Ordering::Relaxed),
            crawler_successes: c.crawler_successes.load(Ordering::Relaxed),
            crawler_failures: c.crawler_failures.load(Ordering::Relaxed),
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

// ---------------------------------------------------------------------------
// Prometheus text exposition (GET /metrics, RFC-015 15.2).
// ---------------------------------------------------------------------------

/// Server-level metrics that are not process-wide counters: uptime, live SSE
/// streams, pending outbox rows and build/schema identity.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerMetrics {
    pub uptime_seconds: f64,
    pub sse_active: u64,
    pub sse_connections_total: u64,
    pub outbox_pending: u64,
    pub version: &'static str,
    pub schema_version: u64,
}

impl Default for ServerMetrics {
    fn default() -> Self {
        Self {
            uptime_seconds: 0.0,
            sse_active: 0,
            sse_connections_total: 0,
            outbox_pending: 0,
            version: "unknown",
            schema_version: 0,
        }
    }
}

/// Help text for the low-cardinality invariant metrics.
fn invariant_help(name: &str) -> &'static str {
    match name {
        "search.evidence.stale_or_excluded_served_total" => {
            "Stale or workspace-excluded candidates served (INV-30-01)"
        }
        "search.idempotency.claim_conflict_total" => {
            "Idempotency claim conflicts rejected (INV-30-02)"
        }
        "budget.reservation.leaked_total" => "Budget reservations never settled (INV-30-03)",
        "cache.cross_mode_hit_total" => "Cache hits across incompatible modes (INV-30-05)",
        "cache.hit_without_claim_total" => "Cache hits without an idempotency claim (INV-30-06)",
        "singleflight.follower_timeout_total" => {
            "Singleflight followers that timed out (INV-30-07)"
        }
        "singleflight.duplicate_execution_total" => {
            "Executions avoided by singleflight coalescing (INV-30-09)"
        }
        "cost.attribution_mismatch_total" => "Cost attribution mismatches (INV-30-10)",
        "index.rebuild.provenance_mismatch_total" => {
            "Rebuild rows whose provenance did not survive (INV-30-12)"
        }
        "index.delete.noop_total" => "Index deletes that did not exist (INV-30-08)",
        "storage.migration.failure_total" => "Migration failures (INV-30-14)",
        "storage.schema.version" => "Current schema (user_version)",
        "auth.workspace_cross_read_total" => "Cross-workspace read attempts denied",
        "fetch.ssrf.address_mismatch_total" => {
            "Connections that did not use the validated address (INV-30-11)"
        }
        "extensions.load_failed_total" => "Extension manifests that failed to load",
        "extensions.quarantined" => "Extensions currently quarantined",
        "fetch.limit_exceeded_total" => "Fetches rejected by the configured limit",
        "fetch.oversize_buffered_bytes" => "Bytes discarded from oversized responses",
        "fetch.robots_disallowed_total" => "Fetches blocked by robots.txt",
        "fetch.crawl_delay_waited_seconds" => "Seconds waited for Crawl-delay",
        "transport.parity_violation_total" => "CLI/HTTP/MCP semantic mismatches (INV-30-19)",
        "quality.conflict_declared_total" => "Unresolved evidence conflicts",
        "metrics.sample_overflow_total" => "Latency samples dropped by the bounded ring",
        "ratelimit.windows_evicted_total" => "Expired rate-limit windows evicted",
        "document.canonical_divergence_total" => "Canonical URL divergences across paths",
        "config.unconsumed_field_total" => "Declared config fields with no consumer",
        "cost.pricing.parse_error_total" => "Pricing rows that failed to parse",
        "cost.estimate.units_missing_total" => "Cost estimates without unit counts",
        "trace.invalid_field_total" => "Trace fields rejected as semantically invalid",
        "singleflight.leader_abandoned_total" => "Singleflight leaders abandoned by followers",
        "budget.reservation.reserved_age_seconds" => "Age of the oldest reserved budget",
        "cache.evicted_total" => "Cache entries evicted by the bounded capacity",
        _ => "Invariant metric (Part 30)",
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Prometheus text exposition format 0.0.4.
///
/// Every metric is prefixed with `searchos_`; latency percentiles and gauges
/// are suffixed with their unit (`_seconds`). Status counts are the only
/// labelled series.
pub fn render_prometheus(snapshot: &MetricsSnapshot, server: &ServerMetrics) -> String {
    #[derive(Default)]
    struct PromBuilder {
        out: String,
    }
    impl PromBuilder {
        fn counter(&mut self, name: &str, help: &str, value: u64, labels: &str) {
            self.out
                .push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
            if labels.is_empty() {
                self.out.push_str(&format!("{name} {value}\n"));
            } else {
                self.out.push_str(&format!("{name}{{{labels}}} {value}\n"));
            }
        }
        fn gauge(&mut self, name: &str, help: &str, value: f64) {
            self.out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
            ));
        }
    }
    let mut builder = PromBuilder {
        out: String::with_capacity(8192),
    };

    builder.counter(
        "searchos_search_requests_total",
        "Total search requests received",
        snapshot.search_requests,
        "",
    );
    builder.counter(
        "searchos_search_local_resolutions_total",
        "Search tasks served by local knowledge",
        snapshot.local_resolutions,
        "",
    );
    builder.counter(
        "searchos_search_external_total",
        "Search tasks with an external acquisition path",
        snapshot.external_searches,
        "",
    );
    builder.counter(
        "searchos_cache_hits_total",
        "Cache hits",
        snapshot.cache_hits,
        "",
    );
    builder.counter(
        "searchos_semantic_reuse_total",
        "Semantic cache reuse hits",
        snapshot.semantic_reuse,
        "",
    );
    builder.counter(
        "searchos_singleflight_merges_total",
        "Executions coalesced by singleflight",
        snapshot.singleflight_merges,
        "",
    );
    builder.counter(
        "searchos_quality_pass_total",
        "Responses that passed the final quality gate",
        snapshot.quality_pass_count,
        "",
    );
    builder.counter(
        "searchos_quality_partial_total",
        "Responses with partial coverage",
        snapshot.quality_partial,
        "",
    );
    builder.counter(
        "searchos_quality_failed_total",
        "Responses that failed the quality gate",
        snapshot.quality_failed,
        "",
    );
    builder.counter(
        "searchos_freshness_violations_total",
        "Responses that served stale local knowledge",
        snapshot.freshness_violations,
        "",
    );
    builder.counter(
        "searchos_coverage_pass_total",
        "Responses that passed the coverage gate",
        snapshot.coverage_pass,
        "",
    );
    builder.counter(
        "searchos_conflicts_total",
        "Responses with unresolved evidence conflicts",
        snapshot.conflict_count,
        "",
    );
    builder.counter(
        "searchos_provider_failures_total",
        "Provider calls that failed",
        snapshot.provider_failure_count,
        "",
    );
    builder.counter(
        "searchos_external_calls_total",
        "External provider calls made",
        snapshot.external_calls,
        "",
    );
    builder.counter(
        "searchos_external_avoided_total",
        "External calls avoided by local reuse",
        snapshot.external_avoided,
        "",
    );
    builder.counter(
        "searchos_valid_local_resolutions_total",
        "Quality-valid local resolutions",
        snapshot.valid_local_resolutions,
        "",
    );
    builder.counter(
        "searchos_cost_micros_total",
        "Estimated cost in microseconds (can be negative for refunds)",
        snapshot.cost_micros_total.unsigned_abs(),
        "",
    );
    builder.counter(
        "searchos_crawler_success_total",
        "Successful crawler runs",
        snapshot.crawler_successes,
        "",
    );
    builder.counter(
        "searchos_crawler_failed_total",
        "Failed crawler runs",
        snapshot.crawler_failures,
        "",
    );
    builder.counter(
        "searchos_index_outbox_processed_total",
        "Outbox rows processed into the index",
        snapshot.index_outbox_processed,
        "",
    );
    builder.counter(
        "searchos_index_outbox_dead_total",
        "Outbox rows moved to the dead-letter state",
        snapshot.index_outbox_dead,
        "",
    );
    builder.counter(
        "searchos_rate_limit_windows_evicted_total",
        "Expired rate-limit windows evicted",
        snapshot
            .invariant_metrics
            .get("ratelimit.windows_evicted_total")
            .copied()
            .unwrap_or(0),
        "",
    );

    builder.gauge(
        "searchos_search_latency_p50_seconds",
        "P50 search latency in seconds",
        snapshot.search_latency.p50 / 1000.0,
    );
    builder.gauge(
        "searchos_search_latency_p95_seconds",
        "P95 search latency in seconds",
        snapshot.search_latency.p95 / 1000.0,
    );
    builder.gauge(
        "searchos_search_latency_p99_seconds",
        "P99 search latency in seconds",
        snapshot.search_latency.p99 / 1000.0,
    );
    builder.gauge(
        "searchos_quality_pass_rate",
        "Share of responses that passed the quality gate",
        snapshot.quality_pass_rate,
    );
    builder.gauge(
        "searchos_cache_hit_rate",
        "Cache hit rate",
        snapshot.cache_hit_rate,
    );
    builder.gauge(
        "searchos_cost_micros_per_quality_pass",
        "Estimated cost per quality-passing answer, in microseconds",
        snapshot.cost_per_quality_passing_answer_micros as f64,
    );

    // Status counts are the only labelled series: the label set is bounded
    // by the status vocabulary, never by user input.
    let mut statuses: Vec<(&String, &u64)> = snapshot.status_counts.iter().collect();
    statuses.sort_by(|a, b| a.0.cmp(b.0));
    for (status, count) in statuses {
        builder.counter(
            "searchos_search_status_total",
            "Search responses by terminal status",
            *count,
            &format!("status=\"{}\"", escape_label(status)),
        );
    }

    // Low-cardinality invariant counters / gauges (Part 30).
    let mut invariants: Vec<(&String, &u64)> = snapshot.invariant_metrics.iter().collect();
    invariants.sort_by(|a, b| a.0.cmp(b.0));
    for (name, value) in invariants {
        let metric = format!("searchos_invariant_{}", name.replace(['.', '/'], "_"));
        builder.counter(&metric, invariant_help(name), *value, "");
    }

    // Server-level metrics.
    builder.gauge(
        "searchos_uptime_seconds",
        "Seconds since the process started",
        server.uptime_seconds,
    );
    builder.gauge(
        "searchos_sse_active",
        "Currently open SSE dashboard streams",
        server.sse_active as f64,
    );
    builder.counter(
        "searchos_sse_connections_total",
        "SSE dashboard streams opened since start",
        server.sse_connections_total,
        "",
    );
    builder.gauge(
        "searchos_index_outbox_pending",
        "Outbox rows still waiting for the index",
        server.outbox_pending as f64,
    );
    builder.gauge(
        "searchos_schema_version",
        "Current schema (user_version)",
        server.schema_version as f64,
    );
    builder.gauge("searchos_build_info", "Build identity (always 1)", 1.0);
    // Replace the unlabelled build_info line with a labelled form so the
    // version and schema are visible in the scrape.
    let labelled = format!(
        "# HELP searchos_build_info Build identity (always 1)\n# TYPE searchos_build_info gauge\nsearchos_build_info{{version=\"{}\",schema_version=\"{}\"}} 1\n",
        escape_label(server.version),
        server.schema_version
    );
    if let Some(start) = builder.out.rfind("# HELP searchos_build_info") {
        builder.out.replace_range(start.., &labelled);
    }

    builder.out
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
    /// Raw counts behind the rates above, so an exporter does not have to
    /// reconstruct them (and Prometheus counters stay monotonic).
    pub local_resolutions: u64,
    pub external_searches: u64,
    pub cache_hits: u64,
    pub semantic_reuse: u64,
    pub singleflight_merges: u64,
    pub quality_partial: u64,
    pub quality_failed: u64,
    pub freshness_violations: u64,
    pub coverage_pass: u64,
    pub external_calls: u64,
    pub external_avoided: u64,
    pub valid_local_resolutions: u64,
    pub crawler_successes: u64,
    pub crawler_failures: u64,
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

    fn sample_snapshot() -> MetricsSnapshot {
        let m = MetricsRegistry::new();
        for i in 1..=10 {
            m.record_search(i as f64, if i % 2 == 0 { "local" } else { "external" });
        }
        m.record_quality("passed");
        m.record_quality("partial");
        m.record_quality("failed");
        m.inc_provider_failure();
        m.record_outbox(true, false);
        m.inc_counter("auth.workspace_cross_read_total");
        m.record_status("completed");
        m.record_status("partial");
        m.snapshot()
    }

    #[test]
    fn prometheus_exports_counters_gauges_and_labels() {
        let snapshot = sample_snapshot();
        let server = ServerMetrics {
            uptime_seconds: 12.5,
            sse_active: 2,
            sse_connections_total: 7,
            outbox_pending: 3,
            version: "0.1.0",
            schema_version: 5,
        };
        let text = render_prometheus(&snapshot, &server);

        // Content type contract: exposition format 0.0.4 (set by the handler).
        for name in [
            "searchos_search_requests_total",
            "searchos_quality_pass_total",
            "searchos_quality_partial_total",
            "searchos_quality_failed_total",
            "searchos_provider_failures_total",
            "searchos_index_outbox_processed_total",
            "searchos_invariant_auth_workspace_cross_read_total",
        ] {
            assert!(
                text.contains(&format!("# TYPE {name} counter")),
                "{name} must be a counter:\n{text}"
            );
        }
        assert!(text.contains("searchos_search_requests_total 10"), "{text}");
        assert!(text.contains("searchos_quality_pass_total 1"), "{text}");

        // Latency is exported in seconds, not milliseconds.
        assert!(
            text.contains("# TYPE searchos_search_latency_p50_seconds gauge"),
            "{text}"
        );

        // Labelled series use the bounded status vocabulary.
        assert!(
            text.contains("searchos_search_status_total{status=\"completed\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("searchos_search_status_total{status=\"partial\"} 1"),
            "{text}"
        );

        // Server metrics.
        assert!(text.contains("searchos_uptime_seconds 12.5"), "{text}");
        assert!(text.contains("searchos_sse_active 2"), "{text}");
        assert!(text.contains("searchos_sse_connections_total 7"), "{text}");
        assert!(text.contains("searchos_index_outbox_pending 3"), "{text}");
        assert!(text.contains("searchos_schema_version 5"), "{text}");

        // Build info is labelled, not bare.
        assert!(
            text.contains("searchos_build_info{version=\"0.1.0\",schema_version=\"5\"} 1"),
            "{text}"
        );

        // Every invariant metric is present, namespaced and sanitized.
        assert!(
            text.contains("searchos_invariant_auth_workspace_cross_read_total"),
            "{text}"
        );
        assert!(
            text.contains("searchos_invariant_storage_schema_version"),
            "{text}"
        );
    }

    #[test]
    fn prometheus_label_values_are_escaped() {
        let mut snapshot = sample_snapshot();
        snapshot
            .status_counts
            .insert("weird\"status\nline".into(), 1);
        let text = render_prometheus(&snapshot, &ServerMetrics::default());
        assert!(
            text.contains("status=\"weird\\\"status\\nline\""),
            "label values must be escaped:\n{text}"
        );
    }

    #[test]
    fn prometheus_is_stable_for_the_same_snapshot() {
        let snapshot = sample_snapshot();
        let server = ServerMetrics::default();
        assert_eq!(
            render_prometheus(&snapshot, &server),
            render_prometheus(&snapshot, &server)
        );
    }
}
