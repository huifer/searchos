//! Entity resolution primitives (RFC-058).
//!
//! An ENTITY is a stable identity for a thing the system keeps encountering
//! under different names ("Tokio", "tokio-rs", "https://tokio.rs/"). This
//! module provides a deterministic, policy-free resolver over caller-owned
//! alias maps. It never GUESSES an identity from text alone: a match must be
//! an alias the operator actually registered (a heuristic source may be
//! attached by the caller, but the decision to trust it stays outside).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable entity identity: `ent_<first 16 hex of FNV-1a(canonical name)>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(pub String);

impl EntityId {
    /// Derives an id from a canonical name. Stable across processes and
    /// independent of insertion order.
    pub fn for_name(canonical_name: &str) -> Self {
        let normalized = normalize_alias(canonical_name);
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in normalized.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(format!("ent_{hash:016x}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalizes one alias for matching (case / surrounding punctuation).
pub fn normalize_alias(alias: &str) -> String {
    alias
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// One registered entity: a canonical name plus aliases it answers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub canonical_name: String,
    /// Aliases INCLUDING the canonical name (normalized, unique).
    pub aliases: Vec<String>,
    /// `operator` = user-declared; `heuristic` = derived from observed text.
    pub source: EntitySource,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitySource {
    Operator,
    Heuristic,
}

impl Entity {
    /// Builds an entity from a canonical name and aliases. The canonical name
    /// is always registered as an alias.
    pub fn new(
        canonical_name: impl Into<String>,
        aliases: impl IntoIterator<Item = impl Into<String>>,
        source: EntitySource,
    ) -> Self {
        let canonical = canonical_name.into();
        let mut names: Vec<String> = std::iter::once(canonical.clone())
            .chain(aliases.into_iter().map(|a| a.into()))
            .filter_map(|a| {
                let normalized = normalize_alias(&a);
                (!normalized.is_empty()).then_some(normalized)
            })
            .collect();
        names.sort();
        names.dedup();
        Self {
            id: EntityId::for_name(&canonical),
            canonical_name: canonical,
            aliases: names,
            source,
            created_at: chrono::Utc::now(),
        }
    }
}

/// One resolved entity mention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityMatch {
    pub entity_id: EntityId,
    pub canonical_name: String,
    /// Alias that produced the match (normalized).
    pub matched_alias: String,
    pub source: EntitySource,
}

/// A resolver over registered entities. Alias collisions are resolved by the
/// caller's registration order: the FIRST entity that claims an alias wins,
/// and later claims are reported (never silently overwritten).
#[derive(Debug, Clone, Default)]
pub struct EntityResolver {
    aliases: BTreeMap<String, EntityMatch>,
}

impl EntityResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one entity. Returns the aliases that were ALREADY claimed
    /// by another entity (callers decide whether that is an error).
    pub fn register(&mut self, entity: &Entity) -> Vec<String> {
        let mut conflicts = Vec::new();
        for alias in &entity.aliases {
            let matched = EntityMatch {
                entity_id: entity.id.clone(),
                canonical_name: entity.canonical_name.clone(),
                matched_alias: alias.clone(),
                source: entity.source,
            };
            use std::collections::btree_map::Entry;
            match self.aliases.entry(alias.clone()) {
                Entry::Vacant(slot) => {
                    slot.insert(matched);
                }
                // First registration wins. A later claim is reported, never
                // allowed to silently re-point an existing alias.
                Entry::Occupied(_) => conflicts.push(alias.clone()),
            }
        }
        conflicts
    }

    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    pub fn len(&self) -> usize {
        self.aliases.len()
    }

    /// Resolves a piece of text (title / URL / snippet) to the LONGEST alias
    /// that appears as a whole token sequence. Longest-match avoids "rust"
    /// shadowing "rust async runtime".
    pub fn resolve(&self, text: &str) -> Option<EntityMatch> {
        let normalized = text.to_lowercase();
        self.aliases
            .iter()
            .filter(|(alias, _)| contains_alias(&normalized, alias))
            .max_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| b.cmp(a)))
            .map(|(_, matched)| matched.clone())
    }

    /// All registered aliases (diagnostics).
    pub fn aliases(&self) -> Vec<String> {
        self.aliases.keys().cloned().collect()
    }
}

/// Word-boundary-safe alias containment.
fn contains_alias(text: &str, alias: &str) -> bool {
    if alias.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let alias_bytes = alias.as_bytes();
    let mut start = 0usize;
    while let Some(found) = text[start..].find(alias) {
        let at = start + found;
        let end = at + alias_bytes.len();
        let boundary_before = text[..at]
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        let boundary_after = text[end..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        if boundary_before && boundary_after {
            return true;
        }
        start = at + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_is_stable_and_order_independent() {
        assert_eq!(EntityId::for_name("Tokio"), EntityId::for_name("tokio"));
        assert_ne!(EntityId::for_name("Tokio"), EntityId::for_name("Axum"));
    }

    #[test]
    fn canonical_name_is_always_an_alias() {
        let e = Entity::new("Tokio", ["tokio-rs"], EntitySource::Operator);
        assert!(e.aliases.iter().any(|a| a == "tokio"));
        assert!(e.aliases.iter().any(|a| a == "tokio-rs"));
    }

    #[test]
    fn longest_alias_wins() {
        let mut r = EntityResolver::new();
        let rust = Entity::new("Rust", ["rust"], EntitySource::Operator);
        let tokio = Entity::new("Tokio", ["tokio async runtime"], EntitySource::Operator);
        r.register(&rust);
        r.register(&tokio);
        let m = r
            .resolve("Introducing the tokio async runtime today")
            .unwrap();
        assert_eq!(m.canonical_name, "Tokio");
        assert_eq!(m.matched_alias, "tokio async runtime");
    }

    #[test]
    fn alias_boundaries_are_respected() {
        let mut r = EntityResolver::new();
        r.register(&Entity::new("Rust", ["rust"], EntitySource::Operator));
        assert!(r.resolve("the rust language").is_some());
        assert!(r.resolve("rustacean community").is_none());
    }

    #[test]
    fn unknown_text_does_not_invent_an_entity() {
        let r = EntityResolver::new();
        assert!(r.resolve("totally unknown product").is_none());
    }

    #[test]
    fn duplicate_alias_claims_are_reported() {
        let mut r = EntityResolver::new();
        r.register(&Entity::new("Alpha", ["shared"], EntitySource::Operator));
        let conflicts = r.register(&Entity::new("Beta", ["shared"], EntitySource::Operator));
        assert_eq!(conflicts, vec!["shared"]);
        assert_eq!(r.resolve("shared").unwrap().canonical_name, "Alpha");
    }
}
