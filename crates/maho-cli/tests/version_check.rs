use maho_cli::utils::version_check::*;
#[test]
fn calver_revisions_and_semver_prereleases_sort_correctly() {
    assert!(is_newer_package_version("2026.9.10-2", "2026.9.10"));
    assert!(is_newer_package_version("2026.10.1", "2026.9.30-2"));
    assert!(!is_newer_package_version("1.0.0-beta.2", "1.0.0"));
    assert_eq!(compare_package_versions("invalid", "1.0.0"), None);
}
#[test]
fn registry_document_selects_channel_or_version_and_rejects_empty_values() {
    let payload = serde_json::json!({"version":" 1.2.3 ", "beta":"1.2.4-beta.1", "empty":" "});
    assert_eq!(read_available_version(&payload, None).as_deref(), Some("1.2.3"));
    assert_eq!(read_available_version(&payload, Some("beta")).as_deref(), Some("1.2.4-beta.1"));
    assert_eq!(read_available_version(&payload, Some("empty")), None);
}
#[tokio::test]
async fn native_brand_without_registry_channel_does_not_advertise_engine_updates() {
    assert!(get_latest_release("1.2.3", Default::default()).await.unwrap().is_none());
    assert!(check_for_new_version("1.2.3").await.is_none());
}
#[test]
fn upstream_nested_network_errors_preserve_unique_errno_details() {
    let causes = [VersionCheckCause { code: Some("ETIMEDOUT"), message: Some("connect timeout") }, VersionCheckCause { code: Some("ENETUNREACH"), message: Some("network unreachable") }, VersionCheckCause { code: Some("ETIMEDOUT"), message: None }];
    assert_eq!(format_version_check_error("fetch failed", &causes), "fetch failed (ETIMEDOUT, ENETUNREACH)");
    assert_eq!(format_version_check_error("fetch failed", &[VersionCheckCause { code: None, message: Some("connect timeout") }]), "fetch failed (cause: connect timeout)");
    assert_eq!(format_version_check_error("root", &[]), "root");
}
