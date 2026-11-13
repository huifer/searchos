//! Unified Resource Model (RFC-064).
//!
//! Not every retrievable thing is a `Document`. A session message, a tool
//! execution, an API record, a knowledge-graph entity and a historical search
//! result all need to be retrievable, deduplicable, permission-checked and
//! citable. Today they are forced through `Document` + `DocumentVersion` +
//! `DocumentChunk`, which is why adding a new source has meant editing the
//! retrieval pipeline instead of registering an adapter.
//!
//! [`SearchResource`] is the shared identity layer: it is **not** a storage
//! schema and it does not replace `Document` (which stays the persistent
//! knowledge unit with its lifecycle and versioning, RFC-006/RFC-029). It is
//! the common shape that sources return and that fusion, dedup and privacy
//! reason over.
//!
//! Identity rules that matter:
//!
//! * `resource_id` is **stable across sources**: the same URL fetched twice,
//!   or the same note ingested under two keys, is one resource discovered by
//!   several sources. Candidate identity and resource identity are separated
//!   (RFC-062 9.3) so "N reposts" never becomes "N independent sources".
//! * `content_hash` is the strongest dedup signal available; a canonical
//!   reference alone cannot tell two versions of one page apart.
//! * `permission` is carried on the resource, not assumed by the caller. A
//!   resource whose permission is unverified must not enter an Agent context.

use crate::source::SourceCategory;
use chrono::{DateTime, Utc};
use searchos_domain::ids::WorkspaceId;
use searchos_domain::now_utc;
use serde::{Deserialize, Serialize};
use std::fmt;

/// What kind of information a resource holds (RFC-064).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    WebPage,
    Document,
    File,
    Transcript,
    Message,
    ToolExecution,
    ApiRecord,
    DatabaseRecord,
    KnowledgeEntity,
    KnowledgeFact,
    SearchResult,
}

impl ResourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WebPage => "web_page",
            Self::Document => "document",
            Self::File => "file",
            Self::Transcript => "transcript",
            Self::Message => "message",
            Self::ToolExecution => "tool_execution",
            Self::ApiRecord => "api_record",
            Self::DatabaseRecord => "database_record",
            Self::KnowledgeEntity => "knowledge_entity",
            Self::KnowledgeFact => "knowledge_fact",
            Self::SearchResult => "search_result",
        }
    }

    /// Resource types whose authority does not decay purely with age.
    ///
    /// User-owned material is not "stale" because it is old; treating it as
    /// such silently discards the user's own knowledge (the RFC-029 G4 rule,
    /// generalised here).
    pub fn is_user_owned(&self) -> bool {
        matches!(
            self,
            Self::Document
                | Self::File
                | Self::Transcript
                | Self::Message
                | Self::ToolExecution
                | Self::KnowledgeEntity
                | Self::KnowledgeFact
        )
    }

    /// Which source category normally produces this resource type. Used for
    /// cross-checking adapters so a `Conversation` source cannot claim to
    /// serve `WebPage` results.
    pub fn default_category(&self) -> SourceCategory {
        match self {
            Self::WebPage => SourceCategory::WebDocument,
            Self::Document | Self::File => SourceCategory::LocalKnowledge,
            Self::Transcript | Self::Message => SourceCategory::Conversation,
            Self::ToolExecution => SourceCategory::AgentMemory,
            Self::ApiRecord | Self::DatabaseRecord => SourceCategory::ExternalApi,
            Self::KnowledgeEntity | Self::KnowledgeFact => SourceCategory::KnowledgeGraph,
            Self::SearchResult => SourceCategory::WebSearch,
        }
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the authoritative content of a resource lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRef {
    /// Content is embedded in the resource itself.
    Inline,
    /// Content lives in the object store under `key`.
    ObjectStore { key: String },
    /// Content must be fetched from `url` before use.
    Remote { url: String },
    /// Content is not available; only the reference and metadata exist.
    ReferenceOnly,
}

/// Permission state of a resource. Fail-closed by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRef {
    pub workspace_id: String,
    /// Agent ids explicitly allowed. Empty means every agent in the workspace
    /// that the mode permits — it never means "everyone".
    pub allowed_agents: Vec<String>,
    /// Whether the resource may be sent to an external provider (RFC-087).
    pub egress_allowed: bool,
    /// True only when the check actually ran. `false` must block context
    /// assembly; it is not "unknown, probably fine".
    pub verified: bool,
}

impl PermissionRef {
    /// A verified, workspace-scoped, egress-forbidden permission.
    pub fn workspace(workspace_id: &WorkspaceId) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            allowed_agents: Vec::new(),
            egress_allowed: false,
            verified: true,
        }
    }

    /// Unverified permission: safe to transport, never safe to render.
    pub fn unverified(workspace_id: &WorkspaceId) -> Self {
        Self {
            verified: false,
            ..Self::workspace(workspace_id)
        }
    }

    /// Whether an agent may read this resource.
    ///
    /// An empty `allowed_agents` allows any agent in the same workspace; a
    /// non-empty list is restrictive. Permission is inherited when knowledge
    /// is shared: one agent's access never widens the workspace's.
    pub fn allows_agent(&self, workspace: &str, agent: Option<&str>) -> bool {
        if !self.verified || self.workspace_id != workspace {
            return false;
        }
        match (agent, self.allowed_agents.is_empty()) {
            (_, true) => true,
            (Some(a), false) => self.allowed_agents.iter().any(|allowed| allowed == a),
            (None, false) => false,
        }
    }
}

/// The shared retrievable-resource identity (RFC-064).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResource {
    /// Stable identity: `sha256(type + canonical_ref)` unless the caller
    /// supplies one. Identical across sources for the same underlying thing.
    pub resource_id: String,
    pub resource_type: ResourceType,
    pub source_category: SourceCategory,
    pub workspace_id: String,
    pub project_id: Option<String>,
    /// Canonical URL / URI / `searchos-local://<workspace>/<source_key>`.
    pub canonical_ref: Option<String>,
    pub content_hash: Option<String>,
    pub title: Option<String>,
    pub content_ref: ContentRef,
    /// When the system first observed this resource.
    pub observed_at: DateTime<Utc>,
    /// When the resource itself was published, when known.
    pub published_at: Option<DateTime<Utc>>,
    /// When the underlying content last changed.
    pub updated_at: Option<DateTime<Utc>>,
    pub permission: PermissionRef,
    /// Provenance chain id, when the source maintains one.
    pub provenance_ref: Option<String>,
    pub language: Option<String>,
}

impl SearchResource {
    /// Builds a resource with a deterministic id derived from its type and
    /// canonical reference, so two sources discovering the same page agree on
    /// identity without coordinating.
    pub fn new(
        resource_type: ResourceType,
        workspace_id: &WorkspaceId,
        canonical_ref: impl Into<String>,
    ) -> Self {
        let canonical_ref = canonical_ref.into();
        Self {
            resource_id: derive_resource_id(resource_type, &canonical_ref),
            resource_type,
            source_category: resource_type.default_category(),
            workspace_id: workspace_id.to_string(),
            project_id: None,
            canonical_ref: Some(canonical_ref),
            content_hash: None,
            title: None,
            content_ref: ContentRef::ReferenceOnly,
            observed_at: now_utc(),
            published_at: None,
            updated_at: None,
            permission: PermissionRef::unverified(workspace_id),
            provenance_ref: None,
            language: None,
        }
    }

    pub fn with_content_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_permission(mut self, permission: PermissionRef) -> Self {
        self.permission = permission;
        self
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Whether this resource may be placed into an Agent context.
    ///
    /// Combines verification and agent scoping so callers cannot forget the
    /// verification half — the half that is easy to skip and impossible to
    /// audit after the fact.
    pub fn context_eligible(&self, agent: Option<&str>) -> bool {
        self.permission.allows_agent(&self.workspace_id, agent)
    }

    /// Strong identity for dedup (RFC-062 10.1): content hash first, then
    /// canonical reference. Two different versions of one URL are two
    /// resources, not one; identical bytes under two references are one.
    pub fn dedup_key(&self) -> Option<String> {
        if let Some(hash) = self.content_hash.as_deref().filter(|h| !h.is_empty()) {
            return Some(format!("hash:{hash}"));
        }
        self.canonical_ref
            .as_deref()
            .map(|reference| format!("ref:{}", canonicalize_reference(reference)))
    }

    /// Whether the source category is a plausible producer of this type.
    /// Adapters call this to fail fast on a mislabelled mapping.
    pub fn category_matches_type(&self) -> bool {
        self.source_category == self.resource_type.default_category()
            // Local knowledge and file system both legitimately produce
            // documents/files; conversation legitimately produces messages
            // and transcripts. Everything else must match exactly.
            || matches!(
                (self.source_category, self.resource_type),
                (SourceCategory::LocalKnowledge, ResourceType::File)
                    | (SourceCategory::FileSystem, ResourceType::Document)
                    | (SourceCategory::AgentMemory, ResourceType::Transcript)
                    | (SourceCategory::AgentMemory, ResourceType::Message)
                    | (SourceCategory::VectorStore, ResourceType::Document)
                    | (SourceCategory::Custom, _)
                    | (_, ResourceType::SearchResult)
            )
    }
}

/// Deterministic resource id.
pub fn derive_resource_id(resource_type: ResourceType, canonical_ref: &str) -> String {
    use identity_digest::digest_hex;
    format!(
        "res_{}",
        digest_hex(&format!("{}|{}", resource_type.as_str(), canonical_ref))
    )
}

/// Light canonicalisation for dedup keys: drop the fragment, strip a single
/// trailing slash, lowercase the scheme+host part. The authoritative
/// canonicaliser lives in the application layer (`merge.rs`) and is reused
/// when sources are wired into the pipeline; this one only has to be stable
/// and conservative.
pub fn canonicalize_reference(reference: &str) -> String {
    let without_fragment = reference.split('#').next().unwrap_or(reference);
    let trimmed = without_fragment.trim_end_matches('/');
    let (prefix, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (format!("{}://", scheme.to_lowercase()), rest.to_string()),
        None => (String::new(), trimmed.to_string()),
    };
    let mut parts = rest.splitn(2, '/');
    let host = parts.next().unwrap_or("").to_lowercase();
    let path = parts.next().unwrap_or("");
    if prefix.is_empty() {
        format!("{host}/{path}")
    } else {
        format!("{prefix}{host}/{path}")
    }
}

/// Local non-cryptographic identity digest.
///
/// Resource identity is **not** a security boundary; content integrity keeps
/// using the storage layer's SHA-256. FNV-1a 64-bit is stable across runs and
/// platforms and avoids pulling another hash dependency into the domain layer.
mod identity_digest {
    /// FNV-1a 64-bit rendered as hex.
    pub fn digest_hex(input: &str) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in input.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> WorkspaceId {
        WorkspaceId::new("ws_test")
    }

    #[test]
    fn resource_id_is_stable_across_sources() {
        let a = SearchResource::new(ResourceType::WebPage, &workspace(), "https://x.dev/doc");
        let b = SearchResource::new(ResourceType::WebPage, &workspace(), "https://x.dev/doc");
        assert_eq!(a.resource_id, b.resource_id);
        // A different reference is a different resource.
        let c = SearchResource::new(ResourceType::WebPage, &workspace(), "https://x.dev/other");
        assert_ne!(a.resource_id, c.resource_id);
    }

    #[test]
    fn dedup_prefers_content_hash_over_reference() {
        let mut a = SearchResource::new(ResourceType::Document, &workspace(), "local://k/1");
        a = a.with_content_hash("abc");
        let mut b = SearchResource::new(ResourceType::Document, &workspace(), "local://k/2");
        b = b.with_content_hash("abc");
        assert_eq!(a.dedup_key(), Some("hash:abc".into()));
        assert_eq!(a.dedup_key(), b.dedup_key(), "same bytes = same resource");
    }

    #[test]
    fn canonical_reference_drops_fragment_and_trailing_slash() {
        assert_eq!(
            canonicalize_reference("HTTPS://Example.COM/a/#seg"),
            "https://example.com/a"
        );
        assert_eq!(
            canonicalize_reference("https://example.com/a/"),
            canonicalize_reference("https://example.com/a")
        );
    }

    #[test]
    fn permission_is_fail_closed_until_verified() {
        let ws = workspace();
        let unverified = SearchResource::new(ResourceType::Document, &ws, "local://k/1");
        assert!(!unverified.context_eligible(None));
        assert!(!unverified.context_eligible(Some("agent-1")));

        let verified = unverified
            .clone()
            .with_permission(PermissionRef::workspace(&ws));
        assert!(verified.context_eligible(None));
    }

    #[test]
    fn agent_allow_list_restricts_across_workspace() {
        let ws = workspace();
        let mut permission = PermissionRef::workspace(&ws);
        permission.allowed_agents = vec!["agent-a".into()];
        let resource = SearchResource::new(ResourceType::Message, &ws, "session://s/1")
            .with_permission(permission);
        assert!(resource.context_eligible(Some("agent-a")));
        assert!(!resource.context_eligible(Some("agent-b")));
        assert!(
            !resource.context_eligible(None),
            "an allow list is restrictive: an anonymous caller is not on it"
        );
    }

    #[test]
    fn cross_workspace_permission_is_denied() {
        let ws = workspace();
        let permission = PermissionRef {
            workspace_id: "ws_other".into(),
            allowed_agents: Vec::new(),
            egress_allowed: false,
            verified: true,
        };
        let resource = SearchResource::new(ResourceType::Document, &ws, "local://k/2")
            .with_permission(permission);
        assert!(!resource.context_eligible(None));
    }

    #[test]
    fn egress_default_is_forbidden() {
        let resource = SearchResource::new(ResourceType::Document, &workspace(), "local://k/3");
        assert!(
            !resource.permission.egress_allowed,
            "sending local content to an external provider must be an explicit grant"
        );
    }

    #[test]
    fn category_and_type_are_consistent() {
        let resource = SearchResource::new(ResourceType::Message, &workspace(), "session://s/1");
        assert!(resource.category_matches_type());
        let mismatched = SearchResource {
            source_category: SourceCategory::WebSearch,
            ..resource
        };
        assert!(!mismatched.category_matches_type());
    }

    #[test]
    fn user_owned_types_are_not_age_stale() {
        for resource_type in [
            ResourceType::Document,
            ResourceType::File,
            ResourceType::Transcript,
            ResourceType::Message,
            ResourceType::KnowledgeFact,
        ] {
            assert!(resource_type.is_user_owned(), "{resource_type}");
        }
        assert!(!ResourceType::WebPage.is_user_owned());
        assert!(!ResourceType::SearchResult.is_user_owned());
    }

    #[test]
    fn serde_round_trip_preserves_identity() {
        let resource = SearchResource::new(ResourceType::WebPage, &workspace(), "https://a.dev/b")
            .with_content_hash("h")
            .with_title("T");
        let json = serde_json::to_value(&resource).unwrap();
        let back: SearchResource = serde_json::from_value(json).unwrap();
        assert_eq!(resource, back);
    }
}
