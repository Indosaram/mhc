use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{archive, download, layout::Layout, lease, manifest, release::Release};
#[cfg(not(windows))]
use super::transaction;

pub use super::lease::SessionLease;

pub enum InstallOutcome {
    Installed,
}

pub fn helper_entry(args: &[String]) -> Option<Result<(), String>> {
    if args.first().map(String::as_str) != Some("--native-update-helper") {
        return None;
    }
    Some(run_helper(args))
}

#[cfg(windows)]
fn run_helper(args: &[String]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("update helper requires exactly one transaction file".into());
    }
    let job = Path::new(&args[1]);
    if !job.is_absolute() {
        return Err("update helper requires an absolute transaction path".into());
    }
    tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|error| error.to_string())?
        .block_on(super::windows_helper::run(job))
}

#[cfg(not(windows))]
fn run_helper(_args: &[String]) -> Result<(), String> {
    Err("the Windows update helper cannot run on this platform".into())
}

pub fn session_lease() -> Result<Option<SessionLease>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = std::fs::canonicalize(executable).map_err(|error| error.to_string())?;
    let root = executable.parent().ok_or("executable has no parent")?;
    if !root.join("manifest.json").is_file()
        && !lease::lock_path(root).map_err(|error| error.to_string())?.exists()
    {
        return Ok(None);
    }
    SessionLease::acquire(root).map(Some).map_err(|error| error.to_string())
}

pub(super) async fn validate_binary(root: &Path, expected: &str) -> Result<(), String> {
    let executable = root.join(format!("mhc{}", std::env::consts::EXE_SUFFIX));
    let mut command = tokio::process::Command::new(executable);
    command.arg("--version").current_dir(root).kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|error| format!("cannot run candidate: {error}"))?;
    let stdout = child.stdout.take().ok_or("candidate stdout unavailable")?;
    use tokio::io::AsyncReadExt;
    let mut bounded_stdout = stdout.take(4097);
    let mut bytes = Vec::new();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        bounded_stdout.read_to_end(&mut bytes).await.map_err(|error| error.to_string())?;
        if bytes.len() > 4096 { return Err("candidate version output exceeds limit".to_owned()); }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if !status.success() { return Err("candidate version check failed".to_owned()); }
        Ok(())
    }).await;
    let result = match outcome {
        Ok(result) => result,
        Err(_) => Err("candidate version check timed out".to_owned()),
    };
    if let Err(error) = result {
        child.kill().await.map_err(|cleanup| format!("{error}; cannot reap candidate: {cleanup}"))?;
        return Err(error);
    }
    let version = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let expected_clean = expected.trim().strip_prefix(['v', 'V']).unwrap_or(expected.trim());
    let got_clean = version.trim().strip_prefix(['v', 'V']).unwrap_or(version.trim());
    if got_clean != expected_clean { return Err(format!("candidate version mismatch: expected {expected}, got {}", version.trim())); }
    Ok(())
}

pub async fn stage(release: &Release, installation: &Path) -> Result<(tempfile::TempDir, PathBuf), String> {
    let parent = installation.parent().ok_or("installation has no parent")?;
    let temporary = tempfile::Builder::new().prefix(".mhc-update-").tempdir_in(parent)
        .map_err(|error| format!("cannot create staging directory: {error}"))?;
    let archive_path = temporary.path().join(&release.asset_name);
    download::verified_archive(release, &archive_path).await?;
    let extraction = temporary.path().join("extracted");
    let zipped = release.asset_name.ends_with(".zip");
    let candidate = tokio::task::spawn_blocking(move || {
        let candidate = archive::extract(&archive_path, &extraction, zipped)?;
        manifest::validate(&candidate)?;
        Ok::<_, String>(candidate)
    }).await.map_err(|error| error.to_string())??;
    validate_binary(&candidate, &release.version).await?;
    Ok((temporary, candidate))
}

#[cfg(not(windows))]
pub async fn install(release: &Release) -> Result<InstallOutcome, String> {
    let layout = Layout::current()?;
    let _exclusive = lease::UpdateLease::acquire(&layout.installation).map_err(|error| error.to_string())?;
    super::processes::ensure_unused(&layout.installation).await?;
    manifest::validate(&layout.installation)?;
    let (temporary, candidate) = stage(release, &layout.installation).await?;
    let backup_root = tempfile::Builder::new().prefix(".mhc-backup-")
        .tempdir_in(layout.installation.parent().ok_or("installation has no parent")?)
        .map_err(|error| error.to_string())?;
    let backup = backup_root.path().join("package");
    super::processes::ensure_unused(&layout.installation).await?;
    let runtime = tokio::runtime::Handle::current();
    let installation = layout.installation.clone();
    let backup_path = backup.clone();
    let expected_version = release.version.clone();
    let joined = tokio::task::spawn_blocking(move || {
        transaction::replace(&installation, &candidate, &backup_path, |installed| {
            manifest::validate(installed)?;
            runtime.block_on(validate_binary(installed, &expected_version))
        })
    }).await;
    let result = match joined {
        Ok(result) => result,
        Err(error) => Err(format!("replacement task failed: {error}")),
    };
    if backup.exists() {
        let retained = backup_root.keep();
        return Err(format!("{}; recovery directory retained at {}", result.err().unwrap_or_else(|| "backup requires recovery".into()), retained.display()));
    }
    result?;
    temporary.close().map_err(|error| format!("staging cleanup failed: {error}"))?;
    backup_root.close().map_err(|error| format!("backup cleanup failed: {error}"))?;
    Ok(InstallOutcome::Installed)
}

#[cfg(windows)]
pub async fn install(release: &Release) -> Result<InstallOutcome, String> {
    let layout = Layout::current()?;
    let exclusive = lease::UpdateLease::acquire(&layout.installation).map_err(|error| error.to_string())?;
    super::processes::ensure_unused(&layout.installation).await?;
    manifest::validate(&layout.installation)?;
    let (temporary, candidate) = stage(release, &layout.installation).await?;
    let workspace = std::fs::canonicalize(temporary.path()).map_err(|error| error.to_string())?;
    let helper = workspace.join("mhc-update-helper.exe");
    std::fs::copy(&layout.executable, &helper).map_err(|error| format!("cannot prepare update helper: {error}"))?;
    let job = super::windows_helper::Job {
        installation: std::fs::canonicalize(&layout.installation).map_err(|error| error.to_string())?,
        candidate: std::fs::canonicalize(candidate).map_err(|error| error.to_string())?,
        backup: workspace.join("backup"),
        version: release.version.clone(),
        parent_pid: std::process::id(),
    };
    let job_path = workspace.join("job.json");
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .open(&job_path).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, &job).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    let log = std::fs::File::create(workspace.join("helper.log")).map_err(|error| error.to_string())?;
    let mut command = tokio::process::Command::new(&helper);
    command.arg("--native-update-helper").arg(&job_path)
        .current_dir(&workspace)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(log);
    std::env::set_current_dir(workspace.parent().ok_or("workspace has no parent")?)
        .map_err(|error| error.to_string())?;
    drop(exclusive);
    let mut child = command.spawn().map_err(|error| format!("cannot start update helper: {error}"))?;
    let retained = temporary.keep();
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let stdout = child.stdout.take().ok_or("helper stdout unavailable")?;
    let mut reader = tokio::io::BufReader::new(stdout.take(65537));
    let mut record = String::new();
    tokio::time::timeout(Duration::from_secs(120), reader.read_line(&mut record)).await
        .map_err(|_| format!("update result timed out; inspect {}", retained.display()))?
        .map_err(|error| error.to_string())?;
    if record.len() > 65536 || !record.ends_with('\n') {
        return Err(format!("helper returned no complete result; inspect {}", retained.display()));
    }
    let receipt: super::windows_helper::Receipt = serde_json::from_str(&record)
        .map_err(|error| format!("invalid helper result: {error}"))?;
    if receipt.version != release.version { return Err("helper version mismatch".into()); }
    if !receipt.committed {
        return Err(receipt.error.unwrap_or_else(|| "update failed without an error record".into()));
    }
    Ok(InstallOutcome::Installed)
}
