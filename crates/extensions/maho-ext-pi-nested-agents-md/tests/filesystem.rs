use maho_ext_pi_nested_agents_md::{containment::resolve_and_contain, find_agents_md_up::find_agents_md_up, inject_directory_context::{InjectionConfig, inject_directory_context}, injection_cache::InjectionCache};
use std::path::Path;

fn tree() -> tempfile::TempDir {
    let tree = tempfile::tempdir().expect("create fixture directory");
    std::fs::create_dir_all(tree.path().join("src/deep")).expect("create nested fixture directory");
    for (path, content) in [("AGENTS.md", "root"), ("src/AGENTS.md", "outer"), ("src/deep/AGENTS.md", "inner"), ("src/deep/file.ts", "x")] { std::fs::write(tree.path().join(path), content).expect("write fixture file"); }
    tree
}

#[test]
fn contained_when_relative_file() {
    let tree = tree();
    let contained = resolve_and_contain(Path::new("src/deep/file.ts"), tree.path()).expect("fixture is contained");
    assert_eq!(contained.0, tree.path().join("src/deep/file.ts").canonicalize().expect("canonicalize fixture"));
}
#[test]
fn contained_when_absolute_file() {
    let tree = tree();
    let file = tree.path().join("src/deep/file.ts");
    let (canonical_file, canonical_root) = resolve_and_contain(&file, tree.path()).expect("absolute fixture contained");
    assert_eq!(canonical_file, file.canonicalize().expect("canonicalize absolute fixture"));
    assert_eq!(canonical_root, tree.path().canonicalize().expect("canonicalize root"));
}
#[test]
fn excluded_when_file_outside_root() {
    let tree = tree();
    let outside = tempfile::tempdir().expect("create outside fixture");
    let file = outside.path().join("file.ts");
    std::fs::write(&file, "outside").expect("write outside file");
    assert!(resolve_and_contain(&file, tree.path()).is_none());
}
#[test]
fn excluded_when_root_itself() {
    let tree = tree();
    assert!(resolve_and_contain(tree.path(), tree.path()).is_none());
}
#[test]
fn excluded_when_missing_file() {
    let tree = tree();
    assert!(resolve_and_contain(Path::new("missing"), tree.path()).is_none());
}
#[test]
fn excluded_when_sibling_prefix() {
    let tree = tree();
    let repo = tree.path().join("repo");
    let outside = tree.path().join("repo-evil");
    std::fs::create_dir(&repo).expect("create root fixture");
    std::fs::create_dir(&outside).expect("create prefix sibling fixture");
    assert!(resolve_and_contain(&outside, &repo).is_none());
}
#[cfg(unix)]
#[test]
fn excluded_when_symlink_escapes_root() {
    let tree = tree();
    let outside = tempfile::tempdir().expect("create outside fixture");
    std::fs::write(outside.path().join("AGENTS.md"), "outside").expect("write outside fixture");
    std::os::unix::fs::symlink(outside.path(), tree.path().join("src/escape")).expect("create escape symlink");
    assert!(resolve_and_contain(&tree.path().join("src/escape/AGENTS.md"), tree.path()).is_none());
    let result = inject_directory_context(Path::new("src/escape/AGENTS.md"), tree.path(), &mut InjectionCache::default(), "a", &InjectionConfig::default());
    assert!(result.injected_files.is_empty());
    assert!(result.injected_text.is_empty());
}
#[test]
fn outermost_when_nested_files() {
    let tree = tree();
    let files = find_agents_md_up(&tree.path().join("src/deep"), tree.path(), &["AGENTS.md"]);
    assert_eq!(files, [tree.path().join("src/AGENTS.md"), tree.path().join("src/deep/AGENTS.md")]);
}
#[test]
fn injected_when_nested_read() {
    let tree = tree();
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut InjectionCache::default(), "a", &InjectionConfig::default());
    assert_eq!(result.injected_files.len(), 2);
    assert!(!result.injected_text.contains("\nroot"));
    assert!(result.errors.is_empty());
}
#[test]
fn deduplicated_when_second_read() {
    let tree = tree();
    let mut cache = InjectionCache::default();
    inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut cache, "a", &InjectionConfig::default());
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut cache, "a", &InjectionConfig::default());
    assert!(result.injected_files.is_empty());
}
#[test]
fn reinjected_when_compacted() {
    let tree = tree();
    let mut cache = InjectionCache::default();
    inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut cache, "a", &InjectionConfig::default());
    cache.clear_session("a");
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut cache, "a", &InjectionConfig::default());
    assert_eq!(result.injected_files.len(), 2);
}
#[test]
fn bounded_when_read_budget_exhausted() {
    let tree = tree();
    let config = InjectionConfig { max_bytes_per_read: 3, ..Default::default() };
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut InjectionCache::default(), "a", &config);
    assert_eq!(result.injected_files.len(), 1);
    assert_eq!(result.injected_files[0].injected_bytes, 3);
    assert!(result.injected_files[0].truncated);
}

#[cfg(unix)]
#[test]
fn unreadable_rules_record_error_without_injection() {
    use std::os::unix::fs::PermissionsExt;
    let tree = tree();
    let rules = tree.path().join("src/AGENTS.md");
    std::fs::set_permissions(&rules, std::fs::Permissions::from_mode(0o000)).expect("make rules unreadable");
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut InjectionCache::default(), "a", &InjectionConfig::default());
    std::fs::set_permissions(&rules, std::fs::Permissions::from_mode(0o644)).expect("restore fixture permissions");
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].0, rules);
    assert_eq!(result.injected_files.len(), 1);
    assert_eq!(result.injected_files[0].absolute_path, tree.path().join("src/deep/AGENTS.md"));
}

#[test]
fn root_rules_alone_are_excluded() {
    let tree = tree();
    std::fs::write(tree.path().join("file.ts"), "x").expect("write root file");
    let result = inject_directory_context(Path::new("file.ts"), tree.path(), &mut InjectionCache::default(), "a", &InjectionConfig::default());
    assert!(result.injected_files.is_empty());
    assert!(result.injected_text.is_empty());
}

#[test]
fn injected_metadata_and_text_follow_outermost_order() {
    let tree = tree();
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut InjectionCache::default(), "a", &InjectionConfig::default());
    assert_eq!(result.injected_files.iter().map(|file| &file.absolute_path).collect::<Vec<_>>(), [&tree.path().join("src/AGENTS.md"), &tree.path().join("src/deep/AGENTS.md")]);
    assert!(result.injected_text.find("outer").expect("outer rules") < result.injected_text.find("inner").expect("inner rules"));
}

#[test]
fn oversized_rules_metadata_tracks_file_budget() {
    let tree = tree();
    std::fs::write(tree.path().join("src/AGENTS.md"), "a".repeat(200_000)).expect("oversized rules");
    let config = InjectionConfig { max_bytes_per_file: 1024, ..Default::default() };
    let result = inject_directory_context(Path::new("src/deep/file.ts"), tree.path(), &mut InjectionCache::default(), "a", &config);
    assert!(result.injected_files[0].truncated);
    assert_eq!(result.injected_files[0].original_bytes, 200_000);
    assert_eq!(result.injected_files[0].injected_bytes, 1024);
}
