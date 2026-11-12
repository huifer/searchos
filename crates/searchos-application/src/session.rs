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
use std::sync::Mutex;

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

/// Thread-safe bounded session store.
#[derive(Debug)]
pub struct SessionStore {
    inner: Mutex<StoreInner>,
    limits: SessionLimits,
}

#[derive(Default, Debug)]
struct StoreInner {
    sessions: BTreeMap<String, SearchSession>,
}

impl SessionStore {
    pub fn new(limits: SessionLimits) -> Self {
        Self {
            inner: Mutex::new(StoreInner::default()),
            limits,
        }
    }

    /// Creates or returns the session, evicting the stalest one when full.
    pub fn create_or_get(&self, session_id: &str, workspace_id: &str) -> SearchSession {
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.sessions.get(session_id) {
            return existing.clone();
        }
        if inner.sessions.len() >= self.limits.max_sessions {
            // Evict the session with the oldest `updated_at`.
            let stalest = inner
                .sessions
                .iter()
                .min_by(|a, b| a.1.updated_at.cmp(&b.1.updated_at))
                .map(|(k, _)| k.clone());
            if let Some(k) = stalest {
                inner.sessions.remove(&k);
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
        Some(session.clone())
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
        self.inner
            .lock()
            .unwrap()
            .sessions
            .remove(session_id)
            .is_some()
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
