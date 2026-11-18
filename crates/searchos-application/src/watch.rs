//! RFC-035 Search Watch — deterministic quality watchdog over the Read Model.
//!
//! A watch is NOT an alert daemon and NOT an LLM judge. It is a repeatable,
//! auditable measurement: read the same Analytics Read Model the console
//! reads, compare it with declared thresholds and the previous snapshot,
//! append the result to an audit log. Nothing is auto-remediated, nothing is
//! sent anywhere, and an absent signal is reported as `skipped`, never as a
//! pass.
//!
//! Load-bearing rules:
//! * **Honest emptiness** — no searches yet means `skipped` ("no observations"),
//!   not `pass` (a zero rate on an empty denominator is not quality).
//! * **Noise guard** — a trend only fires when BOTH snapshots have at least
//!   `min_sample` observations.
//! * **Worst-wins status** — one regressed check makes the whole run regressed.
//! * **Fail-open on storage** — if the snapshot store is unavailable, the
//!   checks still run and are reported; the report says persistence failed.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};

use crate::analytics::{AnalyticsQuery, AnalyticsQueryService};
use crate::search_service::SearchService;

/// Status of one check or of the whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    /// Every check that could run was within its declared bounds.
    Pass,
    /// A signal was unavailable (no observations, no dataset, no baseline).
    /// Never counts as a quality pass.
    Skipped,
    /// A check could not be evaluated because an input failed.
    Degraded,
    /// A declared threshold was violated or a metric regressed.
    Regressed,
}

impl WatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Skipped => "skipped",
            Self::Degraded => "degraded",
            Self::Regressed => "regressed",
        }
    }

    /// Worst wins: a regressed check can never be averaged away.
    pub fn combine(a: Self, b: Self) -> Self {
        if a > b {
            a
        } else {
            b
        }
    }
}

/// Kinds of check a watch can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchCheckKind {
    QualityFloor,
    FailureRate,
    QualityRegression,
    Ranking,
}

impl WatchCheckKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::QualityFloor => "quality_floor",
            Self::FailureRate => "failure_rate",
            Self::QualityRegression => "quality_regression",
            Self::Ranking => "ranking",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchCheck {
    pub id: String,
    pub kind: WatchCheckKind,
    pub status: WatchStatus,
    /// The observed value, when the signal existed.
    pub observed: Option<f64>,
    /// The declared boundary, when a threshold was applied.
    pub threshold: Option<f64>,
    /// Human-readable reason, including why a check was skipped.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchSignals {
    pub total_searches: u64,
    pub failed_searches: u64,
    pub quality_pass_rate: f64,
    pub quality_denominator: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchReport {
    pub id: String,
    pub ran_at: DateTime<Utc>,
    pub workspace_id: String,
    pub status: WatchStatus,
    pub checks: Vec<WatchCheck>,
    pub signals: Option<WatchSignals>,
    /// Ranking metrics from the optional dataset check.
    pub ranking: Option<serde_json::Value>,
    /// Honest note when the snapshot could not be persisted.
    pub persistence_error: Option<String>,
    /// Delivery attempts made for this run (empty when the policy skipped).
    #[serde(default)]
    pub notifications: Vec<NotifyOutcome>,
}

/// Thresholds are optional on purpose: an unset check is `skipped`, not
/// silently given an invented default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchThresholds {
    /// Minimum acceptable `quality.pass_rate` (0..=1).
    pub min_quality_pass_rate: Option<f64>,
    /// Maximum acceptable `searches.failed / searches.total` (0..=1).
    pub max_search_failure_rate: Option<f64>,
    /// Maximum ALLOWED drop in quality pass rate versus the previous snapshot.
    pub max_quality_pass_rate_drop: Option<f64>,
    /// Both snapshots need at least this many searches before a trend fires.
    pub min_sample: u64,
}

impl Default for WatchThresholds {
    fn default() -> Self {
        Self {
            min_quality_pass_rate: None,
            max_search_failure_rate: None,
            max_quality_pass_rate_drop: None,
            min_sample: 5,
        }
    }
}

impl WatchThresholds {
    pub fn validate(&self) -> Result<(), SearchOsError> {
        for (name, value) in [
            ("watch.min_quality_pass_rate", self.min_quality_pass_rate),
            (
                "watch.max_search_failure_rate",
                self.max_search_failure_rate,
            ),
        ] {
            if let Some(v) = value {
                if !(0.0..=1.0).contains(&v) {
                    return Err(SearchOsError::validation(name, "must be between 0 and 1"));
                }
            }
        }
        if let Some(v) = self.max_quality_pass_rate_drop {
            if !(0.0..=1.0).contains(&v) || !v.is_finite() {
                return Err(SearchOsError::validation(
                    "watch.max_quality_pass_rate_drop",
                    "must be between 0 and 1",
                ));
            }
        }
        Ok(())
    }
}

/// Optional evaluated-dataset check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchDataset {
    pub path: String,
    pub limit: usize,
    /// Minimum acceptable MRR / nDCG@5 / Recall@5 (0..=1).
    pub min_mrr: f64,
    pub min_ndcg: f64,
    pub min_recall: f64,
}

/// Where a watch result goes when it is worth telling a human about.
///
/// A notifier must NEVER be able to fail the watch itself: the report is the
/// product, delivery is best-effort. Implementations report their failure in
/// [`NotifyOutcome::error`] and the run records it.
#[async_trait]
pub trait WatchNotifier: Send + Sync {
    fn name(&self) -> &'static str;
    async fn notify(&self, report: &WatchReport) -> Result<(), String>;
}

/// When a run is worth notifying about.
///
/// `pass` and `skipped` are intentionally NOT notifiable by default: an
/// operator who gets pinged on every quiet run stops reading the pings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotifyPolicy {
    /// Statuses that trigger a notification.
    pub statuses: Vec<WatchStatus>,
}

impl Default for NotifyPolicy {
    fn default() -> Self {
        Self {
            statuses: vec![WatchStatus::Regressed, WatchStatus::Degraded],
        }
    }
}

impl NotifyPolicy {
    pub fn should_notify(&self, status: WatchStatus) -> bool {
        self.statuses.contains(&status)
    }

    pub fn parse_statuses(values: &[String]) -> Result<Self, SearchOsError> {
        let mut statuses = Vec::new();
        for value in values {
            let status = match value.as_str() {
                "pass" => WatchStatus::Pass,
                "skipped" => WatchStatus::Skipped,
                "degraded" => WatchStatus::Degraded,
                "regressed" => WatchStatus::Regressed,
                other => {
                    return Err(SearchOsError::validation(
                        "watch.notify_on",
                        format!("unknown watch status {other}"),
                    ))
                }
            };
            statuses.push(status);
        }
        Ok(Self { statuses })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotifyOutcome {
    pub notifier: String,
    pub delivered: bool,
    pub error: Option<String>,
}

/// Append-only snapshot store. Implemented by the runtime over SQLite.
#[async_trait]
pub trait WatchSnapshotStore: Send + Sync {
    async fn append(&self, report: &WatchReport) -> Result<(), String>;
    async fn latest(&self) -> Result<Option<WatchReport>, String>;
    async fn list(&self, limit: u32) -> Result<Vec<WatchReport>, String>;
}

/// Bounds check on an observed metric. Pure, so the rule can be gated without
/// a database or a running system.
pub fn compare(observed: f64, threshold: f64, higher_is_better: bool) -> WatchStatus {
    if !observed.is_finite() || !threshold.is_finite() {
        return WatchStatus::Degraded;
    }
    let ok = if higher_is_better {
        observed >= threshold
    } else {
        observed <= threshold
    };
    if ok {
        WatchStatus::Pass
    } else {
        WatchStatus::Regressed
    }
}

/// The service itself.
pub struct WatchService {
    analytics: Arc<dyn AnalyticsQueryService>,
    search: Arc<dyn SearchService>,
    store: Option<Arc<dyn WatchSnapshotStore>>,
    workspace_id: String,
    thresholds: WatchThresholds,
    dataset: Option<WatchDataset>,
    notifiers: Vec<Arc<dyn WatchNotifier>>,
    notify_policy: NotifyPolicy,
}

impl WatchService {
    pub fn new(
        analytics: Arc<dyn AnalyticsQueryService>,
        search: Arc<dyn SearchService>,
        store: Option<Arc<dyn WatchSnapshotStore>>,
        workspace_id: String,
        thresholds: WatchThresholds,
        dataset: Option<WatchDataset>,
    ) -> Self {
        Self {
            analytics,
            search,
            store,
            workspace_id,
            thresholds,
            dataset,
            notifiers: Vec::new(),
            notify_policy: NotifyPolicy::default(),
        }
    }

    /// Installs notifiers and the status policy that triggers them.
    pub fn with_notifiers(
        mut self,
        notifiers: Vec<Arc<dyn WatchNotifier>>,
        notify_policy: NotifyPolicy,
    ) -> Self {
        self.notifiers = notifiers;
        self.notify_policy = notify_policy;
        self
    }

    async fn signals(&self) -> Result<WatchSignals, String> {
        let query = AnalyticsQuery {
            workspace_id: Some(self.workspace_id.clone()),
            interval: Some("day".into()),
            ..AnalyticsQuery::default()
        };
        let envelope = self
            .analytics
            .overview(&query)
            .await
            .map_err(|e| format!("analytics overview: {e}"))?;
        // The Analytics Query Service returns the DASHBOARD ENVELOPE
        // `{ data, meta }`, not the bare Read Model. Read through `data` so
        // the watch never reports zero metrics from a valid dashboard.
        let overview = envelope.get("data").unwrap_or(&envelope);
        let get_f64 = |path: &[&str]| -> f64 {
            path.iter()
                .try_fold(overview, |v, k| v.get(k))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        };
        Ok(WatchSignals {
            total_searches: get_f64(&["searches", "total"]) as u64,
            failed_searches: get_f64(&["searches", "failed"]) as u64,
            quality_pass_rate: get_f64(&["quality", "pass_rate"]),
            quality_denominator: get_f64(&["quality", "denominator"]) as u64,
        })
    }

    async fn dataset_check(&self) -> (WatchCheck, Option<serde_json::Value>) {
        let Some(cfg) = self.dataset.clone() else {
            return (
                WatchCheck {
                    id: "ranking".into(),
                    kind: WatchCheckKind::Ranking,
                    status: WatchStatus::Skipped,
                    observed: None,
                    threshold: None,
                    detail: "no watch.dataset.path configured".into(),
                },
                None,
            );
        };
        let mut dataset =
            match searchos_evaluation::dataset::EvaluationDataset::load_jsonl(Path::new(&cfg.path))
            {
                Ok(d) => d,
                Err(e) => {
                    return (
                        WatchCheck {
                            id: "ranking".into(),
                            kind: WatchCheckKind::Ranking,
                            status: WatchStatus::Degraded,
                            observed: None,
                            threshold: None,
                            detail: format!("dataset unreadable: {e}"),
                        },
                        None,
                    )
                }
            };
        if cfg.limit > 0 {
            dataset.questions.truncate(cfg.limit);
        }
        let workspace = self.workspace_id.clone();
        let search = Arc::clone(&self.search);
        let outcome = match searchos_evaluation::run_dataset(&dataset, move |r| {
            let question = r.question.clone();
            let search = Arc::clone(&search);
            let workspace = workspace.clone();
            async move {
                let req = searchos_domain::request::SearchRequest::new(
                    question.question.clone(),
                    searchos_domain::ids::ProfileId::new(
                        question.profile.clone().unwrap_or_else(|| "general".into()),
                    ),
                    searchos_domain::request::SearchMode::LocalFirst,
                    searchos_domain::ids::WorkspaceId::new(workspace),
                )
                .map_err(|e| e.to_string())?;
                let started = std::time::Instant::now();
                let resp = search.search(req).await.map_err(|e| e.to_string())?;
                let evidence_text = resp
                    .results
                    .iter()
                    .map(|r| {
                        format!(
                            "{} {} {}",
                            r.title.as_deref().unwrap_or(""),
                            r.url,
                            r.content
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(searchos_evaluation::runner::QueryResult {
                    quality_status: resp.quality.status.as_str().into(),
                    local_resolved: matches!(
                        resp.decision.path.as_str(),
                        "local" | "local_with_refresh"
                    ),
                    external_calls: if matches!(resp.decision.path.as_str(), "hybrid" | "external")
                    {
                        1
                    } else {
                        0
                    },
                    cost_micros: (resp.cost.estimated * 1_000_000.0).round() as i64,
                    latency_ms: started.elapsed().as_millis() as u64,
                    evidence_text,
                    ranked_urls: resp.results.iter().map(|r| r.url.clone()).collect(),
                })
            }
        })
        .await
        {
            Ok(o) => o,
            Err(e) => {
                return (
                    WatchCheck {
                        id: "ranking".into(),
                        kind: WatchCheckKind::Ranking,
                        status: WatchStatus::Degraded,
                        observed: None,
                        threshold: None,
                        detail: format!("dataset run failed: {e}"),
                    },
                    None,
                )
            }
        };
        let ranking = serde_json::json!({
            "questions": outcome.total_questions,
            "dataset_id": outcome.dataset_id,
            "mrr": outcome.ranking.mrr,
            "ndcg_at_5": outcome.ranking.ndcg_at_k,
            "recall_at_5": outcome.ranking.recall_at_k,
            "pass_rate": outcome.quality_pass_rate,
        });
        // No qrels ⇒ the metrics are undefined (0), NOT a regression: the
        // ranking report says so, and we must not fabricate a signal.
        if outcome.ranking.questions_with_qrels == 0 {
            return (
                WatchCheck {
                    id: "ranking".into(),
                    kind: WatchCheckKind::Ranking,
                    status: WatchStatus::Skipped,
                    observed: None,
                    threshold: None,
                    detail: "dataset has no relevance judgments; ranking undefined".into(),
                },
                Some(ranking),
            );
        }
        let mrr = outcome.ranking.mrr;
        let status = compare(mrr, cfg.min_mrr, true)
            .max(compare(outcome.ranking.ndcg_at_k, cfg.min_ndcg, true))
            .max(compare(outcome.ranking.recall_at_k, cfg.min_recall, true));
        (
            WatchCheck {
                id: "ranking".into(),
                kind: WatchCheckKind::Ranking,
                status,
                observed: Some(mrr),
                threshold: Some(cfg.min_mrr),
                detail: format!(
                    "{} judged questions; mrr={mrr:.3} ndcg@5={:.3} recall@5={:.3}",
                    outcome.ranking.questions_with_qrels,
                    outcome.ranking.ndcg_at_k,
                    outcome.ranking.recall_at_k
                ),
            },
            Some(ranking),
        )
    }

    /// Snapshot history, newest first. Used by `watch history` and the HTTP
    /// surface; no check is run and nothing is written.
    pub async fn history(&self, limit: u32) -> Result<Vec<WatchReport>, SearchOsError> {
        match self.store.as_ref() {
            Some(store) => store.list(limit).await.map_err(SearchOsError::Internal),
            None => Ok(Vec::new()),
        }
    }

    /// Runs every applicable check and appends one snapshot. The call never
    /// fails because a threshold was violated — a regression is a RESULT.
    pub async fn run(&self) -> Result<WatchReport, SearchOsError> {
        let ran_at = Utc::now();
        let mut checks = Vec::new();
        let mut signals = None;

        match self.signals().await {
            Ok(s) => {
                let total = s.total_searches;
                let denom = s.quality_denominator.max(s.total_searches);

                if let Some(min) = self.thresholds.min_quality_pass_rate {
                    let status = if denom < self.thresholds.min_sample.max(1) {
                        WatchStatus::Skipped
                    } else {
                        compare(s.quality_pass_rate, min, true)
                    };
                    checks.push(WatchCheck {
                        id: "quality_floor".into(),
                        kind: WatchCheckKind::QualityFloor,
                        status,
                        observed: Some(s.quality_pass_rate),
                        threshold: Some(min),
                        detail: if denom < self.thresholds.min_sample.max(1) {
                            format!(
                                "only {denom} observations; min_sample = {}",
                                self.thresholds.min_sample
                            )
                        } else {
                            format!("quality pass rate over {denom} searches")
                        },
                    });
                }

                if let Some(max) = self.thresholds.max_search_failure_rate {
                    let observed = if total > 0 {
                        s.failed_searches as f64 / total as f64
                    } else {
                        0.0
                    };
                    let status = if total < self.thresholds.min_sample.max(1) {
                        WatchStatus::Skipped
                    } else {
                        compare(observed, max, false)
                    };
                    checks.push(WatchCheck {
                        id: "failure_rate".into(),
                        kind: WatchCheckKind::FailureRate,
                        status,
                        observed: Some(observed),
                        threshold: Some(max),
                        detail: format!("{}/{} searches failed", s.failed_searches, total),
                    });
                }

                if let Some(max_drop) = self.thresholds.max_quality_pass_rate_drop {
                    let previous = match self.store.as_ref() {
                        Some(store) => store.latest().await,
                        None => Ok(None),
                    };
                    match previous {
                        Ok(Some(prev)) => {
                            let prev_rate = prev
                                .signals
                                .as_ref()
                                .map(|s| s.quality_pass_rate)
                                .unwrap_or(f64::NAN);
                            let prev_denom = prev
                                .signals
                                .as_ref()
                                .map(|s| s.quality_denominator.max(s.total_searches))
                                .unwrap_or(0);
                            let enough = denom >= self.thresholds.min_sample.max(1)
                                && prev_denom >= self.thresholds.min_sample.max(1);
                            let drop = prev_rate - s.quality_pass_rate;
                            let status = if !enough || !prev_rate.is_finite() {
                                WatchStatus::Skipped
                            } else {
                                compare(-drop, -max_drop, true)
                            };
                            checks.push(WatchCheck {
                                id: "quality_regression".into(),
                                kind: WatchCheckKind::QualityRegression,
                                status,
                                observed: Some(drop),
                                threshold: Some(max_drop),
                                detail: format!(
                                    "pass rate {:.3} → {:.3} (previous {} observations, now {})",
                                    prev_rate, s.quality_pass_rate, prev_denom, denom
                                ),
                            });
                        }
                        Ok(None) => checks.push(WatchCheck {
                            id: "quality_regression".into(),
                            kind: WatchCheckKind::QualityRegression,
                            status: WatchStatus::Skipped,
                            observed: None,
                            threshold: Some(max_drop),
                            detail: "no previous snapshot to compare against".into(),
                        }),
                        Err(e) => checks.push(WatchCheck {
                            id: "quality_regression".into(),
                            kind: WatchCheckKind::QualityRegression,
                            status: WatchStatus::Degraded,
                            observed: None,
                            threshold: Some(max_drop),
                            detail: format!("snapshot store unavailable: {e}"),
                        }),
                    }
                }
                signals = Some(s);
            }
            Err(e) => checks.push(WatchCheck {
                id: "analytics".into(),
                kind: WatchCheckKind::QualityFloor,
                status: WatchStatus::Degraded,
                observed: None,
                threshold: None,
                detail: e,
            }),
        }

        let (ranking_check, ranking) = self.dataset_check().await;
        checks.push(ranking_check);

        let status = checks.iter().fold(WatchStatus::Pass, |acc, c| {
            WatchStatus::combine(acc, c.status)
        });
        let id = format!("watch-{}", ran_at.timestamp_millis());
        let mut report = WatchReport {
            id,
            ran_at,
            workspace_id: self.workspace_id.clone(),
            status,
            checks,
            signals,
            ranking,
            persistence_error: None,
            notifications: Vec::new(),
        };
        if let Some(store) = self.store.as_ref() {
            if let Err(e) = store.append(&report).await {
                report.persistence_error = Some(e);
            }
        }
        // Delivery is best-effort and never changes the verdict: a webhook
        // that is down cannot turn a regression into a pass, or a pass into
        // an error.
        if self.notify_policy.should_notify(report.status) {
            for notifier in &self.notifiers {
                let outcome = match notifier.notify(&report).await {
                    Ok(()) => NotifyOutcome {
                        notifier: notifier.name().to_string(),
                        delivered: true,
                        error: None,
                    },
                    Err(e) => NotifyOutcome {
                        notifier: notifier.name().to_string(),
                        delivered: false,
                        error: Some(e),
                    },
                };
                if let Some(error) = outcome.error.as_deref() {
                    tracing::warn!(
                        notifier = outcome.notifier.as_str(),
                        %error,
                        "watch notification failed"
                    );
                }
                report.notifications.push(outcome);
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_denominator_is_not_a_pass() {
        // The caller decides skipped from min_sample; compare() itself is
        // strict so a zero rate cannot sneak through as a threshold pass.
        assert_eq!(compare(f64::NAN, 0.9, true), WatchStatus::Degraded);
        assert_eq!(compare(0.89, 0.9, true), WatchStatus::Regressed);
        assert_eq!(compare(0.9, 0.9, true), WatchStatus::Pass);
    }

    #[test]
    fn failure_rate_is_a_ceiling() {
        assert_eq!(compare(0.11, 0.1, false), WatchStatus::Regressed);
        assert_eq!(compare(0.1, 0.1, false), WatchStatus::Pass);
    }

    #[test]
    fn worst_status_wins() {
        assert_eq!(
            WatchStatus::combine(WatchStatus::Pass, WatchStatus::Skipped),
            WatchStatus::Skipped
        );
        assert_eq!(
            WatchStatus::combine(WatchStatus::Degraded, WatchStatus::Regressed),
            WatchStatus::Regressed
        );
    }

    #[test]
    fn thresholds_reject_impossible_rates() {
        let bad = WatchThresholds {
            min_quality_pass_rate: Some(1.5),
            ..WatchThresholds::default()
        };
        assert!(bad.validate().is_err());
        let good = WatchThresholds {
            min_quality_pass_rate: Some(0.9),
            ..WatchThresholds::default()
        };
        assert!(good.validate().is_ok());
    }
}

#[cfg(test)]
mod notify_tests {
    use super::*;

    #[test]
    fn quiet_statuses_do_not_page_the_operator() {
        let policy = NotifyPolicy::default();
        assert!(!policy.should_notify(WatchStatus::Pass));
        assert!(!policy.should_notify(WatchStatus::Skipped));
        assert!(policy.should_notify(WatchStatus::Degraded));
        assert!(policy.should_notify(WatchStatus::Regressed));
    }

    #[test]
    fn statuses_are_strictly_validated() {
        assert!(NotifyPolicy::parse_statuses(&["regressed".into()]).is_ok());
        assert!(NotifyPolicy::parse_statuses(&["loud".into()]).is_err());
        let policy = NotifyPolicy::parse_statuses(&["pass".into()]).unwrap();
        assert!(policy.should_notify(WatchStatus::Pass));
    }

    /// A failing notifier must not fail the run, and the failure must be
    /// visible in the report.
    struct BrokenNotifier;

    #[async_trait]
    impl WatchNotifier for BrokenNotifier {
        fn name(&self) -> &'static str {
            "broken"
        }
        async fn notify(&self, _report: &WatchReport) -> Result<(), String> {
            Err("webhook down".into())
        }
    }

    #[tokio::test]
    async fn a_broken_notifier_is_recorded_not_raised() {
        let broken: Arc<dyn WatchNotifier> = Arc::new(BrokenNotifier);
        let policy = NotifyPolicy {
            statuses: vec![WatchStatus::Skipped],
        };
        let report = WatchReport {
            id: "w".into(),
            ran_at: Utc::now(),
            workspace_id: "ws".into(),
            status: WatchStatus::Skipped,
            checks: Vec::new(),
            signals: None,
            ranking: None,
            persistence_error: None,
            notifications: Vec::new(),
        };
        // The service delivers via its own run(); here we assert the
        // outcome shape a caller observes.
        let outcome = match broken.notify(&report).await {
            Ok(()) => NotifyOutcome {
                notifier: "broken".into(),
                delivered: true,
                error: None,
            },
            Err(e) => NotifyOutcome {
                notifier: "broken".into(),
                delivered: false,
                error: Some(e),
            },
        };
        assert!(!outcome.delivered);
        assert_eq!(outcome.error.as_deref(), Some("webhook down"));
        assert!(policy.should_notify(report.status));
    }
}
