//! Context Assembly & Token Budget (RFC-067 + RFC-068, final RFC Master Index).
//!
//! Search results are not the deliverable an Agent consumes. An Agent consumes
//! a *context*: a bounded, deduplicated, provenance-preserving, citation-safe
//! evidence set that fits its context window. This crate is that layer.
//!
//! It is deliberately a pure, synchronous, I/O-free function of its inputs:
//! no LLM, no summariser, no network, no database. It can therefore be unit
//! tested exhaustively and plugged into Local Mode and Gateway Mode alike.
//!
//! # Invariants
//!
//! 1. `used_tokens <= max_tokens - reserved_tokens`, always.
//! 2. A candidate without `permission_ok` never enters the context; its
//!    filtering is recorded in [`ContextBlock::dropped`] for audit, but its
//!    reference is not exposed in `citation_refs`.
//! 3. Deduplication never discards provenance: every occurrence is retained in
//!    [`AssembledEvidence::seen_in_sources`] / `occurrences`. N occurrences are
//!    **not** N independent sources (RFC-087 §10.3).
//! 4. Conflicting / counter-evidence is ranked **first**, so a high-ranked
//!    majority can never crowd the rebuttal out of the budget.
//! 5. Citations are reserved *before* content. When content no longer fits,
//!    the evidence degrades to a citation-only stub rather than losing its
//!    source. This is the "return the original evidence reference when
//!    compression is unreliable" rule.
//! 6. Text is only ever a verbatim UTF-8-safe prefix of the original — never a
//!    generated summary, never a paraphrase, never a fabricated ellipsis.
//! 7. Truncation is reported honestly via `truncated`, `dropped` and `status`.
//!    A budget-limited context is never reported as `Complete`.
//! 8. Assembly is deterministic: equal inputs produce byte-identical output.
//!
//! # Not yet wired
//!
//! This is the P1 slice for RFC-067/068 (see
//! `docs/gap-analysis-master-index.md`). It is not yet connected to
//! `SearchRequest` / `SearchResponse`; that is P2 and requires the OpenAPI and
//! route-table parity gates.

pub mod estimator;

pub use estimator::{
    truncate_to_tokens, FixedCostEstimator, HeuristicEstimator, TokenEstimator, WordCountEstimator,
};

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

/// Structural overhead charged for each preserved citation reference.
///
/// A reference always costs at least one token, even when its text is empty,
/// so a budget of zero can never silently produce an unbounded citation list.
pub const CITATION_OVERHEAD_TOKENS: usize = 1;

/// Caller-declared budget for one assembled context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Hard ceiling for the whole context block.
    pub max_tokens: usize,
    /// Tokens the caller reserves for its own prompt / response scaffolding.
    /// Never available to evidence.
    pub reserved_tokens: usize,
    /// Maximum distinct `source_id` values represented in the context.
    pub max_sources: usize,
    /// Maximum evidence items from any one source (source diversity).
    pub max_chunks_per_source: usize,
    /// Merge identical resources discovered by more than one source.
    pub deduplicate: bool,
    /// Reserve citations before content; when false, out-of-budget items are
    /// dropped instead of degrading to citation-only stubs.
    pub preserve_citations: bool,
    /// When false, conflicting evidence is explicitly excluded.
    pub include_conflicts: bool,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: 8_000,
            reserved_tokens: 1_000,
            max_sources: 12,
            max_chunks_per_source: 3,
            deduplicate: true,
            preserve_citations: true,
            include_conflicts: true,
        }
    }
}

impl ContextBudget {
    /// Tokens actually available to evidence and citations.
    pub fn usable_tokens(&self) -> usize {
        self.max_tokens.saturating_sub(self.reserved_tokens)
    }

    /// Rejects nonsensical budgets instead of silently assembling zero context.
    pub fn validate(&self) -> Result<(), ContextError> {
        if self.max_tokens == 0 {
            return Err(ContextError::InvalidBudget(
                "max_tokens must be greater than zero".into(),
            ));
        }
        if self.reserved_tokens >= self.max_tokens {
            return Err(ContextError::InvalidBudget(
                "reserved_tokens must be smaller than max_tokens".into(),
            ));
        }
        if self.max_sources == 0 {
            return Err(ContextError::InvalidBudget(
                "max_sources must be greater than zero".into(),
            ));
        }
        if self.max_chunks_per_source == 0 {
            return Err(ContextError::InvalidBudget(
                "max_chunks_per_source must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Invalid caller-declared budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    InvalidBudget(String),
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBudget(reason) => write!(f, "invalid context budget: {reason}"),
        }
    }
}

impl std::error::Error for ContextError {}

/// One raw evidence item offered to the assembler.
///
/// `content` is **original text**. This crate never writes a summary into it.
/// `permission_ok` defaults to `false`: a caller must explicitly assert that
/// the resource passed the workspace/agent permission check before it can
/// enter an Agent's context (RFC-087 §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceChunk {
    /// Stable candidate / chunk id.
    pub chunk_id: String,
    /// Source that discovered this evidence.
    pub source_id: String,
    /// Canonical URL / URI used as the citation.
    pub canonical_ref: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Verbatim original evidence text.
    pub content: String,
    /// Content hash for cross-source deduplication, when known.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Retrieval relevance score (higher is better).
    pub score: f32,
    /// Marks counter-evidence / an unresolved conflict.
    #[serde(default)]
    pub conflicting: bool,
    /// Caller-asserted permission check. Defaults to `false` (fail closed).
    #[serde(default)]
    pub permission_ok: bool,
}

impl EvidenceChunk {
    /// Builds a permitted, non-conflicting evidence item with the required
    /// identity fields. Convenience for tests and adapters.
    pub fn new(
        chunk_id: impl Into<String>,
        source_id: impl Into<String>,
        canonical_ref: impl Into<String>,
        content: impl Into<String>,
        score: f32,
    ) -> Self {
        Self {
            chunk_id: chunk_id.into(),
            source_id: source_id.into(),
            canonical_ref: canonical_ref.into(),
            title: None,
            content: content.into(),
            content_hash: None,
            score,
            conflicting: false,
            permission_ok: true,
        }
    }
}

/// One evidence item in the assembled context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssembledEvidence {
    pub chunk_id: String,
    pub source_id: String,
    pub canonical_ref: String,
    pub title: Option<String>,
    /// Verbatim original text or a verbatim prefix. Empty when only the
    /// citation survived the budget — never a summary.
    pub content: String,
    /// True when `content` is shorter than the original.
    pub truncated: bool,
    pub conflicting: bool,
    /// Number of source observations merged into this item. This is provenance,
    /// **not** a count of independent sources.
    pub occurrences: usize,
    /// Every source that observed this resource (deduplicated, discovery order).
    pub seen_in_sources: Vec<String>,
    /// Citation overhead + content tokens for this item.
    pub estimated_tokens: usize,
}

/// Why an item did not reach the final context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropReason {
    PermissionDenied,
    Duplicate,
    ConflictExcluded,
    SourceCap,
    ChunksPerSourceCap,
    TokenBudget,
    CitationBudget,
}

/// Audit record for a filtered item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DropRecord {
    pub chunk_id: String,
    pub source_id: String,
    pub canonical_ref: String,
    pub reason: DropReason,
}

/// Honest outcome of the assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextStatus {
    /// Everything fitted, nothing was truncated or dropped for budget reasons.
    Complete,
    /// Some content was truncated / degraded to a citation-only stub.
    BudgetLimited,
    /// The budget could not even hold every citation.
    CitationLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub max_tokens: usize,
    pub reserved_tokens: usize,
    pub usable_tokens: usize,
    pub used_tokens: usize,
}

/// The Agent-facing context block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub evidence: Vec<AssembledEvidence>,
    /// Citations for every surviving evidence item, in evidence order.
    pub citation_refs: Vec<String>,
    pub token_usage: TokenUsage,
    pub dropped: Vec<DropRecord>,
    /// Any truncation or citation loss occurred.
    pub truncated: bool,
    pub status: ContextStatus,
}

struct Group<'a> {
    primary: &'a EvidenceChunk,
    seen: Vec<String>,
    occurrences: usize,
    conflicting: bool,
}

/// Assembles a bounded Agent context from raw evidence.
///
/// Pure and deterministic. See the module-level invariant list. Returns
/// [`ContextError::InvalidBudget`] instead of assembling a silent zero context.
pub fn assemble(
    chunks: &[EvidenceChunk],
    budget: &ContextBudget,
    estimator: &dyn TokenEstimator,
) -> Result<ContextBlock, ContextError> {
    budget.validate()?;
    let mut dropped: Vec<DropRecord> = Vec::new();

    // Stage 1 — permission gate. Unauthorised references are NOT exposed in
    // citation_refs: even revealing that a resource exists is a leak.
    let permitted: Vec<&EvidenceChunk> = chunks
        .iter()
        .filter(|c| {
            if c.permission_ok {
                true
            } else {
                dropped.push(drop_record(c, DropReason::PermissionDenied));
                false
            }
        })
        .collect();

    // Stage 2 — explicit conflict policy.
    let mut after_conflicts: Vec<&EvidenceChunk> = Vec::new();
    for c in permitted {
        if c.conflicting && !budget.include_conflicts {
            dropped.push(drop_record(c, DropReason::ConflictExcluded));
        } else {
            after_conflicts.push(c);
        }
    }

    // Stage 3 — cross-source dedup. A content hash is a stronger identity than
    // a reference (two versions of one URL are different resources); when no
    // hash is supplied we fall back to a light canonical reference key. The
    // authoritative canonicalisation lives in the application layer's merge.rs
    // and is reused when this crate is wired into the pipeline (P2).
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for c in after_conflicts {
        if budget.deduplicate {
            let key = dedup_key(c);
            if let Some(&pos) = index.get(&key) {
                let g = &mut groups[pos];
                g.occurrences += 1;
                g.conflicting |= c.conflicting;
                if !g.seen.contains(&c.source_id) {
                    g.seen.push(c.source_id.clone());
                }
                if better_primary(c, g.primary) {
                    g.primary = c;
                }
                dropped.push(drop_record(c, DropReason::Duplicate));
                continue;
            }
            index.insert(key, groups.len());
        }
        groups.push(Group {
            primary: c,
            seen: vec![c.source_id.clone()],
            occurrences: 1,
            conflicting: c.conflicting,
        });
    }

    // Stage 4 — deterministic ranking. Counter-evidence ranks FIRST so that a
    // high-scoring majority can never consume the whole budget and hide the
    // rebuttal. Remaining items are ordered by score, then by id.
    groups.sort_by(|a, b| {
        b.conflicting
            .cmp(&a.conflicting)
            .then_with(|| b.primary.score.total_cmp(&a.primary.score))
            .then_with(|| a.primary.chunk_id.cmp(&b.primary.chunk_id))
    });

    // Stage 5 — source diversity caps.
    let mut selected: Vec<Group<'_>> = Vec::new();
    let mut per_source: HashMap<String, usize> = HashMap::new();
    for g in groups {
        let src = g.primary.source_id.clone();
        let used = per_source.get(&src).copied().unwrap_or(0);
        let is_new_source = used == 0;
        if used >= budget.max_chunks_per_source {
            dropped.push(drop_record(g.primary, DropReason::ChunksPerSourceCap));
            continue;
        }
        if is_new_source && per_source.len() >= budget.max_sources {
            dropped.push(drop_record(g.primary, DropReason::SourceCap));
            continue;
        }
        per_source.insert(src, used + 1);
        selected.push(g);
    }

    // Stage 6 — citation-first budget. Every surviving citation is reserved
    // BEFORE any content is allocated; only the remainder becomes the content
    // pool. That is what makes "compression must not delete sources" true.
    let usable = budget.usable_tokens();
    let cite_cost = |g: &Group<'_>| -> usize {
        if budget.preserve_citations {
            estimator.estimate(&g.primary.canonical_ref) + CITATION_OVERHEAD_TOKENS
        } else {
            0
        }
    };

    let mut citation_kept = vec![false; selected.len()];
    let mut citation_budget = usable;
    let mut citation_limited = false;
    for (i, g) in selected.iter().enumerate() {
        let cost = cite_cost(g);
        if cost <= citation_budget {
            citation_budget -= cost;
            citation_kept[i] = true;
        } else {
            citation_limited = true;
            dropped.push(drop_record(g.primary, DropReason::CitationBudget));
        }
    }
    let mut content_pool = citation_budget;

    // Stage 7 — content allocation, in priority order.
    let mut evidence: Vec<AssembledEvidence> = Vec::new();
    let mut citation_refs: Vec<String> = Vec::new();
    let mut used_tokens = 0usize;
    let mut truncated = citation_limited;

    for (i, g) in selected.iter().enumerate() {
        if !citation_kept[i] {
            continue;
        }
        let cc = cite_cost(g);
        let full = estimator.estimate(&g.primary.content);
        let available = content_pool;
        let (content, content_tokens, is_truncated) = if full <= available {
            content_pool -= full;
            (g.primary.content.clone(), full, false)
        } else if !budget.preserve_citations && available == 0 {
            dropped.push(drop_record(g.primary, DropReason::TokenBudget));
            // A budget-driven drop is a truncation event, not a clean pass.
            truncated = true;
            continue;
        } else {
            let fitted = truncate_to_tokens(&g.primary.content, available, estimator);
            let spent = estimator.estimate(&fitted);
            content_pool = content_pool.saturating_sub(spent);
            (fitted, spent, true)
        };

        if is_truncated {
            truncated = true;
        }
        let estimated_tokens = cc + content_tokens;
        used_tokens += estimated_tokens;

        let canonical_ref = g.primary.canonical_ref.clone();
        citation_refs.push(canonical_ref.clone());
        evidence.push(AssembledEvidence {
            chunk_id: g.primary.chunk_id.clone(),
            source_id: g.primary.source_id.clone(),
            canonical_ref,
            title: g.primary.title.clone(),
            content,
            truncated: is_truncated,
            conflicting: g.conflicting,
            occurrences: g.occurrences,
            seen_in_sources: g.seen.clone(),
            estimated_tokens,
        });
    }

    let status = if citation_limited {
        ContextStatus::CitationLimited
    } else if truncated {
        ContextStatus::BudgetLimited
    } else {
        ContextStatus::Complete
    };

    debug_assert!(used_tokens <= usable, "context budget invariant violated");

    Ok(ContextBlock {
        evidence,
        citation_refs,
        token_usage: TokenUsage {
            max_tokens: budget.max_tokens,
            reserved_tokens: budget.reserved_tokens,
            usable_tokens: usable,
            used_tokens,
        },
        dropped,
        truncated,
        status,
    })
}

/// Convenience wrapper using [`HeuristicEstimator`].
pub fn assemble_with_default_estimator(
    chunks: &[EvidenceChunk],
    budget: &ContextBudget,
) -> Result<ContextBlock, ContextError> {
    assemble(chunks, budget, &HeuristicEstimator)
}

fn dedup_key(c: &EvidenceChunk) -> String {
    if let Some(hash) = c.content_hash.as_ref().filter(|h| !h.is_empty()) {
        return format!("hash:{hash}");
    }
    let without_fragment = c
        .canonical_ref
        .split('#')
        .next()
        .unwrap_or(&c.canonical_ref);
    format!("ref:{}", without_fragment.trim_end_matches('/'))
}

fn better_primary(candidate: &EvidenceChunk, current: &EvidenceChunk) -> bool {
    match candidate.score.total_cmp(&current.score) {
        Ordering::Greater => true,
        Ordering::Equal => candidate.chunk_id < current.chunk_id,
        Ordering::Less => false,
    }
}

fn drop_record(c: &EvidenceChunk, reason: DropReason) -> DropRecord {
    DropRecord {
        chunk_id: c.chunk_id.clone(),
        source_id: c.source_id.clone(),
        canonical_ref: c.canonical_ref.clone(),
        reason,
    }
}

#[cfg(test)]
mod tests;
