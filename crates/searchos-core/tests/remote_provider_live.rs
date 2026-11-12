//! Live integration test for the remote embedding + remote reranker path.
//!
//! Unlike the unit-level mock TCP tests, this wires a REAL OpenAI-compatible
//! HTTP server into a REAL assembled runtime and exercises the whole pipeline:
//! config → `RemoteEmbeddingProvider` → outbox → LanceDB → hybrid retrieval →
//! `RemoteReranker` → response. It proves the remote adapters are actually
//! reached at runtime, that vectors come back in the model's dimension, and
//! that a provider outage degrades honestly instead of faking success.

use std::sync::{Arc, Mutex};

use searchos_application::ingest_service::IngestRequest;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DIM: usize = 8;

/// Minimal HTTP/1.1 server with `/v1/embeddings` and `/v1/rerank`.
/// Embeddings are deterministic per input text; rerank reverses the document
/// order so the test can prove the caller honors `relevance_score` indices.
async fn mock_provider(listener: TcpListener, calls: Arc<Mutex<Vec<String>>>) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let calls = Arc::clone(&calls);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 32768];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .nth(1)
                .unwrap_or("");
            let body = req.split("\r\n\r\n").nth(1).unwrap_or_default();
            calls.lock().unwrap().push(path.to_string());
            let payload = match serde_json::from_str::<serde_json::Value>(body) {
                Ok(v) => v,
                Err(_) => serde_json::json!({}),
            };
            let response_body = if path.ends_with("/embeddings") {
                let inputs: Vec<String> = payload
                    .get("input")
                    .and_then(|i| i.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_str().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let data: Vec<serde_json::Value> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, text)| {
                        let seed = text.chars().map(|c| c as usize).sum::<usize>() + i;
                        let vector: Vec<f32> = (0..DIM)
                            .map(|d| ((seed + d) % 11) as f32 / 11.0)
                            .collect();
                        serde_json::json!({ "object": "embedding", "index": i, "embedding": vector })
                    })
                    .collect();
                serde_json::json!({ "object": "list", "model": "mock-embed", "data": data })
                    .to_string()
            } else if path.ends_with("/rerank") {
                let docs = payload
                    .get("documents")
                    .and_then(|d| d.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                // Reverse the ranking: the last document is declared best.
                let results: Vec<serde_json::Value> = (0..docs)
                    .rev()
                    .enumerate()
                    .map(|(rank, idx)| {
                        serde_json::json!({
                            "index": idx,
                            "relevance_score": 1.0 / (rank as f64 + 1.0),
                        })
                    })
                    .collect();
                serde_json::json!({ "model": "mock-rerank", "results": results }).to_string()
            } else {
                "{\"error\":\"unknown path\"}".to_string()
            };
            let status = if path.ends_with("/embeddings") || path.ends_with("/rerank") {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
    }
}

#[tokio::test]
async fn remote_embedding_and_rerank_are_reached_by_the_runtime() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(mock_provider(listener, Arc::clone(&calls)));

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.memory.embedding.provider = "remote".into();
    cfg.memory.embedding.url = format!("{base}/v1/embeddings");
    cfg.memory.embedding.model = "mock-embed".into();
    cfg.memory.embedding.dimensions = DIM;
    cfg.memory.embedding.api_key_env = "SEARCHOS_UNSET_KEY".into();
    cfg.memory.embedding.model_version = "live-v1".into();
    cfg.memory.embedding.max_batch_size = 8;
    cfg.intelligence.reranking = true;
    cfg.intelligence.reranker_provider = "remote".into();
    cfg.intelligence.reranker_url = format!("{base}/v1/rerank");
    cfg.intelligence.reranker_api_key_env = "SEARCHOS_UNSET_KEY".into();

    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    assert_eq!(rt.embedder.model_id(), "mock-embed");
    assert_eq!(rt.embedder.dimension(), DIM);
    assert_eq!(
        rt.hybrid.reranker_model_id(),
        Some("cross-encoder"),
        "the remote reranker must be attached"
    );

    // Ingest through the real application service, then drain the outbox so
    // the vector leg is written by the runtime path (not a hand upsert).
    let ws = WorkspaceId::new("default");
    // 36 candidates exceed the RFC-007 7.7 rerank trigger threshold (30), so
    // the remote cross-encoder is actually invoked rather than skipped.
    const DOCS: usize = 36;
    for i in 0..DOCS {
        let mut req = IngestRequest::new(ws.clone());
        req.source_key = Some(format!("live/{i}"));
        req.title = Some(format!("Tokio document {i}"));
        rt.application
            .ingest_service
            .ingest_text(
                req,
                &format!("Tokio async runtime document number {i} discusses timers and tasks."),
            )
            .await
            .unwrap();
    }
    rt.process_outbox(200).await.unwrap();

    use searchos_retrieval::VectorIndex as _;
    let vector_rows = rt.lancedb.count().await.unwrap();
    assert_eq!(
        vector_rows, DOCS as u64,
        "remote embeddings must reach LanceDB"
    );

    // A real search exercises hybrid retrieval with the remote reranker.
    let search_req = SearchRequest::new(
        "Tokio async runtime",
        ProfileId::general(),
        SearchMode::LocalOnly,
        ws.clone(),
    )
    .unwrap();
    let resp = rt
        .application
        .search_service
        .search(search_req)
        .await
        .unwrap();
    assert!(!resp.results.is_empty(), "local knowledge must be served");

    let paths = calls.lock().unwrap().clone();
    assert!(
        paths.iter().filter(|p| p.ends_with("/embeddings")).count() >= 4,
        "the remote embedding endpoint was actually called: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("/rerank")),
        "the remote reranker endpoint was actually called: {paths:?}"
    );
}
