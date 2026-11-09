//! Live multi-key rotation test (RFC-009 9.7).
//!
//! Spins up a real TCP HTTP server that emulates an upstream provider:
//! key #1 always answers 429 (rate limited), key #2 answers 200. Verifies
//! that the RotatingProvider (wrapping the real TavilyProvider adapter)
//! fails over to the second key and keeps using it while the first stays
//! limited.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use searchos_provider::traits::{SearchParams, SearchProvider};
use searchos_provider::{
    KeyPool, ProviderErrorKind, RotatingProvider, RotationPolicy, TavilyProvider,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 server: 429 for `bad-key`, 200 for anything else.
async fn mock_upstream(
    listener: TcpListener,
    used_keys: Arc<Mutex<Vec<String>>>,
    requests: Arc<AtomicUsize>,
) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let used_keys = Arc::clone(&used_keys);
        let requests = Arc::clone(&requests);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            requests.fetch_add(1, Ordering::SeqCst);
            let body = req.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
            let key = if body.contains("bad-key") {
                "bad-key"
            } else {
                "good-key"
            };
            used_keys.lock().unwrap().push(key.to_string());
            let response = if key == "bad-key" {
                let body = "{\"error\":\"rate limit\"}";
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 60\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
            } else {
                let body = "{\"results\":[{\"title\":\"Tokio\",\"url\":\"https://tokio.rs\",\"content\":\"async runtime\",\"score\":0.9,\"published_date\":null,\"raw_content\":null}]}";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        });
    }
}

async fn start_mock() -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let used = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    tokio::spawn(mock_upstream(
        listener,
        Arc::clone(&used),
        Arc::clone(&requests),
    ));
    (format!("http://127.0.0.1:{port}"), used, requests)
}

fn rotating(
    base_url: String,
    policy: RotationPolicy,
) -> RotatingProvider<impl Fn(&str) -> Arc<dyn SearchProvider>> {
    RotatingProvider::new(
        "tavily",
        searchos_provider::SearchProvider::capabilities(&TavilyProvider::new("x")),
        KeyPool::new(vec!["bad-key".to_string(), "good-key".to_string()], policy),
        move |k| Arc::new(TavilyProvider::new(k.to_string()).with_base_url(base_url.clone())),
    )
}

#[tokio::test]
async fn round_robin_fails_over_from_rate_limited_key() {
    let (base_url, used, requests) = start_mock().await;
    let provider = rotating(base_url.clone(), RotationPolicy::RoundRobin);

    // First call: bad-key gets 429 → rotation falls through to good-key.
    let result = provider.search(&SearchParams::simple("tokio", 5)).await;
    assert!(result.is_ok(), "rotation must fall through: {result:?}");
    assert_eq!(
        *used.lock().unwrap(),
        vec!["bad-key".to_string(), "good-key".to_string()]
    );

    // Second call: bad-key still limited → good-key again.
    let result2 = provider.search(&SearchParams::simple("tokio", 5)).await;
    assert!(result2.is_ok());
    assert_eq!(used.lock().unwrap().len(), 3);
    assert_eq!(used.lock().unwrap()[2], "good-key");
    assert_eq!(requests.load(Ordering::SeqCst), 3);

    // Key status reflects the rotation state (masked keys).
    let status = provider.key_status().unwrap();
    assert_eq!(status.len(), 2);
    assert_eq!(status[0].state, "rate_limited");
    assert_eq!(status[1].state, "healthy");
    assert!(status[0].masked_key.contains("****"));
}

#[tokio::test]
async fn failover_policy_is_sticky() {
    let (base_url, used, _requests) = start_mock().await;
    let provider = rotating(base_url.clone(), RotationPolicy::Failover);

    let r1 = provider
        .search(&SearchParams::simple("q", 5))
        .await
        .unwrap();
    assert_eq!(r1.entries.len(), 1);
    // bad-key first (sticky), then good-key on failure.
    assert_eq!(used.lock().unwrap()[0], "bad-key");
    assert_eq!(used.lock().unwrap()[1], "good-key");
}

#[tokio::test]
async fn all_keys_limited_reports_429_with_retry_hint() {
    // Mock where every key is limited: use a pool whose only key maps to
    // the 429 path.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let used = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let used = Arc::clone(&used);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = socket.read(&mut buf).await;
                used.lock().unwrap().push("x".to_string());
                let body = "{\"error\":\"rate limit\"}";
                let response = format!(
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    let provider = RotatingProvider::new(
        "tavily",
        searchos_provider::SearchProvider::capabilities(&TavilyProvider::new("x")),
        KeyPool::new(vec!["only-key".to_string()], RotationPolicy::RoundRobin),
        move |k| {
            Arc::new(
                TavilyProvider::new(k.to_string())
                    .with_base_url(format!("http://127.0.0.1:{port}")),
            )
        },
    );
    let err = provider
        .search(&SearchParams::simple("q", 5))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ProviderErrorKind::RateLimit);
    assert_eq!(err.status, Some(429));
    assert!(err.retry_after_seconds.is_some());
    // Single-key pools report auth errors, not rotation — this one key was
    // rate limited so the error surfaces as 429 with a retry hint.
}
