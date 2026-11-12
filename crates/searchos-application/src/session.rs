//! In-process agent session store (bounded, no schema migration).
//!
//! A session is the cross-turn state a search agent needs: the turns it took
//! (query → trace/status) and the open requirements the final quality gate
//! left after the last turn. MCP stdio and the gateway are single processes,
//! so an in-memory bounded store is the honest v1: no persistence beyond the
//! process, no schema change, deterministic eviction when the cap is hit.
//!
//! NOT a cache: sessions are never used to shortcut a search — they only
//! carry the follow-up contract (`focus_requirements`) between turns.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use searchos_domain::decision::CoverageGap;
use searchos_domain::ids::TraceId;

/// One turn of a search session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionTurn {
    pub query: String,
    pub trace_id: TraceId,
    pub status: String,
    pub decision_path: String,
    /// Number of coverage gaps after this turn's final gate.
    pub open_requirements: usize,
    pub recorded_at: String,
}

/// A multi-turn research session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchSession {
    pub session_id: String,
    pub workspace_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub turns: Vec<SessionTurn>,
    /// Open requirements left after the LAST turn. This is what a follow-up
    /// turn should target via `focus_requirements`.
    pub open_requirements: Vec<CoverageGap>,
}

/// Hard bounds so a chatty agent cannot grow the process without limit.
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
    /// Maximum concurrent sessions; the oldest-updated session is evicted.
    pub max_sessions: usize,
    /// Maximum turns kept per session.
    pub max_turns_per_session: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_sessions: 64,
            max_turns_per_session: 20,
        }
    }
}

/// Persistence port for agent sessions. The in-memory store stays the fast
/// read path; every mutation is mirrored through this port so a gateway or
/// CLI restart recovers the follow-up contract (SQLite is the source of truth).
pub trait SessionPersistence: Send + Sync {
    fn load_all(&self) -> Result<Vec<SearchSession>, searchos_domain::SearchOsError>;
    fn save(&self, session: &SearchSession) -> Result<(), searchos_domain::SearchOsError>;
    fn delete(&self, session_id: &str) -> Result<(), searchos_domain::SearchOsError>;
}

/// Thread-safe bounded session store. With a persistence adapter attached,
/// mutations are mirrored to durable storage and the store is rehydrated on
/// construction (newest-updated first, capped at `max_sessions`).
pub struct SessionStore {
    inner: Mutex<StoreInner>,
    limits: SessionLimits,
    persistence: Option<Arc<dyn SessionPersistence>>,
}

#[derive(Default, Debug)]
struct StoreInner {
    sessions: BTreeMap<String, SearchSession>,
}

impl SessionStore {
    pub fn new(limits: SessionLimits) -> Self {
        Self::with_persistence(limits, None)
    }

    pub fn with_persistence(
        limits: SessionLimits,
        persistence: Option<Arc<dyn SessionPersistence>>,
    ) -> Self {
        let mut sessions = BTreeMap::new();
        if let Some(p) = &persistence {
            match p.load_all() {
                Ok(mut loaded) => {
                    loaded.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
                    // Keep only the newest `max_sessions` sessions.
                    if loaded.len() > limits.max_sessions {
                        loaded.drain(0..loaded.len() - limits.max_sessions);
                    }
                    for session in loaded {
                        sessions.insert(session.session_id.clone(), session);
                    }
                }
                Err(e) => tracing::warn!(
                    code = %e.class().code,
                    "agent session rehydration failed; starting with an empty session store"
                ),
            }
        }
        Self {
            inner: Mutex::new(StoreInner { sessions }),
            limits,
            persistence,
        }
    }

    /// Mirrors a session mutation to durable storage. Persistence failures
    /// are logged but never break the in-flight search.
    fn persist_save(&self, session: &SearchSession) {
        if let Some(p) = &self.persistence {
            if let Err(e) = p.save(session) {
                tracing::warn!(code = %e.class().code, "agent session save failed");
            }
        }
    }

    fn persist_delete(&self, session_id: &str) {
        if let Some(p) = &self.persistence {
            if let Err(e) = p.delete(session_id) {
                tracing::warn!(code = %e.class().code, "agent session delete failed");
            }
        }
    }

    /// Creates or returns the session, evicting the stalest one when full.
    pub fn create_or_get(&self, session_id: &str, workspace_id: &str) -> SearchSession {
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.sessions.get(session_id) {
            return existing.clone();
        }
        let mut evicted: Option<String> = None;
        if inner.sessions.len() >= self.limits.max_sessions {
            // Evict the session with the oldest `updated_at`.
            let stalest = inner
                .sessions
                .iter()
                .min_by(|a, b| a.1.updated_at.cmp(&b.1.updated_at))
                .map(|(k, _)| k.clone());
            if let Some(k) = stalest {
                inner.sessions.remove(&k);
                evicted = Some(k);
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        let session = SearchSession {
            session_id: session_id.to_string(),
            workspace_id: workspace_id.to_string(),
            created_at: now.clone(),
            updated_at: now,
            turns: Vec::new(),
            open_requirements: Vec::new(),
        };
        inner
            .sessions
            .insert(session_id.to_string(), session.clone());
        drop(inner);
        if let Some(k) = evicted {
            self.persist_delete(&k);
        }
        self.persist_save(&session);
        session
    }

    /// Records a completed turn and the requirements still open after it.
    /// Returns the updated session.
    pub fn record_turn(
        &self,
        session_id: &str,
        workspace_id: &str,
        turn: SessionTurn,
        open_requirements: Vec<CoverageGap>,
    ) -> Option<SearchSession> {
        let mut inner = self.inner.lock().unwrap();
        let session = inner.sessions.get_mut(session_id)?;
        session.workspace_id = workspace_id.to_string();
        session.updated_at = chrono::Utc::now().to_rfc3339();
        session.turns.push(turn);
        if session.turns.len() > self.limits.max_turns_per_session {
            let overflow = session.turns.len() - self.limits.max_turns_per_session;
            session.turns.drain(0..overflow);
        }
        session.open_requirements = open_requirements;
        let snapshot = session.clone();
        drop(inner);
        self.persist_save(&snapshot);
        Some(snapshot)
    }

    pub fn get(&self, session_id: &str) -> Option<SearchSession> {
        self.inner.lock().unwrap().sessions.get(session_id).cloned()
    }

    /// Lists sessions, newest-updated first (bounded by the store cap).
    pub fn list(&self) -> Vec<SearchSession> {
        let mut all: Vec<SearchSession> = self
            .inner
            .lock()
            .unwrap()
            .sessions
            .values()
            .cloned()
            .collect();
        all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        all
    }

    /// Drops a session explicitly (agent signals "done researching").
    pub fn close(&self, session_id: &str) -> bool {
        let removed = self
            .inner
            .lock()
            .unwrap()
            .sessions
            .remove(session_id)
            .is_some();
        if removed {
            self.persist_delete(session_id);
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_domain::ids::TraceId;

    fn turn(query: &str) -> SessionTurn {
        SessionTurn {
            query: query.into(),
            trace_id: TraceId::new("trace_1"),
            status: "partial".into(),
            decision_path: "hybrid".into(),
            open_requirements: 1,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn record_and_read_back() {
        let store = SessionStore::new(SessionLimits::default());
        store.create_or_get("s1", "ws");
        let updated = store
            .record_turn(
                "s1",
                "ws",
                turn("q1"),
                vec![CoverageGap::new("beta consensus", "evidence")],
            )
            .unwrap();
        assert_eq!(updated.turns.len(), 1);
        assert_eq!(updated.open_requirements.len(), 1);
        assert_eq!(store.get("s1").unwrap().turns.len(), 1);
    }

    #[test]
    fn turn_cap_drops_oldest() {
        let store = SessionStore::new(SessionLimits {
            max_sessions: 8,
            max_turns_per_session: 3,
        });
        store.create_or_get("s", "ws");
        for i in 0..5 {
            store.record_turn("s", "ws", turn(&format!("q{i}")), Vec::new());
        }
        let s = store.get("s").unwrap();
        assert_eq!(s.turns.len(), 3);
        assert_eq!(s.turns[0].query, "q2", "oldest turns dropped first");
    }

    #[test]
    fn cap_evicts_stalest_session() {
        let store = SessionStore::new(SessionLimits {
            max_sessions: 2,
            max_turns_per_session: 4,
        });
        store.create_or_get("a", "ws");
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.create_or_get("b", "ws");
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.create_or_get("c", "ws");
        let ids: Vec<String> = store.list().into_iter().map(|s| s.session_id).collect();
        assert!(!ids.contains(&"a".to_string()), "stalest evicted: {ids:?}");
        store.close("b");
        assert!(store.get("b").is_none());
    }
}
