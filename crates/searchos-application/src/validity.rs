//! Cache-reuse validity gate (RFC-077).
//!
//! The caches (exact / canonical / semantic / retrieval) are in-process and
//! cleared by the runtime on mutation — but only in the SAME process. When a
//! second process (CLI ingest, another replica) writes to the shared data
//! directory, in-memory entries go stale without any local signal. RFC-077
//! therefore requires a HIT-TIME judgment: before a cached result is reused,
//! the documents it cites are checked against the durable store.
//!
//! Semantics (decision red lines):
//! * A cached hit whose cited document was deleted or moved to a NEWER
//!   version is a MISS — recomputed, never served.
//! * The port failing is fail-open (validity is a safety net; an unavailable
//!   port must not disable search) and reported.
//! * The check is bounded: a cache entry never triggers more than
//!   [`MAX_VALIDATED_DOCUMENTS`] lookups; entries citing more documents are
//!   treated as valid (recomputing them wholesale would defeat the cache).

use async_trait::async_trait;
use searchos_domain::ids::{DocumentId, DocumentVersionId};
use searchos_domain::retrieval::RetrievalCandidate;
use searchos_domain::SearchOsError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Upper bound on per-hit validity lookups (RFC-077).
pub const MAX_VALIDATED_DOCUMENTS: usize = 64;

/// Reports the CURRENT version of documents in the durable store.
///
/// `None` for a document means it no longer exists (soft-deleted or purged);
/// a `Some(version)` different from the cached one means the source changed.
#[async_trait]
pub trait EvidenceValidityPort: Send + Sync {
    async fn current_versions(
        &self,
        document_ids: &[DocumentId],
    ) -> Result<HashMap<DocumentId, Option<DocumentVersionId>>, SearchOsError>;
}

/// Distinct documents cited by candidates, in first-seen order, capped.
pub fn referenced_documents(candidates: &[RetrievalCandidate]) -> Vec<DocumentId> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<DocumentId> = Vec::new();
    for candidate in candidates {
        if seen.insert(candidate.document_id.as_str().to_string())
            && out.len() < MAX_VALIDATED_DOCUMENTS
        {
            out.push(candidate.document_id.clone());
        }
    }
    out
}

/// RFC-077: whether a cached candidate list may be reused.
pub async fn candidates_still_valid(
    port: &dyn EvidenceValidityPort,
    candidates: &[RetrievalCandidate],
) -> bool {
    let ids = referenced_documents(candidates);
    if ids.is_empty() {
        return true;
    }
    let current = match port.current_versions(&ids).await {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "validity port unavailable; treating cache entry as valid");
            return true;
        }
    };
    candidates.iter().all(|candidate| {
        match current.get(&candidate.document_id) {
            // Document gone → the cached evidence no longer exists.
            Some(None) => false,
            // Document still there — it must be the SAME version.
            Some(Some(version)) => *version == candidate.version_id,
            // Beyond the validation cap (or the candidate cites no tracked
            // document): treated as valid by the bounded check.
            None => true,
        }
    })
}

/// Documents cited by a cached exact-response payload, capped. The payload's
/// provenance entries carry `document_id` / `document_version_id`.
fn payload_documents(payload: &Value) -> Vec<(DocumentId, Option<DocumentVersionId>)> {
    let mut out: Vec<(DocumentId, Option<DocumentVersionId>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let Some(entries) = payload.get("provenance").and_then(|p| p.as_array()) else {
        return out;
    };
    for entry in entries {
        // `document_id` lives inside the nested `provenance` object; the
        // top-level `document_version_id` is the cited version.
        let Some(id) = entry
            .get("provenance")
            .and_then(|p| p.get("document_id"))
            .and_then(|d| d.as_str())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) || out.len() >= MAX_VALIDATED_DOCUMENTS {
            continue;
        }
        let version = entry
            .get("document_version_id")
            .and_then(|v| v.as_str())
            .map(DocumentVersionId::new);
        out.push((DocumentId::new(id), version));
    }
    out
}

/// RFC-077: whether a cached exact-response payload may be reused.
pub async fn payload_still_valid(port: &dyn EvidenceValidityPort, payload: &Value) -> bool {
    let documents = payload_documents(payload);
    if documents.is_empty() {
        return true;
    }
    let ids: Vec<DocumentId> = documents.iter().map(|(id, _)| id.clone()).collect();
    let current = match port.current_versions(&ids).await {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "validity port unavailable; treating cache entry as valid");
            return true;
        }
    };
    documents
        .iter()
        .all(|(id, cached_version)| match current.get(id) {
            Some(None) => false,
            Some(Some(version)) => Some(version) == cached_version.as_ref(),
            None => true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::SourceId;
    use searchos_domain::retrieval::RetrievalSource;

    struct StubPort {
        state: HashMap<String, Option<String>>,
        fail: bool,
    }

    #[async_trait]
    impl EvidenceValidityPort for StubPort {
        async fn current_versions(
            &self,
            document_ids: &[DocumentId],
        ) -> Result<HashMap<DocumentId, Option<DocumentVersionId>>, SearchOsError> {
            if self.fail {
                return Err(SearchOsError::Internal("validity store down".into()));
            }
            Ok(document_ids
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        self.state
                            .get(id.as_str())
                            .and_then(|v| v.as_deref())
                            .map(DocumentVersionId::new),
                    )
                })
                .collect())
        }
    }

    fn candidate(document: &str, version: &str) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: searchos_domain::ids::ChunkId::new(format!("{document}:{version}")),
            document_id: DocumentId::new(document),
            version_id: DocumentVersionId::new(version),
            title: None,
            snippet: "snippet".into(),
            url: format!("searchos-local://default/{document}"),
            score: 1.0,
            source: RetrievalSource::Hybrid,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
            highlight: None,
        }
    }

    #[tokio::test]
    async fn deleted_document_invalidates_the_cached_entry() {
        let port = StubPort {
            state: HashMap::from([("doc_gone".to_string(), None)]),
            fail: false,
        };
        let candidates = vec![candidate("doc_alive", "v1"), candidate("doc_gone", "v1")];
        assert!(!candidates_still_valid(&port, &candidates).await);
    }

    #[tokio::test]
    async fn changed_version_invalidates_the_cached_entry() {
        let port = StubPort {
            state: HashMap::from([("doc_a".to_string(), Some("v2".to_string()))]),
            fail: false,
        };
        let candidates = vec![candidate("doc_a", "v1")];
        assert!(!candidates_still_valid(&port, &candidates).await);
        let fresh = vec![candidate("doc_a", "v2")];
        assert!(candidates_still_valid(&port, &fresh).await);
    }

    #[tokio::test]
    async fn port_failure_is_fail_open() {
        let port = StubPort {
            state: HashMap::new(),
            fail: true,
        };
        let candidates = vec![candidate("doc_a", "v1")];
        assert!(
            candidates_still_valid(&port, &candidates).await,
            "an unavailable validity store must not disable search"
        );
    }

    #[tokio::test]
    async fn entries_citing_more_than_the_cap_are_treated_valid() {
        // The first MAX_VALIDATED_DOCUMENTS documents exist at v1; the stub
        // reports anything QUERIED but absent as deleted, so entries beyond
        // the cap must not be looked up (they stay valid by the bound).
        let mut state = HashMap::new();
        for i in 0..MAX_VALIDATED_DOCUMENTS {
            state.insert(format!("doc_{i}"), Some("v1".to_string()));
        }
        let port = StubPort { state, fail: false };
        let candidates: Vec<RetrievalCandidate> = (0..(MAX_VALIDATED_DOCUMENTS + 8))
            .map(|i| candidate(&format!("doc_{i}"), "v1"))
            .collect();
        assert!(candidates_still_valid(&port, &candidates).await);
        // The cap bounds the port's lookup list.
        let referenced = referenced_documents(&candidates);
        assert_eq!(referenced.len(), MAX_VALIDATED_DOCUMENTS);
    }

    #[tokio::test]
    async fn payload_validity_follows_the_same_semantics() {
        let port = StubPort {
            state: HashMap::from([
                ("doc_gone".to_string(), None),
                ("doc_a".to_string(), Some("v2".to_string())),
                ("doc_ok".to_string(), Some("v1".to_string())),
            ]),
            fail: false,
        };
        let payload = serde_json::json!({
            "provenance": [
                {
                    "document_version_id": "v1",
                    "provenance": {"document_id": "doc_ok"},
                },
                {
                    "document_version_id": "v1",
                    "provenance": {"document_id": "doc_a"},
                },
            ]
        });
        assert!(!payload_still_valid(&port, &payload).await);

        let payload_ok = serde_json::json!({
            "provenance": [
                {
                    "document_version_id": "v1",
                    "provenance": {"document_id": "doc_ok"},
                },
            ]
        });
        assert!(payload_still_valid(&port, &payload_ok).await);

        let deleted = serde_json::json!({
            "provenance": [
                {
                    "document_version_id": "v1",
                    "provenance": {"document_id": "doc_ok"},
                },
                {
                    "document_version_id": "v1",
                    "provenance": {"document_id": "doc_gone"},
                },
            ]
        });
        assert!(!payload_still_valid(&port, &deleted).await);
    }

    #[tokio::test]
    async fn external_only_payloads_are_trivially_valid() {
        let port = StubPort {
            state: HashMap::new(),
            fail: false,
        };
        let payload = serde_json::json!({
            "provenance": [
                {
                    "document_version_id": null,
                    "provenance": {"document_id": null},
                },
            ]
        });
        assert!(payload_still_valid(&port, &payload).await);
    }

    #[allow(dead_code)]
    fn source_id_helper() -> SourceId {
        SourceId::generate()
    }
}
