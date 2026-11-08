//! Namespace isolation, retention and governance (RFC-013 13.2, 13.5).

use searchos_domain::memory::{MemoryEntry, MemoryNamespace, SharingPolicy};
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};

/// Retention policy per namespace (RFC-013 13.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Days to keep entries; `None` = keep forever.
    pub retention_days: Option<u64>,
    /// Whether export is allowed for this namespace.
    pub export_allowed: bool,
    /// Whether restore is allowed.
    pub restore_allowed: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            retention_days: Some(365),
            export_allowed: true,
            restore_allowed: true,
        }
    }
}

/// Enforces namespace isolation on read/write (RFC-013 13.2/13.4).
///
/// Cross-agent reuse must satisfy: Workspace 授权 + sharing policy allows
/// the reader's namespace.
#[derive(Debug, Clone, Default)]
pub struct NamespaceGuard;

impl NamespaceGuard {
    /// Whether a reader in `reader` may see `entry` living in `owner`.
    ///
    /// Rules:
    /// - Private: only same full namespace.
    /// - WorkspaceShared: same workspace id.
    /// - ProjectShared: same workspace + project.
    /// - GlobalPublic: anyone.
    pub fn may_read(
        owner: &MemoryNamespace,
        sharing: SharingPolicy,
        reader: &MemoryNamespace,
    ) -> bool {
        match sharing {
            SharingPolicy::GlobalPublic => true,
            SharingPolicy::Private => owner == reader,
            SharingPolicy::WorkspaceShared => owner.workspace == reader.workspace,
            SharingPolicy::ProjectShared => {
                owner.workspace == reader.workspace
                    && owner.project.is_some()
                    && owner.project == reader.project
            }
        }
    }

    /// Filters entries for a reader.
    pub fn visible_entries<'a>(
        entries: impl IntoIterator<Item = &'a MemoryEntry>,
        reader: &MemoryNamespace,
    ) -> Vec<MemoryEntry> {
        entries
            .into_iter()
            .filter(|e| Self::may_read(&e.namespace, e.sharing, reader))
            .cloned()
            .collect()
    }

    /// Workspace isolation check for cross-workspace reads: must be
    /// explicitly authorized (RFC-013 13.4) and entry must be GlobalPublic.
    pub fn cross_workspace_read(
        entry: &MemoryEntry,
        reader_ws: &searchos_domain::WorkspaceId,
    ) -> Result<(), SearchOsError> {
        if entry.namespace.workspace == *reader_ws {
            return Ok(());
        }
        if entry.sharing == SharingPolicy::GlobalPublic {
            return Ok(());
        }
        Err(SearchOsError::Security(format!(
            "cross-workspace memory read denied: {} -> {} (default isolation, RFC-013)",
            entry.namespace.workspace, reader_ws
        )))
    }

    /// Applies a retention policy to a set of entries, returning ids to
    /// delete (older than retention window).
    pub fn retention_sweep(
        entries: &[MemoryEntry],
        policy: &RetentionPolicy,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<String> {
        match policy.retention_days {
            None => Vec::new(),
            Some(days) => {
                let cutoff = now - chrono::Duration::days(days as i64);
                entries
                    .iter()
                    .filter(|e| e.updated_at < cutoff)
                    .map(|e| e.id.to_string())
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{MemoryEntryId, WorkspaceId};

    fn entry(ns: MemoryNamespace, sharing: SharingPolicy, age_days: i64) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::generate(),
            namespace: ns,
            sharing,
            title: "t".into(),
            content: "c".into(),
            url: None,
            tags: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now() - chrono::Duration::days(age_days),
            embedding_status: "none".into(),
        }
    }

    #[test]
    fn private_isolated_by_default() {
        let owner = MemoryNamespace {
            workspace: WorkspaceId::new("ws_a"),
            project: None,
            agent: Some("a1".into()),
        };
        let other_agent_same_ws = MemoryNamespace {
            workspace: WorkspaceId::new("ws_a"),
            project: None,
            agent: Some("a2".into()),
        };
        let other_ws = MemoryNamespace {
            workspace: WorkspaceId::new("ws_b"),
            project: None,
            agent: None,
        };
        let e = entry(owner.clone(), SharingPolicy::Private, 0);
        assert!(NamespaceGuard::may_read(&owner, e.sharing, &owner));
        assert!(!NamespaceGuard::may_read(
            &owner,
            e.sharing,
            &other_agent_same_ws
        ));
        assert!(!NamespaceGuard::may_read(&owner, e.sharing, &other_ws));
    }

    #[test]
    fn workspace_shared_visible_within_workspace() {
        let owner = MemoryNamespace {
            workspace: WorkspaceId::new("ws_a"),
            project: None,
            agent: None,
        };
        let reader = MemoryNamespace {
            workspace: WorkspaceId::new("ws_a"),
            project: None,
            agent: Some("x".into()),
        };
        let outsider = MemoryNamespace {
            workspace: WorkspaceId::new("ws_b"),
            project: None,
            agent: None,
        };
        let e = entry(owner.clone(), SharingPolicy::WorkspaceShared, 0);
        assert!(NamespaceGuard::may_read(&owner, e.sharing, &reader));
        assert!(!NamespaceGuard::may_read(&owner, e.sharing, &outsider));
    }

    #[test]
    fn cross_workspace_needs_global_public() {
        let e = entry(
            MemoryNamespace {
                workspace: WorkspaceId::new("ws_a"),
                project: None,
                agent: None,
            },
            SharingPolicy::WorkspaceShared,
            0,
        );
        assert!(NamespaceGuard::cross_workspace_read(&e, &WorkspaceId::new("ws_b")).is_err());
        let mut e2 = e.clone();
        e2.sharing = SharingPolicy::GlobalPublic;
        assert!(NamespaceGuard::cross_workspace_read(&e2, &WorkspaceId::new("ws_b")).is_ok());
    }

    #[test]
    fn retention_sweep_lists_old_ids() {
        let e_old = entry(
            MemoryNamespace {
                workspace: WorkspaceId::new("ws"),
                project: None,
                agent: None,
            },
            SharingPolicy::Private,
            400,
        );
        let e_new = entry(
            MemoryNamespace {
                workspace: WorkspaceId::new("ws"),
                project: None,
                agent: None,
            },
            SharingPolicy::Private,
            10,
        );
        let policy = RetentionPolicy {
            retention_days: Some(365),
            ..Default::default()
        };
        let to_delete = NamespaceGuard::retention_sweep(&[e_old, e_new], &policy, Utc::now());
        assert_eq!(to_delete.len(), 1);
    }
}
