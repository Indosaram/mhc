use crate::errors::ProcessError;
use tokio::io::AsyncReadExt;

pub struct ProcessOutput { pub stdout: String, pub stderr: String, pub exit_code: i32 }
pub async fn collect_process_output_with_timeout(mut child: tokio::process::Child, timeout_ms: u64) -> Result<ProcessOutput, ProcessError> {
    let mut stdout = child.stdout.take().ok_or_else(|| ProcessError::Spawn(std::io::Error::other("stdout missing")))?;
    let mut stderr = child.stderr.take().ok_or_else(|| ProcessError::Spawn(std::io::Error::other("stderr missing")))?;
    let operation = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_, _, status) = tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err), child.wait())?;
        Ok::<_, std::io::Error>(ProcessOutput { stdout: String::from_utf8_lossy(&out).into_owned(), stderr: String::from_utf8_lossy(&err).into_owned(), exit_code: status.code().unwrap_or(0) })
    };
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), operation).await {
        Ok(result) => result.map_err(ProcessError::Spawn),
        Err(_) => {
            #[cfg(unix)]
            if let Some(id) = child.id() { let _signal = nix::sys::signal::kill(nix::unistd::Pid::from_raw(id as i32), nix::sys::signal::Signal::SIGTERM); }
            #[cfg(not(unix))]
            let _signal = child.start_kill();
            tokio::spawn(async move { let _exit = child.wait().await; });
            Err(ProcessError::Timeout(timeout_ms))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn child(script: &str) -> tokio::process::Child {
        tokio::process::Command::new("/bin/sh").args(["-c", script]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("spawn fixture process")
    }
    #[tokio::test] async fn success_output() { let result = collect_process_output_with_timeout(child("printf hello; printf warn >&2"), 5000).await.expect("collect output"); assert_eq!((result.stdout.as_str(), result.stderr.as_str(), result.exit_code), ("hello", "warn", 0)); }
    #[tokio::test] async fn nonzero_output() { let result = collect_process_output_with_timeout(child("printf boom >&2; exit 2"), 5000).await.expect("collect failure"); assert_eq!((result.stderr.as_str(), result.exit_code), ("boom", 2)); }
    #[tokio::test] async fn timeout_error() { assert!(matches!(collect_process_output_with_timeout(child("exec sleep 60"), 20).await, Err(ProcessError::Timeout(20)))); }
    #[tokio::test] async fn empty_output() { let result = collect_process_output_with_timeout(child("exit 0"), 5000).await.expect("collect empty output"); assert!(result.stdout.is_empty() && result.stderr.is_empty()); assert_eq!(result.exit_code, 0); }
}
