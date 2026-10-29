//! SUL-1.0 skill-MCP OAuth helpers kept beside the omo skill MCP manager.
//!
//! The MIT `maho-ext-mcp` builtin carries only the senpi OAuth client. The skill-MCP manager's
//! step-up re-login, scope merging, URL redaction and browser launch live here so the MIT crate
//! stays free of OmO-derived code.

use maho_ext_mcp::{
    auth::{
        commands_auth::{build_provider, run_loopback_auth},
        oauth::OAuthRequestError,
        oauth_provider::CredentialScope,
    },
    config_schema::McpServerConfig,
};

/// Stable union of `existing` and `required`, keeping the existing order.
pub fn merge_scopes(existing: &[String], required: &[String]) -> Vec<String> {
    let mut merged = existing.to_vec();
    for scope in required {
        if !merged.contains(scope) {
            merged.push(scope.clone());
        }
    }
    merged
}

fn sensitive_query_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.contains("key") || lower.contains("token") || lower.contains("secret")
}

/// Query parameters whose key contains `key`, `token` or `secret` become `***REDACTED***`; a value
/// that is not a URL is returned unchanged.
pub fn redact_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_owned();
    };
    let pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return url.to_string();
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in &pairs {
        serializer.append_pair(
            key,
            if sensitive_query_key(key) {
                "***REDACTED***"
            } else {
                value.as_str()
            },
        );
    }
    url.set_query(Some(&serializer.finish()));
    url.to_string()
}

/// Opens `url` in the platform browser, detached; launch failures are ignored.
pub fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Interactive loopback login that always runs the authorization flow: stored tokens are dropped
/// first so newly requested scopes reach the authorization server even when the old token is
/// still fresh.
pub async fn forced_interactive_login(
    name: &str,
    config: &McpServerConfig,
    agent_dir: &std::path::Path,
    client: &reqwest::Client,
) -> Result<(), OAuthRequestError> {
    let provider = build_provider(name, config, agent_dir, None)?;
    provider.invalidate_credentials(CredentialScope::Tokens)?;
    run_loopback_auth(provider, None, client, |url| async move {
        open_browser(url.as_str());
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_scopes_keeps_order_and_skips_duplicates() {
        let existing = vec!["read".to_owned(), "write".to_owned()];
        let required = vec!["write".to_owned(), "admin".to_owned()];
        assert_eq!(
            merge_scopes(&existing, &required),
            vec!["read", "write", "admin"]
        );
    }

    #[test]
    fn redact_url_masks_sensitive_query_values_only() {
        assert_eq!(
            redact_url("https://h.example/mcp?api_key=s&mode=x&token=t"),
            "https://h.example/mcp?api_key=***REDACTED***&mode=x&token=***REDACTED***"
        );
        assert_eq!(redact_url("not a url"), "not a url");
    }
}
