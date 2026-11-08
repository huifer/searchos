//! Memory namespace and entry domain types (RFC-013).

use crate::ids::{MemoryEntryId, WorkspaceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Sharing scope of a memory entry (RFC-013 13.2).
///
/// Default is Private: 私人知识默认不跨 Workspace 泄露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingPolicy {
    Private,
    WorkspaceShared,
    ProjectShared,
    GlobalPublic,
}

/// Namespace path within the Global → Workspace → Project → Agent hierarchy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryNamespace {
    pub workspace: WorkspaceId,
    pub project: Option<String>,
    pub agent: Option<String>,
}

impl MemoryNamespace {
    /// Workspace-scoped namespace string, e.g. `ws_1/project-x/agent_a`.
    pub fn scope_string(&self) -> String {
        match (&self.project, &self.agent) {
            (Some(p), Some(a)) => format!("{}/{}/{}", self.workspace, p, a),
            (Some(p), None) => format!("{}/{}", self.workspace, p),
            (None, Some(a)) => format!("{}/{}", self.workspace, a),
            (None, None) => self.workspace.to_string(),
        }
    }
}

/// A persisted memory entry: reusable knowledge distilled from a search task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: MemoryEntryId,
    pub namespace: MemoryNamespace,
    pub sharing: SharingPolicy,
    pub title: String,
    pub content: String,
    pub url: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub embedding_status: String,
}
