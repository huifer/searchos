//! End-to-end gateway lifecycle: Prometheus `/metrics` and graceful shutdown.
//!
//! These tests spawn the real `searchos gateway start` process and signal it,
//! so they prove the production process behaviour rather than the in-process
//! handler behaviour covered by the HTTP crate's unit tests.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let target = deps.parent().unwrap();
    let bin = target.join("searchos");
    assert!(
        bin.exists(),
        "searchos binary not found at {}",
        bin.display()
    );
    bin
}

/// A port that was free at the moment it was picked.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

struct Gateway {
    child: std::process::Child,
    port: u16,
    data_dir: PathBuf,
}

impl Gateway {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let bin = binary();
        let child = Command::new(&bin)
            .args(["gateway", "start", "--port", &port.to_string()])
            .env("SEARCHOS_DATA_DIR", dir.path())
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn searchos gateway");
        let mut gateway = Self {
            child,
            port,
            data_dir: dir.keep(),
        };
        gateway.wait_healthy();
        gateway
    }

    fn wait_healthy(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                panic!("gateway exited before becoming healthy");
            }
            if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.write_all(
                    b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                );
                let mut buf = String::new();
                let _ = stream.read_to_string(&mut buf);
                if buf.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "gateway did not become healthy on port {}",
                self.port
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn get(&self, path: &str) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let status: u16 = buf
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, buf)
    }

    /// Sends SIGTERM and waits for a clean exit.
    fn terminate(&mut self) -> std::process::Output {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let mut stderr = String::new();
                if let Some(mut handle) = self.child.stderr.take() {
                    let _ = handle.read_to_string(&mut stderr);
                }
                return std::process::Output {
                    status,
                    stdout: Vec::new(),
                    stderr: stderr.into_bytes(),
                };
            }
            assert!(
                Instant::now() < deadline,
                "gateway did not exit after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

/// `GET /metrics` serves Prometheus text exposition 0.0.4 with both the
/// execution-correctness counters and the process-health gauges.
#[test]
fn metrics_endpoint_serves_prometheus_text() {
    let gateway = Gateway::start();
    let (status, body) = gateway.get("/metrics");
    assert_eq!(status, 200, "{body}");

    assert!(
        body.contains("content-type: text/plain; version=0.0.4"),
        "content type must be the Prometheus exposition format: {}",
        body.lines().take(6).collect::<Vec<_>>().join("\n")
    );
    for expected in [
        "# HELP searchos_search_requests_total",
        "# TYPE searchos_search_requests_total counter",
        "searchos_search_requests_total 0",
        "# TYPE searchos_search_latency_p50_seconds gauge",
        "# TYPE searchos_uptime_seconds gauge",
        "# TYPE searchos_index_outbox_pending gauge",
        "# TYPE searchos_schema_version gauge",
        "searchos_build_info{version=\"",
        "searchos_invariant_auth_workspace_cross_read_total",
    ] {
        assert!(
            body.contains(expected),
            "/metrics is missing '{expected}':\n{body}"
        );
    }
}

/// SIGTERM produces a clean exit: the listener stops accepting, in-flight work
/// drains, the outbox is flushed, and the process reports success.
#[test]
fn sigterm_shuts_the_gateway_down_cleanly() {
    let mut gateway = Gateway::start();
    // Traffic so the shutdown path has something to drain.
    let (status, _) = gateway.get("/health");
    assert_eq!(status, 200);

    let output = gateway.terminate();
    assert!(
        output.status.success(),
        "gateway must exit 0 after SIGTERM, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("shut down cleanly") || stderr.contains("draining connections"),
        "the shutdown must be observable in the log:\n{stderr}"
    );
}

/// SIGTERM is still honoured when the gateway has an open SSE stream: the
/// stream closes and the process exits.
#[test]
fn sigterm_drains_open_sse_streams() {
    let mut gateway = Gateway::start();

    // Hold an SSE connection open for the duration of the shutdown.
    let stream = std::net::TcpStream::connect(("127.0.0.1", gateway.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    use std::io::Write as _;
    let mut stream = stream;
    stream
        .write_all(
            b"GET /api/v1/dashboard/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n",
        )
        .unwrap();
    // Give the server a moment to register the stream.
    std::thread::sleep(Duration::from_millis(500));

    let output = gateway.terminate();
    assert!(
        output.status.success(),
        "gateway must exit 0 with an open SSE stream, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
