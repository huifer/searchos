//! Execution planner & provider router (RFC-009).
//!
//! 拆成一组可执行操作，并选择满足质量要求的低成本执行路径。
//! Phase 1 filter (capability/source/quality/rate-limit/health/budget),
//! Phase 2 rank (Quality, Coverage, Freshness, Cost, Latency, Reliability) —
//! quality and hard constraints sort before cost.

pub mod plan;
pub mod router;

pub use plan::PlanBuilder;
pub use router::{ProviderEntry, ProviderRouter, RouterWeights};
