//! RFC-081 / RFC-082 / RFC-083: the external-process extension runtime.
//!
//! An extension manifest can declare `runtime: external_process` with an
//! `entry` command line. The host spawns that process and speaks ONE simple
//! protocol over its stdin/stdout: line-delimited JSON, one request in flight
//! at a time, replies in order.
//!
//! ```text
//! host → child   {"op":"health"}
//! child → host   {"ok":true}
//!
//! host → child   {"op":"invoke","args":{…}}
//! child → host   {"ok":true,"result":{…}}      (or {"ok":false,"error":"…"})
//! ```
//!
//! Isolation (RFC-083) is enforced here, not trusted to the child:
//! * every call has a deadline; a timeout KILLS the child and quarantines it;
//! * a child that exits / closes stdout is quarantined, the host survives;
//! * stderr is drained into a bounded buffer (never blocking the child, never
//!   growing without limit), so diagnostics exist without a deadlock;
//! * `Drop` kills and reaps the child — no orphan processes.
//!
//! The host is deliberately transport-agnostic: crawler extensions (RFC-082)
//! use it via [`ExternalProcessHost::invoke`] with `op: "crawl"`.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// Protocol-level failure of one extension call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalHostError {
    /// The child could not be spawned.
    Spawn(String),
    /// The call did not finish before its deadline (child was killed).
    Timeout { timeout_ms: u64 },
    /// The child exited or closed its stdout.
    Crashed(String),
    /// The reply was not valid JSON or lacked the expected shape.
    Protocol(String),
    /// The child replied `{"ok":false,…}`.
    Rejected(String),
    /// A previous failure quarantined this host.
    Quarantined(String),
}

impl std::fmt::Display for ExternalHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "spawn failed: {e}"),
            Self::Timeout { timeout_ms } => write!(f, "timed out after {timeout_ms}ms"),
            Self::Crashed(e) => write!(f, "process exited: {e}"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::Rejected(e) => write!(f, "extension rejected the call: {e}"),
            Self::Quarantined(e) => write!(f, "quarantined: {e}"),
        }
    }
}

impl std::error::Error for ExternalHostError {}

/// Process isolation state, exposed for lifecycle bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Running,
    /// The process is gone or misbehaved; the host (and the gateway) survive.
    Quarantined,
    /// Explicitly stopped; a later `restart` may bring it back.
    Stopped,
}

/// Bounded stderr capture (last N bytes).
const STDERR_CAPACITY: usize = 8 * 1024;
/// Maximum accepted response line (a hostile child cannot exhaust memory).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

struct Inner {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    stderr: Arc<std::sync::Mutex<String>>,
    state: HostState,
    quarantine_reason: Option<String>,
}

async fn read_bounded_line<R>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        let content_count = if newline.is_some() { count - 1 } else { count };
        if line.len().saturating_add(content_count) > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "extension response line exceeds the configured limit",
            ));
        }
        line.extend_from_slice(&available[..content_count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

/// One running external extension process.
pub struct ExternalProcessHost {
    id: String,
    command: String,
    timeout: Duration,
    inner: Mutex<Inner>,
}

impl ExternalProcessHost {
    /// Spawns the executable with explicit argv. No shell parsing occurs.
    pub async fn spawn(
        id: impl Into<String>,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Self, ExternalHostError> {
        if program.trim().is_empty() {
            return Err(ExternalHostError::Spawn("program must not be empty".into()));
        }
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ExternalHostError::Spawn(format!("{program}: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        let stderr_pipe = child.stderr.take();
        let stderr = Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(pipe) = stderr_pipe {
            let buffer = Arc::clone(&stderr);
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut buf) = buffer.lock() {
                        if buf.len() < STDERR_CAPACITY {
                            let room = STDERR_CAPACITY - buf.len();
                            let line = if line.len() > room {
                                &line[..room]
                            } else {
                                &line
                            };
                            buf.push_str(line);
                            buf.push('\n');
                        }
                    }
                }
            });
        }
        Ok(Self {
            id: id.into(),
            command: std::iter::once(program.to_string())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            timeout,
            inner: Mutex::new(Inner {
                child,
                stdin,
                stdout,
                stderr,
                state: HostState::Running,
                quarantine_reason: None,
            }),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout.as_millis() as u64
    }

    /// Current isolation state.
    pub async fn state(&self) -> HostState {
        self.inner.lock().await.state
    }

    /// Captured stderr tail (bounded).
    pub async fn stderr_tail(&self) -> String {
        let guard = self.inner.lock().await;
        guard
            .stderr
            .lock()
            .map(|buf| buf.clone())
            .unwrap_or_default()
    }

    /// Liveness probe: `{"op":"health"}` must answer `{"ok":true}`.
    pub async fn health(&self) -> Result<(), ExternalHostError> {
        let reply = self.call(json!({"op":"health"})).await?;
        if reply.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            Ok(())
        } else {
            Err(ExternalHostError::Rejected(
                reply.to_string().chars().take(200).collect(),
            ))
        }
    }

    /// Invokes one operation and returns its `result`.
    pub async fn invoke(&self, args: Value) -> Result<Value, ExternalHostError> {
        let reply = self.call(json!({"op":"invoke","args":args})).await?;
        if reply.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(ExternalHostError::Rejected(
                reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("extension reported failure")
                    .to_string(),
            ))
        }
    }

    /// Raw request/response round trip with the isolation guarantees.
    async fn call(&self, request: Value) -> Result<Value, ExternalHostError> {
        let mut inner = self.inner.lock().await;
        if inner.state == HostState::Quarantined {
            return Err(ExternalHostError::Quarantined(
                inner.quarantine_reason.clone().unwrap_or_default(),
            ));
        }
        if inner.state == HostState::Stopped {
            return Err(ExternalHostError::Crashed("host is stopped".into()));
        }
        let line = serde_json::to_string(&request)
            .map_err(|e| ExternalHostError::Protocol(e.to_string()))?;
        let stdin = inner
            .stdin
            .as_mut()
            .ok_or_else(|| ExternalHostError::Crashed("stdin is closed".into()))?;
        if let Err(e) = stdin.write_all(format!("{line}\n").as_bytes()).await {
            return self.quarantine(&mut inner, format!("stdin write: {e}"));
        }
        if let Err(e) = stdin.flush().await {
            return self.quarantine(&mut inner, format!("stdin flush: {e}"));
        }
        let stdout = inner
            .stdout
            .as_mut()
            .ok_or_else(|| ExternalHostError::Crashed("stdout is closed".into()))?;
        let read = tokio::time::timeout(self.timeout, read_bounded_line(stdout)).await;
        let reply = match read {
            Err(_) => {
                // Isolation: a hung child is killed, never waited on.
                let _ = inner.child.start_kill();
                return self.quarantine(
                    &mut inner,
                    format!("no reply within {}ms", self.timeout.as_millis()),
                );
            }
            Ok(Err(e)) => {
                return self.quarantine(&mut inner, format!("stdout read: {e}"));
            }
            Ok(Ok(None)) => {
                return self.quarantine(&mut inner, "child closed stdout".into());
            }
            Ok(Ok(Some(line))) if line.len() > MAX_LINE_BYTES => {
                return self.quarantine(&mut inner, "reply exceeds the line budget".into());
            }
            Ok(Ok(Some(line))) => line,
        };
        let reply = std::str::from_utf8(&reply)
            .map_err(|e| ExternalHostError::Protocol(format!("reply is not UTF-8: {e}")))?;
        let reply = reply.trim();
        if reply.is_empty() {
            return self.quarantine(&mut inner, "empty reply line".into());
        }
        serde_json::from_str::<Value>(reply).map_err(|e| {
            ExternalHostError::Protocol(format!(
                "{e}: {}",
                reply.chars().take(200).collect::<String>()
            ))
        })
    }

    fn quarantine<T>(&self, inner: &mut Inner, reason: String) -> Result<T, ExternalHostError> {
        inner.state = HostState::Quarantined;
        inner.quarantine_reason = Some(reason.clone());
        inner.stdin = None;
        inner.stdout = None;
        let _ = inner.child.start_kill();
        Err(ExternalHostError::Quarantined(reason))
    }

    /// Stops the child deliberately (keeps the record; `restart` can revive).
    pub async fn stop(&self) {
        let mut inner = self.inner.lock().await;
        inner.stdin = None;
        inner.stdout = None;
        let _ = inner.child.start_kill();
        let _ = inner.child.wait().await;
        inner.state = HostState::Stopped;
    }

    /// RFC-082 crawler op over the same protocol: asks the extension for
    /// items it can extract from `url`. The response shape is
    /// `{"items":[{"url":…,"title":…,"content":…}]}`; anything else is a
    /// protocol error, never guessed at.
    pub async fn crawl(&self, url: &str) -> Result<Vec<serde_json::Value>, ExternalHostError> {
        let result = self.invoke(json!({"op":"crawl","url":url})).await;
        let result = match result {
            // The child may implement `crawl` as the top-level op instead of
            // nesting it under `invoke`; retry that shape once on rejection.
            Err(ExternalHostError::Rejected(_)) => {
                self.call(json!({"op":"crawl","url":url})).await?
            }
            other => other?,
        };
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ExternalHostError::Protocol("crawl reply lacks items[]".into()))?;
        Ok(items.clone())
    }
}

impl Drop for ExternalProcessHost {
    fn drop(&mut self) {
        // Best effort: kill the child so a dropped extension never leaks a
        // process. `kill_on_drop(true)` covers the blocking case too.
        if let Ok(mut inner) = self.inner.try_lock() {
            let _ = inner.child.start_kill();
        }
    }
}

/// Runtime manager for external process hosts. This is the process-side
/// counterpart of `ExtensionRegistry`: manifests still own lifecycle state,
/// while this registry owns the live child handles.
#[derive(Default)]
pub struct ExternalProcessRegistry {
    hosts: std::sync::RwLock<BTreeMap<String, Arc<ExternalProcessHost>>>,
}

impl ExternalProcessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn and health-check a manifest before publishing it to callers.
    pub async fn launch(
        &self,
        manifest: &crate::manifest::ExtensionManifest,
    ) -> Result<Arc<ExternalProcessHost>, ExternalHostError> {
        if manifest.runtime != crate::manifest::RuntimeKind::ExternalProcess {
            return Err(ExternalHostError::Protocol(
                "manifest runtime is not external_process".into(),
            ));
        }
        let program = manifest
            .entry
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| ExternalHostError::Spawn("manifest entry is required".into()))?;
        let timeout = Duration::from_millis(
            manifest
                .permissions
                .budget_timeout_ms
                .ok_or_else(|| ExternalHostError::Spawn("budget_timeout_ms is required".into()))?
                .clamp(1, 300_000),
        );
        let host =
            Arc::new(Self::spawn_and_check(&manifest.id, program, &manifest.args, timeout).await?);
        let mut hosts = self
            .hosts
            .write()
            .map_err(|_| ExternalHostError::Protocol("registry lock poisoned".into()))?;
        if hosts.contains_key(&manifest.id) {
            return Err(ExternalHostError::Protocol(format!(
                "extension {} already launched",
                manifest.id
            )));
        }
        hosts.insert(manifest.id.clone(), Arc::clone(&host));
        Ok(host)
    }

    async fn spawn_and_check(
        id: &str,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<ExternalProcessHost, ExternalHostError> {
        let host = ExternalProcessHost::spawn(id, program, args, timeout).await?;
        host.health().await?;
        Ok(host)
    }

    pub fn get(&self, id: &str) -> Option<Arc<ExternalProcessHost>> {
        self.hosts.read().ok()?.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<String> {
        self.hosts
            .read()
            .map(|hosts| hosts.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn stop_all(&self) {
        let hosts = self
            .hosts
            .read()
            .map(|hosts| hosts.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for host in hosts {
            host.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a shell script that answers each request line with `reply`, then
    /// spawns it. Keeping the script in a file avoids argv quoting ambiguity.
    async fn script_host(
        name: &str,
        reply: &str,
        timeout: Duration,
    ) -> (ExternalProcessHost, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nwhile IFS= read -r line; do\n  echo '{}'\ndone\n",
                reply
            ),
        )
        .unwrap();
        let host = ExternalProcessHost::spawn(name, "sh", &[path.display().to_string()], timeout)
            .await
            .unwrap();
        (host, dir)
    }

    #[tokio::test]
    async fn echo_child_does_not_fabricate_health_success() {
        let host = ExternalProcessHost::spawn("cat", "cat", &[], Duration::from_secs(2))
            .await
            .unwrap();
        // `cat` echoes `{"op":"health"}`: no `ok` field, so health must not
        // claim success — a protocol-level rejection, NOT a quarantine.
        assert!(matches!(
            host.health().await,
            Err(ExternalHostError::Rejected(_))
        ));
        assert_eq!(host.state().await, HostState::Running);
    }

    #[tokio::test]
    async fn health_and_invoke_round_trip_a_fixed_reply() {
        let (host, _dir) = script_host(
            "fixed",
            r#"{"ok":true,"result":"pong"}"#,
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(host.health().await, Ok(()));
        assert_eq!(
            host.invoke(serde_json::json!({"x": 1})).await.unwrap(),
            serde_json::json!("pong")
        );
        assert_eq!(host.state().await, HostState::Running);
    }

    #[tokio::test]
    async fn child_crash_quarantines_the_host_and_the_host_survives() {
        let host = ExternalProcessHost::spawn(
            "one-shot",
            "sh",
            &["-c".into(), "head -n 1 >/dev/null".into()],
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let first = host.call(serde_json::json!({"op": "health"})).await;
        assert!(first.is_err(), "child exits after one line");
        assert_eq!(host.state().await, HostState::Quarantined);
        assert!(matches!(
            host.invoke(serde_json::json!({})).await,
            Err(ExternalHostError::Quarantined(_))
        ));
    }

    #[tokio::test]
    async fn hung_child_times_out_and_is_killed() {
        let host = ExternalProcessHost::spawn(
            "hang",
            "sh",
            &["-c".into(), "sleep 30".into()],
            Duration::from_millis(150),
        )
        .await
        .unwrap();
        let started = std::time::Instant::now();
        let result = host.health().await;
        assert!(matches!(result, Err(ExternalHostError::Quarantined(_))));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must be enforced, not waited out"
        );
        assert_eq!(host.state().await, HostState::Quarantined);
    }

    #[tokio::test]
    async fn malformed_reply_is_a_protocol_error_not_a_fabricated_result() {
        let (host, _dir) = script_host("garbage", "not-json", Duration::from_secs(2)).await;
        assert!(matches!(
            host.health().await,
            Err(ExternalHostError::Protocol(_))
        ));
        assert_eq!(host.state().await, HostState::Running);
    }

    #[tokio::test]
    async fn crawler_op_returns_items_and_rejects_missing_shape() {
        let (host, _dir) = script_host(
            "crawler",
            r#"{"ok":true,"result":{"items":[{"url":"https://example.com/a","title":"A"}]}}"#,
            Duration::from_secs(2),
        )
        .await;
        let items = host.crawl("https://example.com/a").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["title"], "A");

        let (host2, _dir2) =
            script_host("bad", r#"{"ok":true,"result":{}}"#, Duration::from_secs(2)).await;
        assert!(host2.crawl("https://example.com/a").await.is_err());
    }
}
