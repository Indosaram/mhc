use maho_cli::utils::changelog::*;
#[test]
fn ignores_headers_inside_fences_and_invalid_dates() {
    let entries = parse_changelog_text("## [1.2.3] - 2026-02-28\nfirst\n```\n## 9.9.9\n```\n## [1.2.2] - 2026-02-30\ninvalid\n## [1.2.1]\nlast");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].version.as_deref(), Some("1.2.3"));
    assert!(entries[0].content.contains("## 9.9.9"));
    assert_eq!(entries[1].content, "last");
}
#[test]
fn calver_revision_order_is_numeric_and_duplicates_are_removed() {
    let entries = parse_changelog_text("## 2026.9.30-10\nten\n## 2026.9.30-2\ntwo\n## 2026.9.30-2\nduplicate\n## 2026.9.30\nbase");
    assert_eq!(compare_versions(&entries[0], &entries[1]), 1);
    let fresh = get_new_entries(&entries, "2026.9.30", Some("2026.9.30-2"));
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].content, "two");
}
#[test]
fn unreleased_resets_section_and_foreign_versions_are_excluded() {
    let entries = parse_changelog_text("## [1.2.0]\nrelease\n## Unreleased\ndraft\n## [1.1.0]\nold");
    assert_eq!(entries.len(), 2);
    assert!(get_new_entries(&entries, "0.0.0-omob.test", None).is_empty());
    assert_eq!(get_new_entries(&entries, "1.1.0", None).len(), 1);
}
#[test]
fn links_pin_versions_and_resolve_repository_paths() {
    let result = normalize_changelog_links("[a](../tui/README.md#intro) [b](https://github.com/badlogic/pi-mono/blob/main/file.md) [c](#local)", "1.2.3");
    assert!(result.contains("/blob/v1.2.3/packages/tui/README.md#intro"));
    assert!(result.contains("https://github.com/earendil-works/pi/blob/v1.2.3/file.md"));
    assert!(result.contains("[c](#local)"));
}
