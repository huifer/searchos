//! Deep-search research runner (RFC-071).
//!
//! A research task is a SESSION that accumulates evidence across bounded
//! rounds. One round is a full search (own task/trace); the runner re-targets
//! the final gate's open requirements as `focus_requirements` until coverage
//! is complete, the round cap is reached, or the TOTAL budget is exhausted.
//!
//! The workspace itself (turns + accumulated evidence) lives in the durable
//! session store; this runner only decides when to stop.

use crate::search_service::SearchService;
use crate::session::{ResearchStopReason, SearchSession, SessionStore};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::response::SearchResponse;
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One round of a deep-search run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRound {
    pub round: u32,
    pub query: String,
    pub trace_id: String,
    pub status: String,
    pub quality: String,
    pub decision_path: String,
    pub result_count: usize,
    pub open_requirements: usize,
    /// Cost of THIS round (estimated micros).
    pub spent_micros: i64,
}

/// The bounded deep-search run's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRun {
    pub session_id: String,
    pub rounds: Vec<ResearchRound>,
    /// Accumulated workspace evidence count after the run.
    pub evidence_count: usize,
    /// Open requirements after the LAST round.
    pub open_requirements: Vec<searchos_domain::decision::CoverageGap>,
    pub covered: bool,
    pub stopped: ResearchStopReason,
    pub total_spent_micros: i64,
    /// The final round's full response (the caller's primary result).
    pub final_response: SearchResponse,
}

/// Deep-search runner over the assembled search service.
pub struct ResearchService {
    search: Arc<dyn SearchService>,
    sessions: Arc<SessionStore>,
}

impl ResearchService {
    pub fn new(search: Arc<dyn SearchService>, sessions: Arc<SessionStore>) -> Self {
        Self { search, sessions }
    }

    /// Runs a bounded deep search. `max_rounds` caps the calls; the ORIGINAL
    /// request budget caps the TOTAL spend across rounds (a round that would
    /// exceed it is not started).
    pub async fn deep_search(
        &self,
        mut req: SearchRequest,
        session_id: &str,
        max_rounds: u32,
    ) -> Result<ResearchRun, SearchOsError> {
        let max_rounds = max_rounds.clamp(1, 8);
        req.session_id = Some(session_id.to_string());
        req.budget.max_acquisition_rounds = req.budget.max_acquisition_rounds.max(1);
        let budget_cap = req.budget.max_total_cost_micros;

        let mut rounds: Vec<ResearchRound> = Vec::new();
        let mut total_spent = 0i64;
        let mut response: Option<SearchResponse> = None;
        let mut stopped = ResearchStopReason::RoundCap;
        let mut focus: Vec<String> = Vec::new();
        let mut next_request = req.clone();
        // Ensure the session exists BEFORE the first round so the workspace
        // id is stable even if the first search fails.
        self.sessions
            .create_or_get(session_id, next_request.workspace_id.as_str());

        for round in 1..=max_rounds {
            if round > 1 {
                if total_spent >= budget_cap {
                    stopped = ResearchStopReason::BudgetExhausted;
                    break;
                }
                next_request = next_request
                    .with_request_id(format!("research_{}", searchos_domain::new_id("rnd")));
                next_request.focus_requirements = focus.clone();
            }
            let resp = self.search.search(next_request.clone()).await?;
            let spent = (resp.cost.estimated * 1_000_000.0).round() as i64;
            total_spent = total_spent.saturating_add(spent);
            let open: Vec<searchos_domain::decision::CoverageGap> = resp.coverage_gaps.clone();
            rounds.push(ResearchRound {
                round,
                query: resp.query.clone(),
                trace_id: resp.trace_id.to_string(),
                status: resp.status.as_str().to_string(),
                quality: resp.quality.status.as_str().to_string(),
                decision_path: resp.decision.path.clone(),
                result_count: resp.results.len(),
                open_requirements: open.len(),
                spent_micros: spent,
            });
            let covered = open.is_empty();
            focus = open.iter().map(|g| g.question.clone()).collect();
            response = Some(resp);
            if covered {
                stopped = ResearchStopReason::Covered;
                break;
            }
            if total_spent >= budget_cap {
                stopped = ResearchStopReason::BudgetExhausted;
            }
        }

        let final_response = response
            .ok_or_else(|| SearchOsError::Internal("research run produced no round".into()))?;
        let session: SearchSession = self.sessions.get(session_id).unwrap_or_else(|| {
            self.sessions
                .create_or_get(session_id, req.workspace_id.as_str())
        });
        // Coverage is judged from the LAST ROUND's final gate. The session's
        // workspace view is bookkeeping; a transport-level session may lag
        // (or be driven by a stub), so it must not define "covered".
        let open_requirements = final_response.coverage_gaps.clone();
        Ok(ResearchRun {
            session_id: session_id.to_string(),
            rounds,
            evidence_count: session.evidence.len(),
            covered: open_requirements.is_empty(),
            open_requirements,
            stopped,
            total_spent_micros: total_spent,
            final_response,
        })
    }
}

/// Builds a request for a research run from transport-friendly inputs.
#[allow(clippy::too_many_arguments)]
pub fn research_request(
    query: &str,
    profile: &str,
    mode: SearchMode,
    workspace: &str,
    max_total_cost_micros: i64,
    max_external_queries: u32,
) -> Result<SearchRequest, SearchOsError> {
    let mut req = SearchRequest::new(
        query,
        ProfileId::new(profile),
        mode,
        WorkspaceId::new(workspace),
    )?;
    req.budget.max_total_cost_micros = max_total_cost_micros;
    req.budget.max_external_queries = max_external_queries;
    Ok(req)
}
