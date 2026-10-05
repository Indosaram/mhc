use maho_server::app_server::gitignore::{add_gitignore_scope, is_ignored};
use std::path::Path;

#[tokio::test]
async fn escaped_whitespace_negation_and_malformed_patterns_follow_pinned_ignore_semantics() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(".gitignore"), "*.log\n!keep.log\nspaced\\ name.txt\n[unclosed\n").unwrap();
    let scopes = add_gitignore_scope(Vec::new(), directory.path()).await;
    assert_eq!(scopes.len(), 1);
    assert!(is_ignored(&scopes, &directory.path().join("a.log"), false));
    assert!(!is_ignored(&scopes, &directory.path().join("keep.log"), false));
    assert!(is_ignored(&scopes, &directory.path().join("spaced name.txt"), false));
    assert!(!is_ignored(&scopes, &directory.path().join("spaced-other.txt"), false));
    assert!(!is_ignored(&scopes, &directory.path().join("src/main.rs"), false));
}

#[tokio::test]
async fn absent_gitignore_leaves_scopes_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let scopes = add_gitignore_scope(Vec::new(), directory.path()).await;
    assert!(scopes.is_empty());
    assert!(!is_ignored(&scopes, &directory.path().join("anything"), false));
    assert!(Path::new(".gitignore").is_relative());
}
