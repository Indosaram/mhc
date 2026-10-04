use std::{path::Path,time::Duration,collections::BTreeMap};
use std::os::unix::process::ExitStatusExt;

#[derive(Debug)]
pub enum ModelProbeError {
    Io(std::io::Error), Timeout { timeout_ms:u64 }, Exit { exit_code:Option<i32>,signal:Option<i32> },
}
impl std::fmt::Display for ModelProbeError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Timeout { timeout_ms } => write!(f,"cursor-agent models exceeded its {timeout_ms}ms deadline"),
            Self::Exit { exit_code,signal } => write!(f,"cursor-agent models failed with code {exit_code:?} and signal {signal:?}"),
        }
    }
}
impl std::error::Error for ModelProbeError {}
pub async fn run_models_probe(executable:&Path,stdout_path:&Path,timeout_ms:u64,home:&str,environment:&BTreeMap<String,String>) -> Result<(),ModelProbeError> {
    let output = std::fs::File::create(stdout_path).map_err(ModelProbeError::Io)?;
    let mut child = tokio::process::Command::new(executable).arg("models")
        .env_clear().envs(crate::environment::cursor_agent_environment(home,environment))
        .stdin(std::process::Stdio::null()).stdout(output).stderr(std::process::Stdio::null()).kill_on_drop(true)
        .spawn().map_err(ModelProbeError::Io)?;
    match tokio::time::timeout(Duration::from_millis(timeout_ms),child.wait()).await {
        Ok(status) => {
            let status = status.map_err(ModelProbeError::Io)?;
            if status.success() { Ok(()) } else { Err(ModelProbeError::Exit { exit_code:status.code(),signal:status.signal() }) }
        }
        Err(_) => {
            child.kill().await.map_err(ModelProbeError::Io)?;
            Err(ModelProbeError::Timeout { timeout_ms })
        }
    }
}
