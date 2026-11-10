use crate::bridge::protocol::{decode_bridge_frame, encode_bridge_frame};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("Kernel process error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Kernel process did not exit after SIGKILL")]
    Retirement,
    #[error("Kernel is closed")]
    Closed,
    #[error("Kernel startup failed: {0}")]
    Startup(String),
    #[error("Kernel exited before completing the cell")]
    Exited,
    #[error("JavaScript worker exited: {0}")]
    ExitedWithStatus(std::process::ExitStatus),
    #[error("{message}")]
    WorkerFailure {
        message: String,
        name: Option<String>,
        stack: Option<String>,
    },
    #[error("Kernel frame serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct SubprocessProcess {
    child: Child,
    /// Captured at spawn: `Child::id` goes `None` once the child is reaped, and the group
    /// survives a crash.
    group_pid: Option<u32>,
    input: mpsc::UnboundedSender<String>,
    output: mpsc::Receiver<Result<Value, ProcessError>>,
    crash_cause: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    retiring: bool,
}

impl SubprocessProcess {
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<Self, ProcessError> {
        Self::spawn_with_frame_token(command, args, cwd, env, None)
    }
    pub fn spawn_with_frame_token(
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &std::collections::HashMap<String, String>,
        token: Option<String>,
    ) -> Result<Self, ProcessError> {
        let mut command = tokio::process::Command::new(command);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let group_pid = child.id();
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| ProcessError::Startup("stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProcessError::Startup("stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProcessError::Startup("stderr unavailable".into()))?;
        let (sender, output) = mpsc::channel(256);
        let error_sender = sender.clone();
        let write_errors = sender.downgrade();
        // Match child.stdin.write: enqueue in order without waiting for pipe capacity.
        let (input_sender, mut writes) = mpsc::unbounded_channel::<String>();
        let writer_task = tokio::spawn(async move {
            while let Some(frame) = writes.recv().await {
                if let Err(error) = input.write_all(frame.as_bytes()).await {
                    if let Some(sender) = write_errors.upgrade() {
                        let _ = sender.send(Err(ProcessError::Io(error))).await;
                    }
                    return;
                }
            }
        });
        let crash_cause = std::sync::Arc::new(std::sync::Mutex::new(None));
        let stderr_crash_cause = crash_cause.clone();
        let stderr_token = token.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let message = match &token {
                            Some(token) => super::super::js::process_worker::decode_process_frame(&line, token),
                            None => decode_bridge_frame(&line, None).unwrap_or_else(|error| serde_json::json!({"type":"text","stream":"stderr","data":format!("{error}\n")})),
                        };
                        if sender.send(Ok(message)).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        if sender.send(Ok(serde_json::json!({"type":"init-failed","error":{"message":error.to_string()}}))).await.is_err() { return; }
                        return;
                    }
                }
            }
        });
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut chunk = [0u8; 8192];
            let mut tail = Vec::new();
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) => return,
                    Ok(length) => {
                        if let Some(token) = &stderr_token {
                            tail.extend_from_slice(&chunk[..length]);
                            if tail.len() > 16 * 1024 {
                                tail.drain(..tail.len() - 16 * 1024);
                            }
                            if let Some(cause) = super::super::js::process_worker::crash_cause_from(
                                &String::from_utf8_lossy(&tail),
                                token,
                            ) {
                                *stderr_crash_cause.lock().expect("JS crash cause lock") =
                                    Some(cause);
                            }
                        }
                        if error_sender.send(Ok(serde_json::json!({"type":"text","stream":"stderr","data":String::from_utf8_lossy(&chunk[..length])}))).await.is_err() { return; }
                    }
                    Err(error) => {
                        maho_ext_api::diagnostic(&format!("kernel stderr read failed: {error}"));
                        return;
                    }
                }
            }
        });
        Ok(Self {
            child,
            group_pid,
            input: input_sender,
            output,
            crash_cause,
            readers: vec![stdout_task, stderr_task, writer_task],
            retiring: false,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
    /// The process-group leader id, retained after reaping so descendants can still be killed.
    pub fn group_pid(&self) -> Option<u32> {
        self.group_pid
    }
    pub async fn send_token(&mut self, token: &str) -> Result<(), ProcessError> {
        self.input
            .send(format!("{token}\n"))
            .map_err(|_| ProcessError::Closed)?;
        Ok(())
    }
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus, ProcessError> {
        self.child.wait().await.map_err(ProcessError::Io)
    }
    pub async fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, ProcessError> {
        self.child.try_wait().map_err(ProcessError::Io)
    }
    pub fn is_retiring(&self) -> bool {
        self.retiring
    }
    pub async fn send(&mut self, message: &Value) -> Result<bool, ProcessError> {
        if self.retiring {
            return Ok(false);
        }
        self.input
            .send(encode_bridge_frame(message)?)
            .map_err(|_| ProcessError::Closed)?;
        Ok(true)
    }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> {
        if self.retiring {
            return Err(ProcessError::Exited);
        }
        match self.output.recv().await {
            Some(message) => message,
            None => {
                let status = self.child.wait().await?;
                let cause = self.crash_cause.lock().expect("JS crash cause lock").take();
                // A crashed kernel still owns its group: reap descendants before reporting the exit.
                self.sweep_group(self.group_pid).await;
                match cause {
                    Some(cause) => Err(super::super::js::worker_host::error_from_bridge(&cause)),
                    None => Err(ProcessError::ExitedWithStatus(status)),
                }
            }
        }
    }
    pub fn retire(&mut self) {
        self.retiring = true;
        for task in &self.readers {
            task.abort();
        }
        #[cfg(unix)]
        if let Some(pid) = self.group_pid
            && let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
    pub async fn terminate(
        &mut self,
        initial_signal: &str,
        escalation: Duration,
    ) -> Result<(), ProcessError> {
        self.retire();
        let pid = self.child.id();
        if self.child.try_wait()?.is_some() {
            self.sweep_group(pid).await;
            self.join_readers().await;
            return Ok(());
        }
        self.signal(initial_signal).await?;
        if let Ok(status) = tokio::time::timeout(escalation, self.child.wait()).await {
            status?;
            self.sweep_group(pid).await;
            self.join_readers().await;
            return Ok(());
        }
        self.signal("KILL").await?;
        tokio::time::timeout(Duration::from_millis(500), self.child.wait())
            .await
            .map_err(|_| ProcessError::Retirement)??;
        self.sweep_group(pid).await;
        self.join_readers().await;
        Ok(())
    }
    async fn join_readers(&mut self) {
        for reader in self.readers.drain(..) {
            let _ = reader.await;
        }
    }
    async fn sweep_group(&self, pid: Option<u32>) {
        #[cfg(unix)]
        if let Some(pid) = pid
            && let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        #[cfg(not(unix))]
        let _ = pid;
    }
    pub async fn shutdown(&mut self, frame: Option<&Value>) -> Result<(), ProcessError> {
        if let Some(frame) = frame {
            let _ = self.send(frame).await;
        }
        self.terminate("TERM", Duration::from_millis(1500)).await
    }
    async fn signal(&mut self, signal: &str) -> Result<(), ProcessError> {
        #[cfg(not(unix))]
        {
            let _ = signal;
            if let Some(pid) = self.child.id() {
                super::super::js::process_tree_host::terminate_process_trees(
                    &[pid],
                    super::super::js::process_tree_host::TerminateProcessTreesOptions {
                        grace_ms: 0,
                        kill_wait_ms: Some(500),
                        owner_pid: None,
                    },
                )
                .await;
            }
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            use rustix::process::Signal;
            let known = match signal {
                "HUP" | "1" => Some(Signal::HUP),
                "INT" | "2" => Some(Signal::INT),
                "QUIT" | "3" => Some(Signal::QUIT),
                "KILL" | "9" => Some(Signal::KILL),
                "TERM" | "15" => Some(Signal::TERM),
                _ => None,
            };
            if let Some(sig) = known {
                let sent = i32::try_from(pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                    .is_some_and(|pid| rustix::process::kill_process_group(pid, sig).is_ok());
                if sent {
                    return Ok(());
                }
                self.child.start_kill()?;
                return Ok(());
            }
            let status = tokio::process::Command::new("kill")
                .args([format!("-{signal}"), "--".into(), format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?;
            if status.success() {
                return Ok(());
            }
        }
        self.child.start_kill()?;
        Ok(())
    }
}

impl Drop for SubprocessProcess {
    fn drop(&mut self) {
        for task in &self.readers {
            task.abort();
        }
        // Dropping a crashed kernel must not leave its descendants alive.
        #[cfg(unix)]
        if let Some(pid) = self.group_pid
            && let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}
