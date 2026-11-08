#[tokio::test]
async fn runtime_assembles_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    // schema migrated
    assert_eq!(
        rt.sqlite.schema_version().unwrap(),
        searchos_storage_sqlite::migrations::MIGRATIONS
            .last()
            .unwrap()
            .version
    );
    // default workspace usable
    let ws = searchos_domain::ids::WorkspaceId::new("ws_default");
    searchos_storage_sqlite::repos::ensure_workspace(&rt.sqlite, &ws, "default").unwrap();
    // index an chunk through tantivy and check consistency vs sqlite (0 == 0)
    let (ti, si) = rt.index_consistency().await.unwrap();
    assert_eq!(ti, si);
}
