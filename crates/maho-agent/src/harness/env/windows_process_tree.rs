//! Native Windows PID termination fallback.

/// Last resort after every taskkill launcher fails: terminate only the requested PID.
/// Like Node's process.kill on Windows, this requests TerminateProcess with exit code 1
/// without waiting for exit or touching descendants. The guard closes the native handle.
#[cfg(windows)]
pub fn terminate_windows_process(pid: u32) -> winsafe::SysResult<()> {
    let process = winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::TERMINATE, false, pid)?;
    process.TerminateProcess(1)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn direct_termination_works_without_owned_child_registration() {
        use std::process::{Command, Stdio};
        use winsafe::{HPROCESS, co};

        // Given: a process blocked on an open stdin pipe, absent from OWNED_CHILDREN.
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/Q", "/C", "set /p value="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn unregistered process");
        // Acquire the exit-event handle before termination; no sleeps or polling.
        let exited = HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, child.id());

        // When: the native fallback receives only the arbitrary PID.
        let terminated = terminate_windows_process(child.id());
        let signaled = exited.and_then(|process| process.WaitForSingleObject(Some(5000)));

        // Then: the process exit event fires and Node's termination code is preserved.
        // Clean up before assertions even when the fallback or bounded wait fails.
        let cleanup = child.kill();
        let status = child.wait().expect("reap unregistered process");
        assert!(terminated.is_ok(), "native termination: {terminated:?}");
        assert_eq!(signaled, Ok(co::WAIT::OBJECT_0));
        assert_eq!(status.code(), Some(1), "cleanup: {cleanup:?}");
    }
}
