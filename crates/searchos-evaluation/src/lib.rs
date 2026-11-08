//! Evaluation framework (RFC-015).
//!
//! - [`dataset`] — dataset schema + JSONL loading (RFC-015 15.3 categories).
//! - [`runner`] — executes a dataset against an async query function and
//!   aggregates quality/cost outcomes.
//! - [`baselines`] — A. External-Only, B. Exact-Cache+TTL, C. SearchOS;
//!   compares under equal quality standards (RFC-015 15.4).
//! - [`noninferiority`] — 质量达到非劣效标准后成本下降才算有效 (15.6).

pub mod baselines;
pub mod dataset;
pub mod noninferiority;
pub mod runner;

pub use baselines::{BaselineComparison, BaselineKind, BaselineResult};
pub use dataset::{DatasetCategory, EvalQuestion, EvaluationDataset};
pub use noninferiority::{check_noninferiority, NonInferiorityConfig, NonInferiorityVerdict};
pub use runner::{run_dataset, RunOutcome};
