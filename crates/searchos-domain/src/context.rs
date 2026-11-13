//! Context Assembly & Token Budget wire contract (RFC-067 / RFC-068).
//!
//! These are the *transport* shapes. The algorithm lives in
//! `searchos-context` ([`crate-agnostic`](searchos_context) pure crate) and
//! the application layer maps between the two. Duplicating the seven budget
//! fields is deliberate: `searchos-domain` must stay free of crate
//! dependencies (RFC-002 2.5 — only serde/chrono/uuid/sha2), and the request
//! contract is exactly what transports negotiate.
//!
//! Invariants the application layer must honour when filling
//! [`ContextBlockSummary`]:
//!
//! * `used_tokens <= usable_tokens` — always.
//! * Citations are reserved before content; a budget-limited context degrades
//!   to citation-only entries instead of losing sources.
//! * `truncated == true` whenever anything was cut, dropped or degraded, and
//!   the status is never `complete` in that case. Partial context is never
//!   reported as complete (same honesty rule as `SearchStatus`).

use serde::{Deserialize, Serialize};

/// Caller-declared budget for one assembled context (RFC-068).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Hard ceiling for the whole context block.
    pub max_tokens: usize,
    /// Tokens the caller reserves for its own prompt scaffolding; never
    /// available to evidence.
    pub reserved_tokens: usize,
    /// Maximum distinct sources represented.
    pub max_sources: usize,
    /// Maximum evidence items from any one source.
    pub max_chunks_per_source: usize,
    /// Merge identical resources discovered by several sources.
    pub deduplicate: bool,
    /// Reserve citations before content; when false, out-of-budget evidence
    /// is dropped instead of degrading to a citation-only entry.
    pub preserve_citations: bool,
    /// When false, conflicting / counter-evidence is excluded.
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

    /// Mirrors the engine-side validation so transports can reject a nonsense
    /// budget at the edge instead of assembling a silent zero context.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_tokens == 0 {
            return Err("context_budget.max_tokens must be greater than zero".into());
        }
        if self.reserved_tokens >= self.max_tokens {
            return Err("context_budget.reserved_tokens must be smaller than max_tokens".into());
        }
        if self.max_sources == 0 {
            return Err("context_budget.max_sources must be greater than zero".into());
        }
        if self.max_chunks_per_source == 0 {
            return Err("context_budget.max_chunks_per_source must be greater than zero".into());
        }
        Ok(())
    }
}

/// Assembly outcome, reported honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStatus {
    /// Everything fitted; nothing was truncated or dropped for budget reasons.
    Complete,
    /// Some content was truncated or degraded to a citation-only entry.
    BudgetLimited,
    /// The budget could not even hold every citation.
    CitationLimited,
}

impl ContextStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::BudgetLimited => "budget_limited",
            Self::CitationLimited => "citation_limited",
        }
    }
}

/// One evidence entry in the Agent-facing context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnippet {
    /// Citation (canonical URL / URI). Always present: compression never
    /// deletes a source.
    pub citation: String,
    pub source_id: String,
    pub title: Option<String>,
    /// Verbatim original text or a verbatim prefix — never a generated
    /// summary. Empty when only the citation survived.
    pub content: String,
    pub truncated: bool,
    pub conflicting: bool,
    /// Source observations merged into this entry. Provenance, not
    /// independence: three reposts are still one fact.
    pub occurrences: usize,
    pub estimated_tokens: usize,
}

/// A dropped evidence item, recorded for audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDrop {
    pub chunk_id: String,
    pub source_id: String,
    pub canonical_ref: String,
    /// Snake-case reason (`permission_denied`, `duplicate`,
    /// `conflict_excluded`, `source_cap`, `chunks_per_source_cap`,
    /// `token_budget`, `citation_budget`).
    pub reason: String,
}

/// The Agent-facing context block attached to a search response (RFC-067).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlockSummary {
    pub status: ContextStatus,
    pub max_tokens: usize,
    pub reserved_tokens: usize,
    pub usable_tokens: usize,
    pub used_tokens: usize,
    pub truncated: bool,
    pub evidence: Vec<ContextSnippet>,
    /// Citations for every surviving evidence entry, in evidence order.
    pub citations: Vec<String>,
    pub dropped: Vec<ContextDrop>,
}

impl ContextBlockSummary {
    pub fn evidence_count(&self) -> usize {
        self.evidence.len()
    }
}
