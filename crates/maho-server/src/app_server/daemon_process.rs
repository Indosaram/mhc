use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonPidFile {
    pub pid: u64,
    pub process_start_time: Option<String>,
}
pub fn parse_daemon_pid_file(text: &str) -> Option<DaemonPidFile> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let pid = object.get("pid")?.as_u64().filter(|pid| *pid > 0)?;
    let identity = match object.get("processStartTime")? {
        Value::Null => None,
        Value::String(identity) if !identity.trim().is_empty() => Some(identity.clone()),
        _ => return None,
    };
    Some(DaemonPidFile { pid, process_start_time: identity })
}

pub async fn process_is_live(pid: u64) -> Result<bool, std::io::Error> {
    let output = tokio::process::Command::new("kill").args(["-0", &pid.to_string()]).output().await?;
    if output.status.success() { return Ok(true); }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("No such process") { return Ok(false); }
    if error.contains("Operation not permitted") { return Ok(true); }
    Err(std::io::Error::other(error.into_owned()))
}
pub async fn read_process_start_time(pid: u64) -> Result<Option<String>, std::io::Error> {
    let output = tokio::process::Command::new("ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().await?;
    if !output.status.success() {
        if output.status.code() == Some(1) || !process_is_live(pid).await? { return Ok(None); }
        return Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    let identity = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if identity.is_empty() { return Err(std::io::Error::other("invalid process identity output")); }
    Ok(Some(identity))
}
pub async fn process_matches_pid_file(file: &DaemonPidFile) -> Result<bool, std::io::Error> {
    let Some(identity) = &file.process_start_time else {
        if !process_is_live(file.pid).await? { return Ok(false); }
        return Err(std::io::Error::other(format!("process identity for live pid {} stayed unreadable after 0 probe attempt(s): pidfile carries no process identity guard",file.pid)));
    };
    let mut last_error = String::new();
    for attempt in 1..=5 {
        match read_process_start_time(file.pid).await {
            Ok(Some(current)) => return Ok(&current == identity),
            Ok(None) => last_error = "process identity probe returned no identity for a live process".into(),
            Err(error) => last_error = error.to_string(),
        }
        if !process_is_live(file.pid).await? { return Ok(false); }
        if attempt < 5 { tokio::time::sleep(std::time::Duration::from_millis(200)).await; }
    }
    Err(std::io::Error::other(format!("process identity for live pid {} stayed unreadable after 5 probe attempt(s): {last_error}",file.pid)))
}
pub async fn wait_for_gone(file: &DaemonPidFile, timeout_ms: u64) -> Result<bool, std::io::Error> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while tokio::time::Instant::now() <= deadline {
        if !process_matches_pid_file(file).await? { return Ok(true); }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(!process_matches_pid_file(file).await?)
}
pub async fn stop_validated_pid(file: &DaemonPidFile, signal: &str) -> Result<(), std::io::Error> {
    if !process_matches_pid_file(file).await? { return Ok(()); }
    let output = tokio::process::Command::new("kill").args([signal, &file.pid.to_string()]).output().await?;
    if !output.status.success() && !String::from_utf8_lossy(&output.stderr).contains("No such process") { return Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned())); }
    match signal {
        "-TERM" | "-SIGTERM" => { wait_for_gone(file,10_000).await?; },
        "-KILL" | "-SIGKILL" => { wait_for_gone(file,2_000).await?; },
        _ => {},
    }
    Ok(())
}
