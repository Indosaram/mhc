use maho_ext_rules::{config::config_from_environment, rules::engine::Engine};
use std::fs;

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
