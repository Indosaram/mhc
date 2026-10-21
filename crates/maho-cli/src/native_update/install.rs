use std::path::{Path, PathBuf};
use std::time::Duration;

use super::pointer::{self, Pointer};
use super::{archive, download, layout::Layout, lease, manifest, release::Release, retention};

pub use super::lease::SessionLease;

pub struct Installed {
    pub version: String,
    pub warnings: Vec<String>,
}

/// Shared lock on this payload's own `R/versions/<v>.lock`, held for the process lifetime. `None` when
/// this binary is not part of a versioned install (flat legacy install, dev build). Never refuses
/// because other sessions or an updater are running.
pub fn session_lease() -> Result<Option<SessionLease>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = std::fs::canonicalize(executable).map_err(|error| error.to_string())?;
    match super::layout::discover(&executable) {
        Ok(layout) => SessionLease::acquire(&layout.root, &layout.version)
            .map(Some)
            .map_err(|error| format!("cannot lock mhc {}: {error}", layout.version)),
        Err(_) => Ok(None),
    }
}

pub(super) async fn check_version(executable: &Path, expected: &str) -> Result<(), String> {
    let mut command = tokio::process::Command::new(executable);
    command.arg("--version").kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(parent) = executable.parent() {
        command.current_dir(parent);
    }
    let mut child = command.spawn().map_err(|error| format!("cannot run {}: {error}", executable.display()))?;
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
        if child.try_wait().map_err(|cleanup| cleanup.to_string())?.is_none() {
            child.kill().await.map_err(|cleanup| format!("{error}; cannot reap candidate: {cleanup}"))?;
        }
        return Err(error);
    }
    let version = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let expected_clean = expected.trim().strip_prefix(['v', 'V']).unwrap_or(expected.trim());
    let got_clean = version.trim().strip_prefix(['v', 'V']).unwrap_or(version.trim());
    if got_clean != expected_clean { return Err(format!("candidate version mismatch: expected {expected}, got {}", version.trim())); }
    Ok(())
}

fn exe(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Downloads, verifies and extracts the release into `workspace`, then validates the staged package.
pub async fn stage(release: &Release, workspace: &Path) -> Result<PathBuf, String> {
    let archive_path = workspace.join(&release.asset_name);
    download::verified_archive(release, &archive_path).await?;
    let extraction = workspace.join("extracted");
    let zipped = release.asset_name.ends_with(".zip");
    let candidate = tokio::task::spawn_blocking(move || {
        let candidate = archive::extract(&archive_path, &extraction, zipped)?;
        manifest::validate(&candidate)?;
        Ok::<_, String>(candidate)
    }).await.map_err(|error| error.to_string())??;
    check_version(&exe(&candidate, "mhc"), &release.version).await?;
    Ok(candidate)
}

/// Places the validated package at `R/versions/<version>`. An existing directory is reused only if its
/// manifest matches the candidate. Published version directories are never replaced.
fn place_package(root: &Path, version: &str, candidate: &Path) -> Result<(), String> {
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).map_err(|error| format!("cannot create {}: {error}", versions.display()))?;
    let target = versions.join(version);
    if target.exists() {
        manifest::validate(&target)?;
        if std::fs::read(target.join("manifest.json")).map_err(|error| error.to_string())?
            != std::fs::read(candidate.join("manifest.json")).map_err(|error| error.to_string())?
        {
            return Err(format!("version {version} is already installed with different content"));
        }
        return Ok(());
    }
    std::fs::rename(candidate, &target)
        .map_err(|error| format!("cannot install package into {}: {error}", target.display()))
}

/// Atomically points new launches at `version`, then runs `verify`; on failure the previous pointer is
/// restored.
async fn switch<F, Fut>(root: &Path, old: &Pointer, version: &str, verify: F) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let previous = if old.current == version { old.previous.clone() } else { Some(old.current.clone()) };
    if let Err(error) = pointer::publish(root, &Pointer::new(version.to_owned(), previous)) {
        return match pointer::publish(root, old) {
            Ok(()) => Err(format!("{error}; previous version restored")),
            Err(restore) => Err(format!("{error}; cannot restore previous version pointer: {restore}")),
        };
    }
    let Err(error) = verify().await else { return Ok(()) };
    match pointer::publish(root, old) {
        Ok(()) => Err(format!("{error}; previous version restored")),
        Err(restore) => Err(format!("{error}; cannot restore previous version pointer: {restore}")),
    }
}

pub async fn install(release: &Release) -> Result<Installed, String> {
    let layout = Layout::current()?;
    let root = layout.root;
    let _update = lease::UpdateLease::acquire(&root).map_err(|error| error.to_string())?;
    let version = pointer::version_dir(&release.version)?;
    let old = pointer::read(&root)?;
    let staging = tempfile::Builder::new().prefix(".staging-").tempdir_in(&root)
        .map_err(|error| format!("cannot create staging directory: {error}"))?;
    let candidate = stage(release, staging.path()).await?;
    place_package(&root, &version, &candidate)?;
    let launcher = exe(&root, "mhc");
    switch(&root, &old, &version, || async {
        let installed = root.join("versions").join(&version);
        manifest::validate(&installed)?;
        check_version(&exe(&installed, "mhc"), &version).await?;
        check_version(&launcher, &version).await
    }).await?;
    staging.close().map_err(|error| format!("staging cleanup failed: {error}"))?;
    let warnings = retention::run(&root, &[version.as_str(), old.current.as_str()]);
    Ok(Installed { version, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn installed_root(versions: &[&str], current: &str) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in versions {
            std::fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        pointer::publish(root.path(), &Pointer::new(current.into(), None))?;
        Ok(root)
    }

    #[tokio::test]
    async fn failed_post_switch_validation_restores_previous_pointer() -> TestResult {
        let root = installed_root(&["1.0.0", "1.1.0"], "1.0.0")?;
        let old = pointer::read(root.path())?;
        let error = switch(root.path(), &old, "1.1.0", || async {
            assert_eq!(pointer::read(root.path())?.current, "1.1.0");
            Err("launcher printed the wrong version".to_owned())
        })
        .await
        .err()
        .ok_or("switch must fail")?;
        assert!(error.contains("previous version restored"), "{error}");
        assert_eq!(pointer::read(root.path())?, old);
        Ok(())
    }

    #[tokio::test]
    async fn successful_switch_records_previous_version() -> TestResult {
        let root = installed_root(&["1.0.0", "1.1.0"], "1.0.0")?;
        let old = pointer::read(root.path())?;
        switch(root.path(), &old, "1.1.0", || async { Ok(()) }).await?;
        assert_eq!(pointer::read(root.path())?, Pointer::new("1.1.0".into(), Some("1.0.0".into())));
        Ok(())
    }

    #[tokio::test]
    async fn update_completes_while_sessions_hold_shared_locks() -> TestResult {
        let root = installed_root(&["1.0.0", "1.1.0", "0.9.0"], "1.0.0")?;
        let first = SessionLease::acquire(root.path(), "1.0.0")?;
        let second = SessionLease::acquire(root.path(), "1.0.0")?;
        let _update = lease::UpdateLease::acquire(root.path())?;
        let old = pointer::read(root.path())?;
        switch(root.path(), &old, "1.1.0", || async { Ok(()) }).await?;
        assert!(retention::run(root.path(), &["1.1.0", "1.0.0"]).is_empty());
        assert_eq!(pointer::read(root.path())?.current, "1.1.0");
        assert!(root.path().join("versions/1.0.0").is_dir(), "running sessions keep their package");
        assert!(root.path().join("versions/0.9.0").is_dir());
        drop((first, second));
        Ok(())
    }
}
