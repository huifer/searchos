//! RFC-034 versioned policy management acceptance gates.

use std::sync::Arc;

use searchos_application::policy::PolicyKind;
use searchos_core::SearchOsConfig;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_policy::QualityPolicy;
use searchos_storage_sqlite::repos::PolicyRepo;

async fn runtime_on(dir: &std::path::Path) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

async fn seed(rt: &Arc<searchos_core::SearchOsRuntime>) {
    let mut req =
        searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/policy".into());
    rt.application
        .ingest_service
        .ingest_text(
            req,
            "Policy management versions quality thresholds and reloads them at runtime.",
        )
        .await
        .unwrap();
    rt.process_outbox(256).await.unwrap();
}

fn request(query: &str) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        SearchMode::LocalFirst,
        WorkspaceId::new("default"),
    )
    .unwrap()
}

#[tokio::test]
async fn policy_history_is_append_only_with_one_active_version() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    let repo = PolicyRepo::new(&rt.sqlite);

    // v1 is seeded from the deployed policy.
    let initial = repo.active("quality").unwrap().unwrap();
    assert_eq!(initial.version, 1);
    assert_eq!(initial.status, "active");

    let stricter = QualityPolicy {
        minimum_quality: 0.95,
        ..Default::default()
    };
    let payload = serde_json::to_string(&stricter).unwrap();
    let v2 = repo
        .submit("quality", &payload, Some("stricter gate"))
        .unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(v2.notes.as_deref(), Some("stricter gate"));

    let active = repo.active("quality").unwrap().unwrap();
    assert_eq!(active.version, 2);
    let history = repo.versions("quality").unwrap();
    assert_eq!(history.len(), 2, "history is never deleted");
    assert!(history.iter().filter(|r| r.status == "active").count() == 1);

    // Rollback reactivates v1 but keeps v2 in the audit trail.
    let rolled = repo.activate("quality", 1).unwrap().unwrap();
    assert_eq!(rolled.version, 1);
    assert_eq!(repo.active("quality").unwrap().unwrap().version, 1);
    assert_eq!(repo.versions("quality").unwrap().len(), 2);
    assert!(repo.activate("quality", 99).unwrap().is_none());

    // Other kinds are untouched by a quality submit.
    assert_eq!(repo.active("freshness").unwrap().unwrap().version, 1);
    assert_eq!(repo.active("source").unwrap().unwrap().version, 1);
}

#[tokio::test]
async fn submitting_a_policy_hot_reloads_the_quality_gate_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(&rt).await;

    let before = rt
        .application
        .search_service
        .search(request("Policy management versions quality thresholds"))
        .await
        .unwrap();
    assert_eq!(before.quality.status.as_str(), "passed");

    let stricter = QualityPolicy {
        minimum_quality: 1.0,
        minimum_distinct_sources: 8,
        ..rt.policy_state.quality()
    };
    let payload = serde_json::to_string(&stricter).unwrap();
    let row = PolicyRepo::new(&rt.sqlite)
        .submit("quality", &payload, Some("runtime hot reload"))
        .unwrap();
    rt.activate_policy(PolicyKind::Quality, &row).await.unwrap();

    // A fresh request id + the new policy stamp must make the search
    // recompute under the LIVE policy (no restart, no stale cache hit).
    let after = rt
        .application
        .search_service
        .search(request("Policy management versions quality thresholds"))
        .await
        .unwrap();
    assert_ne!(
        after.quality.status.as_str(),
        "passed",
        "the stricter policy is live: {:?}",
        after.quality
    );
    assert!(
        !after.replayed,
        "the policy stamp invalidated the old answer"
    );

    // Rollback restores the original verdict.
    let v1 = PolicyRepo::new(&rt.sqlite)
        .activate("quality", 1)
        .unwrap()
        .unwrap();
    rt.activate_policy(PolicyKind::Quality, &v1).await.unwrap();
    let restored = rt
        .application
        .search_service
        .search(request("Policy management versions quality thresholds"))
        .await
        .unwrap();
    assert_eq!(restored.quality.status.as_str(), "passed");
}

#[tokio::test]
async fn active_policy_survives_a_runtime_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let rt = runtime_on(dir.path()).await;
        let stricter = QualityPolicy {
            minimum_quality: 0.93,
            ..rt.policy_state.quality()
        };
        let payload = serde_json::to_string(&stricter).unwrap();
        let row = PolicyRepo::new(&rt.sqlite)
            .submit("quality", &payload, Some("persisted"))
            .unwrap();
        rt.activate_policy(PolicyKind::Quality, &row).await.unwrap();
    }
    // Same data directory: the ACTIVE row wins over the config default.
    let restarted = runtime_on(dir.path()).await;
    assert!((restarted.policy_state.quality().minimum_quality - 0.93).abs() < 1e-6);
    assert_eq!(
        PolicyRepo::new(&restarted.sqlite)
            .versions("quality")
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn replay_and_simulation_consume_the_current_policy() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(&rt).await;
    let resp = rt
        .application
        .search_service
        .search(request("Policy management versions quality thresholds"))
        .await
        .unwrap();
    let plain = rt
        .replay
        .replay(resp.trace_id.as_str(), None)
        .await
        .unwrap();
    assert_eq!(plain.replayed.quality_status, "Passed");

    // A persisted policy change is visible to the offline replay too:
    // 034 and 075 share one policy source of truth.
    let stricter = QualityPolicy {
        minimum_distinct_sources: 8,
        ..rt.policy_state.quality()
    };
    let payload = serde_json::to_string(&stricter).unwrap();
    let row = PolicyRepo::new(&rt.sqlite)
        .submit("quality", &payload, Some("read by replay"))
        .unwrap();
    rt.activate_policy(PolicyKind::Quality, &row).await.unwrap();
    let after = rt
        .replay
        .replay(resp.trace_id.as_str(), None)
        .await
        .unwrap();
    assert_ne!(after.replayed.quality_status, "Passed");
}
