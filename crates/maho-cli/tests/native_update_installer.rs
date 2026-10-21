#![allow(dead_code)]

mod native_update_support;
use native_update_support::*;

use std::path::{Path, PathBuf};
use std::process::Output;

fn repo_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(name)
}

fn stage_env(fx: &Fixture, command: &mut tokio::process::Command) {
    command
        .env("HOME", &fx.home)
        .env("MAHO_CODING_AGENT_DIR", &fx.agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .env_remove("MHC_UPDATE_RELEASE_URL")
        .current_dir(&fx.home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if cfg!(windows) {
        command.env("USERPROFILE", &fx.home);
    }
}

/// `sh install.sh` on unix, `powershell.exe -File install.ps1` on windows.
/// `MHC_PACKAGE_DIR` is an extracted package; nothing is downloaded and PATH is left alone.
fn installer(fx: &Fixture, package: &Path) -> tokio::process::Command {
    let mut command = if cfg!(windows) {
        let mut command = tokio::process::Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]);
        command.arg(repo_script("install.ps1"));
        command
    } else {
        let mut command = tokio::process::Command::new("sh");
        command.arg(repo_script("install.sh"));
        command
    };
    command
        .env("MHC_PACKAGE_DIR", package)
        .env("MHC_INSTALL_DIR", &fx.root)
        .env("MHC_BIN_DIR", fx.dir.path().join("bin"))
        .env("MHC_NO_PATH_UPDATE", "1");
    stage_env(fx, &mut command);
    command
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dest)?;
        } else {
            std::fs::copy(&src, &dest)?;
        }
    }
    Ok(())
}

fn built(work: &Path, name: &str, payload: Payload) -> PathBuf {
    let parent = work.join(name);
    std::fs::create_dir_all(&parent).expect("package parent directory");
    let package = build_package(&parent, payload);
    let payload_exe = package.join(format!("mhc{}", std::env::consts::EXE_SUFFIX));
    assert!(payload_exe.is_file(), "missing {}", payload_exe.display());
    assert!(package.join("manifest.json").is_file(), "missing manifest in {}", package.display());
    package
}

fn probe(version: &str) -> Payload {
    Payload::Probe { version: version.to_owned(), fail_when_installed: false }
}

fn expect_ok(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn reported_version(stdout: &[u8]) -> String {
    let text = String::from_utf8(stdout.to_vec()).expect("version stdout is utf-8");
    let trimmed = text.trim();
    trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed).to_owned()
}

async fn launcher_version(fx: &Fixture) -> String {
    let mut command = mhc(fx, &launcher(fx));
    command.arg("--version");
    let output = run(command).await;
    expect_ok(&output, "launcher --version");
    reported_version(&output.stdout)
}

/// Flat real package at `R` (no `current.json`), then the installer migrates it under the probe.
async fn install_flat_then_probe(fx: &Fixture, work: &Path) {
    copy_tree(&built(work, "real", Payload::RealMhc), &fx.root).expect("copy flat package");
    assert!(!fx.root.join("current.json").exists(), "flat layout has no current.json");
    let output = run(installer(fx, &built(work, "new", probe(NEW_VERSION)))).await;
    expect_ok(&output, "installer over flat layout");
    let real = real_version();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("Installed mhc {NEW_VERSION}")),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    assert_eq!(pointer(fx), (NEW_VERSION.to_owned(), Some(real.clone())));
    assert!(versions(fx).contains(&real), "versions/{real} missing, have {:?}", versions(fx));
}

#[tokio::test]
async fn installer_bootstrap() {
    let fx = fixture();
    let work = tempfile::tempdir().expect("package workspace");
    let package = built(work.path(), "real", Payload::RealMhc);
    let output = run(installer(&fx, &package)).await;
    expect_ok(&output, "installer bootstrap");
    let version = real_version();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("Installed mhc {version}")),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    assert_eq!(pointer(&fx), (version.clone(), None));
    assert_eq!(launcher_version(&fx).await, version);
}

#[tokio::test]
async fn flat_migration() {
    let fx = fixture();
    let work = tempfile::tempdir().expect("package workspace");
    install_flat_then_probe(&fx, work.path()).await;
}

#[tokio::test]
async fn update_after_migration() {
    let fx = fixture();
    let work = tempfile::tempdir().expect("package workspace");
    install_flat_then_probe(&fx, work.path()).await;

    let package = built(work.path(), "newer", probe(NEWER_VERSION));
    let dist = work.path().join("dist");
    std::fs::create_dir_all(&dist).expect("archive directory");
    let (archive_path, sha) = archive(&package, &dist);
    let asset = archive_path.file_name().and_then(|name| name.to_str()).expect("archive file name");
    let server = serve_release(NEWER_VERSION, &archive_path, format!("{sha}  {asset}\n")).await;

    // `current` is the probe after migration, so drive the update from the migrated real payload.
    // Its own session lease keeps versions/<real> during this run; retention is covered elsewhere.
    let real = real_version();
    let migrated = fx.root.join("versions").join(&real).join(format!("mhc{}", std::env::consts::EXE_SUFFIX));
    let mut command = mhc(&fx, &migrated);
    command.arg("update").env("MHC_UPDATE_RELEASE_URL", &server.url).env_remove("MAHO_BRAND");
    let output = run(command).await;
    expect_ok(&output, "migrated mhc update");
    let stdout = String::from_utf8(output.stdout).expect("update stdout is utf-8");
    assert!(
        stdout.contains(&format!("Installed mhc {NEWER_VERSION}")),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(pointer(&fx), (NEWER_VERSION.to_owned(), Some(NEW_VERSION.to_owned())));
    assert_eq!(launcher_version(&fx).await, NEWER_VERSION);
}
