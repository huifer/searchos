//! tracing subscriber setup (RFC-015 15.1 spans).

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Initializes the global tracing subscriber once. RUST_LOG controls the
/// filter (default `info`). `json` enables JSON output for machine callers.
pub fn init(json: bool) {
    if json {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .try_init();
    } else {
        init_with_default("info");
    }
}

/// Initializes with a specific default filter (used by tests/CLI).
pub fn init_with_default(default: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_is_idempotent() {
        crate::logging::init_with_default("warn");
        crate::logging::init_with_default("warn"); // second init must not panic
    }
}
