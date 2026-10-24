//! Secret scrubbing for the environment of model-run subprocesses (bash tool, eval kernel).
//!
//! SAFETY-DIVERGENCE(senpi tools/bash.ts): senpi hands the full host environment to every model-run
//! command. maho removes secret-looking variables from the FINAL model-visible environment, right
//! before spawn (see divergence SD-2). `SSH_AUTH_SOCK` is deliberately kept (git needs it): an
//! accepted risk. Host-side credential use (providers, MCP servers) is unaffected.

use std::collections::BTreeMap;

const SUFFIXES: [&str; 11] = [
    "_TOKEN",
    "_SECRET",
    "_SECRET_KEY",
    "_API_KEY",
    "_APIKEY",
    "_ACCESS_KEY",
    "_PRIVATE_KEY",
    "_PASSWORD",
    "_PASSWD",
    "_CREDENTIALS",
    "_AUTH",
];
const EXACT: [&str; 9] = [
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "NPM_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
];
const PREFIXES: [&str; 1] = ["AWS_SECRET"];
const NEVER: [&str; 3] = ["SSH_AUTH_SOCK", "PATH", "HOME"];

fn numbered_oauth_token(upper: &str) -> bool {
    upper
        .strip_prefix("CLAUDE_CODE_OAUTH_TOKEN_")
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (2..=16).contains(&n))
}

/// Whether `key` names a secret that must not reach a model-run subprocess. Keys in `passthrough`
/// (exact, case-sensitive) are never secret.
pub fn is_secret_key(key: &str, passthrough: &[String]) -> bool {
    if passthrough.iter().any(|kept| kept == key) {
        return false;
    }
    let upper = key.to_ascii_uppercase();
    if NEVER.contains(&upper.as_str()) {
        return false;
    }
    SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
        || EXACT.contains(&upper.as_str())
        || upper == "DATABASE_URL"
        || PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
        || numbered_oauth_token(&upper)
}

/// Remove secret-looking variables from `env`, keeping the `passthrough` names.
pub fn scrub_secrets(env: &mut BTreeMap<String, String>, passthrough: &[String]) {
    env.retain(|key, _| !is_secret_key(key, passthrough));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(keys: &[&str]) -> BTreeMap<String, String> {
        keys.iter()
            .map(|k| ((*k).to_owned(), "v".to_owned()))
            .collect()
    }

    #[test]
    fn removes_every_pattern_class() {
        for key in [
            "FOO_TOKEN",
            "FOO_SECRET",
            "FOO_SECRET_KEY",
            "FOO_API_KEY",
            "FOO_APIKEY",
            "FOO_ACCESS_KEY",
            "FOO_PRIVATE_KEY",
            "FOO_PASSWORD",
            "FOO_PASSWD",
            "FOO_CREDENTIALS",
            "FOO_AUTH",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "NPM_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_2",
            "CLAUDE_CODE_OAUTH_TOKEN_16",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "DATABASE_URL",
            "AWS_SECRETFOO",
        ] {
            let mut vars = env(&[key, "KEEP_ME"]);
            scrub_secrets(&mut vars, &[]);
            assert!(!vars.contains_key(key), "{key} should be scrubbed");
            assert!(vars.contains_key("KEEP_ME"));
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        let mut vars = env(&["github_token", "My_Api_Key", "database_url"]);
        scrub_secrets(&mut vars, &[]);
        assert!(vars.is_empty());
    }

    #[test]
    fn passthrough_keeps_exact_case_sensitive_names() {
        let mut vars = env(&["GITHUB_TOKEN", "github_token", "NPM_TOKEN"]);
        scrub_secrets(&mut vars, &["GITHUB_TOKEN".to_owned()]);
        assert_eq!(vars.keys().collect::<Vec<_>>(), ["GITHUB_TOKEN"]);
    }

    #[test]
    fn never_removes_ssh_auth_sock_path_or_home() {
        let mut vars = env(&["SSH_AUTH_SOCK", "PATH", "HOME", "FOO"]);
        scrub_secrets(&mut vars, &[]);
        assert_eq!(vars.len(), 4);
    }

    #[test]
    fn unnumbered_lookalikes_survive() {
        let mut vars = env(&[
            "CLAUDE_CODE_OAUTH_TOKEN_1",
            "CLAUDE_CODE_OAUTH_TOKEN_17",
            "TOKEN_COUNT",
        ]);
        scrub_secrets(&mut vars, &[]);
        assert_eq!(vars.len(), 3);
    }
}
