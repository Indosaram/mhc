use crate::support::sha256::sha256_hex;
use std::sync::OnceLock;
pub fn device_fingerprint(machine_id: &str, state_dir: &str) -> String {
    sha256_hex(format!("omo-item-device:{machine_id}\0{state_dir}").as_bytes())
}
pub fn read_machine_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| platform_machine_id().unwrap_or_else(|| "no-platform-machine-id".into()))
}
fn platform_machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(text) = std::fs::read_to_string(path)
                && !text.trim().is_empty()
            {
                return Some(text.trim().into());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let text = run(
            std::process::Command::new("ioreg")
                .args(["-rd1", "-c", "IOPlatformExpertDevice"]),
        )?;
        text
            .split("\"IOPlatformUUID\" = \"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .map(str::to_owned)
    }
    #[cfg(target_os = "windows")]
    {
        let text = run(
            std::process::Command::new("reg").args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ]),
        )?;
        text.split("REG_SZ").nth(1).map(|s| s.trim().to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    None
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn run(command: &mut std::process::Command) -> Option<String> {
    use std::process::Stdio;

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (upstream windowsHide).
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = bounded_output(&mut child, std::time::Duration::from_secs(5)).ok()?;
    String::from_utf8(output).ok()
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn bounded_output(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // The guard also covers reader-thread creation and pipe/wait errors.
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            if let Err(error) = self.0.kill() {
                eprintln!("machine-id child cleanup failed: {error}");
            }
            if let Err(error) = self.0.wait() {
                eprintln!("machine-id child reap failed: {error}");
            }
        }
    }

    let child = Cleanup(child);
    let deadline = Instant::now() + timeout;
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::other("machine-id stdout was not piped"))?;
    let (send, receive) = mpsc::sync_channel(1);
    // Drain concurrently so a full stdout pipe cannot prevent child exit.
    std::thread::Builder::new()
        .name("machine-id-stdout".into())
        .spawn(move || {
            let mut output = Vec::new();
            let result = stdout.read_to_end(&mut output).map(|_| output);
            // The receiver is gone when the command timed out or failed.
            if let Err(error) = send.send(result) {
                drop(error);
            }
        })?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(ErrorKind::TimedOut, "machine-id command timed out"));
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(Error::other("machine-id command failed"));
            }
            return receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        Error::new(ErrorKind::TimedOut, "machine-id stdout timed out")
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        Error::other("machine-id stdout reader disconnected")
                    }
                })?;
        }
        // std has no portable timed Child::wait; polling is production-only.
        std::thread::park_timeout(remaining.min(Duration::from_millis(10)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn timeout_kills_and_reaps_a_blocked_child() -> std::io::Result<()> {
        // Given: a child that cannot finish until its still-open stdin closes.
        let test_name = format!(
            "{}::blocked_child",
            module_path!().split_once("::").expect("crate prefix").1
        );
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", &test_name])
            .env("MHC_MACHINE_ID_BLOCKED_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        // When: the deadline is already reached, independent of scheduling.
        let result = bounded_output(&mut child, std::time::Duration::ZERO);

        // Then: timeout is reported and the child has been terminated and reaped.
        assert_eq!(result.expect_err("timeout").kind(), std::io::ErrorKind::TimedOut);
        assert!(!child.try_wait()?.expect("reaped child").success());
        Ok(())
    }

    #[test]
    fn blocked_child() -> std::io::Result<()> {
        if std::env::var_os("MHC_MACHINE_ID_BLOCKED_CHILD").is_some() {
            use std::io::Read;
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte)?;
        }
        Ok(())
    }

    #[cfg(any(unix, windows))]
    fn shell(script: &str) -> Command {
        #[cfg(unix)]
        let mut command = Command::new("/bin/sh");
        #[cfg(windows)]
        let mut command = Command::new("cmd.exe");
        #[cfg(unix)]
        command.args(["-c", script]);
        #[cfg(windows)]
        command.args(["/D", "/C", script]);
        command
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn successful_command_returns_stdout() {
        // Given: a command with known stdout.
        let mut command = shell("echo machine-id-fixture");

        // When: the command completes before the deadline.
        let output = run(&mut command);

        // Then: its machine-consumed output is preserved.
        assert_eq!(output.as_deref().map(str::trim), Some("machine-id-fixture"));
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn failed_command_does_not_return_stdout() {
        // Given: a command that writes output but exits unsuccessfully.
        #[cfg(unix)]
        let mut command = shell("echo machine-id-fixture; exit 7");
        #[cfg(windows)]
        let mut command = shell("echo machine-id-fixture & exit /b 7");

        // When: the command fails.
        let output = run(&mut command);

        // Then: failure follows upstream's missing-machine-id path.
        assert!(output.is_none());
    }
}
