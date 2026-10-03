use crate::bridge::protocol::{decode_bridge_frame, encode_bridge_frame};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
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
    #[error("Kernel frame serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct SubprocessProcess {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Value>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    retiring: bool,
}

impl SubprocessProcess {
    pub fn spawn(command: &str, args: &[String], cwd: &Path, env: &std::collections::HashMap<String, String>) -> Result<Self, ProcessError> {
        let mut command = tokio::process::Command::new(command);
        command.args(args).current_dir(cwd).env_clear().envs(env)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let input = child.stdin.take().ok_or_else(|| ProcessError::Startup("stdin unavailable".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| ProcessError::Startup("stdout unavailable".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| ProcessError::Startup("stderr unavailable".into()))?;
        let (sender, output) = mpsc::channel(256);
        let error_sender = sender.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let message = decode_bridge_frame(&line, None).unwrap_or_else(|error| serde_json::json!({"type":"text","stream":"stderr","data":format!("{error}\n")}));
                        if sender.send(message).await.is_err() { return; }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        if sender.send(serde_json::json!({"type":"init-failed","error":{"message":error.to_string()}})).await.is_err() { return; }
                        return;
                    }
                }
            }
        });
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut chunk = [0u8; 8192];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) => return,
                    Ok(length) => {
                        if error_sender.send(serde_json::json!({"type":"text","stream":"stderr","data":String::from_utf8_lossy(&chunk[..length])})).await.is_err() { return; }
                    }
                    Err(error) => { eprintln!("kernel stderr read failed: {error}"); return; }
                }
            }
        });
        Ok(Self { child, input, output, readers: vec![stdout_task, stderr_task], retiring: false })
    }

    pub fn pid(&self) -> Option<u32> { self.child.id() }
    pub fn is_retiring(&self) -> bool { self.retiring }
    pub async fn send(&mut self, message: &Value) -> Result<bool, ProcessError> {
        if self.retiring { return Ok(false); }
        self.input.write_all(encode_bridge_frame(message)?.as_bytes()).await?;
        self.input.flush().await?;
        Ok(true)
    }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> {
        if self.retiring { return Err(ProcessError::Exited); }
        self.output.recv().await.ok_or(ProcessError::Exited)
    }
    pub fn retire(&mut self) {
        self.retiring = true;
        for task in &self.readers { task.abort(); }
    }
    pub async fn terminate(&mut self, initial_signal: &str, escalation: Duration) -> Result<(), ProcessError> {
        self.retire();
        if self.child.try_wait()?.is_some() { return Ok(()); }
        self.signal(initial_signal).await?;
        if let Ok(status) = tokio::time::timeout(escalation, self.child.wait()).await { status?; return Ok(()); }
        self.signal("KILL").await?;
        tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await.map_err(|_| ProcessError::Retirement)??;
        Ok(())
    }
    pub async fn shutdown(&mut self, frame: Option<&Value>) -> Result<(), ProcessError> {
        if let Some(frame) = frame { let _ = self.send(frame).await; }
        self.terminate("TERM", Duration::from_millis(1500)).await
    }
    async fn signal(&mut self, signal: &str) -> Result<(), ProcessError> {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let status = tokio::process::Command::new("kill").args([format!("-{signal}"), "--".into(), format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await?;
            if status.success() { return Ok(()); }
        }
        self.child.start_kill()?;
        Ok(())
    }
}

impl Drop for SubprocessProcess {
    fn drop(&mut self) {
        for task in &self.readers { task.abort(); }
    }
}
