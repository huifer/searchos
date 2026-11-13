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
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Registry failure modes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("source id '{0}' is already registered at scope {1}")]
    AlreadyRegistered(String, String),
    #[error("source id '{0}' is not registered")]
    NotFound(String),
}

/// A live registration: descriptor + implementation + health.
#[derive(Clone)]
struct Registration {
    descriptor: SearchSourceDescriptor,
    source: Arc<dyn SearchSource>,
    health: SourceHealth,
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
        entries.push(Registration {
            descriptor,
            source,
            health: SourceHealth::Unknown,
        });
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
        pick(entries, requester).map(|r| r.descriptor.clone())
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
            let descriptor = resolved.descriptor.clone();
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
                    descriptor: entry.descriptor.clone(),
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
