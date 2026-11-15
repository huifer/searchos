//! RFC-070 memory lifecycle acceptance gates: retention expiry, duplicate
//! consolidation, dry runs, and read-time freshness.

use std::sync::Arc;

use searchos_core::SearchOsConfig;
use searchos_domain::ids::{MemoryEntryId, WorkspaceId};
use searchos_domain::memory::{MemoryEntry, MemoryNamespace, SharingPolicy};
use searchos_storage_sqlite::repos::MemoryRepo;

async fn runtime_with_retention(
    dir: &std::path::Path,
    retention_days: u64,
) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    cfg.memory.retention_days = retention_days;
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

fn ns() -> MemoryNamespace {
    MemoryNamespace {
        workspace: WorkspaceId::new("default"),
        project: None,
        agent: None,
    }
}

fn entry(workspace: &str, title: &str, content: &str, age_days: i64) -> MemoryEntry {
    let now = chrono::Utc::now();
    MemoryEntry {
        id: MemoryEntryId::generate(),
        namespace: MemoryNamespace {
            workspace: WorkspaceId::new(workspace),
            project: None,
            agent: None,
        },
        sharing: SharingPolicy::WorkspaceShared,
        title: title.into(),
        content: content.into(),
        url: None,
        tags: Vec::new(),
        created_at: now - chrono::Duration::days(age_days),
        updated_at: now - chrono::Duration::days(age_days),
        embedding_status: "none".into(),
    }
}

#[tokio::test]
async fn expiry_uses_the_configured_retention_window() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with_retention(dir.path(), 30).await;
    let repo = MemoryRepo::new(&rt.sqlite);
    let fresh = entry("default", "fresh note", "recent memory content here", 1);
    let old = entry("default", "old note", "stale memory content here", 40);
    repo.insert(&fresh).unwrap();
    repo.insert(&old).unwrap();

    // Dry run: reports the old id, deletes nothing.
    let dry = rt.application.memory_service.expire(false).await.unwrap();
    assert_eq!(dry.expired, vec![old.id.to_string()]);
    assert!(!dry.applied);
    assert_eq!(repo.list_all().unwrap().len(), 2, "dry run mutates nothing");

    // Read-time freshness: the expired entry is not served even before a
    // sweep deletes it.
    let reader = ns();
    let hits = rt
        .application
        .memory_service
        .search("stale memory content", &reader, 5)
        .await
        .unwrap();
    assert!(
        hits.iter().all(|h| h.entry.id != old.id),
        "expired entries never surface"
    );

    // Apply: the expired entry is deleted.
    let applied = rt.application.memory_service.expire(true).await.unwrap();
    assert!(applied.applied);
    let remaining = repo.list_all().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, fresh.id);
}

#[tokio::test]
async fn zero_retention_keeps_entries_forever() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with_retention(dir.path(), 0).await;
    MemoryRepo::new(&rt.sqlite)
        .insert(&entry(
            "default",
            "ancient",
            "very old memory content",
            5_000,
        ))
        .unwrap();
    let report = rt.application.memory_service.expire(false).await.unwrap();
    assert!(report.expired.is_empty());
}

#[tokio::test]
async fn consolidation_keeps_the_newest_copy_and_deletes_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with_retention(dir.path(), 0).await;
    let repo = MemoryRepo::new(&rt.sqlite);
    let text = "The federated planner records an auditable rejection chain for every source.";
    let old_copy = entry("default", "planner notes", text, 9);
    let new_copy = entry("default", "planner notes", text, 1);
    let distinct = entry(
        "default",
        "other notes",
        "A completely different memory body lives here.",
        2,
    );
    repo.insert(&old_copy).unwrap();
    repo.insert(&new_copy).unwrap();
    repo.insert(&distinct).unwrap();

    // Dry run first: nothing deleted.
    let dry = rt
        .application
        .memory_service
        .consolidate(false)
        .await
        .unwrap();
    assert_eq!(dry.merged_groups, 1);
    assert_eq!(dry.removed_duplicates, 1);
    assert_eq!(repo.list_all().unwrap().len(), 3);

    let applied = rt
        .application
        .memory_service
        .consolidate(true)
        .await
        .unwrap();
    assert!(applied.applied);
    let remaining = repo.list_all().unwrap();
    assert_eq!(remaining.len(), 2, "the older duplicate was removed");
    assert!(remaining.iter().any(|e| e.id == new_copy.id));
    assert!(remaining.iter().any(|e| e.id == distinct.id));
    assert!(!remaining.iter().any(|e| e.id == old_copy.id));
}

#[tokio::test]
async fn identical_content_in_different_workspaces_is_not_merged() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with_retention(dir.path(), 0).await;
    let repo = MemoryRepo::new(&rt.sqlite);
    let text = "Workspace-scoped knowledge must never be merged across workspaces.";
    repo.insert(&entry("ws_a", "note", text, 1)).unwrap();
    repo.insert(&entry("ws_b", "note", text, 1)).unwrap();
    let report = rt
        .application
        .memory_service
        .consolidate(true)
        .await
        .unwrap();
    assert_eq!(report.removed_duplicates, 0, "isolation beats dedupe");
    assert_eq!(repo.list_all().unwrap().len(), 2);
}
