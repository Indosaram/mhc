use std::{collections::HashMap, future::Future, path::PathBuf, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

pub struct KernelSpawnOptions {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
#[error("Python kernel process{} did not exit after SIGKILL", .0.map_or(String::new(), |pid| format!(" {pid}")))]
pub struct PythonKernelRetirementError(pub Option<u32>);

pub fn split_command(command_line: &str) -> Result<(String, Vec<String>), &'static str> {
    let mut parts = command_line.split(' ').filter(|part| !part.is_empty());
    let command = parts.next().ok_or("Python interpreter path is empty")?;
    Ok((command.into(), parts.map(str::to_owned).collect()))
}

pub fn default_spawn(options: &KernelSpawnOptions) -> Result<Child, std::io::Error> {
    let mut command = Command::new(&options.command);
    command.args(&options.args).current_dir(&options.cwd).env_clear().envs(&options.env)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0).env("SENPI_PY_KERNEL_PARENT_PID", std::process::id().to_string());
    command.spawn()
}

pub fn number_or_null(value: &serde_json::Value) -> Option<f64> { value.as_f64() }
pub fn signal_or_null(value: &serde_json::Value) -> Option<&str> { value.as_str() }

pub async fn with_timeout<T>(operation: impl Future<Output = T>, timeout: Duration, message: &str) -> Result<T, String> {
    tokio::time::timeout(timeout, operation).await.map_err(|_| message.to_string())
}

pub async fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<bool, std::io::Error> {
    match tokio::time::timeout(timeout, child.wait()).await { Ok(status) => status.map(|_| true), Err(_) => Ok(false) }
}

pub async fn sweep_process_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Like upstream, a missing group or a permission error cannot fail graceful close.
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }
    #[cfg(not(unix))]
    let _ = pid;
}

pub async fn hard_kill(child: &mut Child, timeout: Duration) -> Result<(), PythonKernelRetirementError> {
    let pid = child.id();
    #[cfg(unix)]
    let delivered = if let Some(pid) = pid {
        Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await.is_ok_and(|status| status.success())
    } else { false };
    #[cfg(not(unix))]
    let delivered = false;
    if !delivered && child.start_kill().is_err() { return Ok(()); }
    match wait_for_exit(child, timeout).await {
        Ok(true) => Ok(()),
        _ => Err(PythonKernelRetirementError(pid)),
    }
}
