use std::path::Path;
#[cfg(unix)]
fn kill(pid: i32, signal: nix::sys::signal::Signal) -> Result<(), String> {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
#[cfg(unix)]
pub fn run(args: &[String], run_dir: &Path) -> Result<(), String> {
    use nix::sys::signal::Signal;
    let child: serde_json::Value = super::super::run_artifacts::read_run_json(&run_dir.join("child-started.json")).map_err(|error| error.to_string())?;
    let child_pid = child["pid"].as_i64().and_then(|pid| i32::try_from(pid).ok()).ok_or("invalid child pid")?;
    if let Some(index) = args.iter().position(|arg| arg == "--signal-group") {
        let pid = args.get(index + 1).and_then(|pid| pid.parse::<i32>().ok()).ok_or("signal-group fixture requires a pid and supported signal")?;
        let signal = match args.get(index + 2).map(String::as_str) {
            Some("SIGTERM") => Signal::SIGTERM, Some("SIGKILL") => Signal::SIGKILL,
            _ => return Err("signal-group fixture requires a pid and supported signal".into()),
        };
        kill(child_pid, signal)?;
        if signal == Signal::SIGKILL { kill(pid, signal)?; }
    } else {
        let index = args.iter().position(|arg| arg == "/pid").ok_or("taskkill fixture requires a pid")?;
        let pid = args.get(index + 1).and_then(|pid| pid.parse::<i32>().ok()).ok_or("taskkill fixture requires a pid")?;
        super::super::run_artifacts::write_run_json_atomic(&run_dir.join("taskkill-invocation.json"), &serde_json::json!({"args":args}), 0o600).map_err(|error| error.to_string())?;
        kill(child_pid, Signal::SIGKILL)?; kill(pid, Signal::SIGKILL)?;
    }
    Ok(())
}
