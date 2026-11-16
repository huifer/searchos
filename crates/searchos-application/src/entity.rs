//! Entity resolution service (RFC-058).
//!
//! The resolver is a READ model over a durable alias table: startup rebuilds
//! the in-memory alias map from SQLite, registrations write through and then
//! publish into the map. Search results are annotated with a stable
//! `entity_id` when their title or URL matches a REGISTERED alias — never
//! from a guessed identity.

use searchos_domain::entity::{Entity, EntityMatch, EntityResolver};
use searchos_domain::SearchOsError;
use std::sync::RwLock;

/// Read surface used by the search pipeline.
pub trait EntityLookup: Send + Sync {
    fn resolve(&self, text: &str) -> Option<EntityMatch>;
    fn alias_count(&self) -> usize;
}

/// Durability surface used by registration transports.
#[async_trait::async_trait]
pub trait EntityStore: Send + Sync {
    async fn register(&self, entity: &Entity) -> Result<Vec<String>, SearchOsError>;
    async fn entities(&self) -> Result<Vec<Entity>, SearchOsError>;
}

/// Shared in-memory resolver rebuilt from the durable store at startup.
#[derive(Default)]
pub struct EntityRegistry {
    resolver: RwLock<EntityResolver>,
}

impl EntityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the whole alias map from durable entities (called once at
    /// runtime init). Multiple entities sharing an alias resolve to the
    /// FIRST one in id order — deterministic across processes.
    pub fn rebuild(&self, entities: Vec<Entity>) {
        let mut resolver = EntityResolver::new();
        let mut sorted = entities;
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        for entity in &sorted {
            let _ = resolver.register(entity);
        }
        *self.resolver.write().unwrap() = resolver;
    }

    /// Publishes one new entity into the read model.
    pub fn publish(&self, entity: &Entity) {
        self.resolver.write().unwrap().register(entity);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.resolver.read().unwrap().aliases()
    }
}

impl EntityLookup for EntityRegistry {
    fn resolve(&self, text: &str) -> Option<EntityMatch> {
        self.resolver.read().unwrap().resolve(text)
    }

    fn alias_count(&self) -> usize {
        self.resolver.read().unwrap().len()
    }
}

/// Annotates response results with stable entity ids (RFC-058).
///
/// Matching uses title first, then canonical URL. The resolver's
/// longest-alias rule keeps a specific entity from being shadowed by a
/// shorter one. No annotation is written when nothing matched.
pub fn annotate_results(
    lookup: &dyn EntityLookup,
    results: &mut [searchos_domain::response::SearchResultItem],
) {
    for result in results {
        let matched = result
            .title
            .as_deref()
            .and_then(|t| lookup.resolve(t))
            .or_else(|| lookup.resolve(&result.url))
            .or_else(|| lookup.resolve(&result.content));
        if let Some(matched) = matched {
            result.entity_id = Some(matched.entity_id.0);
        }
    }
}
