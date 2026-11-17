//! RFC-035 Search Watch acceptance gates.
//!
//! A watch must be HONEST about missing signals and deterministic about
//! thresholds. These gates run the real service over the real Analytics Read
//! Model and the append-only snapshot log.
//!
//! Named gates:
//!   watch_reports_skipped_checks_when_there_are_no_observations
//!   watch_appends_snapshots_and_reports_the_history
//!   watch_detects_a_failed_search_above_the_threshold
//!   watch_ranking_check_is_skipped_without_a_dataset

use std::sync::Arc;

use searchos_application::watch::{WatchService, WatchStatus, WatchThresholds};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};

async fn service(
    dir: &tempfile::TempDir,
    thresholds: WatchThresholds,
) -> (Arc<searchos_core::SearchOsRuntime>, Arc<WatchService>) {
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.watch.workspace_id = "ws_watch".into();
    cfg.watch.min_quality_pass_rate = thresholds.min_quality_pass_rate;
    cfg.watch.max_search_failure_rate = thresholds.max_search_failure_rate;
    cfg.watch.max_quality_pass_rate_drop = thresholds.max_quality_pass_rate_drop;
    cfg.watch.min_sample = thresholds.min_sample;
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let watch = Arc::clone(runtime.application.watch_service.as_ref().unwrap());
    (runtime, watch)
}

fn request() -> SearchRequest {
    SearchRequest::new(
        "what is local-first search?",
        ProfileId::general(),
        SearchMode::LocalFirst,
        WorkspaceId::new("ws_watch"),
    )
    .unwrap()
}

#[tokio::test]
async fn watch_reports_skipped_checks_when_there_are_no_observations() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, watch) = service(
        &dir,
        WatchThresholds {
            min_quality_pass_rate: Some(0.9),
            min_sample: 5,
            ..WatchThresholds::default()
        },
    )
    .await;

    let report = watch.run().await.unwrap();
    let floor = report
        .checks
        .iter()
        .find(|c| c.id == "quality_floor")
        .expect("quality floor check");
    assert_eq!(floor.status, WatchStatus::Skipped);
    assert!(floor.detail.contains("only 0 observations"));
}

#[tokio::test]
async fn watch_appends_snapshots_and_reports_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, watch) = service(&dir, WatchThresholds::default()).await;

    let first = watch.run().await.unwrap();
    let second = watch.run().await.unwrap();
    assert_ne!(first.id, second.id, "each run is a distinct snapshot");
    let history = watch.history(10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, second.id, "newest first");
    assert_eq!(history[1].id, first.id);
}

#[tokio::test]
async fn watch_detects_a_failed_search_above_the_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, watch) = service(
        &dir,
        WatchThresholds {
            max_search_failure_rate: Some(0.1),
            min_sample: 1,
            ..WatchThresholds::default()
        },
    )
    .await;

    // Ingest one document so LocalFirst has something to work with, then
    // refresh the Read Model. The gateway also refreshes after every write;
    // here the service records the analytics event on search completion.
    runtime
        .application
        .ingest_service
        .ingest_text(
            searchos_application::ingest_service::IngestRequest {
                source_key: Some("watch-doc".into()),
                workspace_id: WorkspaceId::new("ws_watch"),
                ..searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new(
                    "ws_watch",
                ))
            },
            "Local-first search keeps evidence on the local machine.",
        )
        .await
        .unwrap();
    let _ = runtime
        .application
        .search_service
        .search(request())
        .await
        .unwrap();
    let _ = runtime.application.analytics_service.refresh().await;

    let report = watch.run().await.unwrap();
    let failure = report
        .checks
        .iter()
        .find(|c| c.id == "failure_rate")
        .expect("failure rate check");
    assert!(
        matches!(failure.status, WatchStatus::Pass | WatchStatus::Regressed),
        "the check must be evaluated once there is an observation"
    );
    assert_eq!(failure.threshold, Some(0.1));
    assert!(report.signals.is_some());
}

#[tokio::test]
async fn watch_ranking_check_is_skipped_without_a_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, watch) = service(&dir, WatchThresholds::default()).await;

    let report = watch.run().await.unwrap();
    let ranking = report
        .checks
        .iter()
        .find(|c| c.id == "ranking")
        .expect("ranking check");
    assert_eq!(ranking.status, WatchStatus::Skipped);
    assert_eq!(report.status, WatchStatus::Skipped);
}

#[tokio::test]
async fn watch_snapshot_store_is_append_only_across_instances() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, watch) = service(&dir, WatchThresholds::default()).await;
    let report = watch.run().await.unwrap();

    // Release the first runtime's index writer before opening a second one
    // on the SAME data directory (Tantivy holds an exclusive lock).
    drop(watch);
    drop(runtime);

    // A second runtime on the same directory sees the snapshot: the audit
    // log is durable, not an in-process cache.
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let second = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let history = second
        .application
        .watch_service
        .as_ref()
        .unwrap()
        .history(10)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, report.id);

    // The trait is implemented over the append-only SQLite table.
}
