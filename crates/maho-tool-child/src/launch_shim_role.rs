//! The `launch-shim` role (senpi#3566, plan todo 4, Windows): started by a tool child for an explicit `detached: true`
//! launch. It starts the command outside the child's Job Object, writes `{"pid":N}` on fd 3 so the child can report
//! `command_started{breakaway:true}`, waits, and exits with the command's exit code. The shim itself stays in the job.
use crate::launch_env_names::{LAUNCH_SHIM_COMMAND_LINE_ENV, ProcessEnv};

/// The descriptor the shim reports the command's pid on.
pub const PID_FD: i32 = 3;

/// `runLaunchShimRole`. `write_pid` writes the given line to [`PID_FD`]; the Windows caller passes [`write_pid_fd`].
pub fn run_launch_shim_role(
    env: &ProcessEnv,
    write_pid: impl FnOnce(&str) -> std::io::Result<()>,
) -> ! {
    let command_line = env.get(LAUNCH_SHIM_COMMAND_LINE_ENV);
    let kernel = crate::windows_breakaway::load_kernel32_process();
    let kernel_available = kernel.is_some();
    if command_line.is_none_or(|line| line.is_empty()) || !kernel_available {
        eprintln!(
            "launch-shim: {}",
            if kernel_available {
                "command line unavailable"
            } else {
                "kernel32 unavailable"
            }
        );
        std::process::exit(70);
    }
    #[cfg(windows)]
    {
        let command_line = command_line.map(String::as_str).unwrap_or_default();
        let Some(kernel) = kernel else {
            std::process::exit(70);
        };
        // start_breakaway hands the command an environment without the shim's own role and command line.
        let code = match crate::windows_breakaway::start_breakaway(kernel, command_line, env) {
            Ok(command) => {
                let line = format!("{}\n", serde_json::json!({ "pid": command.pid }));
                if let Err(error) = write_pid(&line) {
                    eprintln!("launch-shim: {error}");
                    std::process::exit(1);
                }
                command.wait()
            }
            Err(error) => Err(error),
        };
        match code {
            // Windows exit codes are unsigned 32-bit; `process.exit` passes them through unchanged.
            Ok(code) => std::process::exit(code as i32),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (write_pid, kernel);
        std::process::exit(70);
    }
}

/// `writeSync(PID_FD, line)`: writes to the C runtime's descriptor 3 (the pipe the parent tool child handed the shim).
/// Rust's std has no handle for a raw CRT descriptor, so this calls the CRT `_write` directly.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn write_pid_fd(line: &str) -> std::io::Result<()> {
    unsafe extern "C" {
        fn _write(fd: i32, buffer: *const std::ffi::c_void, count: u32) -> i32;
    }
    let mut rest = line.as_bytes();
    while !rest.is_empty() {
        let count = u32::try_from(rest.len()).unwrap_or(u32::MAX);
        // SAFETY: `rest` is a live slice of at least `count` bytes and `_write` only reads from it.
        let written = unsafe { _write(PID_FD, rest.as_ptr().cast(), count) };
        if written <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        rest = &rest[written as usize..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_is_reported_on_descriptor_three() {
        assert_eq!(PID_FD, 3);
    }
}
