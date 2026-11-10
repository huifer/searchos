//! End-to-end OTLP/HTTP export test against a real local TCP collector.
//!
//! This is a dedicated test binary: the OpenTelemetry global tracer/meter
//! providers can only be installed once per process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use searchos_observability::logging;
use searchos_observability::otlp::{self, OtlpConfig, OtlpHandle};

#[derive(Clone)]
struct Received(Arc<Mutex<Vec<(String, usize)>>>);

impl Received {
    fn paths(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn body_bytes(&self, path: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, n)| *n)
            .sum()
    }
}

async fn start_collector() -> (u16, Received) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Received(Arc::new(Mutex::new(Vec::new())));
    let sink = received.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let sink = sink.clone();
            tokio::spawn(async move {
                // Read until the connection goes idle: the exporter may use
                // Content-Length or chunked encoding, so header-only parsing
                // would respond before the body arrives and reset the socket.
                let mut buf = Vec::with_capacity(16384);
                let mut chunk = [0u8; 8192];
                loop {
                    match tokio::time::timeout(Duration::from_millis(200), socket.read(&mut chunk))
                        .await
                    {
                        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                        Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                if buf.is_empty() {
                    return;
                }
                let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
                let head = String::from_utf8_lossy(&buf);
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/unknown")
                    .to_string();
                let body_len = head_end
                    .map(|pos| buf.len().saturating_sub(pos + 4))
                    .unwrap_or(0);
                sink.0.lock().unwrap().push((path, body_len));

                let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(response).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (port, received)
}

async fn wait_for(requests: &Received, predicate: impl Fn(&Received) -> bool) -> bool {
    for _ in 0..60 {
        if predicate(requests) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

// A multi-thread runtime is required: `force_flush()` is a blocking call,
// and on a current-thread runtime it would starve the mock collector's accept
// loop (the exporter waits for a response the collector cannot send).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otlp_exports_traces_and_metrics_over_http() {
    let (port, received) = start_collector().await;
    let config = OtlpConfig {
        enabled: true,
        endpoint: format!("http://127.0.0.1:{port}"),
        service_name: "searchos-otlp-test".into(),
        metrics_interval_seconds: 1,
        ..Default::default()
    };

    // Sanity: the mock collector must be reachable before blaming the SDK.
    let mut probe = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    use std::io::Write as _;
    probe.write_all(b"GET /probe HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    drop(probe);
    let handle: OtlpHandle = otlp::init(&config).unwrap().expect("OTLP must be enabled");
    let OtlpHandle { layer, guard } = handle;
    logging::init_with_otlp("info", layer);
    assert!(guard.is_enabled());

    // One tracing span becomes one OTel span; the in-process registry has at
    // least one metric to observe. Use ERROR level deliberately:
    // `logging::init_with_otlp` applies `EnvFilter::try_from_default_env()`, and
    // CI sets `RUST_LOG=warn` globally, which silently filtered out the old
    // info-level span and made this assertion fail only on GitHub runners.
    searchos_observability::MetricsRegistry::global().inc_cache_hit();
    tracing::error_span!("otlp_smoke_span", component = "compatibility").in_scope(|| {
        tracing::error!("hello from the OTLP export test");
    });

    guard.force_flush().expect("force_flush must succeed");
    assert!(
        wait_for(&received, |r| r.paths().iter().any(|p| p == "/v1/traces")).await,
        "no trace export arrived; saw {:?}",
        received.paths()
    );
    assert!(
        received.body_bytes("/v1/traces") > 0,
        "trace export carried no protobuf payload"
    );

    // The periodic reader exports metrics on its own schedule; dropping the
    // guard performs one final export for both providers.
    drop(guard);
    assert!(
        wait_for(&received, |r| r.paths().iter().any(|p| p == "/v1/metrics")).await,
        "no metric export arrived; saw {:?}",
        received.paths()
    );
    assert!(
        received.body_bytes("/v1/metrics") > 0,
        "metric export carried no protobuf payload"
    );
}
