//! RFC-033 Online Feedback acceptance gates.
//!
//! The learning loop only closes if a human judgement survives as a graded
//! qrel that the evaluator can consume. These gates run the real service and
//! the real SQLite projection.
//!
//! Named gates:
//!   feedback_is_append_only_and_workspace_scoped
//!   disputed_feedback_never_becomes_a_qrel
//!   feedback_dataset_feeds_the_evaluator

use searchos_application::feedback::{FeedbackService, FeedbackVerdict, NewFeedback};

fn input(ws: &str, query: &str, url: &str, verdict: FeedbackVerdict) -> NewFeedback {
    NewFeedback {
        workspace_id: ws.into(),
        task_id: None,
        query: query.into(),
        url: url.into(),
        verdict,
        note: None,
    }
}

async fn service(
    dir: &tempfile::TempDir,
) -> (
    std::sync::Arc<searchos_core::SearchOsRuntime>,
    std::sync::Arc<FeedbackService>,
) {
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let feedback = runtime
        .application
        .feedback_service
        .as_ref()
        .unwrap()
        .clone();
    (runtime, feedback)
}

#[tokio::test]
async fn feedback_is_append_only_and_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, feedback) = service(&dir).await;

    feedback
        .submit(input(
            "ws_a",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Helpful,
        ))
        .await
        .unwrap();
    feedback
        .submit(input(
            "ws_a",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Helpful,
        ))
        .await
        .unwrap();
    feedback
        .submit(input(
            "ws_b",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Wrong,
        ))
        .await
        .unwrap();

    let a = feedback.list("ws_a", 50).await.unwrap();
    let b = feedback.list("ws_b", 50).await.unwrap();
    assert_eq!(a.len(), 2, "a second judgement is a NEW row, not an edit");
    assert_eq!(b.len(), 1, "workspaces never see each other's feedback");

    let qrels = feedback.qrels("ws_a", 2).await.unwrap();
    assert_eq!(qrels.accepted.len(), 1);
    assert_eq!(qrels.accepted[0].grade, 2);
    assert!(qrels.excluded.is_empty());
}

#[tokio::test]
async fn disputed_feedback_never_becomes_a_qrel() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, feedback) = service(&dir).await;

    feedback
        .submit(input(
            "ws",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Canonical,
        ))
        .await
        .unwrap();
    feedback
        .submit(input("ws", "q", "https://a.test/x", FeedbackVerdict::Wrong))
        .await
        .unwrap();
    // A second canonical vote does not paper over the contradiction.
    feedback
        .submit(input(
            "ws",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Canonical,
        ))
        .await
        .unwrap();

    let qrels = feedback.qrels("ws", 2).await.unwrap();
    assert!(qrels.accepted.is_empty());
    assert_eq!(qrels.excluded.len(), 1);
    assert_eq!(
        qrels.excluded[0].reason,
        searchos_application::feedback::ExclusionReason::Disputed
    );
    assert_eq!(qrels.excluded[0].grades, vec![3, 0, 3]);
}

#[tokio::test]
async fn feedback_survives_a_runtime_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = service(&dir).await;
    let event = first
        .application
        .feedback_service
        .as_ref()
        .unwrap()
        .submit(input(
            "ws",
            "q",
            "https://a.test/x",
            FeedbackVerdict::Helpful,
        ))
        .await
        .unwrap();
    drop(first);

    let second = searchos_core::SearchOsRuntime::init({
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg
    })
    .await
    .unwrap();
    let stored = second
        .application
        .feedback_service
        .as_ref()
        .unwrap()
        .list("ws", 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, event.id);
}

#[tokio::test]
async fn feedback_dataset_feeds_the_evaluator() {
    // The aggregate is pure; feed its dataset through the real evaluator
    // stub to prove the qrels are consumable, without a live search.
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, feedback) = service(&dir).await;
    for _ in 0..2 {
        feedback
            .submit(input(
                "ws",
                "q",
                "https://a.test/canonical",
                FeedbackVerdict::Canonical,
            ))
            .await
            .unwrap();
        feedback
            .submit(input(
                "ws",
                "q",
                "https://a.test/related",
                FeedbackVerdict::Related,
            ))
            .await
            .unwrap();
    }
    let dataset = feedback.dataset("ws", 2).await.unwrap();
    assert_eq!(dataset.questions.len(), 1);
    assert_eq!(dataset.questions[0].relevance_grades.len(), 2);
    assert_eq!(
        dataset.questions[0].relevant_urls,
        vec!["https://a.test/canonical".to_string()]
    );

    // The JSONL shape must be accepted by `EvaluationDataset::load_jsonl`.
    let dir2 = tempfile::tempdir().unwrap();
    let path = dir2.path().join("feedback.jsonl");
    let mut jsonl = String::new();
    for q in &dataset.questions {
        jsonl.push_str(&serde_json::to_string(q).unwrap());
        jsonl.push('\n');
    }
    std::fs::write(&path, jsonl).unwrap();
    let loaded = searchos_evaluation::dataset::EvaluationDataset::load_jsonl(&path).unwrap();
    // The loader derives dataset_id from the FILE NAME (not a field in the
    // JSONL), so assert on the qrels that actually crossed the boundary.
    assert_eq!(loaded.questions.len(), 1);
    assert_eq!(loaded.questions[0].relevance_grades[0].grade, 3);

    // The append-only SQLite projection is already proven by
    // `feedback_is_append_only_and_workspace_scoped` and the restart gate.
}
