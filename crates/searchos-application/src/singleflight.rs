//! Singleflight request merging (RFC-009 9.8).
//!
//! 相同有效请求并发时: one executor runs; others wait and share the result.
//! Every caller keeps its own agent attribution; the real external cost is
//! recorded once; shared benefit recorded separately.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// In-flight entry.
#[derive(Clone)]
struct InFlight<V: Clone + Send + Sync + 'static> {
    result: Arc<std::sync::OnceLock<Result<V, String>>>,
    notify: Arc<Notify>,
}

/// Coordinates deduplication of concurrent identical requests by
/// fingerprint.
#[derive(Default)]
pub struct SingleflightCoordinator {
    in_flight: Mutex<HashMap<String, InFlight<serde_json::Value>>>,
}

impl SingleflightCoordinator {
    /// New coordinator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Executes `op` for `fingerprint`; concurrent callers with the same
    /// fingerprint share the leader's result. Returns
    /// `(value, was_leader)`; shared executions let the caller record a
    /// shared (non-billed) cost event.
    pub async fn execute<F, Fut>(
        &self,
        fingerprint: &str,
        op: F,
    ) -> Result<(serde_json::Value, bool), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
    {
        // Register as leader or follower.
        let mut map = self.in_flight.lock().await;
        if let Some(existing) = map.get(fingerprint) {
            let entry = InFlight {
                result: Arc::clone(&existing.result),
                notify: Arc::clone(&existing.notify),
            };
            drop(map);
            // Wait for the leader.
            entry.notify.notified().await;
            let v = entry
                .result
                .get()
                .cloned()
                .ok_or_else(|| "leader dropped before result".to_string())?;
            return v.map(|v| (v, false));
        }
        let entry = InFlight {
            result: Arc::new(std::sync::OnceLock::new()),
            notify: Arc::new(Notify::new()),
        };
        map.insert(
            fingerprint.to_string(),
            InFlight {
                result: Arc::clone(&entry.result),
                notify: Arc::clone(&entry.notify),
            },
        );
        drop(map);

        // Execute as leader (outside the lock).
        let outcome = op().await;
        let mut map = self.in_flight.lock().await;
        map.remove(fingerprint);
        drop(map);
        entry
            .result
            .set(outcome.clone())
            .ok()
            .ok_or_else(|| "result slot already set".to_string())?;
        entry.notify.notify_waiters();
        outcome.map(|v| (v, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_same_fingerprint_merges() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let c = Arc::clone(&counter);
            let sf = Arc::clone(&sf);
            handles.push(tokio::spawn(async move {
                sf.execute("fp-1", || {
                    let c = Arc::clone(&c);
                    async move {
                        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // Simulate external latency so followers wait.
                        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                        Ok(serde_json::json!({"answer": 42}))
                    }
                })
                .await
                .unwrap()
            }));
        }
        let results = futures_join(handles).await;
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "merged into one execution"
        );
        assert_eq!(
            results.iter().filter(|(_, leader)| *leader).count(),
            1,
            "one leader"
        );
        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn different_fingerprints_execute_independently() {
        let sf = SingleflightCoordinator::new();
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = Arc::clone(&counter);
        let (r1, _) = sf
            .execute("fp-a", || {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(serde_json::json!(1))
                }
            })
            .await
            .unwrap();
        let (r2, _) = sf
            .execute("fp-b", || {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(serde_json::json!(2))
                }
            })
            .await
            .unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_ne!(r1, r2);
    }

    #[tokio::test]
    async fn error_shared_with_followers() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let mut handles = Vec::new();
        for _ in 0..5 {
            let sf = Arc::clone(&sf);
            handles.push(tokio::spawn(async move {
                sf.execute("fp-err", || async { Err("provider down".to_string()) })
                    .await
            }));
        }
        let results = futures_join(handles).await;
        assert!(results.iter().all(|r| r.is_err()));
    }

    async fn futures_join<T: Send + 'static>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    }
}
