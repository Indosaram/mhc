use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn offline_flags_require_an_explicit_truthy_value() {
    for value in [None, Some(""), Some("0"), Some("false"), Some("no")] {
        assert!(!is_env_truthy(value));
    }
    for value in ["1", "true", "TRUE", "yes", "YeS"] {
        assert!(is_env_truthy(Some(value)));
    }
}

#[test]
fn prerelease_identifiers_compare_numerically_and_ignore_build_metadata() {
    assert_eq!(
        compare_versions("1.0.0-alpha.10", "1.0.0-alpha.2"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.20-dev.10", "2026.10.20-dev.2"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("1.0.0+build.1", "1.0.0+build.2"),
        Ok(Ordering::Equal)
    );
}

#[test]
fn current_version_returns_non_empty_identity() {
    let version = current_version();
    assert!(!version.trim().is_empty());
}

#[test]
fn target_mapping_covers_known_platforms_and_rejects_unknown() {
    assert_eq!(target_from_str("linux-x86_64"), Ok("linux-x86_64"));
    assert_eq!(target_from_str("macos-aarch64"), Ok("macos-aarch64"));
    assert_eq!(target_from_str("windows-x86_64"), Ok("windows-x86_64"));
    assert!(target_from_str("linux-armv7").is_err());
    assert!(target_from_str("unknown-x86_64").is_err());
    assert!(target_from_str("").is_err());
}

#[test]
fn asset_names_map_to_expected_package_archives() {
    assert_eq!(
        asset_name_for_target("linux-x86_64"),
        Ok("mhc-linux-x86_64.tar.gz".to_string())
    );
    assert_eq!(
        asset_name_for_target("macos-aarch64"),
        Ok("mhc-macos-aarch64.tar.gz".to_string())
    );
    assert_eq!(
        asset_name_for_target("windows-x86_64"),
        Ok("mhc-windows-x86_64.zip".to_string())
    );
    assert!(asset_name_for_target("freebsd-x86_64").is_err());
}

#[test]
fn compare_versions_calver_3_part_and_4_part() {
    assert_eq!(
        compare_versions("2026.10.20", "2026.10.19"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.19", "2026.10.20"),
        Ok(Ordering::Less)
    );
    assert_eq!(
        compare_versions("2026.10.20", "2026.10.20"),
        Ok(Ordering::Equal)
    );

    assert_eq!(
        compare_versions("2026.10.19.2", "2026.10.19.1"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.19.1", "2026.10.19.2"),
        Ok(Ordering::Less)
    );
    assert_eq!(
        compare_versions("2026.10.19.2", "2026.10.19.2"),
        Ok(Ordering::Equal)
    );

    assert_eq!(
        compare_versions("2026.10.19.2", "2026.10.19"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.19", "2026.10.19.2"),
        Ok(Ordering::Less)
    );

    assert_eq!(
        compare_versions("2026.10.20", "2026.10.19.99"),
        Ok(Ordering::Greater)
    );
}

#[test]
fn compare_versions_dev_dash_revisions_and_dates() {
    assert_eq!(
        compare_versions("2026.10.20-1", "2026.10.20"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.20-2", "2026.10.20-1"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.20-1", "2026.10.20-2"),
        Ok(Ordering::Less)
    );

    assert_eq!(
        compare_versions("2026.10.20.1", "2026.10.20-1"),
        Ok(Ordering::Equal)
    );
    assert_eq!(
        compare_versions("2026.10.21", "2026.10.20-2"),
        Ok(Ordering::Greater)
    );
}

#[test]
fn compare_versions_optional_v_and_leading_zeros() {
    assert_eq!(
        compare_versions("v2026.10.20", "2026.10.20"),
        Ok(Ordering::Equal)
    );
    assert_eq!(
        compare_versions("V2026.10.20", "v2026.10.20"),
        Ok(Ordering::Equal)
    );
    assert_eq!(
        compare_versions("v2026.10.19.2", "2026.10.19.1"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.04.05", "2026.4.5"),
        Ok(Ordering::Equal)
    );
    assert_eq!(
        compare_versions("2026.10.01", "2026.09.30"),
        Ok(Ordering::Greater)
    );
}

#[test]
fn compare_versions_semver_and_workspace_baseline() {
    assert_eq!(compare_versions("1.2.3", "1.2.2"), Ok(Ordering::Greater));
    assert_eq!(compare_versions("1.2.3", "1.2.3"), Ok(Ordering::Equal));
    assert_eq!(compare_versions("1.2.2", "1.2.3"), Ok(Ordering::Less));

    assert_eq!(
        compare_versions("2026.10.20", "0.0.0"),
        Ok(Ordering::Greater)
    );
    assert_eq!(compare_versions("0.0.0", "2026.10.20"), Ok(Ordering::Less));
}

#[test]
fn compare_versions_prerelease_rules() {
    assert_eq!(
        compare_versions("2026.10.20", "2026.10.20-dev"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.20-dev", "2026.10.20"),
        Ok(Ordering::Less)
    );

    assert_eq!(
        compare_versions("2026.10.20-dev.2", "2026.10.20-dev.1"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("2026.10.20-dev.1", "2026.10.20-dev.2"),
        Ok(Ordering::Less)
    );

    assert_eq!(
        compare_versions("2026.10.20.1", "2026.10.20.1-dev"),
        Ok(Ordering::Greater)
    );
    assert_eq!(
        compare_versions("1.0.0", "1.0.0-alpha.1"),
        Ok(Ordering::Greater)
    );
}

#[test]
fn compare_versions_rejects_invalid_inputs() {
    assert!(compare_versions("invalid", "2026.10.20").is_err());
    assert!(compare_versions("2026.10.20", "invalid").is_err());
    assert!(compare_versions("", "2026.10.20").is_err());
    assert!(compare_versions("v", "2026.10.20").is_err());
    assert!(compare_versions("2026.xx.yy", "2026.10.20").is_err());
    for invalid in [
        "2026.00.10",
        "2026.13.01",
        "2026.02.29",
        "2026.04.31",
        "2026.10.20.18446744073709551616",
    ] {
        assert!(
            compare_versions(invalid, "2026.10.20").is_err(),
            "{invalid}"
        );
    }
    assert_eq!(
        compare_versions("2028.02.29", "2028.02.28"),
        Ok(Ordering::Greater)
    );
}

#[test]
fn is_newer_version_evaluates_correctly() {
    assert_eq!(is_newer_version("2026.10.20", "2026.10.19"), Ok(true));
    assert_eq!(is_newer_version("2026.10.19", "2026.10.20"), Ok(false));
    assert_eq!(is_newer_version("2026.10.20", "2026.10.20"), Ok(false));
}

async fn start_mock_release_server(
    payload: String,
    status: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let url = format!("http://127.0.0.1:{}/releases/latest", addr.port());
    let handle = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, handle)
}

#[tokio::test]
async fn latest_resolves_valid_release_and_assets() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
            },
            {
                "name": "mhc-linux-x86_64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let release = latest_from_endpoint(Some(&url), Some("linux-x86_64"))
        .await
        .expect("successful release lookup");
    server.await.expect("server join");

    assert_eq!(release.version, "2026.10.20.1");
    assert_eq!(release.tag, "v2026.10.20.1");
    assert_eq!(release.asset_name, "mhc-linux-x86_64.tar.gz");
    assert_eq!(
        release.asset_url,
        "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
    );
    assert_eq!(
        release.checksum_url,
        "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
    );
}

#[tokio::test]
async fn latest_rejects_draft_release() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": true,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
            },
            {
                "name": "mhc-linux-x86_64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("draft"));
}

#[tokio::test]
async fn latest_rejects_prerelease() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": false,
        "prerelease": true,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
            },
            {
                "name": "mhc-linux-x86_64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("prerelease"));
}

#[tokio::test]
async fn latest_rejects_missing_target_asset() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-macos-aarch64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-macos-aarch64.tar.gz"
            },
            {
                "name": "mhc-macos-aarch64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-macos-aarch64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("missing asset for target"));
}

#[tokio::test]
async fn latest_rejects_missing_checksum_asset() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
            }
        ]
    })
    .to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("missing checksum asset"));
}

#[tokio::test]
async fn latest_rejects_invalid_tag_name() {
    let release_json = serde_json::json!({
        "tag_name": "not-a-valid-tag",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz"
            },
            {
                "name": "mhc-linux-x86_64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("invalid tag"));
}

#[tokio::test]
async fn latest_rejects_non_loopback_http_endpoint() {
    let result = latest_from_endpoint(
        Some("http://example.com/releases/latest"),
        Some("linux-x86_64"),
    )
    .await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("HTTP is only permitted for loopback")
    );
}

#[tokio::test]
async fn latest_rejects_non_loopback_asset_url_from_loopback_endpoint() {
    let release_json = serde_json::json!({
        "tag_name": "v2026.10.20.1",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "mhc-linux-x86_64.tar.gz",
                "browser_download_url": "http://evil.com/mhc-linux-x86_64.tar.gz"
            },
            {
                "name": "mhc-linux-x86_64.tar.gz.sha256",
                "browser_download_url": "http://127.0.0.1:8999/download/mhc-linux-x86_64.tar.gz.sha256"
            }
        ]
    }).to_string();

    let (url, server) = start_mock_release_server(release_json, "200 OK").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("non-loopback asset URL"));
}

#[tokio::test]
async fn latest_rejects_http_server_error() {
    let (url, server) =
        start_mock_release_server("{}".to_string(), "500 Internal Server Error").await;
    let result = latest_from_endpoint(Some(&url), Some("linux-x86_64")).await;
    server.await.expect("server join");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("HTTP status 500"));
}

#[tokio::test]
async fn available_cached_returns_release_when_cached_version_is_newer() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_file = temp_dir.path().join("cache/mhc-latest-release.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let record = CacheRecord {
        schema_version: 1,
        endpoint: "https://api.github.com/repos/Indosaram/mhc/releases/latest".to_string(),
        target: "linux-x86_64".to_string(),
        checked_at: now,
        success: true,
        release: Some(Release {
            version: "9999.12.31".to_string(),
            tag: "v9999.12.31".to_string(),
            asset_url: "https://github.com/Indosaram/mhc/releases/download/v9999.12.31/mhc-linux-x86_64.tar.gz".to_string(),
            checksum_url: "https://github.com/Indosaram/mhc/releases/download/v9999.12.31/mhc-linux-x86_64.tar.gz.sha256".to_string(),
            asset_name: "mhc-linux-x86_64.tar.gz".to_string(),
        }),
        failure_reason: None,
    };
    write_cache_atomic(&cache_file, &record).expect("write cache");

    let cached = available_cached_with(
        &cache_file,
        Some("https://api.github.com/repos/Indosaram/mhc/releases/latest"),
        Some("linux-x86_64"),
        Some(true),
    )
    .await;

    assert!(cached.is_some());
    assert_eq!(cached.unwrap().version, "9999.12.31");
}

#[tokio::test]
async fn available_cached_returns_none_when_cached_version_is_older_or_equal() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_file = temp_dir.path().join("cache/mhc-latest-release.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let record = CacheRecord {
        schema_version: 1,
        endpoint: "https://api.github.com/repos/Indosaram/mhc/releases/latest".to_string(),
        target: "linux-x86_64".to_string(),
        checked_at: now,
        success: true,
        release: Some(Release {
            version: "0.0.0".to_string(),
            tag: "v0.0.0".to_string(),
            asset_url: "https://github.com/Indosaram/mhc/releases/download/v0.0.0/mhc-linux-x86_64.tar.gz".to_string(),
            checksum_url: "https://github.com/Indosaram/mhc/releases/download/v0.0.0/mhc-linux-x86_64.tar.gz.sha256".to_string(),
            asset_name: "mhc-linux-x86_64.tar.gz".to_string(),
        }),
        failure_reason: None,
    };
    write_cache_atomic(&cache_file, &record).expect("write cache");

    let cached = available_cached_with(
        &cache_file,
        Some("https://api.github.com/repos/Indosaram/mhc/releases/latest"),
        Some("linux-x86_64"),
        Some(true),
    )
    .await;

    assert!(cached.is_none());
}

#[tokio::test]
async fn available_cached_avoids_storms_on_recent_cached_failure() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_file = temp_dir.path().join("cache/mhc-latest-release.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let record = CacheRecord {
        schema_version: 1,
        endpoint: "https://api.github.com/repos/Indosaram/mhc/releases/latest".to_string(),
        target: "linux-x86_64".to_string(),
        checked_at: now,
        success: false,
        release: None,
        failure_reason: Some("HTTP 500".to_string()),
    };
    write_cache_atomic(&cache_file, &record).expect("write cache");

    let cached = available_cached_with(
        &cache_file,
        Some("https://api.github.com/repos/Indosaram/mhc/releases/latest"),
        Some("linux-x86_64"),
        Some(false),
    )
    .await;

    assert!(cached.is_none());
}

#[tokio::test]
async fn available_cached_handles_malformed_cache_without_panic() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_file = temp_dir.path().join("cache/mhc-latest-release.json");
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::write(&cache_file, b"corrupted non-json bytes \x00\xff").unwrap();

    let cached = available_cached_with(
        &cache_file,
        Some("https://api.github.com/repos/Indosaram/mhc/releases/latest"),
        Some("linux-x86_64"),
        Some(true),
    )
    .await;

    assert!(cached.is_none());
}

#[test]
fn cache_replacement_updates_existing_file_without_leaving_temporary_files() {
    let directory = tempfile::tempdir().expect("cache directory");
    let path = directory.path().join("release.json");
    let mut record = CacheRecord {
        schema_version: CACHE_SCHEMA_VERSION,
        endpoint: "http://127.0.0.1/releases/latest".into(),
        target: "linux-x86_64".into(),
        checked_at: 1,
        success: false,
        release: None,
        failure_reason: Some("first failure".into()),
    };
    write_cache_atomic(&path, &record).expect("initial cache");
    record.checked_at = 2;
    record.failure_reason = Some("second failure".into());
    write_cache_atomic(&path, &record).expect("replace cache");
    let stored: CacheRecord =
        serde_json::from_slice(&std::fs::read(&path).expect("cache bytes")).expect("cache record");
    assert_eq!(stored.checked_at, 2);
    assert_eq!(stored.failure_reason.as_deref(), Some("second failure"));
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("cache entries")
            .count(),
        1
    );
}
