#[cfg(target_os = "linux")]
#[test]
fn release_driver_builds_pinned_sources_and_preserves_foreign_files() {
    let root = tempfile::tempdir().expect("scratch release repository");
    let result = std::process::Command::new("bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/release-driver.sh"
        ))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/release-all.sh"
        ))
        .arg(root.path())
        .output()
        .expect("run release fixture");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn release_configuration_does_not_expose_private_addresses_or_user_directories() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let address = regex::Regex::new(r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b").expect("IPv4 pattern");
    for path in [
        "scripts/release-all.sh",
        "scripts/release-linux.sh",
        "scripts/release-windows.ps1",
        "scripts/release-macos.sh",
        "docs/release.md",
    ] {
        let text = std::fs::read_to_string(root.join(path)).expect("release file");
        assert!(
            !address
                .find_iter(&text)
                .filter_map(|s| s.as_str().parse::<std::net::Ipv4Addr>().ok())
                .any(|ip| ip.is_private() || matches!(ip.octets(), [100, 64..=127, _, _])),
            "{path} contains a private address"
        );
        assert!(
            !text.contains("/Users/") && !text.contains(r"\Users\"),
            "{path} contains a user-specific home path"
        );
    }
}
