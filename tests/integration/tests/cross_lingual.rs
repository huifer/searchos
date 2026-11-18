//! RFC-037 cross-lingual retrieval acceptance gates.
//!
//! A Chinese query must be able to find an English document that describes
//! the same concept, WITHOUT machine translation or an LLM: a curated term
//! dictionary produces one extra recall list, fused with the original.
//!
//! Named gates:
//!   cross_lingual_query_finds_english_document
//!   cross_lingual_is_opt_in_and_never_replaces_original_results
//!   cross_lingual_variant_competes_for_the_same_query_budget

use std::sync::Arc;

use searchos_application::search_service::SearchService;
use searchos_application::search_service::{SearchDependencies, SearchServiceImpl};
use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};

async fn harness(
    dir: &tempfile::TempDir,
    cross_lingual: bool,
) -> (
    std::sync::Arc<searchos_core::SearchOsRuntime>,
    SearchServiceImpl,
) {
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.intelligence.cross_lingual = cross_lingual;
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let deps = Arc::new(SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
        cost_config: searchos_cost::CostModelConfig::default_config(),
        bm25: runtime.fulltext.clone(),
        vector: Some(runtime.vector.clone()),
        hybrid: Some(runtime.hybrid.clone()),
        embedder: runtime.embedder.clone(),
        canonical_cache: runtime.canonical_cache.clone(),
        semantic_cache: runtime.semantic_cache.clone(),
        retrieval_cache: runtime.retrieval_cache.clone(),
        embedding_cache: runtime.embedding_cache.clone(),
        rerank_cache: runtime.rerank_cache.clone(),
        router: runtime.provider_registry.clone(),
        search_providers: vec![],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(searchos_application::singleflight::SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        cross_lingual,
        reranking: true,
        allow_external: false,
        promotion: None,
        session_store: None,
        federation_registry: None,
        federation_strategy: "legacy".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    });
    (Arc::clone(&runtime), SearchServiceImpl::new(deps))
}

fn request(query: &str) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        SearchMode::LocalFirst,
        WorkspaceId::new("ws_default"),
    )
    .unwrap()
}

async fn seed_english_document(runtime: &searchos_core::SearchOsRuntime) {
    let doc = searchos_domain::retrieval::IndexedDocument {
        chunk_id: ChunkId::new("chunk_crosslingual_en"),
        document_id: DocumentId::new("doc_crosslingual_en"),
        version_id: DocumentVersionId::new("docv_crosslingual_en"),
        title: Some("BM25 ranking algorithm".into()),
        body: "BM25 is a ranking algorithm used by full text search engines to score documents."
            .into(),
        url: "https://learn.test/bm25-ranking-algorithm".into(),
        language: Some("en".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now(),
    };
    runtime.fulltext.index(doc).await.unwrap();
    runtime.fulltext.commit().await.unwrap();
}

#[tokio::test]
async fn cross_lingual_query_finds_english_document() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, service) = harness(&dir, true).await;
    seed_english_document(&runtime).await;

    // Only Chinese terms: no shared ASCII token can explain a hit. The ONLY
    // way to find the English document is the translated recall list.
    let response = service.search(request("排序算法")).await.unwrap();
    assert!(
        response
            .results
            .iter()
            .any(|r| r.url.contains("bm25-ranking-algorithm")),
        "a Chinese query must find the English document via the translated recall list"
    );
}

#[tokio::test]
async fn cross_lingual_is_opt_in_and_never_replaces_original_results() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, off) = harness(&dir, false).await;
    seed_english_document(&runtime).await;
    let response = off.search(request("排序算法")).await.unwrap();
    assert!(
        !response
            .results
            .iter()
            .any(|r| r.url.contains("bm25-ranking-algorithm")),
        "with the flag off, behaviour must be unchanged"
    );

    // Seed a CHINESE document too: the original query must still find it
    // after the cross-lingual list is fused in.
    let doc = searchos_domain::retrieval::IndexedDocument {
        chunk_id: ChunkId::new("chunk_crosslingual_zh"),
        document_id: DocumentId::new("doc_crosslingual_zh"),
        version_id: DocumentVersionId::new("docv_crosslingual_zh"),
        title: Some("BM25 排序算法".into()),
        body: "BM25 排序算法用于全文检索的相关性评分。".into(),
        url: "https://learn.test/bm25-zh".into(),
        language: Some("zh".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now(),
    };
    runtime.fulltext.index(doc).await.unwrap();
    runtime.fulltext.commit().await.unwrap();

    // Fresh runtime + service so caches from the "off" run cannot mask the
    // "on" result. Same data dir would hit the Tantivy lock; use fresh paths.
    drop(runtime);
    drop(off);
    let idx = dir.path().join("idx_on");
    std::fs::create_dir_all(&idx).unwrap();
    let on_runtime = searchos_core::SearchOsRuntime::init_with_paths(
        &{
            let mut cfg = searchos_core::SearchOsConfig::default();
            cfg.app.data_dir = dir.path().to_path_buf();
            cfg.intelligence.cross_lingual = true;
            cfg
        },
        &dir.path().join("data/searchos.db"),
        &idx.join("tantivy"),
        &idx.join("lancedb"),
        &idx.join("objects"),
    )
    .await
    .unwrap();
    // The "on" index only has the SQLite state; seed both docs into its fresh
    // Tantivy index to reproduce the corpus (the earlier index dir is locked).
    seed_english_document(&on_runtime).await;
    let doc = searchos_domain::retrieval::IndexedDocument {
        chunk_id: ChunkId::new("chunk_crosslingual_zh"),
        document_id: DocumentId::new("doc_crosslingual_zh"),
        version_id: DocumentVersionId::new("docv_crosslingual_zh"),
        title: Some("BM25 排序算法".into()),
        body: "BM25 排序算法用于全文检索的相关性评分。".into(),
        url: "https://learn.test/bm25-zh".into(),
        language: Some("zh".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now(),
    };
    on_runtime.fulltext.index(doc).await.unwrap();
    on_runtime.fulltext.commit().await.unwrap();

    let deps = Arc::new(SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
        cost_config: searchos_cost::CostModelConfig::default_config(),
        bm25: on_runtime.fulltext.clone(),
        vector: Some(on_runtime.vector.clone()),
        hybrid: Some(on_runtime.hybrid.clone()),
        embedder: on_runtime.embedder.clone(),
        canonical_cache: on_runtime.canonical_cache.clone(),
        semantic_cache: on_runtime.semantic_cache.clone(),
        retrieval_cache: on_runtime.retrieval_cache.clone(),
        embedding_cache: on_runtime.embedding_cache.clone(),
        rerank_cache: on_runtime.rerank_cache.clone(),
        router: on_runtime.provider_registry.clone(),
        search_providers: vec![],
        local_port: on_runtime.local_port(),
        traces: on_runtime.traces(),
        cost_sink: on_runtime.cost_sink(),
        task_recorder: on_runtime.task_recorder(),
        singleflight: Arc::new(searchos_application::singleflight::SingleflightCoordinator::new()),
        cache: on_runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: on_runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        cross_lingual: true,
        reranking: true,
        allow_external: false,
        promotion: None,
        session_store: None,
        federation_registry: None,
        federation_strategy: "legacy".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    });
    let on = SearchServiceImpl::new(deps);
    let response = on.search(request("排序算法")).await.unwrap();
    let urls: Vec<&str> = response.results.iter().map(|r| r.url.as_str()).collect();
    assert!(
        urls.contains(&"https://learn.test/bm25-zh"),
        "the original-language result must survive fusion: {urls:?}"
    );
    assert!(
        urls.contains(&"https://learn.test/bm25-ranking-algorithm"),
        "the cross-language result must join it: {urls:?}"
    );
}

#[tokio::test]
async fn cross_lingual_variant_competes_for_the_same_query_budget() {
    let variants = searchos_query::crosslingual::cross_lingual_variants("BM25 排序算法", 2);
    assert_eq!(variants.len(), 1);
    // The variant is deterministic and does not include the original query.
    assert_eq!(variants[0], "BM25 ranking algorithm");
}
