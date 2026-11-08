//! Version snapshots (RFC-011 11.4).
//!
//! 每个任务启动时绑定 Extension Version Snapshot; 任务中途不允许无提示切换.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Frozen versions of all extensions at task start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VersionSnapshot {
    /// extension id → exact semver.
    pub versions: HashMap<String, String>,
    /// Snapshot creation time (RFC stamp).
    pub created_at: String,
}

impl VersionSnapshot {
    /// Captures current versions of all registered extensions.
    pub fn capture(current: &HashMap<String, String>) -> Self {
        Self {
            versions: current.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Whether the snapshot is still consistent with current versions.
    pub fn consistent_with(&self, current: &HashMap<String, String>) -> bool {
        self.versions == *current
    }

    /// One extension's pinned version.
    pub fn version_of(&self, id: &str) -> Option<&str> {
        self.versions.get(id).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn snapshot_freezes_versions() {
        let mut current = HashMap::new();
        current.insert("ext.tavily".to_string(), "1.0.0".to_string());
        let snap = VersionSnapshot::capture(&current);
        assert_eq!(snap.version_of("ext.tavily"), Some("1.0.0"));
        assert!(snap.consistent_with(&current));
        // 升级后新任务用新版本; 旧任务快照检测到不一致.
        current.insert("ext.tavily".to_string(), "1.1.0".to_string());
        assert!(!snap.consistent_with(&current));
    }

    #[test]
    fn serializes() {
        let snap = VersionSnapshot::capture(&Default::default());
        let json = serde_json::to_string(&snap).unwrap();
        let back: VersionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }
}
