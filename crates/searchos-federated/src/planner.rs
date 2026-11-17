//! Federated Search Planner (RFC-062 §7).
//!
//! Decides **which sources to query, in what order, under what budget**. This
//! is pure logic: no execution, no I/O, no provider calls, no LLM. That is
//! what makes the decision auditable and replayable (P-011 Explainable
//! Decision) and what lets the acceptance tests assert the invariants directly.
//!
//! Planning order is mandatory and each rejection is recorded with a reason:
//!
//! ```text
//! Policy → Permission → Scope → Capability → Existing Evidence → Cost → Reliability → Select
//! ```
//!
//! Load-bearing invariants:
//!
//! * `SearchMode` is the **permission boundary**. A `FederationStrategy` can
//!   never widen it (ADR-062-008): `local_only` cannot reach a web source,
//!   `external_only` cannot be satisfied by a local source.
//! * Fail-open is an availability property, never a quality verdict
//!   (ADR-062-004 / INV-FED-01). A required source that is unavailable
//!   produces `missing_requirements` + `failed_sources` and a `partial`
//!   outcome — it does not silently degrade to "we found something else".
//! * The planner never fans out to every registered source by default
//!   (ADR-062-002).
//! * Already-covered requirements are never re-purchased (RFC-062 §12.2).

use crate::source::{
    SearchSourceDescriptor, SourceCandidate, SourceCategory, SourceCostProfile,
    SourceExecutionStatus, SourceSearchResponse,
};
use searchos_domain::ids::WorkspaceId;
use searchos_domain::SearchMode;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

fn default_stop_when_covered() -> bool {
    true
}

fn default_max_latency_ms() -> u64 {
    // Matches `SearchBudget::default().max_execution_time_ms`.
    30_000
}

/// How the selected sources are executed. Orthogonal to `SearchMode`
/// (ADR-062-008): strategy chooses *shape*, mode chooses *permission*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationStrategy {
    /// Planner picks legal sources for the open requirements.
    #[default]
    Auto,
    /// Execute the selected sources concurrently.
    Parallel,
    /// Execute in priority order, stopping as soon as coverage is satisfied.
    Sequential,
    /// Only sources that can fill a still-open requirement are selected.
    Selective,
}

impl FederationStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Parallel => "parallel",
            Self::Sequential => "sequential",
            Self::Selective => "selective",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "parallel" => Ok(Self::Parallel),
            "sequential" => Ok(Self::Sequential),
            "selective" => Ok(Self::Selective),
            other => Err(format!(
                "unknown federation strategy {other}; use auto|parallel|sequential|selective"
            )),
        }
    }

    /// Whether the strategy may execute more than one source at a time.
    /// `Parallel` does **not** mean "every registered source".
    pub fn fans_out(&self) -> bool {
        !matches!(self, Self::Sequential)
    }
}

/// RFC-080: how selected sources are ORDERED.
///
/// This is deliberately orthogonal to [`FederationStrategy`]: `strategy`
/// decides the execution shape (parallel / sequential / selective), while
/// `selection` decides which eligible source the plan tries FIRST. The
/// default keeps the historical reliability/priority order byte-for-byte —
/// cost-adaptive routing is opt-in so no existing contract changes shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    /// Local first, then declared-authority, then configured priority.
    /// The historical order; latency and success rate act as gates only.
    #[default]
    ReliabilityFirst,
    /// Among equally local/authoritative sources, order by EXPECTED QUALITY
    /// PER UNIT COST, not by configured priority: a cheap source with a
    /// higher observed success rate and metadata trust is tried before an
    /// expensive one that merely declares a higher priority.
    CostAdaptive,
}

impl SelectionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReliabilityFirst => "reliability_first",
            Self::CostAdaptive => "cost_adaptive",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "reliability_first" => Ok(Self::ReliabilityFirst),
            "cost_adaptive" => Ok(Self::CostAdaptive),
            other => Err(format!(
                "unknown source selection mode {other}; use reliability_first|cost_adaptive"
            )),
        }
    }
}

/// Inputs the planner reasons over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanningContext {
    pub query: String,
    pub mode: SearchMode,
    pub strategy: FederationStrategy,
    pub workspace_id: String,
    /// Requirement ids the query needs satisfied.
    pub requirements: Vec<String>,
    /// Requirement ids already answered by local/cached evidence. These are
    /// never re-purchased (cost and quality both depend on it).
    pub covered_requirements: Vec<String>,
    /// Requirement ids that MUST be answered by an authoritative source
    /// (e.g. `required_official_source`). Fail-open cannot substitute these.
    pub required_requirements: Vec<String>,
    pub max_sources: usize,
    pub max_parallel_sources: usize,
    /// Total estimated-cost ceiling for the plan, in micro units.
    pub max_estimated_cost_micros: i64,
    /// Wall-clock budget for the whole execution, in milliseconds. A source
    /// whose declared timeout cannot fit inside it is rejected before it is
    /// ever started, and the executor caps every per-source deadline by the
    /// remaining part of this budget (RFC-080 latency budget).
    #[serde(default = "default_max_latency_ms")]
    pub max_latency_ms: u64,
    /// Requirements that must be answered before the plan may stop.
    #[serde(default = "default_stop_when_covered")]
    pub stop_when_covered: bool,
    /// Runtime compatibility flag for adapters registered by SearchOS. A
    /// generic billable provider can answer any open gap only when this is
    /// true; specialized sources declare their requirement ids.
    #[serde(default)]
    pub generic_sources_can_answer: bool,
    /// RFC-080 source ordering. Default keeps the historical order.
    #[serde(default)]
    pub selection: SelectionMode,
}

impl PlanningContext {
    /// Requirements still open after known coverage.
    pub fn open_requirements(&self) -> Vec<String> {
        self.requirements
            .iter()
            .filter(|r| !self.covered_requirements.contains(r))
            .cloned()
            .collect()
    }
}

impl Default for PlanningContext {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::LocalFirst,
            strategy: FederationStrategy::default(),
            workspace_id: "default".into(),
            requirements: Vec::new(),
            covered_requirements: Vec::new(),
            required_requirements: Vec::new(),
            max_sources: 8,
            max_parallel_sources: 8,
            max_estimated_cost_micros: i64::MAX,
            max_latency_ms: default_max_latency_ms(),
            stop_when_covered: true,
            generic_sources_can_answer: true,
            selection: SelectionMode::default(),
        }
    }
}

/// Why a candidate source was rejected. Recorded, never silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The mode permission boundary forbids this source category.
    ModePermission,
    /// Disabled by configuration.
    Disabled,
    /// Registration is not visible to this workspace.
    Scope,
    /// It cannot perform the operation the requirement needs.
    Capability,
    /// Its last known state is a circuit breaker or repeated failure.
    Reliability,
    /// No open requirement maps to what it satisfies.
    NotRelevant,
    /// Its estimated cost does not fit the remaining budget.
    Budget,
    /// Its declared timeout exceeds the request's wall-clock budget.
    Latency,
    /// Every requirement it could answer is already covered.
    AlreadyCovered,
    /// Fan-out limit reached.
    FanOutLimit,
    /// Cost profile is billable and the mode forbids spending.
    CostForbidden,
}

impl RejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModePermission => "mode_permission",
            Self::Disabled => "disabled",
            Self::Scope => "scope",
            Self::Capability => "capability",
            Self::Reliability => "reliability",
            Self::NotRelevant => "not_relevant",
            Self::Budget => "budget",
            Self::Latency => "latency",
            Self::AlreadyCovered => "already_covered",
            Self::FanOutLimit => "fan_out_limit",
            Self::CostForbidden => "cost_forbidden",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedSource {
    pub source_id: String,
    pub reason: RejectionReason,
    pub detail: String,
}

/// One selected source and the requirements it was picked to answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTask {
    pub source_id: String,
    pub category: SourceCategory,
    /// Requirements this task is expected to satisfy.
    pub requirements: Vec<String>,
    /// Whether the source will produce a real external bill.
    pub billable: bool,
    pub estimated_cost_micros: i64,
    /// True when the source cannot answer any requirement and is only being
    /// queried as a fallback filler — the quality gate must not read this as
    /// coverage.
    pub filler: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageExecutionMode {
    Sequential,
    Parallel,
    Adaptive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchStage {
    pub stage_id: String,
    pub execution_mode: StageExecutionMode,
    pub tasks: Vec<SourceTask>,
    /// Stage deadline, derived from the slowest selected source.
    pub timeout_ms: u64,
}

/// A condition under which execution may stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    /// Every open requirement has an answer from an eligible source.
    RequirementsCovered,
    /// Cumulative estimated cost reached the ceiling.
    EstimatedCostExhausted,
    /// The deadline was reached.
    DeadlineReached,
    /// No further source can fill an open requirement.
    NoEligibleSourceRemains,
}

/// The auditable plan.
/// Auditable score for one eligible source under cost-adaptive selection.
/// Present for EVERY eligible source, selected or rejected, so a reviewer
/// can see why the cheaper source went first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSelectionScore {
    pub source_id: String,
    /// 0.0..=1.0: observed success rate blended with declared metadata trust.
    pub expected_quality: f32,
    /// Estimated cost of one execution, in micro units.
    pub cost_micros: i64,
    /// Expected quality per MILLION micro units. Free sources use a capped
    /// density instead of infinity so the value stays serialisable.
    pub value_density: f64,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FederatedSearchPlan {
    pub strategy: FederationStrategy,
    pub mode: SearchMode,
    pub stages: Vec<SearchStage>,
    pub stop_conditions: Vec<StopCondition>,
    /// Full audit trail: why each candidate was rejected.
    pub rejections: Vec<RejectedSource>,
    pub estimated_cost_micros: i64,
    pub max_deadline_ms: u64,
    /// Requirements left with no eligible source. These must surface as
    /// `missing_requirements` in the response, never as a silent pass.
    pub unanswerable_requirements: Vec<String>,
    /// Requirements that are both required and have no eligible source —
    /// the plan cannot produce a quality-passing result.
    pub unmet_required_requirements: Vec<String>,
    /// RFC-080: the ordering policy actually used (auditability).
    #[serde(default)]
    pub selection: SelectionMode,
    /// RFC-080: per-source score for eligible sources, even when not selected.
    #[serde(default)]
    pub selection_scores: Vec<SourceSelectionScore>,
}

impl FederatedSearchPlan {
    /// Selected source ids **in execution order**.
    ///
    /// Order is preserved on purpose: it is part of the auditable decision
    /// (P-011) — the local-first guarantee and the sequential stop point are
    /// both statements about order, and a sorted set would erase them. Use
    /// [`Self::source_set`] when membership is what matters.
    pub fn selected_source_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for stage in &self.stages {
            for task in &stage.tasks {
                if !ids.contains(&task.source_id) {
                    ids.push(task.source_id.clone());
                }
            }
        }
        ids
    }

    /// Selected source ids, sorted and deduplicated.
    pub fn source_set(&self) -> Vec<String> {
        let mut ids = self.selected_source_ids();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn task_count(&self) -> usize {
        self.stages.iter().map(|s| s.tasks.len()).sum()
    }

    /// Whether this plan can possibly satisfy every required requirement.
    pub fn can_satisfy_requirements(&self) -> bool {
        self.unmet_required_requirements.is_empty()
    }
}

/// Blended quality signal for a source (RFC-080 §8.2).
///
/// SUCCESS RATE is the live EMA from observed outcomes, not a declared
/// claim. METADATA TRUST is the declared confidence in the source's own
/// provenance. A source that never returns usable results cannot beat a
/// cheaper reliable one just by promising a high priority.
pub fn expected_quality(descriptor: &SearchSourceDescriptor) -> f32 {
    let success = descriptor.reliability.success_rate.clamp(0.0, 1.0);
    let trust = descriptor.reliability.metadata_trust.clamp(0.0, 1.0);
    (success * (0.5 + 0.5 * trust)).clamp(0.0, 1.0)
}

/// Free executions get the largest finite density; billable ones are scaled
/// to quality per one million micro units so the ratio stays readable.
pub fn value_density(descriptor: &SearchSourceDescriptor) -> f64 {
    let quality = expected_quality(descriptor) as f64;
    let cost = descriptor.cost.estimated_cost_micros.max(0) as f64;
    if cost == 0.0 {
        f64::MAX
    } else {
        quality * 1_000_000.0 / cost
    }
}

/// Deterministic source-selection planner.
#[derive(Debug, Default, Clone, Copy)]
pub struct FederatedPlanner;

impl FederatedPlanner {
    /// Plans one federated search.
    ///
    /// Selection is a filter chain in the mandatory order. The first stage
    /// always contains the highest-precedence *local* sources when the mode
    /// allows them, so Local First (P-002) is a property of the plan, not a
    /// convention in the caller.
    pub fn plan(
        context: &PlanningContext,
        descriptors: &[SearchSourceDescriptor],
    ) -> FederatedSearchPlan {
        let open = context.open_requirements();
        let mut rejections = Vec::new();
        let mut eligible: Vec<&SearchSourceDescriptor> = Vec::new();

        // Policy + permission + disabled + scope + capability + reliability.
        for descriptor in descriptors {
            if let Some(reason) = reject_reason(context, descriptor, &open) {
                rejections.push(RejectedSource {
                    source_id: descriptor.source_id.to_string(),
                    reason,
                    detail: rejection_detail(context, descriptor),
                });
                continue;
            }
            eligible.push(descriptor);
        }

        // Deterministic candidate order: required-authoritative first, then
        // local before external (P-002), then priority, then id.
        // Ordering keys, in force: LOCAL FIRST (P-002) dominates, then how
        // strongly a source is tied to an open requirement, then its
        // configured priority, then its id for determinism. Locality is the
        // primary key on purpose: an external source that happens to declare
        // a requirement must never be planned ahead of local knowledge that
        // can answer the same question for free.
        eligible.sort_by(|a, b| {
            a.category
                .is_external()
                .cmp(&b.category.is_external())
                .then_with(|| requirement_authority(b, &open).cmp(&requirement_authority(a, &open)))
                // RFC-080: cost-adaptive ordering sits AFTER the permission
                // boundary, locality and requirement authority. A required
                // authoritative source is never traded away to save money —
                // fail-open is an availability property, not a quality pass.
                .then_with(|| {
                    if context.selection == SelectionMode::CostAdaptive {
                        value_density(b)
                            .partial_cmp(&value_density(a))
                            .unwrap_or(Ordering::Equal)
                    } else {
                        Ordering::Equal
                    }
                })
                .then_with(|| b.priority.cmp(&a.priority))
                .then_with(|| a.source_id.as_str().cmp(b.source_id.as_str()))
        });

        let mut stages: Vec<SearchStage> = Vec::new();
        let mut cost_total: i64 = 0;
        let mut selected_count = 0usize;
        // Factual coverage = pre-covered plus authoritative claims. Generic
        // sources may cross-verify the same still-open gap; they do not close
        // it at plan time.
        let mut covered: Vec<String> = context.covered_requirements.clone();
        let mut unanswerable: Vec<String> = Vec::new();

        for descriptor in eligible.iter().copied() {
            if selected_count >= context.max_sources {
                rejections.push(RejectedSource {
                    source_id: descriptor.source_id.to_string(),
                    reason: RejectionReason::FanOutLimit,
                    detail: format!("max_sources = {}", context.max_sources),
                });
                continue;
            }
            // Requirement attribution. A source that DECLARES requirement
            // ids is authoritative for them: selecting it marks them as
            // factually covered, the sequential stop may fire, and a later
            // source claiming the same id is a re-purchase. A GENERIC
            // adapter — the full-text index, a search provider; wiring
            // cannot know what each will answer — may be tasked with the
            // open requirements when `generic_sources_can_answer` is set,
            // but its claim is optimistic and NEVER counts as factual
            // coverage: coverage is adjudicated by the quality gate, not by
            // plan-time selection. This is also what keeps the fail-open
            // boundary strict — a generic source can never "satisfy" a
            // REQUIRED gap just by being selected (RFC-062 §8.3).
            let generic_source = descriptor.satisfies_requirements.is_empty();
            // An authoritative claim is deduped against FACTUAL coverage
            // only: a free generic source being *asked* the same question
            // is not an answer, and must never veto the one source that is
            // declared authoritative for the gap.
            let declared_fills: Vec<String> = descriptor
                .satisfies_requirements
                .iter()
                .filter(|r| open.iter().any(|o| o == *r))
                .filter(|r| !covered.contains(r))
                .cloned()
                .collect();
            let fills: Vec<String> = if !declared_fills.is_empty() {
                declared_fills
            } else if generic_source
                && context.generic_sources_can_answer
                && context.strategy != FederationStrategy::Selective
            {
                open.iter()
                    .filter(|r| !covered.contains(r))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            // INV-30-04 / "no re-buying", generalised to every strategy:
            // a source that answers nothing open may still be planned as
            // local filler (free, and Hybrid/ExternalFirst legitimately
            // supplement with local evidence), but it may NEVER be planned
            // when it would produce a real bill. Spending on an already
            // covered requirement is the exact waste this system exists to
            // remove, and a "filler" flag does not make a bill harmless.
            if fills.is_empty() && descriptor.cost.billable {
                // "Already covered" is true in two cases: the request
                // declared requirements and every one of them is closed
                // (planned or pre-covered), or this source's own
                // requirements were satisfied by earlier evidence.
                // "Already covered" here means FACTUALLY covered: either the
                // caller supplied pre-existing evidence for every gap, or an
                // authoritative (declared) source has already been planned
                // for it. An optimistic generic claim never closes a gap —
                // otherwise selecting the free local index would silently
                // veto the one source that could actually answer a gap local
                // retrieval cannot cover.
                let all_covered = (!context.requirements.is_empty()
                    && open.iter().all(|o| covered.contains(o)))
                    || descriptor
                        .satisfies_requirements
                        .iter()
                        .any(|r| context.covered_requirements.contains(r));
                rejections.push(RejectedSource {
                    source_id: descriptor.source_id.to_string(),
                    reason: if all_covered {
                        RejectionReason::AlreadyCovered
                    } else {
                        RejectionReason::NotRelevant
                    },
                    detail: if all_covered {
                        "every open requirement is already covered; a billable source would re-purchase"
                            .to_string()
                    } else {
                        "billable source fills no open requirement".to_string()
                    },
                });
                continue;
            }

            // Selective means "only sources that can fill an open
            // requirement". The test is per-source (`fills`), never global
            // remaining-open: a source that answers nothing still answers
            // nothing after the last gap closed.
            if context.strategy == FederationStrategy::Selective && fills.is_empty() {
                let all_covered = !open.is_empty() && open.iter().all(|o| covered.contains(o));
                rejections.push(RejectedSource {
                    source_id: descriptor.source_id.to_string(),
                    reason: if all_covered {
                        RejectionReason::AlreadyCovered
                    } else {
                        RejectionReason::NotRelevant
                    },
                    detail: format!(
                        "selective strategy skips '{}': it fills no open requirement",
                        descriptor.name
                    ),
                });
                continue;
            }

            if cost_total + descriptor.cost.estimated_cost_micros
                > context.max_estimated_cost_micros
            {
                rejections.push(RejectedSource {
                    source_id: descriptor.source_id.to_string(),
                    reason: RejectionReason::Budget,
                    detail: format!(
                        "estimated {} micros exceeds remaining budget {}",
                        descriptor.cost.estimated_cost_micros,
                        context.max_estimated_cost_micros.saturating_sub(cost_total)
                    ),
                });
                continue;
            }

            cost_total += descriptor.cost.estimated_cost_micros;
            selected_count += 1;
            for requirement in &fills {
                if !generic_source && !covered.contains(requirement) {
                    covered.push(requirement.clone());
                }
            }

            let task = SourceTask {
                source_id: descriptor.source_id.to_string(),
                category: descriptor.category,
                filler: fills.is_empty(),
                requirements: fills,
                billable: descriptor.cost.billable,
                estimated_cost_micros: descriptor.cost.estimated_cost_micros,
            };
            push_task(
                &mut stages,
                context.strategy,
                task,
                descriptor.latency.timeout_ms,
            );

            // Planning-time stop belongs to `Sequential` alone, and it keys
            // on FACTUAL coverage: an optimistic generic claim never stops
            // the plan. It is defined
            // as "execute by priority, stop when coverage is met". For Auto /
            // Parallel / Selective the same intent is emitted as the runtime
            // `StopCondition::RequirementsCovered` so the EXECUTOR can cancel
            // later stages — truncating the plan here would hide sources the
            // caller is still entitled to see and would make the audit trail
            // lie about what was available.
            if context.strategy == FederationStrategy::Sequential
                && context.stop_when_covered
                && open.iter().all(|o| covered.contains(o))
            {
                break;
            }
        }

        // Requirements no selected source can answer.
        for requirement in &open {
            if !covered.contains(requirement) {
                unanswerable.push(requirement.clone());
            }
        }
        let unmet_required: Vec<String> = context
            .required_requirements
            .iter()
            .filter(|r| !covered.contains(r))
            .cloned()
            .collect();

        let max_deadline_ms = stages
            .iter()
            .map(|s| s.timeout_ms)
            .max()
            .unwrap_or_default();

        let mut stop_conditions = vec![
            StopCondition::EstimatedCostExhausted,
            StopCondition::DeadlineReached,
        ];
        if context.stop_when_covered {
            stop_conditions.insert(0, StopCondition::RequirementsCovered);
        }
        if !unanswerable.is_empty() {
            stop_conditions.push(StopCondition::NoEligibleSourceRemains);
        }

        // RFC-080 audit trail: score every eligible source (selected or not)
        // BEFORE the plan moves the stages.
        let selection_scores: Vec<SourceSelectionScore> = {
            let mut scores: Vec<SourceSelectionScore> = eligible
                .iter()
                .map(|d| SourceSelectionScore {
                    source_id: d.source_id.to_string(),
                    expected_quality: expected_quality(d),
                    cost_micros: d.cost.estimated_cost_micros,
                    value_density: value_density(d),
                    selected: stages.iter().any(|s| {
                        s.tasks
                            .iter()
                            .any(|t| t.source_id == d.source_id.to_string())
                    }),
                })
                .collect();
            scores.sort_by(|a, b| a.source_id.as_str().cmp(b.source_id.as_str()));
            scores
        };

        FederatedSearchPlan {
            strategy: context.strategy,
            mode: context.mode,
            stages,
            stop_conditions,
            rejections,
            estimated_cost_micros: cost_total,
            max_deadline_ms,
            unanswerable_requirements: unanswerable,
            unmet_required_requirements: unmet_required,
            selection: context.selection,
            selection_scores,
        }
    }
}

/// The mandatory rejection chain. Order matters: permission is checked
/// before capability so a forbidden source is never described as "capable".
fn reject_reason(
    context: &PlanningContext,
    descriptor: &SearchSourceDescriptor,
    _open: &[String],
) -> Option<RejectionReason> {
    let external = descriptor.category.is_external();
    let allowed = if external {
        context.mode.allows_external()
    } else {
        context.mode.allows_local()
    };
    if !allowed {
        return Some(RejectionReason::ModePermission);
    }
    if context.mode == SearchMode::LocalOnly && descriptor.cost.billable {
        return Some(RejectionReason::CostForbidden);
    }
    if !descriptor.enabled {
        return Some(RejectionReason::Disabled);
    }
    if !descriptor
        .scope
        .serves(&WorkspaceId::new(context.workspace_id.clone()))
    {
        return Some(RejectionReason::Scope);
    }
    if !capabilities_useful(descriptor) {
        return Some(RejectionReason::Capability);
    }
    if descriptor.reliability.circuit_open {
        return Some(RejectionReason::Reliability);
    }
    // RFC-080: a source that cannot finish inside the request's wall-clock
    // budget must be rejected with an AUDITABLE reason, not started and then
    // silently cut off by a global deadline.
    if descriptor.latency.timeout_ms > context.max_latency_ms {
        return Some(RejectionReason::Latency);
    }
    None
}

fn capabilities_useful(descriptor: &SearchSourceDescriptor) -> bool {
    descriptor.capabilities.lexical_search
        || descriptor.capabilities.semantic_search
        || descriptor.capabilities.exact_lookup
}

fn rejection_detail(context: &PlanningContext, descriptor: &SearchSourceDescriptor) -> String {
    format!(
        "mode {} / category {} / billable {}",
        context.mode.as_str(),
        descriptor.category.as_str(),
        descriptor.cost.billable
    )
}

/// How strongly a source is tied to an open requirement.
fn requirement_authority(descriptor: &SearchSourceDescriptor, open: &[String]) -> u8 {
    if descriptor
        .satisfies_requirements
        .iter()
        .any(|r| open.iter().any(|o| o == r))
    {
        2
    } else if !descriptor.satisfies_requirements.is_empty() {
        1
    } else {
        0
    }
}

/// Places a task in the stage list according to strategy.
fn push_task(
    stages: &mut Vec<SearchStage>,
    strategy: FederationStrategy,
    task: SourceTask,
    timeout_ms: u64,
) {
    match strategy {
        FederationStrategy::Sequential => {
            let stage = SearchStage {
                stage_id: format!("stage{}", stages.len() + 1),
                execution_mode: StageExecutionMode::Sequential,
                tasks: vec![task],
                timeout_ms,
            };
            stages.push(stage);
        }
        FederationStrategy::Parallel | FederationStrategy::Auto => {
            if stages.is_empty() {
                stages.push(SearchStage {
                    stage_id: "stage1".into(),
                    execution_mode: StageExecutionMode::Parallel,
                    tasks: Vec::new(),
                    timeout_ms: 0,
                });
            }
            let stage = stages.last_mut().expect("stage exists");
            stage.tasks.push(task);
            stage.timeout_ms = stage.timeout_ms.max(timeout_ms);
        }
        FederationStrategy::Selective => {
            if stages.is_empty() {
                stages.push(SearchStage {
                    stage_id: "stage1".into(),
                    execution_mode: StageExecutionMode::Adaptive,
                    tasks: Vec::new(),
                    timeout_ms: 0,
                });
            }
            let stage = stages.last_mut().expect("stage exists");
            stage.tasks.push(task);
            stage.timeout_ms = stage.timeout_ms.max(timeout_ms);
        }
    }
}

/// Cost profile helper so adapters do not repeat the micros convention.
pub fn billable_cost(estimated_cost_micros: i64) -> SourceCostProfile {
    SourceCostProfile {
        estimated_cost_micros,
        currency: "USD".into(),
        billable: true,
    }
}

pub fn free_cost() -> SourceCostProfile {
    SourceCostProfile::default()
}

/// Aggregates executed source results into the federated outcome.
///
/// This is the fail-open boundary: results are returned (availability), but
/// `required_failure` reports the sources the quality gate can never treat as
/// satisfied. Keeping this function pure means the invariant is testable
/// without a runtime (ADR-062-004 / INV-FED-01).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FederatedOutcome {
    pub candidates: Vec<SourceCandidate>,
    pub executed: Vec<String>,
    pub failed_sources: Vec<String>,
    pub missing_requirements: Vec<String>,
    /// True when a required requirement could not be met: the response must
    /// be `partial` / `insufficient_evidence`, never `completed`.
    pub required_failure: bool,
}

pub fn aggregate(
    responses: &[SourceSearchResponse],
    plan: &FederatedSearchPlan,
) -> FederatedOutcome {
    let mut candidates = Vec::new();
    let mut executed = Vec::new();
    let mut failed_sources = Vec::new();
    for response in responses {
        executed.push(response.source_id.to_string());
        if response.status.is_failure() {
            failed_sources.push(response.source_id.to_string());
        }
        candidates.extend(response.candidates.iter().cloned());
    }
    // A permission-denied source is an unauthorized touch, not just a slow
    // one: it is reported separately so the audit can tell the two apart.
    for response in responses {
        if response.status == SourceExecutionStatus::PermissionDenied {
            failed_sources.push(response.source_id.to_string());
        }
    }
    // Required requirements stay unmet even when another source returned
    // something superficially similar; that is the fail-open boundary.
    let mut missing = plan.unmet_required_requirements.clone();
    missing.sort();
    missing.dedup();
    let required_failure = !missing.is_empty();
    FederatedOutcome {
        candidates,
        executed,
        failed_sources,
        missing_requirements: missing,
        required_failure,
    }
}
