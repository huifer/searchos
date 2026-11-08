//! Transactional outbox events (RFC-006 6.7, ADR-011).
//!
//! SQLite transaction writes: document version + chunk metadata + outbox
//! event, then commit. A background worker consumes outbox rows to update
//! Tantivy / LanceDB; both indexes are rebuildable, never the SoT.

use serde::{Deserialize, Serialize};

/// Which entity the outbox row refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxEntity {
    Document,
    DocumentChunk,
    MemoryEntry,
}

impl OutboxEntity {
    /// Storage name used in `index_outbox.entity_type`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::DocumentChunk => "document_chunk",
            Self::MemoryEntry => "memory_entry",
        }
    }

    /// Parses from the storage name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "document" => Self::Document,
            "document_chunk" => Self::DocumentChunk,
            "memory_entry" => Self::MemoryEntry,
            _ => return None,
        })
    }
}

/// Operation the index worker should apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOperation {
    Upsert,
    Delete,
}

impl OutboxOperation {
    /// Storage name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }

    /// Parses from the storage name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "upsert" => Self::Upsert,
            "delete" => Self::Delete,
            _ => return None,
        })
    }
}

/// One pending index-sync instruction (row of `index_outbox`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxEvent {
    pub entity_type: OutboxEntity,
    pub entity_id: String,
    pub operation: OutboxOperation,
    /// Payload needed by the worker (e.g. serialized IndexedDocument).
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_roundtrip() {
        for e in [
            OutboxEntity::Document,
            OutboxEntity::DocumentChunk,
            OutboxEntity::MemoryEntry,
        ] {
            assert_eq!(OutboxEntity::parse(e.as_str()), Some(e));
        }
        for o in [OutboxOperation::Upsert, OutboxOperation::Delete] {
            assert_eq!(OutboxOperation::parse(o.as_str()), Some(o));
        }
    }

    #[test]
    fn payload_json_roundtrip() {
        let ev = OutboxEvent {
            entity_type: OutboxEntity::DocumentChunk,
            entity_id: "chunk_1".into(),
            operation: OutboxOperation::Upsert,
            payload: serde_json::json!({"chunk_id": "chunk_1", "text": "hi"}),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: OutboxEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }
}
