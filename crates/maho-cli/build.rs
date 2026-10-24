//! Embeds the release trust root (the minisign public key used by `mhc update`) into the binary.
//!
//! * `MHC_RELEASE_PUBKEY_FILE` selects the key file (default `<repo>/mhc-release.pub`); a relative path is
//!   resolved against the repository root. A missing file embeds `None`.
//! * `MHC_RELEASE_CHANNEL` is `release` | `candidate`; it defaults to `release` when `MHC_BUILD_VERSION` is set
//!   and to `dev` otherwise. Release builds must embed a key that is not the test fixture; candidate builds must
//!   embed the test fixture; a versioned build without any key fails.
//! There is no runtime override of the trust root.

use std::path::{Path, PathBuf};

fn key_line(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .map(str::to_owned)
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/maho-cli lives two levels below the repository root")
        .to_path_buf();
    let fixture_file = manifest_dir.join("tests/fixtures/release-test-key/test.pub");

    println!("cargo:rerun-if-env-changed=MHC_RELEASE_PUBKEY_FILE");
    println!("cargo:rerun-if-env-changed=MHC_RELEASE_CHANNEL");
    println!("cargo:rerun-if-env-changed=MHC_BUILD_VERSION");
    println!("cargo:rerun-if-changed={}", fixture_file.display());

    let key_file = match std::env::var_os("MHC_RELEASE_PUBKEY_FILE").filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() { path } else { repo_root.join(path) }
        }
        None => repo_root.join("mhc-release.pub"),
    };
    println!("cargo:rerun-if-changed={}", key_file.display());

    let versioned = std::env::var("MHC_BUILD_VERSION").is_ok_and(|value| !value.trim().is_empty());
    let channel = match std::env::var("MHC_RELEASE_CHANNEL").ok().filter(|value| !value.is_empty()) {
        Some(value) => value,
        None if versioned => "release".to_owned(),
        None => "dev".to_owned(),
    };
    if !matches!(channel.as_str(), "release" | "candidate" | "dev") {
        panic!("MHC_RELEASE_CHANNEL must be `release` or `candidate`, got `{channel}`");
    }

    let key = key_line(&key_file);
    let fixture = key_line(&fixture_file);
    if versioned && key.is_none() {
        panic!(
            "MHC_BUILD_VERSION is set but no release public key was found at {}; release builds always embed a key",
            key_file.display()
        );
    }
    match channel.as_str() {
        "release" if key.is_some() && key == fixture => {
            panic!("release builds must not embed the test fixture key ({})", fixture_file.display())
        }
        "candidate" if key.is_none() || key != fixture => {
            panic!("candidate builds must embed the test fixture key; set MHC_RELEASE_PUBKEY_FILE to {}", fixture_file.display())
        }
        _ => {}
    }

    let literal = match &key {
        Some(key) => format!("Some({key:?})"),
        None => "None".to_owned(),
    };
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(
        out_dir.join("release_pubkey.rs"),
        format!(
            "pub const RELEASE_PUBKEY: Option<&str> = {literal};\npub const RELEASE_CHANNEL: &str = {channel:?};\n"
        ),
    )
    .expect("write release_pubkey.rs");
}
