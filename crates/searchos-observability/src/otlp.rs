//! Opt-in OTLP/HTTP exporter (RFC-015 15.4, Wave 4).
//!
//! SearchOS always exposes its in-process counters as Prometheus text on
//! `GET /metrics`. This module adds **optional** OpenTelemetry export over
//! OTLP/HTTP binary:
//!
//! - **Traces**: a global `SdkTracerProvider` with a batch span processor,
//!   bridged from `tracing` spans by `tracing-opentelemetry`.
//! - **Metrics**: the *same* `MetricsRegistry` snapshot as `GET /metrics`,
//!   exported through observable gauges so no counter is double-counted or
//!   reconstructed from rates.
//!
//! It is **off by default**: with no configuration, nothing is constructed,
//! no background thread starts and no network call is made. Turning it on
//! never changes the Prometheus surface — the two exporters are independent
//! readers of one registry.
//!
//! The returned [`OtlpGuard`] flushes both providers on drop, which the
//! gateway's graceful shutdown path relies on to avoid losing the last spans.

use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::metrics::{Meter, MeterProvider, ObservableGauge};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{
    MetricExporter, Protocol, SpanExporter, WithExportConfig, WithHttpConfig,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::{Layer, Registry};

use crate::metrics::MetricsRegistry;

/// Configuration for OTLP/HTTP export. All fields are plain data so the core
/// config crate can build one without depending on any OpenTelemetry type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    /// Master switch. Off by default.
    pub enabled: bool,
    /// Collector base, e.g. `http://localhost:4318`. Signal paths
    /// (`/v1/traces`, `/v1/metrics`) are appended automatically.
    pub endpoint: String,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Extra HTTP headers (e.g. an auth token). Values are never logged.
    pub headers: HashMap<String, String>,
    /// Periodic metrics reader interval.
    pub metrics_interval_seconds: u64,
    pub export_traces: bool,
    pub export_metrics: bool,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://127.0.0.1:4318".into(),
            service_name: "searchos".into(),
            headers: HashMap::new(),
            metrics_interval_seconds: 15,
            export_traces: true,
            export_metrics: true,
        }
    }
}

impl OtlpConfig {
    /// Validates the config before anything is constructed, so a bad endpoint
    /// fails at startup rather than in a background exporter thread.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let endpoint = self.endpoint.trim();
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            return Err(format!(
                "otlp_endpoint must be an http(s) URL, got {endpoint:?}"
            ));
        }
        if self.service_name.trim().is_empty() {
            return Err("otlp_service_name must not be empty".into());
        }
        if self.metrics_interval_seconds == 0 {
            return Err("otlp_metrics_interval_seconds must be > 0".into());
        }
        if !self.export_traces && !self.export_metrics {
            return Err("OTLP is enabled but both traces and metrics are disabled".into());
        }
        Ok(())
    }

    /// The full per-signal URL, without ever duplicating the signal path.
    pub fn signal_endpoint(&self, signal: &str) -> String {
        let base = self.endpoint.trim().trim_end_matches('/');
        let path = format!("/v1/{signal}");
        if base.ends_with(&path) {
            base.to_string()
        } else {
            format!("{base}{path}")
        }
    }
}

/// Parses `k=v,k2=v2` header specs. Malformed pairs are skipped; values are
/// never included in errors so a token cannot leak into logs.
pub fn parse_headers(spec: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in spec.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        if let Some((key, value)) = pair.split_once('=') {
            let key = key.trim();
            if !key.is_empty() {
                out.insert(key.to_string(), value.trim().to_string());
            }
        }
    }
    out
}

/// The `tracing` layer plus the provider guard. Keep both alive: dropping the
/// guard shuts the exporters down, and dropping the layer detaches the bridge.
pub struct OtlpHandle {
    pub layer: Box<dyn Layer<Registry> + Send + Sync + 'static>,
    pub guard: OtlpGuard,
}

/// Owns the providers and the observable instruments. Instruments must stay
/// alive for the whole process lifetime or their callbacks are unregistered.
pub struct OtlpGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    _u64_gauges: Vec<ObservableGauge<u64>>,
    _f64_gauges: Vec<ObservableGauge<f64>>,
    enabled: bool,
}

impl OtlpGuard {
    /// Whether export was actually installed.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Flushes both providers without shutting them down (used in tests and
    /// before a long drain).
    pub fn force_flush(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Some(provider) = &self.tracer_provider {
            if let Err(e) = provider.force_flush() {
                errors.push(format!("traces: {e}"));
            }
        }
        if let Some(provider) = &self.meter_provider {
            if let Err(e) = provider.force_flush() {
                errors.push(format!("metrics: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for OtlpGuard {
    fn drop(&mut self) {
        // `shutdown` performs a final export. Errors are logged, never
        // panicked: telemetry must never take the process down.
        if let Some(provider) = &self.meter_provider {
            if let Err(e) = provider.shutdown() {
                tracing::warn!(error = %e, "OTLP metrics shutdown failed");
            }
        }
        if let Some(provider) = &self.tracer_provider {
            if let Err(e) = provider.shutdown() {
                tracing::warn!(error = %e, "OTLP traces shutdown failed");
            }
        }
    }
}

/// Installs the OTLP exporters and returns the bridge layer + guard.
///
/// Returns `Ok(None)` when export is disabled, so the caller's default path is
/// literally unchanged. Never panics: a collector that is unreachable at
/// startup is logged and reported as an error, not a crash.
pub fn init(config: &OtlpConfig) -> Result<Option<OtlpHandle>, String> {
    config.validate()?;
    if !config.enabled {
        return Ok(None);
    }

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();
    let headers = config.headers.clone();

    let tracer_provider = if config.export_traces {
        let mut builder = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(config.signal_endpoint("traces"))
            .with_timeout(Duration::from_secs(5));
        if !headers.is_empty() {
            builder = builder.with_headers(headers.clone());
        }
        let exporter = builder
            .build()
            .map_err(|e| format!("OTLP span exporter: {e}"))?;
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        Some(provider)
    } else {
        None
    };

    let (meter_provider, u64_gauges, f64_gauges) = if config.export_metrics {
        let mut builder = MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(config.signal_endpoint("metrics"))
            .with_timeout(Duration::from_secs(5));
        if !headers.is_empty() {
            builder = builder.with_headers(headers);
        }
        let exporter = builder
            .build()
            .map_err(|e| format!("OTLP metric exporter: {e}"))?;
        let reader = PeriodicReader::builder(exporter)
            .with_interval(Duration::from_secs(config.metrics_interval_seconds))
            .build();
        let provider = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource)
            .build();
        global::set_meter_provider(provider.clone());
        let meter = provider.meter("searchos");
        let (u64s, f64s) = register_snapshot_gauges(&meter);
        (Some(provider), u64s, f64s)
    } else {
        (None, Vec::new(), Vec::new())
    };

    let guard = OtlpGuard {
        tracer_provider: tracer_provider.clone(),
        meter_provider,
        _u64_gauges: u64_gauges,
        _f64_gauges: f64_gauges,
        enabled: true,
    };

    // The layer bridges `tracing` spans to the OTel tracer. With no provider
    // (metrics-only mode) we still install a no-op-shaped empty layer so the
    // subscriber shape stays the same for every configuration.
    let layer: Box<dyn Layer<Registry> + Send + Sync + 'static> = match tracer_provider {
        Some(provider) => {
            Box::new(tracing_opentelemetry::layer().with_tracer(provider.tracer("searchos")))
        }
        None => Box::new(tracing_subscriber::layer::Identity::new()),
    };

    tracing::info!(
        endpoint = %config.endpoint,
        service_name = %config.service_name,
        traces = config.export_traces,
        metrics = config.export_metrics,
        "OTLP/HTTP export enabled"
    );

    Ok(Some(OtlpHandle { layer, guard }))
}

/// Registers observable gauges that read the process-wide `MetricsRegistry`
/// snapshot at collection time. Names match the Prometheus series so a scrape
/// and an OTLP export can be correlated directly.
fn register_snapshot_gauges(
    meter: &Meter,
) -> (Vec<ObservableGauge<u64>>, Vec<ObservableGauge<f64>>) {
    let mut u64s = Vec::new();
    let mut f64s = Vec::new();

    macro_rules! u64_gauge {
        ($name:expr, $field:ident) => {{
            u64s.push(
                meter
                    .u64_observable_gauge($name)
                    .with_callback(move |observer| {
                        let snap = MetricsRegistry::global().snapshot();
                        observer.observe(snap.$field, &[]);
                    })
                    .build(),
            );
        }};
    }
    macro_rules! f64_gauge {
        ($name:expr, $expr:expr) => {{
            f64s.push(
                meter
                    .f64_observable_gauge($name)
                    .with_callback(move |observer| {
                        let snap = MetricsRegistry::global().snapshot();
                        let value: f64 = $expr(&snap);
                        observer.observe(value, &[]);
                    })
                    .build(),
            );
        }};
    }

    u64_gauge!("searchos.search.requests", search_requests);
    u64_gauge!("searchos.quality.pass", quality_pass_count);
    u64_gauge!("searchos.conflicts", conflict_count);
    u64_gauge!("searchos.provider.failures", provider_failure_count);
    u64_gauge!("searchos.outbox.processed", index_outbox_processed);
    u64_gauge!("searchos.outbox.dead", index_outbox_dead);
    u64_gauge!("searchos.local.resolutions", local_resolutions);
    u64_gauge!("searchos.external.searches", external_searches);
    u64_gauge!("searchos.cache.hits", cache_hits);
    u64_gauge!("searchos.semantic.reuse", semantic_reuse);
    u64_gauge!("searchos.singleflight.merges", singleflight_merges);
    u64_gauge!("searchos.external.calls", external_calls);
    u64_gauge!("searchos.external.avoided", external_avoided);
    u64_gauge!("searchos.freshness.violations", freshness_violations);
    u64_gauge!("searchos.coverage.pass", coverage_pass);

    f64_gauge!(
        "searchos.quality.pass_rate",
        |s: &crate::metrics::MetricsSnapshot| s.quality_pass_rate
    );
    f64_gauge!(
        "searchos.cache.hit_rate",
        |s: &crate::metrics::MetricsSnapshot| s.cache_hit_rate
    );
    f64_gauge!(
        "searchos.search.latency.p95_seconds",
        |s: &crate::metrics::MetricsSnapshot| { s.search_latency.p95 / 1000.0 }
    );
    f64_gauge!(
        "searchos.cost.micros_per_quality_pass",
        |s: &crate::metrics::MetricsSnapshot| { s.cost_per_quality_passing_answer_micros as f64 }
    );

    // Status counts are a labelled series with a bounded vocabulary.
    u64s.push(
        meter
            .u64_observable_gauge("searchos.search.status")
            .with_callback(|observer| {
                let snap = MetricsRegistry::global().snapshot();
                for (status, count) in snap.status_counts {
                    observer.observe(count, &[KeyValue::new("status", status)]);
                }
            })
            .build(),
    );

    (u64s, f64s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_and_no_validation_of_the_default_endpoint() {
        let cfg = OtlpConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.validate().is_ok());
        assert!(init(&cfg).unwrap().is_none());
    }

    #[test]
    fn signal_endpoints_are_appended_once() {
        let cfg = OtlpConfig {
            enabled: true,
            endpoint: "http://collector:4318/".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.signal_endpoint("traces"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            cfg.signal_endpoint("metrics"),
            "http://collector:4318/v1/metrics"
        );

        let explicit = OtlpConfig {
            endpoint: "https://otel.example.com/v1/traces".into(),
            ..cfg
        };
        assert_eq!(
            explicit.signal_endpoint("traces"),
            "https://otel.example.com/v1/traces"
        );
    }

    #[test]
    fn validation_rejects_bad_configuration() {
        let bad_endpoint = OtlpConfig {
            enabled: true,
            endpoint: "collector:4318".into(),
            ..Default::default()
        };
        assert!(bad_endpoint.validate().unwrap_err().contains("http(s)"));

        let no_signal = OtlpConfig {
            enabled: true,
            export_traces: false,
            export_metrics: false,
            ..Default::default()
        };
        assert!(no_signal.validate().unwrap_err().contains("both"));

        let zero_interval = OtlpConfig {
            enabled: true,
            metrics_interval_seconds: 0,
            ..Default::default()
        };
        assert!(zero_interval.validate().is_err());
    }

    #[test]
    fn header_parsing_skips_malformed_pairs_and_never_leaks_values() {
        let headers = parse_headers("authorization= Bearer secret , x-tenant=acme, broken, =oops");
        assert_eq!(headers.get("authorization"), Some(&"Bearer secret".into()));
        assert_eq!(headers.get("x-tenant"), Some(&"acme".into()));
        assert_eq!(headers.len(), 2);
        let err = OtlpConfig {
            enabled: true,
            endpoint: "ftp://nope".into(),
            headers: parse_headers("authorization=Bearer secret"),
            ..Default::default()
        }
        .validate()
        .unwrap_err();
        assert!(!err.contains("secret"), "error must not echo header values");
    }

    #[test]
    fn metrics_only_mode_reports_its_shape() {
        let cfg = OtlpConfig {
            enabled: true,
            export_traces: false,
            export_metrics: true,
            metrics_interval_seconds: 1,
            endpoint: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
        assert!(!cfg.export_traces);
        assert!(cfg.export_metrics);
    }
}
