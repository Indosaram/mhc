#[derive(Debug)]
pub struct TaskkillResult { pub status: Option<i32>, pub signal: Option<String> }
pub trait ProcessGroupRuntime {
    fn is_windows(&self) -> bool;
    fn run_taskkill(&self, pid: i32) -> Result<TaskkillResult, String>;
    fn kill_group(&self, pid: i32) -> Result<(), String>;
    fn probe_group(&self, pid: i32) -> std::io::Result<()>;
}
pub fn validate_process_group_pid(pid: f64) -> Result<i32, String> {
    if !pid.is_finite() || pid.fract() != 0.0 || pid <= 0.0 || pid > f64::from(i32::MAX) {
        return Err(format!("process-group pid must be a positive integer: {pid}"));
    }
    Ok(pid as i32)
}
pub fn terminate_process_group(pid: f64, runtime: &dyn ProcessGroupRuntime) -> Result<(), String> {
    let pid = validate_process_group_pid(pid)?;
    if !runtime.is_windows() { return runtime.kill_group(pid); }
    let result = runtime.run_taskkill(pid)?;
    if result.status == Some(0) { return Ok(()); }
    let detail = match result.signal { Some(signal) => format!("signal {signal}"), None => format!("exit code {}", result.status.map_or_else(|| "null".into(), |code| code.to_string())) };
    Err(format!("taskkill failed with {detail}"))
}
pub fn process_group_is_alive(pid: f64, runtime: &dyn ProcessGroupRuntime) -> Result<bool, String> {
    let pid = validate_process_group_pid(pid)?;
    Ok(match runtime.probe_group(pid) { Ok(()) => true, Err(error) => error.raw_os_error() == Some(1) })
}
pub struct NativeProcessGroupRuntime;
impl ProcessGroupRuntime for NativeProcessGroupRuntime {
    fn is_windows(&self) -> bool { cfg!(windows) }
    fn run_taskkill(&self, pid: i32) -> Result<TaskkillResult, String> {
        let status = std::process::Command::new("taskkill").args(["/pid", &pid.to_string(), "/T", "/F"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map_err(|error| error.to_string())?;
        Ok(TaskkillResult { status: status.code(), signal: None })
    }
    fn kill_group(&self, pid: i32) -> Result<(), String> {
        #[cfg(unix)]
        { match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), nix::sys::signal::Signal::SIGKILL) {
            Ok(())|Err(nix::errno::Errno::ESRCH)=>Ok(()),Err(error)=>Err(error.to_string()),
        } }
        #[cfg(not(unix))]
        { Err(format!("POSIX group {pid} unavailable")) }
    }
    fn probe_group(&self, pid: i32) -> std::io::Result<()> {
        #[cfg(unix)]
        { nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), None).map_err(|error| std::io::Error::from_raw_os_error(error as i32)) }
        #[cfg(not(unix))]
        { Err(std::io::Error::new(std::io::ErrorKind::Unsupported, format!("native group probe for {pid} unavailable"))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Runtime { windows: bool, status: Option<i32>, signal: Option<String>, errno: Option<i32> }
    impl ProcessGroupRuntime for Runtime {
        fn is_windows(&self) -> bool { self.windows }
        fn run_taskkill(&self, _: i32) -> Result<TaskkillResult, String> { Ok(TaskkillResult { status: self.status, signal: self.signal.clone() }) }
        fn kill_group(&self, pid: i32) -> Result<(), String> { assert_eq!(pid, 42); Ok(()) }
        fn probe_group(&self, _: i32) -> std::io::Result<()> { self.errno.map_or(Ok(()), |errno| Err(std::io::Error::from_raw_os_error(errno))) }
    }
    #[test]
    fn invalid_pid_never_reaches_runtime() {
        for pid in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY] { assert!(validate_process_group_pid(pid).is_err()); }
    }
    #[test]
    fn windows_failure_retains_signal_detail() {
        let runtime = Runtime { windows: true, status: None, signal: Some("SIGTERM".into()), errno: None };
        assert_eq!(terminate_process_group(42.0, &runtime).unwrap_err(), "taskkill failed with signal SIGTERM");
    }
    #[test]
    fn permission_denied_is_live_but_missing_is_dead() {
        let runtime = Runtime { windows: false, status: None, signal: None, errno: Some(1) };
        assert!(process_group_is_alive(42.0, &runtime).unwrap());
        assert!(!process_group_is_alive(42.0, &Runtime { errno: Some(3), ..runtime }).unwrap());
    }
}
