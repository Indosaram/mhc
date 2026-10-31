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
// senpi version-check.test.ts and brand-update-channel.test.ts pure cases.
#[test]
fn upstream_compares_package_versions() {
    assert!(compare_package_versions("0.70.6", "0.70.5").unwrap() > 0);
    assert_eq!(compare_package_versions("0.70.5", "0.70.5"), Some(0));
    assert!(compare_package_versions("0.70.4", "0.70.5").unwrap() < 0);
    assert!(compare_package_versions("5.0.0-beta.20", "5.0.0-beta.9").unwrap() > 0);
    assert!(!is_newer_package_version("0.70.5", "0.70.5"));
    assert!(is_newer_package_version("0.70.6", "0.70.5"));
}
#[test]
fn upstream_orders_same_day_calver_revisions_after_the_bare_release() {
    assert!(compare_package_versions("2026.7.30-2", "2026.7.30").unwrap() > 0);
    assert!(compare_package_versions("2026.7.30", "2026.7.30-2").unwrap() < 0);
    assert!(compare_package_versions("2026.7.31", "2026.7.30-99").unwrap() > 0);
    assert!(!is_newer_package_version("2026.7.30", "2026.7.30-2"));
    assert!(is_newer_package_version("2026.7.30-2", "2026.7.30"));
}
#[test]
fn upstream_never_advertises_an_update_for_branded_build_labels() {
    let label = "omo@c6e7dd7 2026-09-04 10:17 +09:00";
    assert!(!is_newer_package_version("2026.9.3-3", label));
    assert!(!is_newer_package_version(label, "2026.9.3-3"));
}
#[test]
fn upstream_release_changelog_urls_follow_the_channel_or_the_engine() {
    assert_eq!(get_release_changelog_url("2026.6.28-4", None), "https://github.com/code-yeongyu/senpi/blob/v2026.6.28-4/packages/coding-agent/CHANGELOG.md");
    assert_eq!(get_release_changelog_url("2026.8.10", None), "https://github.com/code-yeongyu/senpi/blob/v2026.8.10/packages/coding-agent/CHANGELOG.md");
    assert_eq!(
        get_release_changelog_url("5.0.0-0.beta.4", Some("https://github.com/code-yeongyu/oh-my-openagent/releases/tag/v{version}")),
        "https://github.com/code-yeongyu/oh-my-openagent/releases/tag/v5.0.0-0.beta.4"
    );
}
#[test]
fn upstream_reads_the_branded_dist_tag_or_the_engine_version() {
    let dist_tags = serde_json::json!({"latest": "0.0.0-beta.0", "beta": "5.0.0-0.beta.4"});
    assert_eq!(read_available_version(&dist_tags, Some("beta")).as_deref(), Some("5.0.0-0.beta.4"));
    assert_eq!(read_available_version(&dist_tags, Some("missing")), None);
    assert_eq!(read_available_version(&serde_json::json!({"version": "2026.8.10"}), None).as_deref(), Some("2026.8.10"));
    assert_eq!(read_available_version(&serde_json::json!({}), None), None);
}
