//! RFC-035 dispatch: notifications and the built-in schedule.
//!
//! A watchdog that can only be run by hand is a report, not a watch. These
//! gates prove the two dispatch halves: a webhook actually receives the
//! report (and only the report), and the gateway's schedule runs it on an
//! interval and stops cleanly.
//!
//! Named gates:
//!   webhook_receives_the_report_and_failures_are_recorded
//!   schedule_runs_the_watch_and_stops_cleanly

use std::sync::Arc as StdArc;

use searchos_application::watch::WatchStatus;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A minimal HTTP sink: accepts one connection, returns 200, records the
/// body. Deliberately raw TCP — no HTTP framework in a test gate.
struct WebhookSink {
    port: u16,
    bodies: StdArc<std::sync::Mutex<Vec<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl WebhookSink {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies: StdArc<std::sync::Mutex<Vec<String>>> = Default::default();
        let bodies_for_task = bodies.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let bodies = bodies_for_task.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut total = Vec::new();
                    loop {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                total.extend_from_slice(&buf[..n]);
                                // End of headers + JSON body: stop early.
                                if total.windows(4).any(|w| w == b"\r\n\r\n")
                                    && !String::from_utf8_lossy(&total)
                                        .split("\r\n\r\n")
                                        .nth(1)
                                        .unwrap_or("")
                                        .trim()
                                        .is_empty()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    let raw = String::from_utf8_lossy(&total).to_string();
                    if let Some(body) = raw.split("\r\n\r\n").nth(1) {
                        bodies.lock().unwrap().push(body.to_string());
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                        .await;
                    let _ = socket.flush().await;
                });
            }
        });
        Self {
            port,
            bodies,
            handle,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }
}

fn runtime_with(
    dir: &tempfile::TempDir,
    notify_on: &[&str],
    webhook: Option<String>,
) -> std::sync::Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.watch.workspace_id = "ws_watch".into();
    cfg.watch.notify_on = notify_on.iter().map(|s| s.to_string()).collect();
    cfg.watch.webhook_url = webhook;
    cfg.watch.min_sample = 1;
    futures_first_tick_block_on(
        async move { searchos_core::SearchOsRuntime::init(cfg).await.unwrap() },
    )
}

/// Runs the future on the current tokio runtime (tests are #[tokio::test]).
fn futures_first_tick_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_receives_the_report_and_failures_are_recorded() {
    let sink = WebhookSink::start().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with(&dir, &["skipped"], Some(sink.url()));

    // Empty workspace ⇒ the quality checks are skipped, and "skipped" is in
    // the notify policy, so delivery happens even with zero traffic.
    let watch = runtime.application.watch_service.as_ref().unwrap();
    let report = watch.run().await.unwrap();
    assert_eq!(report.status, WatchStatus::Skipped);
    assert_eq!(report.notifications.len(), 2, "log + webhook");
    let webhook = report
        .notifications
        .iter()
        .find(|n| n.notifier == "webhook")
        .expect("webhook notifier present");
    assert!(webhook.delivered, "webhook delivery: {:?}", webhook.error);
    assert!(report.notifications.iter().any(|n| n.notifier == "log"));

    // Give the sink a beat, then verify ONLY report fields travelled.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let received = {
        let bodies = sink.bodies.lock().unwrap();
        assert!(!bodies.is_empty(), "webhook must have received a body");
        bodies[0].clone()
    };
    let body: serde_json::Value = serde_json::from_str(received.as_str()).unwrap();
    assert_eq!(body["type"], "searchos.watch");
    assert_eq!(body["id"], report.id.as_str());
    assert_eq!(body["status"], "skipped");
    assert!(
        body.get("checks").is_some(),
        "check verdicts are part of the audit"
    );
    assert!(
        body.get("note").is_none() && body.get("documents").is_none(),
        "document content and notes never leave the machine"
    );

    // A broken webhook must not fail the run and must be recorded. Use a
    // SEPARATE data dir: a second runtime on the same dir hits the Tantivy
    // writer lock (known gotcha).
    let dir2 = tempfile::tempdir().unwrap();
    let broken = runtime_with(&dir2, &["skipped"], Some("http://127.0.0.1:1/hook".into()));
    let report = broken
        .application
        .watch_service
        .as_ref()
        .unwrap()
        .run()
        .await
        .unwrap();
    let webhook = report
        .notifications
        .iter()
        .find(|n| n.notifier == "webhook")
        .expect("webhook notifier present");
    assert!(!webhook.delivered);
    assert!(webhook.error.is_some());
    sink.handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_runs_the_watch_and_stops_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with(&dir, &["regressed", "degraded"], None);

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let scheduler_runtime = std::sync::Arc::clone(&runtime);
    let handle = tokio::spawn(async move {
        searchos_interface_http::routes::run_watch_scheduler(
            scheduler_runtime,
            1, // one-second interval
            stop_rx,
        )
        .await;
    });

    // Two ticks at most; one snapshot is enough for the gate.
    let mut snapshots = 0usize;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        snapshots = runtime
            .application
            .watch_service
            .as_ref()
            .unwrap()
            .history(100)
            .await
            .unwrap()
            .len();
        if snapshots >= 1 {
            break;
        }
    }
    assert!(snapshots >= 1, "the schedule must produce snapshots");

    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;
    let after_stop = runtime
        .application
        .watch_service
        .as_ref()
        .unwrap()
        .history(100)
        .await
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let settled = runtime
        .application
        .watch_service
        .as_ref()
        .unwrap()
        .history(100)
        .await
        .unwrap()
        .len();
    assert_eq!(
        after_stop, settled,
        "the scheduler must stop cleanly on shutdown"
    );
}
