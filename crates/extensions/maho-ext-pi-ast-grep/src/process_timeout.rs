use crate::errors::{ProcessError, SearchTimeoutError};
use tokio::io::AsyncReadExt;

pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
pub async fn collect_process_output_with_timeout(
    mut child: tokio::process::Child,
    timeout_ms: u64,
) -> Result<ProcessOutput, ProcessError> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProcessError::Spawn(std::io::Error::other("stdout missing")))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProcessError::Spawn(std::io::Error::other("stderr missing")))?;
    let operation = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_, _, status) = tokio::try_join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        )?;
        Ok::<_, std::io::Error>(ProcessOutput {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code: status.code().unwrap_or(0),
        })
    };
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), operation).await {
        Ok(result) => result.map_err(ProcessError::Spawn),
        Err(_) => {
            // Upstream sends SIGTERM, then after 1000ms sends SIGKILL only when the child is still
            // running and `proc.killed` is false (the SIGTERM could not be delivered).
            #[cfg(unix)]
            let killed = child.id().is_some_and(|id| {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(id as i32),
                    nix::sys::signal::Signal::SIGTERM,
                )
                .is_ok()
            });
            #[cfg(not(unix))]
            let killed = child.start_kill().is_ok();
            tokio::spawn(async move {
                if !killed {
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    if matches!(child.try_wait(), Ok(None)) {
                        let _kill = child.start_kill();
                    }
                }
                let _exit = child.wait().await;
            });
            Err(ProcessError::Timeout(SearchTimeoutError::new(timeout_ms)))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn child(script: &str) -> tokio::process::Child {
        tokio::process::Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn fixture process")
    }
    #[tokio::test]
    async fn success_output() {
        let result =
            collect_process_output_with_timeout(child("printf hello; printf warn >&2"), 5000)
                .await
                .expect("collect output");
        assert_eq!(
            (
                result.stdout.as_str(),
                result.stderr.as_str(),
                result.exit_code
            ),
            ("hello", "warn", 0)
        );
    }
    #[tokio::test]
    async fn nonzero_output() {
        let result = collect_process_output_with_timeout(child("printf boom >&2; exit 2"), 5000)
            .await
            .expect("collect failure");
        assert_eq!((result.stderr.as_str(), result.exit_code), ("boom", 2));
    }
    #[tokio::test]
    async fn timeout_error() {
        assert!(matches!(
            collect_process_output_with_timeout(child("exec sleep 60"), 20).await,
            Err(ProcessError::Timeout(SearchTimeoutError { timeout_ms: 20 }))
        ));
    }
    #[tokio::test]
    async fn timeout_message_matches_upstream() {
        let error = collect_process_output_with_timeout(child("exec sleep 60"), 200)
            .await
            .err()
            .expect("timeout");
        assert!(
            error
                .to_string()
                .to_lowercase()
                .contains("timeout after 200ms")
        );
    }
    #[tokio::test]
    async fn empty_output() {
        let result = collect_process_output_with_timeout(child("exit 0"), 5000)
            .await
            .expect("collect empty output");
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
        assert_eq!(result.exit_code, 0);
    }
}
