//! RFC-058 entity resolution acceptance: registered aliases annotate search
//! results with stable entity ids, survive restart, and never invent ids.

use std::sync::Arc;

use searchos_application::ingest_service::IngestRequest;
use searchos_core::SearchOsConfig;
use searchos_domain::entity::{Entity, EntitySource};
use searchos_domain::ids::WorkspaceId;
use searchos_domain::request::{SearchMode, SearchRequest};

async fn runtime_on(dir: &std::path::Path) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

async fn seed(rt: &Arc<searchos_core::SearchOsRuntime>) {
    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/tokio".into());
    rt.application
        .ingest_service
        .ingest_text(
            req,
            "The tokio async runtime powers asynchronous Rust services.",
        )
        .await
        .unwrap();
    rt.process_outbox(256).await.unwrap();
}

fn request(query: &str) -> SearchRequest {
    SearchRequest::new(
        query,
        searchos_domain::ids::ProfileId::general(),
        SearchMode::LocalOnly,
        WorkspaceId::new("default"),
    )
    .unwrap()
}

#[tokio::test]
async fn registered_aliases_annotate_search_results() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(&rt).await;

    let entity = Entity::new("Tokio", ["tokio async runtime"], EntitySource::Operator);
    let conflicts = rt.register_entity(&entity).await.unwrap();
    assert!(conflicts.is_empty());

    let resp = rt
        .application
        .search_service
        .search(request("tokio async runtime"))
        .await
        .unwrap();
    assert!(!resp.results.is_empty());
    let annotated: Vec<&str> = resp
        .results
        .iter()
        .filter_map(|r| r.entity_id.as_deref())
        .collect();
    assert!(
        annotated.contains(&entity.id.as_str()),
        "the registered entity id is attached: {annotated:?}"
    );
}

#[tokio::test]
async fn unregistered_text_never_invents_an_entity() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(&rt).await;
    let resp = rt
        .application
        .search_service
        .search(request("tokio async runtime"))
        .await
        .unwrap();
    assert!(
        resp.results.iter().all(|r| r.entity_id.is_none()),
        "without a registration no entity is invented"
    );
}

#[tokio::test]
async fn entity_registry_survives_runtime_restart_and_keeps_first_owner() {
    let dir = tempfile::tempdir().unwrap();
    let first = Entity::new("Tokio", ["tokio async runtime"], EntitySource::Operator);
    {
        let rt = runtime_on(dir.path()).await;
        assert!(rt.register_entity(&first).await.unwrap().is_empty());
        let later = Entity::new(
            "Other Project",
            ["tokio async runtime"],
            EntitySource::Operator,
        );
        let conflicts = rt.register_entity(&later).await.unwrap();
        assert_eq!(conflicts, vec!["tokio async runtime"]);
    }
    let restarted = runtime_on(dir.path()).await;
    let matched = searchos_application::entity::EntityLookup::resolve(
        restarted.entity_registry.as_ref(),
        "the tokio async runtime is fast",
    )
    .unwrap();
    assert_eq!(matched.entity_id, first.id);
    assert_eq!(matched.canonical_name, "Tokio");
    assert_eq!(
        searchos_application::entity::EntityLookup::alias_count(restarted.entity_registry.as_ref()),
        3
    );
}
