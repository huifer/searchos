//! Singleflight request merging (RFC-009 9.8, Part 30 INV-30-05).
//!
//! Concurrent identical requests share one execution. A `watch` channel
//! stores the leader's result *before* the map entry is removed, so a follower
//! arriving after publication still terminates with that result. A
//! `LeaderGuard` releases a cancelled / panicked leader's key, and every
//! follower wait is bounded.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};

/// Default bounded follower wait. A wedged leader can never hold followers
/// forever; the search service turns a timeout into insufficient evidence.
pub const DEFAULT_FOLLOWER_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep a published result briefly so "late" followers do not re-execute.
const COMPLETED_RETENTION: Duration = Duration::from_millis(250);

type Outcome = Result<serde_json::Value, String>;
type InFlightMap = Arc<Mutex<HashMap<String, Arc<InFlight>>>>;

/// In-flight entry; the receiver is retained so `Sender::send` cannot fail
/// while the leader is publishing.
#[derive(Debug)]
struct InFlight {
    id: u64,
    tx: watch::Sender<Option<Outcome>>,
    rx: watch::Receiver<Option<Outcome>>,
}

impl InFlight {
    fn new(id: u64) -> Arc<Self> {
        let (tx, rx) = watch::channel(None);
        Arc::new(Self { id, tx, rx })
    }
}

/// Coordinates deduplication of concurrent identical requests by fingerprint.
#[derive(Default)]
pub struct SingleflightCoordinator {
    in_flight: InFlightMap,
    next_id: AtomicU64,
}

impl SingleflightCoordinator {
    /// New coordinator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Executes `op` for `fingerprint`; concurrent callers with the same
    /// fingerprint share the leader's result. Returns `(value, was_leader)`.
    pub async fn execute<F, Fut>(
        &self,
        fingerprint: &str,
        op: F,
    ) -> Result<(serde_json::Value, bool), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Outcome>,
    {
        self.execute_with_timeout(fingerprint, DEFAULT_FOLLOWER_TIMEOUT, op)
            .await
    }

    /// Executes with an explicit follower timeout (tests and bounded callers).
    pub async fn execute_with_timeout<F, Fut>(
        &self,
        fingerprint: &str,
        timeout: Duration,
        op: F,
    ) -> Result<(serde_json::Value, bool), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Outcome>,
    {
        let mut map = self.in_flight.lock().await;
        if let Some(existing) = map.get(fingerprint).cloned() {
            drop(map);
            let value = wait_for(existing, timeout).await?;
            return Ok((value, false));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = InFlight::new(id);
        map.insert(fingerprint.to_string(), Arc::clone(&entry));
        drop(map);

        let mut guard = LeaderGuard {
            key: fingerprint.to_string(),
            entry: Arc::clone(&entry),
            map: Arc::clone(&self.in_flight),
            published: false,
        };
        let outcome = op().await;
        guard.publish(outcome.clone());
        outcome.map(|value| (value, true))
    }

    /// Joins an existing leader without ever becoming one.
    pub async fn join(
        &self,
        fingerprint: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let existing = {
            let map = self.in_flight.lock().await;
            map.get(fingerprint)
                .cloned()
                .ok_or_else(|| "no in-flight leader".to_string())?
        };
        wait_for(existing, timeout).await
    }

    /// Drops all retained completed entries (index mutation / cache flush).
    pub async fn clear(&self) {
        self.in_flight.lock().await.clear();
    }
}

/// Releases a key when the leader is cancelled, panics, or returns without
/// publishing. Publish happens before removal, never after it.
struct LeaderGuard {
    key: String,
    entry: Arc<InFlight>,
    map: InFlightMap,
    published: bool,
}

impl LeaderGuard {
    fn publish(&mut self, outcome: Outcome) {
        // 1) set the result, 2) notify every current/late follower, 3) remove
        // the map entry after a short retention window.
        let _ = self.entry.tx.send(Some(outcome));
        self.published = true;
        let key = self.key.clone();
        let id = self.entry.id;
        let map = Arc::clone(&self.map);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(COMPLETED_RETENTION).await;
                let mut map = map.lock().await;
                remove_if_same(&mut map, &key, id);
            });
        }
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        tracing::warn!(key = %self.key, "singleflight leader abandoned; releasing key");
        let _ = self
            .entry
            .tx
            .send(Some(Err("leader_abandoned".to_string())));
        let key = self.key.clone();
        let id = self.entry.id;
        let map = Arc::clone(&self.map);
        if let Ok(mut guard) = self.map.try_lock() {
            remove_if_same(&mut guard, &key, id);
        } else if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut map = map.lock().await;
                remove_if_same(&mut map, &key, id);
            });
        }
    }
}

fn remove_if_same(map: &mut HashMap<String, Arc<InFlight>>, key: &str, id: u64) {
    if map.get(key).map(|e| e.id) == Some(id) {
        map.remove(key);
    }
}

async fn wait_for(entry: Arc<InFlight>, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut rx = entry.rx.clone();
    if let Some(outcome) = rx.borrow().clone() {
        return outcome;
    }
    match tokio::time::timeout(timeout, rx.changed()).await {
        Ok(Ok(())) => rx
            .borrow()
            .clone()
            .ok_or_else(|| "leader dropped before result".to_string())?,
        // Sender dropped: a previously published value may still be visible.
        Ok(Err(_)) => rx
            .borrow()
            .clone()
            .ok_or_else(|| "leader dropped before result".to_string())?,
        Err(_) => {
            tracing::warn!(
                timeout_ms = timeout.as_millis(),
                "singleflight follower timeout"
            );
            Err("singleflight_timeout".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_same_fingerprint_merges() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let counter = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let c = Arc::clone(&counter);
            let sf = Arc::clone(&sf);
            handles.push(tokio::spawn(async move {
                sf.execute("fp-1", || {
                    let c = Arc::clone(&c);
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Ok(serde_json::json!({"answer": 42}))
                    }
                })
                .await
                .unwrap()
            }));
        }
        let results = futures_join(handles).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
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
        let counter = Arc::new(AtomicU64::new(0));
        let c = Arc::clone(&counter);
        let (r1, _) = sf
            .execute("fp-a", || {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!(1))
                }
            })
            .await
            .unwrap();
        let (r2, _) = sf
            .execute("fp-b", || {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!(2))
                }
            })
            .await
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
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

    /// Part 30 §30.11: a follower that polls after publication must terminate
    /// with the stored result, not wait for a wakeup that will never arrive.
    #[tokio::test]
    async fn follower_arriving_after_publish_terminates() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let executed = Arc::new(AtomicU64::new(0));
        let leader_sf = Arc::clone(&sf);
        let leader_executed = Arc::clone(&executed);
        let leader = tokio::spawn(async move {
            leader_sf
                .execute("late", || {
                    let executed = Arc::clone(&leader_executed);
                    async move {
                        executed.fetch_add(1, Ordering::SeqCst);
                        Ok(serde_json::json!({"published": true}))
                    }
                })
                .await
        });
        leader.await.unwrap().unwrap();
        // The entry is intentionally retained briefly after send().
        tokio::time::sleep(Duration::from_millis(10)).await;
        let (value, was_leader) = sf
            .execute("late", || async {
                panic!("late follower must not execute a second time")
            })
            .await
            .unwrap();
        assert!(!was_leader);
        assert_eq!(value, serde_json::json!({"published": true}));
        assert_eq!(executed.load(Ordering::SeqCst), 1);
    }

    /// Part 30 §30.11: cancellation must not strand the key forever.
    #[tokio::test]
    async fn leader_cancel_releases_key() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let leader = tokio::spawn({
            let sf = Arc::clone(&sf);
            async move {
                sf.execute("cancelled", || async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(serde_json::json!({"never": true}))
                })
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        leader.abort();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (value, was_leader) = sf
            .execute("cancelled", || async {
                Ok(serde_json::json!({"ok": true}))
            })
            .await
            .unwrap();
        assert!(was_leader, "abandoned key was released");
        assert_eq!(value, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn follower_wait_is_bounded() {
        let sf = Arc::new(SingleflightCoordinator::new());
        let leader = tokio::spawn({
            let sf = Arc::clone(&sf);
            async move {
                sf.execute("slow", || async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok(serde_json::json!({"late": true}))
                })
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let err = sf
            .execute_with_timeout("slow", Duration::from_millis(20), || async {
                panic!("follower must never become leader")
            })
            .await
            .unwrap_err();
        assert_eq!(err, "singleflight_timeout");
        let _ = leader.await.unwrap();
    }

    async fn futures_join<T: Send + 'static>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    }
}
