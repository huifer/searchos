//! RFC-071 research workspace acceptance gates: deep-search rounds, stop
//! conditions, accumulated evidence, and workspace persistence.

use std::sync::Arc;

use searchos_application::ingest_service::IngestRequest;
use searchos_application::research::research_request;
use searchos_core::SearchOsConfig;
use searchos_domain::request::SearchMode;

async fn runtime_on(dir: &std::path::Path) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

async fn seed(rt: &Arc<searchos_core::SearchOsRuntime>, key: &str, text: &str) {
    let mut req = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
    req.source_key = Some(key.into());
    rt.application
        .ingest_service
        .ingest_text(req, text)
        .await
        .unwrap();
    rt.process_outbox(256).await.unwrap();
}

#[tokio::test]
async fn deep_search_records_rounds_and_stops_when_covered() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(
        &rt,
        "notes/research",
        "The research workspace accumulates evidence across bounded deep-search rounds.",
    )
    .await;

    let req = research_request(
        "research workspace accumulated evidence",
        "general",
        SearchMode::LocalFirst,
        "default",
        100_000,
        5,
    )
    .unwrap();
    let run = rt
        .research
        .deep_search(req, "research_gate_one", 3)
        .await
        .unwrap();

    assert_eq!(run.session_id, "research_gate_one");
    assert!(!run.rounds.is_empty());
    assert_eq!(run.rounds[0].round, 1);
    assert_eq!(run.rounds[0].decision_path, "local");
    assert!(
        run.covered || run.stopped == searchos_application::session::ResearchStopReason::RoundCap,
        "run stops honestly: {:?}",
        run.stopped
    );
    assert!(
        run.evidence_count > 0,
        "the workspace accumulated the served evidence"
    );
    // The workspace the caller inspects later carries the same evidence.
    let workspace = rt.session_store.get("research_gate_one").unwrap();
    assert!(!workspace.turns.is_empty());
    assert_eq!(workspace.evidence.len(), run.evidence_count);
    assert!(workspace
        .evidence
        .iter()
        .any(|e| e.url.contains("notes/research")));
}

#[tokio::test]
async fn evidence_accumulates_across_turns_and_dedupes_by_url() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    seed(
        &rt,
        "notes/alpha",
        "Alpha note about the research workspace accumulation.",
    )
    .await;

    for _ in 0..2 {
        let req = research_request(
            "research workspace accumulation",
            "general",
            SearchMode::LocalFirst,
            "default",
            100_000,
            2,
        )
        .unwrap();
        rt.research
            .deep_search(req, "research_gate_two", 1)
            .await
            .unwrap();
    }
    let workspace = rt.session_store.get("research_gate_two").unwrap();
    assert!(workspace.turns.len() >= 2, "two turns recorded");
    let urls: Vec<&str> = workspace.evidence.iter().map(|e| e.url.as_str()).collect();
    let mut deduped = urls.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(urls.len(), deduped.len(), "evidence is deduped by URL");
}

/// Deterministic stub: every round keeps ONE open requirement so the runner
/// must stop on its own conditions (round cap / total budget).
struct StubSearch {
    cost_micros: f64,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl searchos_application::search_service::SearchService for StubSearch {
    async fn search(
        &self,
        req: searchos_domain::request::SearchRequest,
    ) -> Result<searchos_domain::response::SearchResponse, searchos_domain::SearchOsError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let gap = searchos_domain::decision::CoverageGap {
            question: format!("still open: {}", req.focus_requirements.join(" ")),
            missing_kind: "evidence".into(),
            status: searchos_domain::decision::RequirementStatus::Missing,
            priority: searchos_domain::decision::GapPriority::Normal,
            strategy: searchos_domain::decision::GapResolutionStrategy::SearchOfficialSource,
            requirement_id: "gap".into(),
        };
        Ok(searchos_domain::response::SearchResponse {
            query: req.query.clone(),
            decision: searchos_domain::response::DecisionReport {
                path: "hybrid".into(),
                reason: "local_coverage_insufficient".into(),
            },
            results: Vec::new(),
            quality: searchos_domain::response::QualityReport {
                status: searchos_domain::response::QualityStatus::Partial,
                freshness: searchos_domain::response::FreshnessStatus::Verified,
                coverage: searchos_domain::response::CoverageStatus::Partial,
                score: 0.5,
                failed_constraints: Vec::new(),
            },
            cost: searchos_domain::response::CostReport {
                estimated: self.cost_micros / 1_000_000.0,
                actual: None,
                currency: "USD".into(),
            },
            provenance: Vec::new(),
            trace_id: searchos_domain::ids::TraceId::generate(),
            status: searchos_domain::response::SearchStatus::Partial,
            warnings: Vec::new(),
            task_id: None,
            replayed: false,
            coverage_gaps: vec![gap],
            session_id: req.session_id.clone(),
            focus_requirements: req.focus_requirements.clone(),
            context: None,
            evidence_graph: None,
        })
    }

    async fn plan(
        &self,
        req: &searchos_domain::request::SearchRequest,
    ) -> Result<
        searchos_application::search_service::SearchPlanReport,
        searchos_domain::SearchOsError,
    > {
        let _ = req;
        unimplemented!("stub plan")
    }
}

#[tokio::test]
async fn the_runner_stops_on_the_round_cap() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    let stub = Arc::new(StubSearch {
        cost_micros: 0.0,
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let runner = searchos_application::research::ResearchService::new(
        stub.clone(),
        rt.session_store.clone(),
    );
    let req = research_request(
        "always open research loop",
        "general",
        SearchMode::LocalFirst,
        "default",
        100_000,
        5,
    )
    .unwrap();
    let run = runner
        .deep_search(req, "research_stub_cap", 3)
        .await
        .unwrap();
    assert_eq!(run.rounds.len(), 3, "the round cap stops the loop");
    assert_eq!(
        run.stopped,
        searchos_application::session::ResearchStopReason::RoundCap
    );
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(run.rounds[1].query, "always open research loop");
    assert!(!run.open_requirements.is_empty());
}

#[tokio::test]
async fn the_runner_stops_when_the_total_budget_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;
    let stub = Arc::new(StubSearch {
        cost_micros: 60_000.0,
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let runner = searchos_application::research::ResearchService::new(
        stub.clone(),
        rt.session_store.clone(),
    );
    let req = research_request(
        "expensive open research loop",
        "general",
        SearchMode::LocalFirst,
        "default",
        100_000,
        5,
    )
    .unwrap();
    let run = runner
        .deep_search(req, "research_stub_budget", 8)
        .await
        .unwrap();
    assert!(
        run.rounds.len() < 8,
        "budget stops before the round cap: {:?}",
        run.rounds.len()
    );
    assert_eq!(
        run.stopped,
        searchos_application::session::ResearchStopReason::BudgetExhausted
    );
    assert!(run.total_spent_micros >= 100_000);
}

#[tokio::test]
async fn workspace_survives_a_runtime_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let rt = runtime_on(dir.path()).await;
        seed(
            &rt,
            "notes/persist",
            "Persistent research workspace evidence note.",
        )
        .await;
        let req = research_request(
            "persistent research workspace evidence",
            "general",
            SearchMode::LocalFirst,
            "default",
            100_000,
            2,
        )
        .unwrap();
        rt.research
            .deep_search(req, "research_gate_persist", 1)
            .await
            .unwrap();
    }
    let rt = runtime_on(dir.path()).await;
    let workspace = rt.session_store.get("research_gate_persist").unwrap();
    assert_eq!(workspace.turns.len(), 1);
    assert!(
        !workspace.evidence.is_empty(),
        "accumulated evidence persisted with the session"
    );
}
