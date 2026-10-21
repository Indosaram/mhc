#![allow(dead_code)]

mod native_update_support;

use std::path::{Path, PathBuf};
use std::process::Output;

use native_update_support::*;

fn exe(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn payload(fx: &Fixture, version: &str) -> PathBuf {
    fx.root.join("versions").join(version).join(exe("mhc"))
}

fn checksum_line(sha256: &str) -> String {
    let target = maho_cli::native_update::release::target().expect("supported update target");
    let asset = maho_cli::native_update::release::asset_name_for_target(target)
        .expect("release asset name");
    format!("{sha256}  {asset}\n")
}

fn install_real(fx: &Fixture) -> String {
    let version = real_version();
    let package = build_package(&fx.dir.path().join("pkg-real"), Payload::RealMhc);
    install_versioned(fx, &version, &package, None);
    version
}

fn build_probe(fx: &Fixture, name: &str, version: &str) -> PathBuf {
    build_package(
        &fx.dir.path().join(name),
        Payload::Probe {
            version: version.to_owned(),
            fail_when_installed: false,
        },
    )
}

async fn serve_probe(fx: &Fixture, name: &str, version: &str) -> ReleaseServer {
    let package = build_probe(fx, name, version);
    let dist = fx.dir.path().join(format!("{name}-dist"));
    std::fs::create_dir_all(&dist).expect("archive directory");
    let (archive_path, sha256) = archive(&package, &dist);
    serve_release(version, &archive_path, checksum_line(&sha256)).await
}

async fn run_update(fx: &Fixture, exe_path: &Path, server: &ReleaseServer) -> Output {
    let mut command = mhc(fx, exe_path);
    command
        .arg("update")
        .env("MHC_UPDATE_RELEASE_URL", &server.url);
    run(command).await
}

fn assert_installed(output: &Output, version: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "update to {version} exited {:?}\n{stdout}{stderr}",
        output.status.code(),
    );
    assert!(
        stdout.contains(&format!("Installed mhc {version}")),
        "update to {version} missing install confirmation\n{stdout}{stderr}",
    );
}

fn assert_versions(fx: &Fixture, expected: &[&str]) {
    let mut expected: Vec<String> = expected
        .iter()
        .map(|version| (*version).to_owned())
        .collect();
    let mut installed = versions(fx);
    expected.sort();
    installed.sort();
    assert_eq!(installed, expected);
}

fn assert_pointer_kept(fx: &Fixture) {
    let (current, previous) = pointer(fx);
    let installed = versions(fx);
    assert!(
        installed.iter().any(|version| version == &current),
        "current {current} removed from {installed:?}"
    );
    if let Some(previous) = previous {
        assert!(
            installed.iter().any(|version| version == &previous),
            "previous {previous} removed from {installed:?}"
        );
    }
}

fn canonical(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => panic!("canonicalize {}: {error}", path.display()),
    }
}

fn reported_version(stdout: &str) -> &str {
    stdout.trim().trim_start_matches(['v', 'V'])
}

/// Launcher follows `current.json`. Point it at the real payload so the next update runs mhc.
fn point_at_real(fx: &Fixture, version: &str) {
    let body = serde_json::json!({
        "schemaVersion": 1,
        "current": version,
        "previous": null,
    });
    std::fs::write(fx.root.join("current.json"), format!("{body}\n"))
        .expect("rewrite current.json");
}

#[tokio::test]
async fn running_sessions_survive_update() {
    let fx = fixture();
    let version_a = install_real(&fx);
    let mut first = spawn_session(&fx).await;
    let mut second = spawn_session(&fx).await;

    let server = serve_probe(&fx, "pkg-b", NEW_VERSION).await;
    let output = run_update(&fx, &launcher(&fx), &server).await;
    assert_installed(&output, NEW_VERSION);

    assert!(
        first.alive_roundtrip("still-1").await,
        "first session stopped answering"
    );
    assert!(
        second.alive_roundtrip("still-2").await,
        "second session stopped answering"
    );
    #[cfg(target_os = "linux")]
    {
        let expected = payload(&fx, &version_a);
        for session in [&first, &second] {
            let running = session.executable().expect("linux session executable");
            assert_eq!(canonical(&running), canonical(&expected));
        }
    }
    assert_eq!(pointer(&fx), (NEW_VERSION.to_owned(), Some(version_a)));

    let marker = probe_marker(&fx);
    let mut command = mhc(&fx, &launcher(&fx));
    command.arg("--version").env("MHC_PROBE_MARKER", &marker);
    let version = run(command).await;
    let stdout = String::from_utf8_lossy(&version.stdout);
    let stderr = String::from_utf8_lossy(&version.stderr);
    assert!(
        version.status.success(),
        "launcher --version exited {:?}\n{stdout}{stderr}",
        version.status.code(),
    );
    assert_eq!(reported_version(&stdout), NEW_VERSION, "{stderr}");
    let marked = std::fs::read_to_string(&marker).expect("probe marker");
    assert_eq!(
        canonical(Path::new(marked.trim())),
        canonical(&payload(&fx, NEW_VERSION))
    );

    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn retention_defers_held_version() {
    let fx = fixture();
    let stale = build_probe(&fx, "pkg-stale", "0.0.1");
    install_versioned(&fx, "0.0.1", &stale, None);
    install_versioned(&fx, "0.0.2", &stale, None);
    let version_a = install_real(&fx);
    let lease = maho_cli::native_update::install::SessionLease::acquire(&fx.root, "0.0.1")
        .expect("lease 0.0.1");

    let server = serve_probe(&fx, "pkg-b", NEW_VERSION).await;
    let output = run_update(&fx, &launcher(&fx), &server).await;
    assert_installed(&output, NEW_VERSION);
    assert_eq!(
        pointer(&fx),
        (NEW_VERSION.to_owned(), Some(version_a.clone()))
    );
    assert_pointer_kept(&fx);
    assert_versions(&fx, &[&version_a, NEW_VERSION, "0.0.1"]);
    drop(lease);
    drop(server);

    point_at_real(&fx, &version_a);
    let server = serve_probe(&fx, "pkg-newer", NEWER_VERSION).await;
    let output = run_update(&fx, &launcher(&fx), &server).await;
    assert_installed(&output, NEWER_VERSION);
    assert_eq!(
        pointer(&fx),
        (NEWER_VERSION.to_owned(), Some(version_a.clone()))
    );
    assert_pointer_kept(&fx);
    assert_versions(&fx, &[&version_a, NEWER_VERSION]);
}

#[tokio::test]
async fn settings_and_sentinel_preserved() {
    let fx = fixture();
    install_real(&fx);
    let session = spawn_session(&fx).await;
    let server = serve_probe(&fx, "pkg-next", NEW_VERSION).await;
    let output = run_update(&fx, &launcher(&fx), &server).await;
    assert_installed(&output, NEW_VERSION);
    assert!(state_untouched(&fx), "user settings or sentinel changed");
    session.close().await;
}
