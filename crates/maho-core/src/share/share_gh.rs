//! Port of senpi packages/coding-agent/src/core/share/share-gh.ts.

use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::share_session::{GhStatus, ShareUploadResult};

/// `gh auth status`: missing when `gh` cannot be run, logged out when it exits non-zero.
pub fn check_gh_auth() -> GhStatus {
    match std::process::Command::new("gh")
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Err(_) => GhStatus::Missing,
        Ok(status) if status.success() => GhStatus::Ok,
        Ok(_) => GhStatus::LoggedOut,
    }
}

/// Creates a secret gist from `html` piped through stdin, so the export never lands on disk. Resolving or
/// dropping `cancel` aborts the upload and kills `gh`.
pub async fn upload_secret_gist(
    html: &str,
    file_name: &str,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> ShareUploadResult {
    let spawned = tokio::process::Command::new("gh")
        .args(["gist", "create", "--filename", file_name, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return ShareUploadResult {
                code: None,
                stdout: String::new(),
                stderr: error.to_string(),
                aborted: false,
            };
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let payload = html.to_owned();
    let work = async move {
        let write = async move {
            if let Some(mut stdin) = stdin {
                // A closed pipe surfaces through gh's exit status; the write error itself carries nothing more.
                let _ = stdin.write_all(payload.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
        };
        let read_out = async move {
            let mut text = String::new();
            if let Some(mut out) = stdout {
                let mut bytes = Vec::new();
                let _ = out.read_to_end(&mut bytes).await;
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            text
        };
        let read_err = async move {
            let mut text = String::new();
            if let Some(mut err) = stderr {
                let mut bytes = Vec::new();
                let _ = err.read_to_end(&mut bytes).await;
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            text
        };
        let ((), out, err) = tokio::join!(write, read_out, read_err);
        let status = child.wait().await;
        (status, out, err)
    };
    tokio::select! {
        (status, stdout, stderr) = work => match status {
            Ok(status) => ShareUploadResult { code: status.code(), stdout, stderr, aborted: false },
            Err(error) => ShareUploadResult { code: None, stdout, stderr: error.to_string(), aborted: false },
        },
        _ = &mut cancel => ShareUploadResult { code: None, stdout: String::new(), stderr: String::new(), aborted: true },
    }
}
