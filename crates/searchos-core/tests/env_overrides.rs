//! Environment-overrides tests. This is a dedicated test binary so mutating
//! process environment variables cannot race with other test modules.

use std::sync::Mutex;

use searchos_core::config::{
    apply_env_overrides, SearchOsConfig, ENV_AUTH_TOKEN, ENV_DATA_DIR, ENV_OTLP_ENABLED,
    ENV_OTLP_ENDPOINT, ENV_OTLP_HEADERS, ENV_OTLP_SERVICE_NAME,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_env() {
    for key in [
        ENV_DATA_DIR,
        ENV_AUTH_TOKEN,
        ENV_OTLP_ENABLED,
        ENV_OTLP_ENDPOINT,
        ENV_OTLP_SERVICE_NAME,
        ENV_OTLP_HEADERS,
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn defaults_leave_otlp_disabled_and_require_no_collector() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    let cfg = SearchOsConfig::default();
    let otlp = cfg.observability.otlp_config();
    assert!(!otlp.enabled);
    assert!(otlp.validate().is_ok());
}

#[test]
fn env_enables_otlp_and_sets_the_endpoint_and_service_name() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    std::env::set_var(ENV_OTLP_ENABLED, "true");
    std::env::set_var(ENV_OTLP_ENDPOINT, "http://collector:4318");
    std::env::set_var(ENV_OTLP_SERVICE_NAME, "searchos-ci");

    let mut cfg = SearchOsConfig::default();
    apply_env_overrides(&mut cfg);
    let otlp = cfg.observability.otlp_config();

    assert!(otlp.enabled);
    assert_eq!(otlp.endpoint, "http://collector:4318");
    assert_eq!(otlp.service_name, "searchos-ci");
    assert_eq!(
        otlp.signal_endpoint("traces"),
        "http://collector:4318/v1/traces"
    );
    assert!(otlp.validate().is_ok());
    clear_env();
}

#[test]
fn bearer_token_env_is_applied_on_the_real_load_path() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    std::env::set_var(ENV_AUTH_TOKEN, "secret-from-env");
    std::env::set_var(ENV_DATA_DIR, "/tmp/searchos-env-test");

    let mut cfg = SearchOsConfig::default();
    apply_env_overrides(&mut cfg);

    assert_eq!(cfg.server.auth_token, "secret-from-env");
    assert_eq!(cfg.app.data_dir.to_string_lossy(), "/tmp/searchos-env-test");
    // The redacted export must never carry the token.
    let redacted = cfg.redacted().to_string();
    assert!(!redacted.contains("secret-from-env"));
    clear_env();
}

#[test]
fn header_secret_is_parsed_but_never_logged() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    std::env::set_var(ENV_OTLP_ENABLED, "1");
    std::env::set_var(ENV_OTLP_HEADERS, "authorization=Bearer collector-secret");

    let mut cfg = SearchOsConfig::default();
    apply_env_overrides(&mut cfg);
    let otlp = cfg.observability.otlp_config();

    assert_eq!(
        otlp.headers.get("authorization").map(String::as_str),
        Some("Bearer collector-secret")
    );
    assert!(!cfg.redacted().to_string().contains("collector-secret"));
    clear_env();
}

#[test]
fn standard_otel_endpoint_env_enables_export() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otel.example.com");

    let mut cfg = SearchOsConfig::default();
    apply_env_overrides(&mut cfg);
    assert!(cfg.observability.otlp_enabled);
    assert_eq!(cfg.observability.otlp_endpoint, "https://otel.example.com");
    clear_env();
}

#[test]
fn explicit_disable_wins_over_the_standard_endpoint_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    std::env::set_var(ENV_OTLP_ENABLED, "false");
    std::env::set_var(ENV_OTLP_ENDPOINT, "http://explicit:4318");
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://global:4318");

    let mut cfg = SearchOsConfig::default();
    apply_env_overrides(&mut cfg);
    assert!(!cfg.observability.otlp_enabled);
    assert_eq!(cfg.observability.otlp_endpoint, "http://explicit:4318");
    clear_env();
}

#[test]
fn invalid_otlp_endpoint_is_rejected_by_config_validation() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env();
    let mut cfg = SearchOsConfig::default();
    cfg.observability.otlp_enabled = true;
    cfg.observability.otlp_endpoint = "collector:4318".into();
    let errors = cfg.validate().unwrap_err();
    assert!(errors.iter().any(|e| e.contains("http(s)")));
    clear_env();
}
