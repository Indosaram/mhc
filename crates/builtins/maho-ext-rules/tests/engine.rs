use maho_ext_rules::{config::config_from_environment, rules::engine::Engine};
use std::fs;

#[test]
fn injected_discovery_and_content_drive_static_loading() {
    use maho_ext_rules::rules::{engine::EngineDeps, finder::{FinderOptions, RuleDiscoveryCache}, types::RuleCandidate};
    struct Fixture;
    impl EngineDeps for Fixture {
        fn find_project_root(&self, _: &std::path::Path) -> Option<std::path::PathBuf> { Some("/fixture".into()) }
        fn find_candidates(&self, _: FinderOptions<'_>, _: &mut RuleDiscoveryCache) -> Vec<RuleCandidate> {
            vec![RuleCandidate { path: "/fixture/AGENTS.md".into(), real_path: "/fixture/AGENTS.md".into(), source: "AGENTS.md".into(), distance: 0, is_global: false, is_single_file: true, relative_path: "AGENTS.md".into() }]
        }
        fn read_file(&self, _: &std::path::Path) -> Option<String> { Some("---\nalwaysApply: true\n---\nfixture".into()) }
    }
    // Given
    let mut engine = Engine::with_deps(config_from_environment(|_| None), "/home".into(), Box::new(Fixture));
    // When
    let loaded = engine.load_static_rules(std::path::Path::new("/fixture"));
    // Then
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(loaded.rules[0].candidate.source, "AGENTS.md");
    assert!(loaded.diagnostics.is_empty());
}

#[test]
fn static_root_single_file_uses_first_source() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    fs::write(root.path().join("AGENTS.md"), "first").unwrap();
    fs::write(root.path().join("CLAUDE.md"), "second").unwrap();
    let mut engine = Engine::new(config_from_environment(|_| None), home.path().into());
    let loaded = engine.load_static_rules(root.path());
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(loaded.rules[0].candidate.source, "AGENTS.md");
}
#[test]
fn dynamic_targets_deduplicate_shared_rule() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    fs::create_dir_all(root.path().join(".omo/rules")).unwrap();
    fs::write(root.path().join(".omo/rules/r.md"), "---\nglobs: '**/*.rs'\n---\nfixture").unwrap();
    for name in ["a.rs", "b.rs"] { fs::write(root.path().join(name), "").unwrap(); }
    let mut engine = Engine::new(config_from_environment(|_| None), home.path().into());
    let loaded = engine.load_dynamic_rules(root.path(), &[root.path().join("a.rs"), root.path().join("b.rs")]).unwrap();
    assert_eq!(loaded.rules.len(), 1);
}
#[test]
fn project_rule_symlink_outside_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    fs::write(home.path().join("outside.md"), "fixture").unwrap();
    std::os::unix::fs::symlink(home.path().join("outside.md"), root.path().join("AGENTS.md")).unwrap();
    let mut engine = Engine::new(config_from_environment(|_| None), home.path().into());
    let loaded = engine.load_static_rules(root.path());
    assert!(loaded.rules.is_empty());
    assert_eq!(loaded.diagnostics.len(), 1);
}
#[test]
fn fingerprints_invalidate_on_rule_addition_and_reset() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    let target = root.path().join("a.rs");
    fs::write(&target, "").unwrap();
    let mut engine = Engine::new(config_from_environment(|_| None), home.path().into());
    let first = engine.fingerprint_dynamic_targets(root.path(), &[target.clone(), target.clone()]);
    assert_eq!(first.len(), 1);
    assert!(!engine.is_dynamic_target_fingerprint_current(&first[0]));
    engine.commit_dynamic_target_fingerprints(&first);
    let unchanged = engine.fingerprint_dynamic_targets(root.path(), std::slice::from_ref(&target));
    assert!(engine.is_dynamic_target_fingerprint_current(&unchanged[0]));
    fs::write(root.path().join("AGENTS.md"), "new").unwrap();
    let changed = engine.fingerprint_dynamic_targets(root.path(), &[target]);
    assert_ne!(first[0].fingerprint, changed[0].fingerprint);
    assert!(!engine.is_dynamic_target_fingerprint_current(&changed[0]));
    engine.reset_session(None);
    assert!(!engine.is_dynamic_target_fingerprint_current(&first[0]));
}
#[test]
fn relative_paths_cross_siblings_and_normalize_dot_segments() {
    use std::path::Path;
    use maho_ext_rules::rules::engine::relative_path;
    assert_eq!(relative_path(Path::new("/tmp/project/left"), Path::new("/tmp/project/right/file.rs")), "../right/file.rs");
    assert_eq!(relative_path(Path::new("/tmp/project"), Path::new("/tmp/project/dir/../file.rs")), "file.rs");
    assert_eq!(relative_path(Path::new("/tmp/project"), Path::new("/tmp/project")), "");
}
