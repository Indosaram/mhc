use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use utils::codegraph::*;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn fixture_archive(root_name: &str, executable_name: &str, content: &str) -> (Vec<u8>, String) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let bundle = root.join(root_name);
    let bin_dir = bundle.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let exec_file = bin_dir.join(executable_name);
    fs::write(&exec_file, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exec_file, fs::Permissions::from_mode(0o755));
    }
    let archive_path = root.join("archive.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive_path)
        .arg(root_name)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = fs::read(&archive_path).unwrap();
    let sha256 = sha256_hex(&bytes);
    (bytes, sha256)
}

#[test]
fn provision_uses_default_manifest_when_no_override() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();

    let downloader = |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        Ok(b"not the real archive".to_vec())
    };

    let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: None,
        platform_key: Some("darwin-arm64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    assert_eq!(result.provisioned, false);
    assert!(result.error.as_ref().unwrap().contains("checksum mismatch"));
}

#[test]
fn provision_extracts_verified_archive_and_installs_binary() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();
    let (archive_bytes, archive_sha256) = fixture_archive(
        "codegraph-darwin-arm64",
        "codegraph",
        "#!/bin/sh\nprintf 'codegraph fixture\\n'\n",
    );

    let mut assets = BTreeMap::new();
    assets.insert(
        "darwin-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: archive_sha256,
            url: "memory://codegraph-darwin-arm64.tar.gz".to_string(),
        },
    );

    let downloader = move |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        Ok(archive_bytes.clone())
    };

    let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets,
            version: "1.5.0".to_string(),
        }),
        platform_key: Some("darwin-arm64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    let expected_bin = install_tmp.path().join("bin").join("codegraph");
    assert_eq!(
        result,
        CodegraphProvisionResult {
            bin_path: Some(expected_bin.clone()),
            error: None,
            provisioned: true,
        }
    );
    assert!(
        fs::read_to_string(&expected_bin)
            .unwrap()
            .contains("codegraph fixture")
    );
}

#[test]
fn provision_extracts_npm_tgz_and_installs_cmd() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();
    let (archive_bytes, archive_sha256) =
        fixture_archive("package", "codegraph.cmd", "codegraph fixture\n");

    let mut assets = BTreeMap::new();
    assets.insert(
        "win32-x64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph.cmd".to_string(),
            sha256: archive_sha256,
            url: "memory://codegraph-win32-x64-1.5.0.tgz".to_string(),
        },
    );

    let downloader = move |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        Ok(archive_bytes.clone())
    };

    let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets,
            version: "1.5.0".to_string(),
        }),
        platform_key: Some("win32-x64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    let expected_bin = install_tmp.path().join("bin").join("codegraph.cmd");
    assert_eq!(
        result,
        CodegraphProvisionResult {
            bin_path: Some(expected_bin.clone()),
            error: None,
            provisioned: true,
        }
    );
    assert!(
        fs::read_to_string(&expected_bin)
            .unwrap()
            .contains("codegraph fixture")
    );
}

#[test]
fn provision_marker_makes_calls_idempotent() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();
    let lock_tmp2 = tempdir().unwrap();
    let (archive_bytes, archive_sha256) =
        fixture_archive("codegraph-darwin-arm64", "codegraph", "fixture\n");

    let mut assets = BTreeMap::new();
    assets.insert(
        "darwin-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: archive_sha256,
            url: "memory://codegraph-darwin-arm64.tar.gz".to_string(),
        },
    );

    let downloads = Arc::new(AtomicUsize::new(0));
    let downloads_clone = downloads.clone();
    let downloader = move |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        downloads_clone.fetch_add(1, Ordering::SeqCst);
        Ok(archive_bytes.clone())
    };

    let first = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets,
            version: "1.5.0".to_string(),
        }),
        platform_key: Some("darwin-arm64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    let second = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: None,
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp2.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets: BTreeMap::new(),
            version: "1.5.0".to_string(),
        }),
        platform_key: Some("darwin-arm64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    assert_eq!(first.provisioned, true);
    assert_eq!(second.provisioned, true);
    assert_eq!(downloads.load(Ordering::SeqCst), 1);
}

#[test]
fn provision_serializes_concurrent_calls_with_a_per_host_lock() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();
    let (archive_bytes, archive_sha256) =
        fixture_archive("codegraph-darwin-arm64", "codegraph", "fixture\n");
    let manifest = || {
        let mut assets = BTreeMap::new();
        assets.insert(
            "darwin-arm64".to_string(),
            CodegraphProvisionAsset {
                executable_name: "codegraph".to_string(),
                sha256: archive_sha256.clone(),
                url: "memory://codegraph-darwin-arm64.tar.gz".to_string(),
            },
        );
        CodegraphProvisionManifest {
            assets,
            version: "1.5.0".to_string(),
        }
    };
    let downloads = AtomicUsize::new(0);
    let (download_started_tx, download_started_rx) = std::sync::mpsc::channel::<()>();
    let (release_download_tx, release_download_rx) = std::sync::mpsc::channel::<()>();

    let (first, second) = std::thread::scope(|scope| {
        let downloads = &downloads;
        let archive_bytes = &archive_bytes;
        let manifest = &manifest;
        let install_dir = install_tmp.path().to_path_buf();
        let lock_dir = lock_tmp.path().to_path_buf();
        let (first_install_dir, first_lock_dir) = (install_dir.clone(), lock_dir.clone());
        let first = scope.spawn(move || {
            // Holds the lock mid-download until the second caller is seen contending.
            let downloader = |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
                downloads.fetch_add(1, Ordering::SeqCst);
                let _ = download_started_tx.send(());
                let _ = release_download_rx.recv();
                Ok(archive_bytes.clone())
            };
            ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
                downloader: Some(&downloader),
                download_timeout_ms: None,
                force_bad_checksum: None,
                install_dir: Some(first_install_dir),
                lock_dir: first_lock_dir,
                lock_stale_ms: None,
                lock_wait_ms: None,
                manifest: Some(manifest()),
                platform_key: Some("darwin-arm64".to_string()),
                version: "1.5.0".to_string(),
                sleep_fn: None,
                now_ms_fn: None,
            })
        });
        download_started_rx.recv().unwrap();
        let second = scope.spawn(move || {
            let downloader = |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
                downloads.fetch_add(1, Ordering::SeqCst);
                Ok(archive_bytes.clone())
            };
            // The lock poll is the contention signal: release the first download then.
            let sleep = |ms: u64| {
                let _ = release_download_tx.send(());
                std::thread::sleep(std::time::Duration::from_millis(ms));
            };
            ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
                downloader: Some(&downloader),
                download_timeout_ms: None,
                force_bad_checksum: None,
                install_dir: Some(install_dir),
                lock_dir,
                lock_stale_ms: None,
                lock_wait_ms: None,
                manifest: Some(manifest()),
                platform_key: Some("darwin-arm64".to_string()),
                version: "1.5.0".to_string(),
                sleep_fn: Some(&sleep),
                now_ms_fn: None,
            })
        });
        (first.join().unwrap(), second.join().unwrap())
    });

    assert_eq!((first.provisioned, second.provisioned), (true, true));
    assert_eq!(downloads.load(Ordering::SeqCst), 1);
}

#[test]
fn provision_fails_gracefully_on_checksum_mismatch() {
    let install_tmp = tempdir().unwrap();
    let lock_tmp = tempdir().unwrap();

    let downloader =
        |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> { Ok(b"wrong".to_vec()) };

    let mut assets = BTreeMap::new();
    assets.insert(
        "darwin-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "0000".to_string(),
            url: "memory://codegraph".to_string(),
        },
    );

    let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_tmp.path().to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets,
            version: "1.5.0".to_string(),
        }),
        platform_key: Some("darwin-arm64".to_string()),
        version: "1.5.0".to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    assert_eq!(result.provisioned, false);
    assert!(result.error.unwrap().contains("checksum"));
    assert!(!install_tmp.path().join("bin").join("codegraph").exists());
}

#[test]
fn provision_replaces_stale_runtime_version() {
    // (marker file version, version recorded inside it); the last case is a pin-named
    // marker whose recorded version does not match the pin.
    for (marker_version, stale_version) in [
        ("1.0.1", "1.0.1"),
        ("1.4.1", "1.4.1"),
        (CODEGRAPH_PINNED_VERSION, "1.0.1"),
    ] {
        let install_tmp = tempdir().unwrap();
        let install_dir = install_tmp.path();
        let lock_tmp = tempdir().unwrap();

        let stale_bin = install_dir.join("bin").join("codegraph");
        let stale_marker = install_dir
            .join(".provisioned")
            .join(format!("codegraph-{marker_version}.json"));
        fs::create_dir_all(install_dir.join("bin")).unwrap();
        fs::create_dir_all(install_dir.join(".provisioned")).unwrap();
        fs::write(&stale_bin, "stale codegraph\n").unwrap();
        fs::write(
            &stale_marker,
            format!(
                "{}\n",
                serde_json::json!({ "binPath": stale_bin.to_string_lossy(), "version": stale_version })
            ),
        )
        .unwrap();

        let (archive_bytes, archive_sha256) = fixture_archive(
            "codegraph-darwin-arm64",
            "codegraph",
            "upgraded codegraph\n",
        );

        let mut assets = BTreeMap::new();
        assets.insert(
            "darwin-arm64".to_string(),
            CodegraphProvisionAsset {
                executable_name: "codegraph".to_string(),
                sha256: archive_sha256,
                url: "memory://codegraph-darwin-arm64.tar.gz".to_string(),
            },
        );

        let downloader = move |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
            Ok(archive_bytes.clone())
        };

        let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
            downloader: Some(&downloader),
            download_timeout_ms: None,
            force_bad_checksum: None,
            install_dir: Some(install_dir.to_path_buf()),
            lock_dir: lock_tmp.path().to_path_buf(),
            lock_stale_ms: None,
            lock_wait_ms: None,
            manifest: Some(CodegraphProvisionManifest {
                assets,
                version: CODEGRAPH_PINNED_VERSION.to_string(),
            }),
            platform_key: Some("darwin-arm64".to_string()),
            version: CODEGRAPH_PINNED_VERSION.to_string(),
            sleep_fn: None,
            now_ms_fn: None,
        });

        assert_eq!(result.provisioned, true);
        assert_eq!(result.bin_path, Some(stale_bin.clone()));
        assert_eq!(
            fs::read_to_string(&stale_bin).unwrap(),
            "upgraded codegraph\n"
        );
        let current_marker = install_dir
            .join(".provisioned")
            .join(format!("codegraph-{CODEGRAPH_PINNED_VERSION}.json"));
        let marker: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&current_marker).unwrap()).unwrap();
        assert_eq!(
            marker,
            serde_json::json!({ "binPath": stale_bin.to_string_lossy(), "version": CODEGRAPH_PINNED_VERSION })
        );
    }
}

#[test]
fn provision_retains_previous_runtime_when_upgrade_archive_corrupt() {
    let install_tmp = tempdir().unwrap();
    let install_dir = install_tmp.path();
    let lock_tmp = tempdir().unwrap();

    let stale_bin = install_dir.join("bin").join("codegraph");
    let stale_marker = install_dir
        .join(".provisioned")
        .join("codegraph-1.4.1.json");
    fs::create_dir_all(install_dir.join("bin")).unwrap();
    fs::create_dir_all(install_dir.join(".provisioned")).unwrap();
    fs::write(&stale_bin, "stale codegraph\n").unwrap();
    fs::write(
        &stale_marker,
        format!(
            "{}\n",
            serde_json::json!({ "binPath": stale_bin.to_string_lossy(), "version": "1.4.1" })
        ),
    )
    .unwrap();

    let downloader = |_asset: &CodegraphProvisionAsset| -> Result<Vec<u8>, String> {
        Ok(b"invalid archive".to_vec())
    };

    let mut assets = BTreeMap::new();
    assets.insert(
        "darwin-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "0000".to_string(),
            url: "memory://codegraph-darwin-arm64.tar.gz".to_string(),
        },
    );

    let result = ensure_codegraph_provisioned(&EnsureCodegraphProvisionedOptions {
        downloader: Some(&downloader),
        download_timeout_ms: None,
        force_bad_checksum: None,
        install_dir: Some(install_dir.to_path_buf()),
        lock_dir: lock_tmp.path().to_path_buf(),
        lock_stale_ms: None,
        lock_wait_ms: None,
        manifest: Some(CodegraphProvisionManifest {
            assets,
            version: CODEGRAPH_PINNED_VERSION.to_string(),
        }),
        platform_key: Some("darwin-arm64".to_string()),
        version: CODEGRAPH_PINNED_VERSION.to_string(),
        sleep_fn: None,
        now_ms_fn: None,
    });

    assert_eq!(result.provisioned, false);
    assert!(result.error.unwrap().contains("checksum mismatch"));
    assert_eq!(fs::read_to_string(&stale_bin).unwrap(), "stale codegraph\n");
    let current_marker = install_dir
        .join(".provisioned")
        .join(format!("codegraph-{CODEGRAPH_PINNED_VERSION}.json"));
    assert!(!current_marker.exists());
}
