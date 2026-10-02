use maho_ext_pi_nested_agents_md::find_agents_md_up::find_agents_md_up;
fn fixture(files: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("create discovery fixture");
    std::fs::create_dir_all(root.path().join("a/b/c/d")).expect("create nested directories");
    for file in files { let path = root.path().join(file); std::fs::create_dir_all(path.parent().expect("file parent")).expect("create file directory"); std::fs::write(path, "rules").expect("write rules"); }
    root
}
#[test] fn outermost_first() { let root = fixture(&["AGENTS.md", "a/AGENTS.md", "a/b/AGENTS.md"]); assert_eq!(find_agents_md_up(&root.path().join("a/b"), root.path(), &["AGENTS.md"]), vec![root.path().join("a/AGENTS.md"), root.path().join("a/b/AGENTS.md")]); }
#[test] fn ignores_claude_by_default() { let root = fixture(&["a/AGENTS.md", "a/CLAUDE.md"]); assert_eq!(find_agents_md_up(&root.path().join("a"), root.path(), &["AGENTS.md"]), vec![root.path().join("a/AGENTS.md")]); }
#[test] fn excludes_root_file() { let root = fixture(&["AGENTS.md"]); assert!(find_agents_md_up(&root.path().join("a"), root.path(), &["AGENTS.md"]).is_empty()); }
#[test] fn no_down_scan() { let root = fixture(&["a/b/AGENTS.md"]); assert!(find_agents_md_up(&root.path().join("a"), root.path(), &["AGENTS.md"]).is_empty()); }
#[test] fn empty_when_no_rules() { let root = fixture(&[]); assert!(find_agents_md_up(&root.path().join("a/b"), root.path(), &["AGENTS.md"]).is_empty()); }
#[test] fn deep_order() { let root = fixture(&["a/AGENTS.md", "a/b/AGENTS.md", "a/b/c/AGENTS.md", "a/b/c/d/AGENTS.md"]); assert_eq!(find_agents_md_up(&root.path().join("a/b/c/d"), root.path(), &["AGENTS.md"]), ["a/AGENTS.md", "a/b/AGENTS.md", "a/b/c/AGENTS.md", "a/b/c/d/AGENTS.md"].map(|path| root.path().join(path))); }
#[test] fn configured_fallback() { let root = fixture(&["a/AGENTS.md", "a/b/CLAUDE.md"]); assert_eq!(find_agents_md_up(&root.path().join("a/b"), root.path(), &["AGENTS.md", "CLAUDE.md"]), vec![root.path().join("a/AGENTS.md"), root.path().join("a/b/CLAUDE.md")]); }
#[test] fn prefers_first_name() { let root = fixture(&["a/AGENTS.md", "a/CLAUDE.md"]); assert_eq!(find_agents_md_up(&root.path().join("a"), root.path(), &["AGENTS.md", "CLAUDE.md"]), vec![root.path().join("a/AGENTS.md")]); }
#[test] fn empty_at_root() { let root = fixture(&["AGENTS.md"]); assert!(find_agents_md_up(root.path(), root.path(), &["AGENTS.md"]).is_empty()); }
#[test] fn excludes_shared_prefix_sibling() { let root = fixture(&["foobar/AGENTS.md"]); std::fs::create_dir(root.path().join("foo")).expect("create root"); std::fs::create_dir(root.path().join("foobar/child")).expect("create starting directory"); assert!(find_agents_md_up(&root.path().join("foobar/child"), &root.path().join("foo"), &["AGENTS.md"]).is_empty()); }
