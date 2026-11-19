//! RFC-060: the agent registry.
//!
//! `agent_id` is already a first-class dimension in analytics, the cost
//! ledger and decision traces, but it was a free-form string: anyone could
//! send any value, and an operator had no way to declare "this agent exists,
//! belongs to this workspace, and may spend this much". The registry closes
//! that: an agent is a declared, workspace-scoped identity.
//!
//! Rules:
//! * the id is an operator-chosen SLUG (stable, readable, no generated
//!   UUID): `claude-code`, `nightly-reporter`, `ci-bot`;
//! * ids are normalized (trim + lowercase) so `Claude-Code` and
//!   `claude-code` cannot become two identities;
//! * `enabled` is the lifecycle flag — a disabled agent keeps its history
//!   (analytics and traces still reference it) but cannot start new work
//!   when enforcement is on;
//! * the quota field is advisory metadata for v1: it is stored and returned,
//!   never silently enforced (the rate limiter remains the executor).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AgentId;

/// Longest accepted agent id.
pub const MAX_AGENT_ID_LEN: usize = 64;

/// Normalizes an operator-supplied agent id.
///
/// Returns `None` when the value cannot be an identity: empty, longer than
/// [`MAX_AGENT_ID_LEN`], or containing anything outside `[a-z0-9._-]`.
pub fn normalize_agent_id(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() || lower.len() > MAX_AGENT_ID_LEN {
        return None;
    }
    if lower
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
    {
        Some(lower)
    } else {
        None
    }
}

/// A declared agent identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub workspace_id: String,
    /// Display name; defaults to the id when the caller omits it.
    pub name: String,
    pub description: Option<String>,
    /// Disabled agents keep their history but cannot start work under
    /// enforcement.
    pub enabled: bool,
    /// Declared request budget per minute. Advisory in v1: stored and
    /// surfaced, not silently enforced (the rate limiter executes limits).
    pub max_requests_per_minute: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Agent {
    pub fn new(
        workspace_id: &str,
        id: &str,
        name: Option<String>,
        description: Option<String>,
        max_requests_per_minute: Option<u32>,
    ) -> Result<Self, String> {
        let normalized = normalize_agent_id(id).ok_or_else(|| {
            format!("invalid agent id {id:?}: use 1..={MAX_AGENT_ID_LEN} chars of [a-z0-9._-]")
        })?;
        if workspace_id.trim().is_empty() {
            return Err("workspace_id must not be empty".into());
        }
        if let Some(quota) = max_requests_per_minute {
            if quota == 0 {
                return Err("max_requests_per_minute must be > 0 when set".into());
            }
        }
        let now = Utc::now();
        Ok(Self {
            id: AgentId::new(normalized.clone()),
            workspace_id: workspace_id.to_string(),
            name: name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or(normalized),
            description,
            enabled: true,
            max_requests_per_minute,
            created_at: now,
            updated_at: now,
        })
    }

    /// Applies an update, preserving the creation timestamp.
    pub fn apply_update(
        &mut self,
        name: Option<String>,
        description: Option<Option<String>>,
        max_requests_per_minute: Option<Option<u32>>,
        enabled: Option<bool>,
    ) -> Result<(), String> {
        if let Some(name) = name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                self.name = trimmed.to_string();
            }
        }
        if let Some(description) = description {
            self.description = description;
        }
        if let Some(quota) = max_requests_per_minute {
            if quota == Some(0) {
                return Err("max_requests_per_minute must be > 0 when set".into());
            }
            self.max_requests_per_minute = quota;
        }
        if let Some(enabled) = enabled {
            self.enabled = enabled;
        }
        self.updated_at = Utc::now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_normalized_and_strict() {
        assert_eq!(
            normalize_agent_id(" Claude-Code "),
            Some("claude-code".into())
        );
        assert_eq!(normalize_agent_id("ci_bot.v2"), Some("ci_bot.v2".into()));
        assert_eq!(normalize_agent_id(""), None);
        assert_eq!(normalize_agent_id("   "), None);
        assert_eq!(normalize_agent_id("bad space"), None);
        assert_eq!(normalize_agent_id("emoji🤖"), None);
        assert_eq!(normalize_agent_id(&"x".repeat(MAX_AGENT_ID_LEN + 1)), None);
    }

    #[test]
    fn id_is_the_only_identity_across_case() {
        let a = Agent::new("ws", "Claude-Code", None, None, None).unwrap();
        let b = Agent::new("ws", "claude-code", None, None, None).unwrap();
        assert_eq!(a.id, b.id, "case must never create a second identity");
        assert_eq!(a.name, "claude-code", "name defaults to the normalized id");
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert!(Agent::new("", "bot", None, None, None).is_err());
        assert!(Agent::new("ws", "bot", None, None, Some(0)).is_err());
        assert!(Agent::new("ws", "bot", None, None, Some(60)).is_ok());
    }

    #[test]
    fn update_preserves_identity_and_created_at() {
        let mut agent = Agent::new("ws", "bot", None, None, None).unwrap();
        let created = agent.created_at;
        agent
            .apply_update(
                Some("Nightly Bot".into()),
                None,
                Some(Some(30)),
                Some(false),
            )
            .unwrap();
        assert_eq!(agent.id.as_str(), "bot");
        assert_eq!(agent.name, "Nightly Bot");
        assert_eq!(agent.max_requests_per_minute, Some(30));
        assert!(!agent.enabled);
        assert_eq!(agent.created_at, created);
        assert!(agent.apply_update(None, None, Some(Some(0)), None).is_err());
    }
}
