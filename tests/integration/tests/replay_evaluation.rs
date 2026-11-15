//! RFC-040 Replay / RFC-075 Policy Simulation / RFC-086 Evaluation
//! acceptance gates over the assembled runtime.

use std::sync::Arc;

use searchos_application::ingest_service::IngestRequest;
use searchos_application::replay::PolicyOverride;
use searchos_core::SearchOsConfig;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};

async fn runtime_on(dir: &std::path::Path) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

async fn seed_knowledge(rt: &Arc<searchos_core::SearchOsRuntime>) {
    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/replay".into());
    rt.application
        .ingest_service
        .ingest_text(
            req,
            "SearchOS replay reconstructs a recorded request offline and evaluates policy changes without billing providers.",
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
async fn replay_reconstructs_the_recorded_request_offline() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed_knowledge(&rt).await;
    let resp = rt
        .application
        .search_service
        .search(request(
            "SearchOS replay reconstructs a recorded request offline",
        ))
        .await
        .unwrap();

    let outcome = rt
        .replay
        .replay(resp.trace_id.as_str(), None)
        .await
        .unwrap();
    assert_eq!(
        outcome.query,
        "SearchOS replay reconstructs a recorded request offline"
    );
    assert_eq!(outcome.trace_id, resp.trace_id.as_str());
    assert!(!outcome.task_id.is_empty());
    assert!(outcome.offline, "no provider is ever called by a replay");
    assert!(!outcome.policy_applied);
    assert!(outcome.baseline.is_none());
    assert_eq!(
        outcome.original.decision_path, outcome.replayed.decision_path,
        "with unchanged knowledge and policy the decision must agree"
    );
    assert!(!outcome.decision_changed);
    assert!(!outcome.quality_changed);
    assert!(!outcome.notes.is_empty(), "offline caveats are stated");
}

#[tokio::test]
async fn unknown_trace_is_an_explicit_error() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    let err = rt
        .replay
        .replay("trace_does_not_exist", None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("trace_id") || err.to_string().contains("UNKNOWN"),
        "{err}"
    );
}

#[tokio::test]
async fn policy_simulation_uses_a_baseline_and_applies_the_override() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed_knowledge(&rt).await;
    let resp = rt
        .application
        .search_service
        .search(request(
            "SearchOS replay reconstructs a recorded request offline and evaluates policy changes",
        ))
        .await
        .unwrap();

    // An impossible quality bar changes the quality verdict but must still
    // be an OFFLINE, non-billing, non-writing simulation.
    let simulated = rt
        .replay
        .replay(
            resp.trace_id.as_str(),
            Some(PolicyOverride {
                minimum_quality: Some(1.0),
                minimum_coverage: Some(1.0),
                minimum_distinct_sources: Some(8),
            }),
        )
        .await
        .unwrap();
    assert!(simulated.policy_applied);
    let baseline = simulated.baseline.as_ref().expect("baseline run recorded");
    assert!(simulated.offline);
    assert!(
        simulated.replayed.quality_status != baseline.quality_status,
        "the override must be visible: baseline={:?} simulated={:?}",
        baseline.quality_status,
        simulated.replayed.quality_status
    );
    assert!(simulated.quality_changed);
    // The stricter policy can also push the DECISION out of the local
    // path; offline that divergence is reported honestly as a rejection,
    // never disguised as a success (decision red lines).
    assert!(
        simulated.quality_changed || simulated.decision_changed,
        "a stricter policy must change at least one observable outcome"
    );
    assert!(
        simulated.replayed.decision_path == "reject"
            || simulated.replayed.decision_path == baseline.decision_path,
        "offline divergence is honest: {:?}",
        simulated.replayed
    );
}

#[tokio::test]
async fn evaluation_aggregates_recent_replays() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    for q in [
        "evaluation question one",
        "evaluation question two",
        "evaluation question three",
    ] {
        rt.application
            .search_service
            .search(request(q))
            .await
            .unwrap();
    }
    let report = rt.replay.evaluate(20, Some("default"), None).await.unwrap();
    assert!(
        report.evaluated >= 3,
        "all recent traces replayed: {report:?}"
    );
    assert!(report.per_trace.iter().all(|o| o.offline));
    assert!(!report.policy_applied);
    assert!(report.decision_agreement_rate >= 0.0 && report.decision_agreement_rate <= 1.0);
    assert_eq!(
        report.decision_changes,
        report
            .per_trace
            .iter()
            .filter(|o| o.decision_changed)
            .count()
    );
}
