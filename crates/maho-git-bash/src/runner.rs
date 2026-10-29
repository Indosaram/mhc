//! Running one command through `bash.exe -lc` (spec §5.3).

use crate::host::{RunOutput, RunRequest};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Win32 `CREATE_NO_WINDOW` process creation flag.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn read_lossy(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// Spawn `<bash> -lc <command>` with stdin closed and stdout/stderr redirected to files in a
/// fresh temporary directory under `temp_root` (system temp dir when `None`). The directory is
/// removed before returning. On timeout the child is killed and the output written so far is
/// returned with `timed_out: true`.
pub async fn run_command(
    request: &RunRequest,
    temp_root: Option<&Path>,
) -> Result<RunOutput, String> {
    let dir = {
        let mut builder = tempfile::Builder::new();
        builder.prefix("mhc-git-bash-");
        match temp_root {
            Some(root) => builder.tempdir_in(root),
            None => builder.tempdir(),
        }
    }
    .map_err(|error| format!("could not create a temporary output directory: {error}"))?;
    let stdout_path = dir.path().join("stdout");
    let stderr_path = dir.path().join("stderr");
    let open = |path: &Path| {
        std::fs::File::create(path)
            .map_err(|error| format!("could not create {}: {error}", path.display()))
    };
    let stdout_file = open(&stdout_path)?;
    let stderr_file = open(&stderr_path)?;

    let mut command = tokio::process::Command::new(&request.bash);
    command
        .arg("-lc")
        .arg(&request.command)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true);
    if let Some(workdir) = &request.workdir {
        command.current_dir(workdir);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    // The command keeps its own handles to the output files; release them so the directory can
    // be removed once the child is done.
    drop(command);
    let (status, timed_out) =
        match tokio::time::timeout(Duration::from_millis(request.timeout_ms), child.wait()).await {
            Ok(status) => (status.map_err(|error| error.to_string())?, false),
            Err(_) => {
                kill_tree(&mut child).await;
                (child.wait().await.map_err(|error| error.to_string())?, true)
            }
        };

    let output = RunOutput {
        exit_code: status.code(),
        stdout: read_lossy(&stdout_path),
        stderr: read_lossy(&stderr_path),
        timed_out,
    };
    let dir_path = dir.path().to_path_buf();
    if let Err(error) = dir.close() {
        // The output is already read; report the leftover directory on the server's stderr
        // rather than failing the call.
        eprintln!(
            "mhc-git-bash: could not remove {}: {error}",
            dir_path.display()
        );
    }
    Ok(output)
}

/// Kill the child and, on Windows, its descendants: MSYS bash keeps a stub process while the
/// real program runs as its child, and that child holds the output files open.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let mut taskkill = tokio::process::Command::new("taskkill");
            taskkill
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW);
            if let Ok(status) = taskkill.status().await
                && status.success()
            {
                return;
            }
        }
    }
    // The child may have exited between the timeout and now; a failed kill then is expected.
    if let Err(error) = child.start_kill()
        && error.kind() != std::io::ErrorKind::InvalidInput
    {
        eprintln!("mhc-git-bash: could not kill the timed-out command: {error}");
    }
}
