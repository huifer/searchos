//! tracing subscriber setup (RFC-015 15.1 spans).

use std::io::IsTerminal;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Initializes the global tracing subscriber once. RUST_LOG controls the
/// filter (default `info`). `json` enables JSON output for machine callers.
pub fn init(json: bool) {
    init_with_layer("info", json, None);
}

/// Initializes with a specific default filter (used by tests/CLI).
pub fn init_with_default(default: &str) {
    init_with_layer(default, false, None);
}

/// Initializes with an extra telemetry layer (the opt-in OTLP bridge).
///
/// The layer is boxed so the caller can decide at runtime whether OTLP is
/// enabled without changing the subscriber's type. `try_init` keeps this
/// idempotent: a second call (tests, embedded use) never panics.
pub fn init_with_otlp(default: &str, layer: Box<dyn Layer<Registry> + Send + Sync + 'static>) {
    init_with_layer(default, false, Some(layer));
}

fn init_with_layer(
    default: &str,
    json: bool,
    extra: Option<Box<dyn Layer<Registry> + Send + Sync + 'static>>,
) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    // The boxed layer is typed for `Registry`, so it must be attached directly
    // to the registry, before any other layer changes the subscriber type.
    let subscriber = Registry::default().with(extra).with(filter);

    if json {
        let _ = subscriber
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_ansi(false)
                    .with_writer(std::io::stderr),
            )
            .try_init();
    } else {
        let _ = subscriber
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_ansi(std::io::stderr().is_terminal())
                    .with_writer(std::io::stderr),
            )
            .try_init();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_is_idempotent() {
        crate::logging::init_with_default("warn");
        crate::logging::init_with_default("warn"); // second init must not panic
    }

    #[test]
    fn init_with_an_extra_layer_is_idempotent() {
        use tracing_subscriber::layer::Identity;
        let layer: Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync> =
            Box::new(Identity::new());
        crate::logging::init_with_otlp("warn", layer);
        let second: Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync> =
            Box::new(Identity::new());
        crate::logging::init_with_otlp("warn", second);
    }
}
