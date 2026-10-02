use maho_cli::utils::git::*;
#[test]
fn generic_git_urls_preserve_clone_address_and_separate_ref() {
    let source = parse_generic_git_url("git:git@example.org:team/repo.git@branch/sub").unwrap();
    assert_eq!(source.repo, "git@example.org:team/repo.git");
    assert_eq!(source.path, "team/repo");
    assert_eq!(source.reference.as_deref(), Some("branch/sub"));
    assert!(source.pinned);
    assert!(parse_generic_git_url("example.org/team/repo").is_none());
    assert!(parse_generic_git_url("git:example.org/team/repo").is_some());
}
#[test]
fn generic_git_install_paths_reject_encoded_traversal_and_invalid_escapes() {
    for source in ["git:example.org/team/%2e%2e/repo", "git:example.org/team/repo%5cfile", "git:example.org/team/repo%xx"] { assert!(parse_generic_git_url(source).is_none()); }
}
