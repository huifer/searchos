//! Decision trace collection (RFC-004 4.5, RFC-015 15.1).
//!
//! Traces never store API keys, private data or malicious web instructions
//! (RFC-004 4.5 note).

use async_trait::async_trait;
use searchos_domain::decision::DecisionTrace;
use searchos_domain::SearchOsError;

/// Trace sink (persisted by searchos-storage-sqlite; tests use in-memory).
#[async_trait]
pub trait TraceCollector: Send + Sync {
    /// Records one decision trace. 覆盖率目标 100% (RFC-015 15.5).
    async fn record(&self, trace: DecisionTrace) -> Result<(), SearchOsError>;

    /// Fetches a trace by id (`GET /decisions/:trace_id`, `trace show`).
    async fn get(&self, trace_id: &str) -> Result<Option<DecisionTrace>, SearchOsError>;
}

/// In-memory collector (used by tests and the local single-run CLI).
#[derive(Debug, Default)]
pub struct InMemoryTraceCollector {
    traces: std::sync::RwLock<Vec<DecisionTrace>>,
}

impl InMemoryTraceCollector {
    /// New empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of recorded traces.
    pub fn len(&self) -> usize {
        self.traces.read().unwrap().len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl TraceCollector for InMemoryTraceCollector {
    async fn record(&self, trace: DecisionTrace) -> Result<(), SearchOsError> {
        self.traces.write().unwrap().push(trace);
        Ok(())
    }

    async fn get(&self, trace_id: &str) -> Result<Option<DecisionTrace>, SearchOsError> {
        Ok(self
            .traces
            .read()
            .unwrap()
            .iter()
            .find(|t| t.trace_id.as_str() == trace_id)
            .cloned())
    }
}
