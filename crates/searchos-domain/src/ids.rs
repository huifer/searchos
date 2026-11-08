//! Strongly-typed identifiers (RFC-002 2.3).
//!
//! All IDs use strong newtype wrappers to avoid mixing `String`s of
//! different entities ("所有 ID 使用强类型封装").

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! strong_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// ID prefix, e.g. `doc` for `doc_uuid`.
            pub const PREFIX: &'static str = $prefix;

            /// Generates a new unique ID with the crate-standard prefix.
            pub fn generate() -> Self {
                Self($crate::new_id($prefix))
            }

            /// Wraps an existing external value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrows the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}

strong_id!(RequestId, "req");
strong_id!(TaskId, "task");
strong_id!(TraceId, "trace");
strong_id!(WorkspaceId, "ws");
strong_id!(AgentId, "agent");
strong_id!(DocumentId, "doc");
strong_id!(DocumentVersionId, "docv");
strong_id!(ChunkId, "chunk");
strong_id!(EvidenceId, "ev");
strong_id!(SourceId, "src");
strong_id!(CostEventId, "cost");
strong_id!(PlanId, "plan");
strong_id!(ProfileId, "profile");
strong_id!(ProviderId, "prov");
strong_id!(ExtensionId, "ext");
strong_id!(MemoryEntryId, "mem");
strong_id!(EvaluationRunId, "evalrun");

/// Search profile identifier such as `general`, `latest` (RFC-003 3.5).
impl ProfileId {
    /// Built-in `general` profile.
    pub fn general() -> Self {
        Self("general".into())
    }
    /// Built-in `technical` profile.
    pub fn technical() -> Self {
        Self("technical".into())
    }
    /// Built-in `latest` profile.
    pub fn latest() -> Self {
        Self("latest".into())
    }
    /// Built-in `research` profile.
    pub fn research() -> Self {
        Self("research".into())
    }
    /// Built-in `exact` profile.
    pub fn exact() -> Self {
        Self("exact".into())
    }
    /// Built-in `offline` profile.
    pub fn offline() -> Self {
        Self("offline".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_use_prefix() {
        let id = DocumentId::generate();
        assert!(id.as_str().starts_with("doc_"));
        assert_eq!(id.to_string(), id.0);
    }

    #[test]
    fn serializes_transparently() {
        let id = TaskId::new("task_1");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"task_1\"");
        let back: TaskId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }
}
