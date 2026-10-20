use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{layout, lease, manifest, processes, transaction};

#[derive(Serialize, Deserialize)]
pub(super) struct Job {
    pub installation: PathBuf,
    pub candidate: PathBuf,
    pub backup: PathBuf,
    pub version: String,
    pub parent_pid: u32,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Receipt {
    pub version: String,
    pub committed: bool,
    pub error: Option<String>,
}

fn read_job(path: &Path) -> Result<Job, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err("invalid update job file".into());
    }
    let job: Job = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("invalid update job: {error}"))?;
    let workspace = std::fs::canonicalize(path.parent().ok_or("job has no parent")?)
        .map_err(|error| error.to_string())?;
    let installation =
        std::fs::canonicalize(&job.installation).map_err(|error| error.to_string())?;
    let candidate = std::fs::canonicalize(&job.candidate).map_err(|error| error.to_string())?;
    if installation != job.installation
        || candidate != job.candidate
        || workspace.parent() != installation.parent()
        || !candidate.starts_with(&workspace)
        || job.backup != workspace.join("backup")
        || job.backup.exists()
        || job.parent_pid == 0
        || job.parent_pid == std::process::id()
    {
        return Err("update job paths or parent identity are invalid".into());
    }
    layout::reject_state_overlap(
        &installation,
        Path::new(&maho_core::config::get_agent_dir()),
    )?;
    super::release::compare_versions(&job.version, &job.version)?;
    Ok(job)
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    use std::io::Write;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("receipt has no parent")?)
            .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut temporary, receipt).map_err(|error| error.to_string())?;
    temporary.flush().map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

async fn wait_for_parent(pid: u32) -> Result<(), String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; try {{ $p=[Diagnostics.Process]::GetProcessById({pid}) }} catch [System.ArgumentException] {{ exit 0 }}; if ($null -ne $p -and -not $p.WaitForExit(60000)) {{ throw 'Updater parent did not exit' }}"
    );
    let mut command = tokio::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    match tokio::time::timeout(std::time::Duration::from_secs(65), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err("updater parent wait failed; backup retained for recovery".into()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => {
            child
                .kill()
                .await
                .map_err(|error| format!("cannot reap parent waiter: {error}"))?;
            Err("parent exit wait timed out; backup retained for recovery".into())
        }
    }
}

async fn commit(job: &Job) -> Result<(), String> {
    let _exclusive =
        lease::UpdateLease::acquire(&job.installation).map_err(|error| error.to_string())?;
    processes::ensure_unused_except(&job.installation, job.parent_pid).await?;
    manifest::validate(&job.installation)?;
    manifest::validate(&job.candidate)?;
    super::install::validate_binary(&job.candidate, &job.version).await?;
    let installation = job.installation.clone();
    let candidate = job.candidate.clone();
    let backup = job.backup.clone();
    let version = job.version.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        transaction::replace_entries_retaining_backup(
            &installation,
            &candidate,
            &backup,
            |installed| {
                manifest::validate(installed)?;
                runtime.block_on(super::install::validate_binary(installed, &version))
            },
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(super) async fn run(path: &Path) -> Result<(), String> {
    let job = read_job(path)?;
    let workspace = std::fs::canonicalize(path.parent().ok_or("job has no parent")?)
        .map_err(|error| error.to_string())?;
    let running =
        std::fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if running.parent() != Some(workspace.as_path()) {
        return Err("update helper must run from its transaction workspace".into());
    }
    std::env::set_current_dir(&workspace).map_err(|error| error.to_string())?;
    let outcome = commit(&job).await;
    let receipt = Receipt {
        version: job.version,
        committed: outcome.is_ok(),
        error: outcome.as_ref().err().cloned(),
    };
    write_receipt(&workspace.join("receipt.json"), &receipt)?;
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &receipt).map_err(|error| error.to_string())?;
        writeln!(stdout).map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
    }
    cleanup_backup_after_parent(
        &job.backup,
        outcome.is_ok(),
        wait_for_parent(job.parent_pid),
    )
    .await?;
    if !job.backup.exists() {
        let script = format!(
            "$ErrorActionPreference='Stop'; try {{ $p=[Diagnostics.Process]::GetProcessById({}) }} catch [System.ArgumentException] {{ $p=$null }}; if ($null -ne $p -and -not $p.WaitForExit(60000)) {{ throw 'Helper did not exit' }}; for ($i = 0; $i -lt 10; $i++) {{ try {{ Remove-Item -LiteralPath $env:MHC_UPDATE_CLEANUP -Recurse -Force -ErrorAction Stop; exit 0 }} catch {{ if ($i -eq 9) {{ throw }}; Start-Sleep -Milliseconds 200 }} }}",
            std::process::id()
        );
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .env("MHC_UPDATE_CLEANUP", &workspace)
            .current_dir(workspace.parent().ok_or("workspace has no parent")?)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot launch helper cleanup: {error}"))?;
    }
    outcome
}

async fn cleanup_backup_after_parent(
    backup: &Path,
    committed: bool,
    parent_exit: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    parent_exit.await?;
    if committed {
        let mut removed = false;
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            match std::fs::remove_dir_all(backup) {
                Ok(()) => {
                    removed = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    removed = true;
                    break;
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        if !removed {
            std::fs::remove_dir_all(backup)
                .map_err(|error| format!("backup cleanup failed: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn backup_cleanup_waits_for_parent_and_preserves_recovery_on_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        for (committed, parent_ok) in [(true, true), (true, false), (false, true)] {
            let root = tempfile::tempdir()?;
            let backup = root.path().join("backup");
            std::fs::create_dir(&backup)?;
            std::fs::write(backup.join("mhc.exe"), b"old")?;
            let (subscribed, subscription) = tokio::sync::oneshot::channel();
            let (exited, exit) = tokio::sync::oneshot::channel();
            let cleanup_path = backup.clone();
            let cleanup = tokio::spawn(async move {
                cleanup_backup_after_parent(&cleanup_path, committed, async {
                    subscribed
                        .send(())
                        .map_err(|_| "subscription closed".to_owned())?;
                    exit.await.map_err(|_| "exit signal closed".to_owned())?
                })
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), subscription).await??;
            assert_eq!(std::fs::read(backup.join("mhc.exe"))?, b"old");
            exited
                .send(if parent_ok {
                    Ok(())
                } else {
                    Err("wait failed".into())
                })
                .map_err(|_| "cleanup exited before parent")?;
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), cleanup).await??;
            assert_eq!(result.is_ok(), parent_ok);
            assert_eq!(backup.exists(), !(committed && parent_ok));
        }
        Ok(())
    }

    #[test]
    fn receipts_replace_atomically_and_preserve_failure_details()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("receipt.json");
        write_receipt(
            &path,
            &Receipt {
                version: "2026.10.20.2".into(),
                committed: false,
                error: Some("replacement failed".into()),
            },
        )?;
        let failed: Receipt = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert!(!failed.committed);
        assert_eq!(failed.error.as_deref(), Some("replacement failed"));
        write_receipt(
            &path,
            &Receipt {
                version: "2026.10.20.2".into(),
                committed: true,
                error: None,
            },
        )?;
        let success: Receipt = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert!(success.committed);
        assert!(success.error.is_none());
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn rejects_job_that_places_backup_outside_workspace() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("installed");
        let workspace = root.path().join("transaction");
        std::fs::create_dir(&installation)?;
        std::fs::create_dir_all(workspace.join("candidate"))?;
        let installation = std::fs::canonicalize(installation)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let job = Job {
            installation,
            candidate: std::fs::canonicalize(workspace.join("candidate"))?,
            backup: root.path().join("unrelated-user-data"),
            version: "2026.10.20.2".into(),
            parent_pid: if std::process::id() == 1 { 2 } else { 1 },
        };
        let path = workspace.join("job.json");
        std::fs::write(&path, serde_json::to_vec(&job)?)?;
        assert!(read_job(&path).is_err());
        assert!(job.installation.is_dir());
        assert!(job.candidate.is_dir());
        Ok(())
    }
}
