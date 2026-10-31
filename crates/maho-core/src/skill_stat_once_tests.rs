//! Stat-once skill loading and ignore rules from the walk's own listing (senpi omo#11437).

use std::path::{Path, PathBuf};

use crate::skill_discovery::{IgnoreMatcher, add_ignore_rules_from_entries};
use crate::skills::load_skill_from_file_internal;

/// A file whose last change is well outside the source cache's racy-write window.
fn settled_metadata() -> std::fs::Metadata {
    std::fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("stat")
}

#[test]
fn a_user_skill_is_read_through_the_cache_with_the_callers_stat() {
    // Given: a user skill read once under a settled stat the caller already took.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("cached").join("SKILL.md");
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, "---\nname: cached\ndescription: first\n---\nbody\n").expect("write");
    let file = file.to_string_lossy().into_owned();
    let stats = settled_metadata();
    let (first, _) = load_skill_from_file_internal(&file, "user", None, None, Some(&stats));
    assert_eq!(first.expect("first load").description, "first");
    // When: the bytes change but the caller passes the same stat.
    std::fs::write(&file, "---\nname: cached\ndescription: second\n---\nbody\n").expect("rewrite");
    let (again, _) = load_skill_from_file_internal(&file, "user", None, None, Some(&stats));
    // Then: the loader trusted the caller's stat and reused the cached source.
    assert_eq!(again.expect("second load").description, "first");
    // And without a caller stat the file's own fresh stat misses the cache.
    let (fresh, _) = load_skill_from_file_internal(&file, "user", None, None, None);
    assert_eq!(fresh.expect("fresh load").description, "second");
}

#[test]
fn ignore_rules_come_from_the_listing_the_walk_already_read() {
    // Given: a directory holding an ignore file.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".gitignore"), "ignored.md\n").expect("write");
    let root = dir.path().to_string_lossy().into_owned();
    // When: the listing does not name the ignore file.
    let mut unlisted = IgnoreMatcher::new();
    add_ignore_rules_from_entries(&mut unlisted, &root, &root, &[dir.path().join("kept.md")]);
    // Then: it is not opened.
    assert!(!unlisted.ignores("ignored.md"));

    // When: the listing names it.
    let mut listed = IgnoreMatcher::new();
    let entries: Vec<PathBuf> = vec![dir.path().join(".gitignore"), dir.path().join("kept.md")];
    add_ignore_rules_from_entries(&mut listed, &root, &root, &entries);
    // Then: its rules apply.
    assert!(listed.ignores("ignored.md"));
    assert!(!listed.ignores("kept.md"));
}

#[test]
fn a_listed_ignore_file_that_does_not_open_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    let mut matcher = IgnoreMatcher::new();
    add_ignore_rules_from_entries(&mut matcher, &root, &root, &[dir.path().join(".ignore")]);
    assert!(!matcher.ignores("anything.md"));
}
