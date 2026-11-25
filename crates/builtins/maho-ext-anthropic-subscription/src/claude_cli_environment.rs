//! Port of senpi `anthropic-subscription/claude-cli-environment.ts` (v2026.11.19-5).
use std::{collections::BTreeMap, path::Path};

/// Host variables that would redirect a `claude` CLI-store account away from its own dir: the secure-store redirect
/// (undocumented, moves only the keychain item), an inherited config dir, and the host's browser.
const HOST_REDIRECT_NAMES: [&str; 3] = [
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CONFIG_DIR",
    "BROWSER",
];

/// Inherited credentials the CLI reads from a file descriptor; they would beat the account's own login (senpi#4436).
const INHERITED_CREDENTIAL_DESCRIPTORS: [&str; 2] = [
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];

/// Environment for every spawn of a CLI-store account (turns and `claude auth *`). An inherited
/// `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` takes precedence over the stored claude.ai login inside the CLI,
/// so they are stripped with the rest of the managed auth set before the account dir is pinned.
pub fn build_claude_cli_environment(
    parent: &BTreeMap<String, String>,
    config_dir: &Path,
    browser: Option<&str>,
) -> BTreeMap<String, String> {
    let mut environment = crate::auth_environment::strip_managed_auth_environment(parent);
    for name in HOST_REDIRECT_NAMES
        .iter()
        .chain(INHERITED_CREDENTIAL_DESCRIPTORS.iter())
    {
        environment.remove(*name);
    }
    environment.insert(
        "CLAUDE_CONFIG_DIR".into(),
        config_dir.to_string_lossy().into_owned(),
    );
    environment.insert("NO_COLOR".into(), "1".into());
    if let Some(browser) = browser {
        environment.insert("BROWSER".into(), browser.into());
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_the_account_dir_and_strips_every_inherited_credential_and_redirect() {
        let parent: BTreeMap<String, String> = [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_2",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "BROWSER",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
            "SENPI_PRIVATE",
            "PATH",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), "inherited".to_owned()))
        .collect();
        let environment = build_claude_cli_environment(&parent, Path::new("/accounts/a"), None);
        assert_eq!(
            environment,
            [
                ("CLAUDE_CONFIG_DIR".to_owned(), "/accounts/a".to_owned()),
                ("NO_COLOR".to_owned(), "1".to_owned()),
                ("PATH".to_owned(), "inherited".to_owned()),
            ]
            .into()
        );
    }
}
