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
#[test] fn upstream_https_url() { let result = parse_git_url("https://github.com/user/repo").unwrap(); assert_eq!((result.host.as_str(), result.path.as_str(), result.repo.as_str()), ("github.com", "user/repo", "https://github.com/user/repo")); }
#[test] fn upstream_ssh_url() { let result = parse_git_url("ssh://git@github.com/user/repo").unwrap(); assert_eq!(result.repo, "ssh://git@github.com/user/repo"); assert_eq!(result.path, "user/repo"); }
#[test] fn upstream_protocol_ref() { let result = parse_git_url("https://github.com/user/repo@v1.0.0").unwrap(); assert_eq!(result.repo, "https://github.com/user/repo"); assert_eq!(result.reference.as_deref(), Some("v1.0.0")); }
#[test] fn upstream_prefixed_scp() { let result = parse_git_url("git:git@github.com:user/repo").unwrap(); assert_eq!(result.repo, "git@github.com:user/repo"); assert_eq!(result.path, "user/repo"); }
#[test] fn upstream_prefixed_host() { let result = parse_git_url("git:github.com/user/repo").unwrap(); assert_eq!(result.repo, "https://github.com/user/repo"); }
#[test] fn upstream_prefixed_scp_ref() { let result = parse_git_url("git:git@github.com:user/repo@v1.0.0").unwrap(); assert_eq!(result.repo, "git@github.com:user/repo"); assert_eq!(result.reference.as_deref(), Some("v1.0.0")); }
#[test] fn upstream_unsafe_paths() { for source in ["git:git@evil.example:../../victim/repo", "https://evil.example/..%2F..%2Fvictim/repo", "https://evil.example/..%2F..%2Fvictim/repo%", "git:git@evil.example:/absolute/repo", "git:git@evil.example:user\\repo/name", "git:git@evil.example:user/repo\0name"] { assert!(parse_git_url(source).is_none()); } }
#[test] fn upstream_unprefixed_scp_rejected() { assert!(parse_git_url("git@github.com:user/repo").is_none()); }
#[test] fn upstream_unprefixed_host_rejected() { assert!(parse_git_url("github.com/user/repo").is_none()); }
#[test] fn upstream_user_repo_rejected() { assert!(parse_git_url("user/repo").is_none()); }
#[test]
fn hosted_shorthands_preserve_upstream_clone_address() {
    for (input, host, path) in [("git:user/repo", "github.com", "user/repo"), ("git:github:user/repo", "github.com", "user/repo"), ("git:gitlab:group/sub/repo", "gitlab.com", "group/sub/repo"), ("git:bitbucket:user/repo", "bitbucket.org", "user/repo"), ("git:gist:abcdef", "gist.github.com", "null/abcdef")] {
        let parsed = parse_git_url(input).unwrap();
        assert_eq!(parsed.host, host); assert_eq!(parsed.path, path);
        assert_eq!(parsed.repo, format!("https://{}", &input[4..])); assert!(!parsed.pinned);
    }
    let parsed = parse_git_url("git:user/repo@release").unwrap(); assert_eq!(parsed.repo, "https://user/repo"); assert_eq!(parsed.reference.as_deref(), Some("release"));
}
