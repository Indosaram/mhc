use std::{collections::BTreeSet, fs};
use maho_ext_rules::rules::{finder::{FinderOptions, RuleDiscoveryCache, find_rule_candidates}, scanner::scan_rule_files};

#[test]
fn scanner_excludes_build_directories_and_limits_depth() {
    // Given
    let root = tempfile::tempdir().unwrap();
    for name in ["nested", "dist"] { fs::create_dir(root.path().join(name)).unwrap(); }
    for name in ["a.md", "nested/b.mdc", "dist/c.md", "ignored.txt"] { fs::write(root.path().join(name), "").unwrap(); }
    // When
    let files = scan_rule_files(root.path(), None, Some(0));
    // Then
    assert_eq!(files.iter().map(|file| file.path.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["a.md"]);
    assert_eq!(scan_rule_files(root.path(), None, None).len(), 2);
}

#[test]
fn scanner_breaks_symlink_cycles_and_preserves_aliases() {
    // Given
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.md"), "").unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("cycle")).unwrap();
    std::os::unix::fs::symlink(root.path().join("a.md"), root.path().join("alias.md")).unwrap();
    // When
    let files = scan_rule_files(root.path(), None, None);
    // Then
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].real_path, fs::canonicalize(root.path().join("a.md")).unwrap());
}

#[test]
fn discovery_walks_target_ancestors_and_keeps_both_home_single_files() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("AGENTS.md"), "").unwrap();
    fs::write(root.path().join("nested/AGENTS.md"), "").unwrap();
    fs::create_dir_all(home.path().join(".config/opencode")).unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    fs::write(home.path().join(".config/opencode/AGENTS.md"), "").unwrap();
    fs::write(home.path().join(".claude/CLAUDE.md"), "").unwrap();
    let disabled = BTreeSet::new();
    let target = root.path().join("nested/file.rs");
    // When
    let files = find_rule_candidates(FinderOptions { project_root: Some(root.path()), target_file: Some(&target), home_dir: home.path(), disabled_sources: &disabled, skip_user_home: false }, &mut RuleDiscoveryCache::default());
    // Then
    assert_eq!(files.len(), 4);
    assert_eq!(files.iter().map(|file| file.distance).collect::<Vec<_>>(), [0, 1, 9999, 9999]);
}

#[test]
fn discovery_omits_disabled_sources_and_caches_missing_files() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let disabled = BTreeSet::from(["CLAUDE.md".into()]);
    let mut cache = RuleDiscoveryCache::default();
    let find = |cache: &mut RuleDiscoveryCache| find_rule_candidates(FinderOptions { project_root: Some(root.path()), target_file: None, home_dir: root.path(), disabled_sources: &disabled, skip_user_home: true }, cache);
    assert!(find(&mut cache).is_empty());
    fs::write(root.path().join("AGENTS.md"), "").unwrap();
    fs::write(root.path().join("CLAUDE.md"), "").unwrap();
    // When
    let files = find(&mut cache);
    // Then
    assert!(files.is_empty());
    assert_eq!(find(&mut RuleDiscoveryCache::default()).len(), 1);
}
