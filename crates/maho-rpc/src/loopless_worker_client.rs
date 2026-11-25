//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-worker-client.ts`.

use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Notify, oneshot};

use crate::loopless_runtime_settings::LooplessCliConfiguration;
use crate::loopless_worker_protocol::{HostToLooplessWorker, LooplessWorkerToHost};

/// The host's side of one loopless worker isolate (`senpi#5413`): start it in a cwd, then close it.
#[async_trait]
pub trait LooplessWorker: Send + Sync {
    async fn wait_exited(&self);
    fn is_exited(&self) -> bool;
    async fn start(
        &self,
        configuration: LooplessCliConfiguration,
        cwd: String,
    ) -> Result<Vec<String>, String>;
    /// Asks the isolate to dispose its runtime; past `grace_ms` the isolate is terminated. Settles
    /// once it exited.
    async fn close(&self, grace_ms: u64);
}

struct ChildState {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pending_ready: Option<oneshot::Sender<Result<Vec<String>, String>>>,
    failure: Option<String>,
    exited: bool,
}

/// Subprocess-backed `LooplessWorker` speaking JSONL over stdin/stdout.
pub struct SubprocessLooplessWorker {
    state: Arc<Mutex<ChildState>>,
    exit_notify: Arc<Notify>,
}

impl SubprocessLooplessWorker {
    pub fn spawn(mut command: Command) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        let state = Arc::new(Mutex::new(ChildState {
            child: Some(child),
            stdin,
            pending_ready: None,
            failure: None,
            exited: false,
        }));
        let exit_notify = Arc::new(Notify::new());

        if let Some(stdout) = stdout {
            let state_reader = Arc::clone(&state);
            let notify_reader = Arc::clone(&exit_notify);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(msg) = serde_json::from_str::<LooplessWorkerToHost>(trimmed) {
                        let tx = state_reader
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .pending_ready
                            .take();
                        if let Some(tx) = tx {
                            let _ = match msg {
                                LooplessWorkerToHost::Ready { tools } => tx.send(Ok(tools)),
                                LooplessWorkerToHost::Failure { error } => tx.send(Err(error)),
                            };
                        }
                    }
                }
                let child = state_reader
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .child
                    .take();
                if let Some(mut child) = child {
                    let _ = child.wait().await;
                }
                let tx = {
                    let mut guard = state_reader.lock().unwrap_or_else(PoisonError::into_inner);
                    guard.exited = true;
                    guard.pending_ready.take()
                };
                if let Some(tx) = tx {
                    let reason = state_reader
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .failure
                        .clone()
                        .unwrap_or_else(|| "loopless_worker_exited".to_owned());
                    let _ = tx.send(Err(reason));
                }
                notify_reader.notify_waiters();
            });
        }

        Ok(Self { state, exit_notify })
    }

    async fn send(&self, msg: &HostToLooplessWorker) -> Result<(), String> {
        let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut stdin = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stdin
            .take()
            .ok_or_else(|| "loopless_worker_stdin_closed".to_owned())?;
        let res = stdin.write_all(line.as_bytes()).await;
        let flush = if res.is_ok() {
            stdin.flush().await
        } else {
            Ok(())
        };
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stdin = Some(stdin);
        res.and(flush).map_err(|e| e.to_string())
    }
}

#[async_trait]
impl LooplessWorker for SubprocessLooplessWorker {
    async fn wait_exited(&self) {
        loop {
            let notified = self.exit_notify.notified();
            if self.is_exited() {
                return;
            }
            notified.await;
        }
    }

    fn is_exited(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exited
    }

    async fn start(
        &self,
        configuration: LooplessCliConfiguration,
        cwd: String,
    ) -> Result<Vec<String>, String> {
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.exited {
                return Err(guard
                    .failure
                    .clone()
                    .unwrap_or_else(|| "loopless_worker_exited".to_owned()));
            }
            guard.pending_ready = Some(tx);
        }
        if let Err(err) = self
            .send(&HostToLooplessWorker::Start { configuration, cwd })
            .await
        {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pending_ready = None;
            return Err(err);
        }
        rx.await
            .unwrap_or_else(|_| Err("loopless_worker_exited".to_owned()))
    }

    async fn close(&self, grace_ms: u64) {
        let _ = self.send(&HostToLooplessWorker::Close).await;
        if tokio::time::timeout(Duration::from_millis(grace_ms), self.wait_exited())
            .await
            .is_err()
        {
            let child = self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .child
                .take();
            if let Some(mut child) = child {
                let _ = child.kill().await;
            }
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            guard.exited = true;
            if let Some(tx) = guard.pending_ready.take() {
                let _ = tx.send(Err("loopless_worker_exited".to_owned()));
            }
            self.exit_notify.notify_waiters();
        }
    }
}

/// Spawns the current executable in `--loopless-worker` mode (or custom command builder).
pub fn start_loopless_worker_with(command: Command) -> Result<Arc<dyn LooplessWorker>, String> {
    Ok(Arc::new(SubprocessLooplessWorker::spawn(command)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subprocess_worker_handles_ready_and_close() {
        // Spawn a tiny shell script pretending to be a loopless worker over stdin/stdout.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(
            "read line; printf '{\"type\":\"ready\",\"tools\":[\"read\"]}\\n'; read close_line; exit 0",
        );
        let worker = SubprocessLooplessWorker::spawn(cmd).expect("spawn");
        let tools = worker
            .start(LooplessCliConfiguration::default(), "/tmp".into())
            .await
            .expect("ready");
        assert_eq!(tools, vec!["read"]);
        worker.close(2_000).await;
        assert!(worker.is_exited());
    }
}
