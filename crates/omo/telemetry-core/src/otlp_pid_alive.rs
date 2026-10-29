//! Rust port of `packages/telemetry-core/src/otlp-pid-alive.ts`.

/// Probes a pid without signalling it. A process that exists but belongs to another user still
/// answers alive, exactly like the TS `EPERM` case.
pub fn is_pid_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        return std::path::Path::new(&format!("/proc/{pid}")).exists();
    }
    // This crate forbids `unsafe`, so the non-Linux probe shells out to `kill -0` rather than calling
    // `libc::kill`. "Not permitted" still means the process exists.
    match std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
    {
        Ok(output) => {
            output.status.success()
                || String::from_utf8_lossy(&output.stderr).contains("permitted")
        }
        Err(_) => true,
    }
}
