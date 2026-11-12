//! Live OpenAI-compatible embedding provider test.
//!
//! Spins up a real TCP HTTP server that returns embeddings out of order,
//! verifies batching, dimension validation, model metadata and auth-header
//! handling. No network access outside localhost; no API keys are used.

use std::sync::{Arc, Mutex};

use searchos_provider::embedding::{
    EmbeddingProvider, RemoteEmbeddingConfig, RemoteEmbeddingProvider,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 server. Returns `data[]` reversed on purpose so the
/// client must restore input order by `index`. Captures request bodies and
/// Authorization headers for assertions.
async fn mock_embeddings(
    listener: TcpListener,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    dim: usize,
) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let seen = Arc::clone(&seen);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let auth = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("authorization:"))
                .unwrap_or("")
                .to_string();
            let body = req.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
            seen.lock().unwrap().push((auth, body.clone()));

            let input: Vec<serde_json::Value> = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("input").and_then(|i| i.as_array()).cloned())
                .unwrap_or_default();
            // Deliberately reversed indices.
            let data: Vec<serde_json::Value> = input
                .iter()
                .enumerate()
                .rev()
                .map(|(i, _)| {
                    serde_json::json!({
                        "object": "embedding",
                        "index": i,
                        "embedding": (0..dim).map(|d| ((i + d) % 7) as f32 / 7.0).collect::<Vec<f32>>(),
                    })
                })
                .collect();
            let payload =
                serde_json::json!({ "object": "list", "data": data, "model": "mock-embed" });
            let text = payload.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                text.len(),
                text
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
    }
}

fn endpoint(base: &str) -> String {
    format!("{base}/v1/embeddings")
}

#[tokio::test]
async fn remote_embedding_restores_batch_order_and_metadata() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(mock_embeddings(listener, Arc::clone(&seen), 4));

    std::env::set_var("SEARCHOS_TEST_EMBED_KEY", "sk-test-secret");
    let cfg = RemoteEmbeddingConfig {
        endpoint: endpoint(&format!("http://{addr}")),
        model: "mock-embed".into(),
        api_key_env: "SEARCHOS_TEST_EMBED_KEY".into(),
        dimensions: 4,
        timeout_seconds: 5,
        model_version: "v3".into(),
        normalized: true,
        max_batch_size: 2,
    };
    let provider = RemoteEmbeddingProvider::new(cfg).unwrap();
    assert_eq!(provider.model_id(), "mock-embed");
    assert_eq!(provider.model_version(), "v3");
    assert_eq!(provider.dimension(), 4);
    assert!(provider.normalized());

    let texts: Vec<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let vectors = provider.embed(&texts).await.unwrap();
    assert_eq!(vectors.len(), 3);
    assert_eq!(vectors[0][0], 0.0, "index 0 → alpha");
    assert_eq!(vectors[1][0], 1.0f32 / 7.0, "index 1 → beta");
    // "gamma" starts its own batch (max_batch_size=2), so it is index 0
    // within that request; the client must still append it third.
    assert_eq!(vectors[2][0], 0.0, "index 0 of the second batch → gamma");
    assert_eq!(vectors[2], vectors[0], "per-batch indices restart");

    // 3 texts / batch size 2 → 2 HTTP requests, both authenticated, and the
    // key never appears in config-derived metadata.
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "batching is applied");
    assert!(requests
        .iter()
        .all(|(auth, _)| auth.contains("sk-test-secret")));
    assert!(requests
        .iter()
        .all(|(_, body)| body.contains("\"dimensions\":4")));
    assert!(requests[0].1.contains("alpha") && requests[0].1.contains("beta"));
    assert!(requests[1].1.contains("gamma"));
    std::env::remove_var("SEARCHOS_TEST_EMBED_KEY");
}

#[tokio::test]
async fn dimension_mismatch_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(mock_embeddings(listener, seen, 8));

    let cfg = RemoteEmbeddingConfig {
        endpoint: endpoint(&format!("http://{addr}")),
        model: "mock-embed".into(),
        api_key_env: "SEARCHOS_UNSET_KEY".into(),
        dimensions: 4,
        timeout_seconds: 5,
        model_version: "v1".into(),
        normalized: true,
        max_batch_size: 8,
    };
    let provider = RemoteEmbeddingProvider::new(cfg).unwrap();
    let err = provider
        .embed(&["alpha".to_string()])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("dimension mismatch"), "got {err}");
}

#[tokio::test]
async fn empty_endpoint_is_rejected() {
    let cfg = RemoteEmbeddingConfig {
        endpoint: "  ".into(),
        ..Default::default()
    };
    assert!(RemoteEmbeddingProvider::new(cfg).is_err());
}

#[tokio::test]
async fn empty_batch_skips_the_network() {
    let cfg = RemoteEmbeddingConfig {
        endpoint: "http://127.0.0.1:1/v1/embeddings".into(),
        ..Default::default()
    };
    let provider = RemoteEmbeddingProvider::new(cfg).unwrap();
    assert!(provider.embed(&[]).await.unwrap().is_empty());
}
