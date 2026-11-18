//! Search Source Registry (RFC-063).
//!
//! Registration, discovery, capability lookup, enable/disable, health and
//! scope precedence. The registry is deliberately the *only* way a source
//! enters the system: the planner is never handed an ad-hoc list, which is how
//! an unregistered or unauthorized source stays out of execution.
//!
//! Scope precedence is `Agent > Project > Workspace > Global`. A
//! higher-precedence registration may override a lower one for its own
//! audience; it can never relax a permission or security rule.

use crate::source::{
    SearchSource, SearchSourceDescriptor, SourceCategory, SourceDescriptorRef, SourceFilter,
    SourceHealth, SourceScope,
};
use searchos_domain::ids::{SourceId, WorkspaceId};
use searchos_provider::circuit::{CircuitBreaker, CircuitState};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Consecutive failures before a source is cut off (RFC-080 slice).
const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
/// How long a tripped source stays out before ONE half-open probe is allowed.
const CIRCUIT_COOLDOWN: Duration = Duration::from_secs(30);
/// Weight of the newest outcome in the success-rate moving average.
const SUCCESS_RATE_EMA_WEIGHT: f32 = 0.2;

/// Registry failure modes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("source id '{0}' is already registered at scope {1}")]
    AlreadyRegistered(String, String),
    #[error("source id '{0}' is not registered")]
    NotFound(String),
}

/// A live registration: descriptor + implementation + health + breaker.
#[derive(Clone)]
struct Registration {
    descriptor: SearchSourceDescriptor,
    source: Arc<dyn SearchSource>,
    health: SourceHealth,
    breaker: Arc<CircuitBreaker>,
}

impl Registration {
    fn new(descriptor: SearchSourceDescriptor, source: Arc<dyn SearchSource>) -> Self {
        let breaker = CircuitBreaker::new(CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_COOLDOWN);
        // A descriptor seeded from the provider registry (keys already
        // limited at startup) starts in the same state as the live breaker.
        if descriptor.reliability.circuit_open {
            for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
                breaker.record_failure();
            }
        }
        Self {
            descriptor,
            source,
            health: SourceHealth::Unknown,
            breaker: Arc::new(breaker),
        }
    }

    /// Descriptor as the planner must see it: the breaker is authoritative
    /// for `circuit_open`, so a cooldown that elapsed turns the source into a
    /// single half-open probe instead of a permanent disable.
    fn effective_descriptor(&self) -> SearchSourceDescriptor {
        let mut descriptor = self.descriptor.clone();
        let state = self.breaker.check();
        descriptor.reliability.circuit_open = state == CircuitState::Open;
        descriptor.reliability.consecutive_failures = self.breaker.failures();
        descriptor
    }
}

/// In-process source registry.
///
/// One `source_id` may be registered at several scopes (a `Global` default
/// plus narrower overrides). Lookup resolves the highest-precedence
/// registration visible to the requester, so a workspace can swap an
/// implementation without touching other workspaces.
#[derive(Clone)]
pub struct InMemorySourceRegistry {
    inner: Arc<RwLock<HashMap<String, Vec<Registration>>>>,
}

impl Default for InMemorySourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySourceRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registers a source. Re-registering the same id **at the same scope**
    /// fails loudly rather than silently replacing a live implementation.
    pub fn register(&self, source: Arc<dyn SearchSource>) -> Result<(), RegistryError> {
        let descriptor = source.descriptor();
        let id = descriptor.source_id.as_str().to_string();
        let mut guard = self
            .inner
            .write()
            .map_err(|_| RegistryError::AlreadyRegistered(id.clone(), "poisoned".into()))?;
        let entries = guard.entry(id.clone()).or_default();
        if let Some(existing) = entries
            .iter()
            .find(|r| r.descriptor.scope == descriptor.scope)
        {
            return Err(RegistryError::AlreadyRegistered(
                id,
                scope_label(&existing.descriptor.scope),
            ));
        }
        entries.push(Registration::new(descriptor, source));
        Ok(())
    }

    pub fn unregister(&self, source_id: &SourceId) -> Result<(), RegistryError> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| RegistryError::NotFound(source_id.to_string()))?;
        if guard.remove(source_id.as_str()).is_none() {
            return Err(RegistryError::NotFound(source_id.to_string()));
        }
        Ok(())
    }

    /// Resolves the highest-precedence registration visible to `requester`.
    pub fn get(
        &self,
        source_id: &SourceId,
        requester: &WorkspaceId,
    ) -> Option<Arc<dyn SearchSource>> {
        let guard = self.inner.read().ok()?;
        let entries = guard.get(source_id.as_str())?;
        pick(entries, requester).map(|r| r.source.clone())
    }

    /// Descriptor of the registration [`get`](Self::get) would resolve.
    pub fn descriptor_for(
        &self,
        source_id: &SourceId,
        requester: &WorkspaceId,
    ) -> Option<SearchSourceDescriptor> {
        let guard = self.inner.read().ok()?;
        let entries = guard.get(source_id.as_str())?;
        pick(entries, requester).map(|r| r.effective_descriptor())
    }

    /// Enable/disable applies to every scope registration of this id: turning
    /// a source off must not leave a lower-precedence copy reachable.
    pub fn set_enabled(&self, source_id: &SourceId, enabled: bool) -> Result<(), RegistryError> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| RegistryError::NotFound(source_id.to_string()))?;
        let entries = guard
            .get_mut(source_id.as_str())
            .ok_or_else(|| RegistryError::NotFound(source_id.to_string()))?;
        for entry in entries.iter_mut() {
            entry.descriptor.enabled = enabled;
        }
        Ok(())
    }

    pub fn set_health(&self, source_id: &SourceId, health: SourceHealth) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entries) = guard.get_mut(source_id.as_str()) {
                for entry in entries.iter_mut() {
                    entry.health = health.clone();
                }
            }
        }
    }

    /// Outcome feedback loop (RFC-080 slice): execution results update the
    /// source's LIVE reliability profile, which the planner and the trusted
    /// fusion weights read on the next request.
    ///
    /// Three consecutive failures open the circuit (`reject_reason` then
    /// returns `RejectionReason::Reliability`); after the cooldown ONE
    /// half-open probe is allowed, and a probe failure re-opens immediately.
    /// Any success resets the counter and closes it. `success_rate` is an
    /// exponential moving average over the outcome stream — a rate, not a
    /// per-request score. All scope registrations of the id are updated, so a
    /// higher-precedence override can never keep a tripped source alive.
    ///
    /// Returns `Some(true)` only on the closed → open TRANSITION (for the
    /// circuit-opened counter); `None` means the id is not registered.
    pub fn record_outcome(&self, source_id: &SourceId, success: bool) -> Option<bool> {
        let mut guard = self.inner.write().ok()?;
        let entries = guard.get_mut(source_id.as_str())?;
        let mut just_opened = false;
        for entry in entries.iter_mut() {
            let was_open = entry.breaker.check() == CircuitState::Open;
            let rate = &mut entry.descriptor.reliability.success_rate;
            if success {
                entry.breaker.reset();
                *rate = (*rate * (1.0 - SUCCESS_RATE_EMA_WEIGHT) + SUCCESS_RATE_EMA_WEIGHT)
                    .clamp(0.0, 1.0);
            } else {
                entry.breaker.record_failure();
                *rate = (*rate * (1.0 - SUCCESS_RATE_EMA_WEIGHT)).clamp(0.0, 1.0);
            }
            let is_open = entry.breaker.check() == CircuitState::Open;
            entry.descriptor.reliability.circuit_open = is_open;
            entry.descriptor.reliability.consecutive_failures = entry.breaker.failures();
            if !was_open && is_open {
                just_opened = true;
            }
        }
        Some(just_opened)
    }

    /// Seeds a persisted result-level quality score into every scope copy of
    /// a source (used when the runtime restores learned state at startup).
    pub fn seed_result_quality(&self, source_id: &SourceId, quality: f32, samples: u32) {
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        let Some(entries) = guard.get_mut(source_id.as_str()) else {
            return;
        };
        for entry in entries.iter_mut() {
            entry.descriptor.reliability.result_quality = quality.clamp(0.0, 1.0);
            entry.descriptor.reliability.result_samples = samples;
        }
    }

    /// The learned result-level quality of a source, read from any live
    /// scope copy (all copies are updated together, so they agree).
    pub fn learned_quality(&self, source_id: &SourceId) -> Option<(f32, u32)> {
        let guard = self.inner.read().ok()?;
        let entries = guard.get(source_id.as_str())?;
        let entry = entries.first()?;
        Some((
            entry.descriptor.reliability.result_quality,
            entry.descriptor.reliability.result_samples,
        ))
    }

    /// Result-level quality learning (RFC-080 slice): the fraction of a
    /// source's JUDGED results that the relevance judge marked relevant.
    ///
    /// Only judged results count — an unjudged result carries no information
    /// and must not push the average down. The value is an exponential moving
    /// average, so one bad query cannot erase a track record, and it is
    /// updated on EVERY scope registration of the id so a higher-precedence
    /// override cannot keep a stale score alive.
    pub fn record_result_quality(&self, source_id: &SourceId, relevant_ratio: f32) {
        const RESULT_QUALITY_EMA_WEIGHT: f32 = 0.2;
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        let Some(entries) = guard.get_mut(source_id.as_str()) else {
            return;
        };
        for entry in entries.iter_mut() {
            let reliability = &mut entry.descriptor.reliability;
            let ratio = relevant_ratio.clamp(0.0, 1.0);
            if reliability.result_samples == 0 {
                reliability.result_quality = ratio;
            } else {
                reliability.result_quality = reliability.result_quality
                    * (1.0 - RESULT_QUALITY_EMA_WEIGHT)
                    + ratio * RESULT_QUALITY_EMA_WEIGHT;
            }
            reliability.result_samples = reliability.result_samples.saturating_add(1);
        }
    }

    /// Live latency feedback (RFC-080 slice): observed execution latency
    /// updates the source's p50 (EMA) and p95 (decaying upper envelope, with
    /// a p50-based floor). `timeout_ms` is a DECLARED hard bound and is never
    /// inferred downward — a fast source must not acquire a false-short
    /// timeout. All scope registrations of the id are updated.
    ///
    /// The planner and `GET /sources` read these live values, so a source
    /// that became slow is visible before it breaks a latency SLA.
    pub fn record_latency(&self, source_id: &SourceId, observed_ms: u64) {
        const P50_EMA_WEIGHT: f32 = 0.3;
        const P95_DECAY: f64 = 0.9;
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        let Some(entries) = guard.get_mut(source_id.as_str()) else {
            return;
        };
        for entry in entries.iter_mut() {
            let latency = &mut entry.descriptor.latency;
            let p50 = latency.p50_ms as f32 * (1.0 - P50_EMA_WEIGHT)
                + observed_ms as f32 * P50_EMA_WEIGHT;
            latency.p50_ms = p50.max(1.0) as u64;
            let envelope = (latency.p95_ms as f64 * P95_DECAY)
                .max(observed_ms as f64)
                .max(latency.p50_ms as f64 * 2.0);
            latency.p95_ms = envelope.max(1.0) as u64;
        }
    }

    pub fn health(&self, source_id: &SourceId, requester: &WorkspaceId) -> Option<SourceHealth> {
        let guard = self.inner.read().ok()?;
        let entries = guard.get(source_id.as_str())?;
        pick(entries, requester).map(|r| r.health.clone())
    }

    /// Descriptors visible to `requester`, after filtering.
    ///
    /// One id contributes at most one descriptor — the resolved one — so a
    /// planner can never count a `Global` default and its workspace override
    /// as two independent sources (RFC-074 independence starts here).
    pub fn list(
        &self,
        filter: &SourceFilter,
        requester: &WorkspaceId,
    ) -> Vec<SearchSourceDescriptor> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };
        let mut out: Vec<SearchSourceDescriptor> = Vec::new();
        for entries in guard.values() {
            let Some(resolved) = pick(entries, requester) else {
                continue;
            };
            let descriptor = resolved.effective_descriptor();
            if filter.enabled_only && !descriptor.enabled {
                continue;
            }
            if !filter.categories.is_empty() && !filter.categories.contains(&descriptor.category) {
                continue;
            }
            if !capabilities_match(&descriptor, filter) {
                continue;
            }
            out.push(descriptor);
        }
        // Deterministic: priority first, then id.
        out.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.source_id.as_str().cmp(b.source_id.as_str()))
        });
        out
    }

    /// Every registration regardless of visibility — diagnostics only.
    pub fn all_references(&self) -> Vec<SourceDescriptorRef> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entries in guard.values() {
            for entry in entries {
                out.push(SourceDescriptorRef {
                    descriptor: entry.effective_descriptor(),
                    health: entry.health.clone(),
                });
            }
        }
        out.sort_by(|a, b| {
            a.descriptor
                .source_id
                .as_str()
                .cmp(b.descriptor.source_id.as_str())
        });
        out
    }
}

/// Highest-precedence visible registration, deterministically.
fn pick<'a>(entries: &'a [Registration], requester: &WorkspaceId) -> Option<&'a Registration> {
    entries
        .iter()
        .filter(|r| r.descriptor.scope.serves(requester))
        .max_by(|a, b| {
            a.descriptor
                .scope
                .precedence()
                .cmp(&b.descriptor.scope.precedence())
                .then_with(|| a.descriptor.priority.cmp(&b.descriptor.priority))
                .then_with(|| {
                    a.descriptor
                        .source_id
                        .as_str()
                        .cmp(b.descriptor.source_id.as_str())
                })
        })
}

fn capabilities_match(descriptor: &SearchSourceDescriptor, filter: &SourceFilter) -> bool {
    if filter.require_lexical && !descriptor.capabilities.lexical_search {
        return false;
    }
    if filter.require_semantic && !descriptor.capabilities.semantic_search {
        return false;
    }
    if filter.require_provenance && !descriptor.capabilities.provenance_metadata {
        return false;
    }
    if let Some(workspace) = filter.workspace_id.as_ref() {
        if !descriptor.scope.serves(workspace) {
            return false;
        }
    }
    true
}

pub fn scope_label(scope: &SourceScope) -> String {
    match scope {
        SourceScope::Global => "global".into(),
        SourceScope::Workspace(ws) => format!("workspace:{ws}"),
        SourceScope::Project { workspace, project } => format!("project:{workspace}/{project}"),
        SourceScope::Agent { workspace, agent } => format!("agent:{workspace}/{agent}"),
    }
}

/// Distinct categories present in a catalog, sorted.
pub fn categories_of(descriptors: &[SearchSourceDescriptor]) -> Vec<SourceCategory> {
    let mut set: Vec<SourceCategory> = descriptors.iter().map(|d| d.category).collect();
    set.sort_by_key(|c| c.as_str().to_string());
    set.dedup();
    set
}
