use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_redact_url_when_https_url_carrying_token_then_credential_is_masked() {
    let url = "https://ghp_abcdef1234567890@github.com/acme/memory.git";
    let redacted = redact_url(url);
    assert_eq!(redacted, "https://***@github.com/acme/memory.git");
    assert!(!redacted.contains("ghp_abcdef1234567890"));
}

#[test]
fn test_redact_url_when_https_url_with_user_and_password_then_both_halves_are_masked() {
    let url = "https://alice:s3cr3t-pat@gitlab.example.com/team/memory.git";
    let redacted = redact_url(url);
    assert_eq!(
        redacted,
        "https://***:***@gitlab.example.com/team/memory.git"
    );
    assert!(!redacted.contains("s3cr3t-pat"));
    assert!(!redacted.contains("alice"));
}

#[test]
fn test_redact_url_when_ssh_scp_style_url_then_user_info_is_masked() {
    let url = "git@github.com:acme/memory.git";
    let redacted = redact_url(url);
    assert_eq!(redacted, "***@github.com:acme/memory.git");
    assert!(!redacted.contains("git@"));
}

#[test]
fn test_redact_url_when_ssh_url_with_user_info_then_user_info_is_masked() {
    let url = "ssh://deploy:key123@git.example.com:2222/srv/memory.git";
    let redacted = redact_url(url);
    assert_eq!(
        redacted,
        "ssh://***:***@git.example.com:2222/srv/memory.git"
    );
    assert!(!redacted.contains("key123"));
}

#[test]
fn test_redact_url_when_urls_without_credentials_then_pass_through_unchanged() {
    let urls = [
        "https://github.com/acme/memory.git",
        "file:///tmp/mirror.git",
        "/srv/mirrors/memory.git",
    ];

    for url in urls {
        assert_eq!(redact_url(url), url);
    }
}

#[test]
fn test_redact_url_when_free_text_containing_credentialed_url_then_masked() {
    let line = "fatal: could not read from https://x-token:abc123@github.com/acme/memory.git";
    let redacted = redact_url(line);
    assert!(redacted.contains("https://***:***@github.com/acme/memory.git"));
    assert!(!redacted.contains("abc123"));
}

#[test]
fn test_redact_url_when_empty_url_then_empty_string_returned() {
    assert_eq!(redact_url(""), "");
}
