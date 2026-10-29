use std::process::Command;

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/d", "/s", "/c", command]);
        cmd
    } else {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// Run through the platform shell; stderr is folded in as a trailing `[stderr: ...]` marker.
pub fn execute_command(command: &str) -> String {
    match shell_command(command).output() {
        Ok(output) => {
            let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if output.status.success() {
                if err.is_empty() {
                    return out;
                }
                return if out.is_empty() {
                    format!("[stderr: {err}]")
                } else {
                    format!("{out}\n[stderr: {err}]")
                };
            }
            let message = if err.is_empty() {
                format!("Command failed: {command}")
            } else {
                err
            };
            if out.is_empty() {
                format!("[stderr: {message}]")
            } else {
                format!("{out}\n[stderr: {message}]")
            }
        }
        Err(error) => format!("[stderr: {error}]"),
    }
}
