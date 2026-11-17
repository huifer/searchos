//! Knowledge collections (RFC-030 v1).
//!
//! A collection is a NAMED, workspace-scoped grouping of documents. It is a
//! management/lifecycle view, NOT a second index: membership never changes
//! what retrieval can see, and a deleted document is hidden from collection
//! listings until explicitly requested (the same audit rule as
//! `GET /documents`).
//!
//! Collection ids are deterministic from (workspace, name) so repeated
//! `collection get/create` calls across processes agree without coordination.

use crate::ids::WorkspaceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CollectionId(pub String);

impl CollectionId {
    pub fn for_name(workspace: &WorkspaceId, name: &str) -> Self {
        let key = format!("{}|{}", workspace.as_str(), normalize_name(name));
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in key.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(format!("col_{hash:016x}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    pub workspace_id: String,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Collection {
    pub fn new(workspace: &WorkspaceId, name: &str, description: Option<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: CollectionId::for_name(workspace, name),
            name: name.trim().to_string(),
            workspace_id: workspace.to_string(),
            description,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionSummary {
    pub collection: Collection,
    /// Number of NON-deleted member documents.
    pub document_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::DocumentId;

    #[test]
    fn collection_id_is_stable_per_workspace_and_name() {
        let ws = WorkspaceId::new("ws_a");
        assert_eq!(
            CollectionId::for_name(&ws, "Research"),
            CollectionId::for_name(&ws, "research")
        );
        assert_ne!(
            CollectionId::for_name(&ws, "research"),
            CollectionId::for_name(&WorkspaceId::new("ws_b"), "research")
        );
    }

    #[test]
    fn collection_carries_owning_workspace() {
        let ws = WorkspaceId::new("ws_a");
        let collection = Collection::new(&ws, "Research", Some("papers".into()));
        assert_eq!(collection.workspace_id, "ws_a");
        assert_eq!(collection.description.as_deref(), Some("papers"));
    }

    #[test]
    fn document_id_is_a_distinct_type() {
        let doc = DocumentId::new("doc_1");
        assert_eq!(doc.as_str(), "doc_1");
    }
}
