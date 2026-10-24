#![allow(dead_code)]

use crate::native_update_support;

use std::path::{Path, PathBuf};
use std::process::Output;

use native_update_support::*;
use sha2::{Digest, Sha256};

fn asset_name() -> String {
    let target = maho_cli::native_update::release::target().expect("supported update target");
    maho_cli::native_update::release::asset_name_for_target(target).expect("release asset name")
}

fn checksum_line(digest: &str) -> String {
    format!("{digest}  {}\n", asset_name())
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_status(output: &Output, success: bool) {
    let code = output.status.code().expect("child exited");
    assert_eq!(code == 0, success, "exit {code}\n{}", output_text(output));
}

fn assert_stdout_contains(output: &Output, needle: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(needle),
        "stdout missing {needle:?}\n{}",
        output_text(output)
    );
}

fn install_real(fx: &Fixture) -> PathBuf {
    let package = build_package(&fx.dir.path().join("pkg-real"), Payload::RealMhc);
    install_versioned(fx, &real_version(), &package, None);
    package
}

fn archive_of(fx: &Fixture, package: &Path) -> (PathBuf, String) {
    let out = fx.dir.path().join("archives");
    std::fs::create_dir_all(&out).expect("archive directory");
    archive(package, &out)
}

fn probe_package(fx: &Fixture, fail_when_installed: bool) -> PathBuf {
    let label = if fail_when_installed { "pkg-fail" } else { "pkg-probe" };
    build_package(
        &fx.dir.path().join(label),
        Payload::Probe { version: NEW_VERSION.to_owned(), fail_when_installed },
    )
}

fn update_to(fx: &Fixture, url: &str) -> tokio::process::Command {
    let mut command = mhc(fx, &launcher(fx));
    command.arg("update").env("MHC_UPDATE_RELEASE_URL", url);
    command
}

async fn printed_version(fx: &Fixture) -> String {
    let mut command = mhc(fx, &launcher(fx));
    command.arg("--version");
    let output = run(command).await;
    assert_status(&output, true);
    String::from_utf8_lossy(&output.stdout).trim().trim_start_matches(['v', 'V']).to_owned()
}

fn launcher_bytes(fx: &Fixture) -> Vec<u8> {
    std::fs::read(launcher(fx)).expect("read launcher")
}

fn staging_entries(fx: &Fixture) -> Vec<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&fx.root).expect("read install root") {
        let name = entry.expect("install root entry").file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".staging-") {
            names.push(name.into_owned());
        }
    }
    names
}

#[tokio::test]
async fn success_update_a_to_b() {
    let fx = fixture();
    install_real(&fx);
    let package = probe_package(&fx, false);
    let (archive_path, digest) = archive_of(&fx, &package);
    let server = serve_release(NEW_VERSION, &archive_path, checksum_line(&digest)).await;
    let before = launcher_bytes(&fx);
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, true);
    assert_stdout_contains(&output, &format!("Installed mhc {NEW_VERSION}"));
    assert_eq!(pointer(&fx), (NEW_VERSION.to_owned(), Some(real_version())));
    assert_eq!(launcher_bytes(&fx), before);
    assert_eq!(printed_version(&fx).await, NEW_VERSION);
    assert!(state_untouched(&fx), "user state changed");
}

#[tokio::test]
async fn latest_is_noop() {
    let fx = fixture();
    let package = install_real(&fx);
    let (archive_path, digest) = archive_of(&fx, &package);
    let version = real_version();
    let before_versions = versions(&fx);
    let before_pointer = pointer(&fx);
    let server = serve_release(&version, &archive_path, checksum_line(&digest)).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, true);
    assert_stdout_contains(&output, "is up to date");
    assert_eq!(versions(&fx), before_versions, "update created a version directory");
    assert_eq!(pointer(&fx), before_pointer);
}

#[tokio::test]
async fn bad_checksum_refused() {
    let fx = fixture();
    install_real(&fx);
    let package = probe_package(&fx, false);
    let (archive_path, _) = archive_of(&fx, &package);
    let before_pointer = pointer(&fx);
    let before_versions = versions(&fx);
    let server = serve_release(NEW_VERSION, &archive_path, checksum_line(&"deadbeef".repeat(8))).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, false);
    assert_eq!(pointer(&fx), before_pointer);
    assert_eq!(versions(&fx), before_versions);
}

#[tokio::test]
async fn invalid_archive_refused() {
    let fx = fixture();
    install_real(&fx);
    let garbage = fx.dir.path().join("garbage.bin");
    let bytes = b"this is not an mhc archive";
    std::fs::write(&garbage, bytes).expect("write garbage archive");
    let digest = format!("{:x}", Sha256::digest(bytes));
    let before_pointer = pointer(&fx);
    let before_versions = versions(&fx);
    let server = serve_release(NEW_VERSION, &garbage, checksum_line(&digest)).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, false);
    assert_eq!(pointer(&fx), before_pointer);
    assert_eq!(versions(&fx), before_versions);
    let staging = staging_entries(&fx);
    assert!(staging.is_empty(), "staging left behind: {staging:?}");
}

#[tokio::test]
async fn rollback_after_installed_validation_failure() {
    let fx = fixture();
    install_real(&fx);
    let previous = pointer(&fx).1;
    let package = probe_package(&fx, true);
    let (archive_path, digest) = archive_of(&fx, &package);
    let server = serve_release(NEW_VERSION, &archive_path, checksum_line(&digest)).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, false);
    let reported = output_text(&output);
    assert!(reported.contains("previous version restored"), "{reported}");
    assert_eq!(pointer(&fx), (real_version(), previous));
    assert_eq!(printed_version(&fx).await, real_version());
}

fn flip_one_byte(path: &Path) {
    let mut bytes = std::fs::read(path).expect("read archive");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(path, bytes).expect("write tampered archive");
}

#[tokio::test]
async fn tampered_archive_fails_signature_and_installs_nothing() {
    let fx = fixture();
    install_real(&fx);
    let package = probe_package(&fx, false);
    let (archive_path, _) = archive_of(&fx, &package);
    // The checksum matches the tampered bytes, so only the signature can catch the change.
    let signed = std::fs::read(&archive_path).expect("read archive");
    flip_one_byte(&archive_path);
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&archive_path).expect("read tampered")));
    let before_pointer = pointer(&fx);
    let before_versions = versions(&fx);
    let server = serve_release_signed(NEW_VERSION, &archive_path, checksum_line(&digest), Signing::ValidFor(signed)).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, false);
    assert!(output_text(&output).contains("signature verification failed"), "{}", output_text(&output));
    assert_eq!(pointer(&fx), before_pointer);
    assert_eq!(versions(&fx), before_versions);
    assert!(staging_entries(&fx).is_empty());
}

#[tokio::test]
async fn signature_from_a_different_key_is_refused() {
    let fx = fixture();
    install_real(&fx);
    let package = probe_package(&fx, false);
    let (archive_path, digest) = archive_of(&fx, &package);
    let before_pointer = pointer(&fx);
    let server = serve_release_signed(NEW_VERSION, &archive_path, checksum_line(&digest), Signing::OtherKey).await;
    let output = run(update_to(&fx, &server.url)).await;
    assert_status(&output, false);
    assert!(output_text(&output).contains("signature verification failed"), "{}", output_text(&output));
    assert_eq!(pointer(&fx), before_pointer);
}

#[tokio::test]
async fn missing_signature_is_refused_for_signed_era_versions() {
    let fx = fixture();
    install_real(&fx);
    let package = probe_package(&fx, false);
    let (archive_path, digest) = archive_of(&fx, &package);
    let before_pointer = pointer(&fx);
    let server = serve_release_signed(NEW_VERSION, &archive_path, checksum_line(&digest), Signing::Missing).await;
    for allow in [false, true] {
        let mut command = update_to(&fx, &server.url);
        if allow {
            command.arg("--allow-unsigned");
        }
        let output = run(command).await;
        assert_status(&output, false);
        assert!(output_text(&output).contains("no signature"), "{}", output_text(&output));
        assert_eq!(pointer(&fx), before_pointer);
    }
}

#[tokio::test]
async fn verbose_version_reports_channel_and_embedded_key() {
    let fx = fixture();
    install_real(&fx);
    let mut command = mhc(&fx, &launcher(&fx));
    command.args(["--version", "--verbose"]);
    let output = run(command).await;
    assert_status(&output, true);
    let text = output_text(&output);
    assert!(text.contains("channel: "), "{text}");
    assert!(text.contains("release-key: "), "{text}");
}
