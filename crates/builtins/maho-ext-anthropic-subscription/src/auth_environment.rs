use std::collections::BTreeMap;

fn is_claude_oauth_token_name(name: &str) -> bool {
    name == "CLAUDE_CODE_OAUTH_TOKEN"
        || name.strip_prefix("CLAUDE_CODE_OAUTH_TOKEN_")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
}

pub fn has_request_oauth_token(request: Option<&BTreeMap<String, String>>) -> bool {
    request.is_some_and(|request| request.iter().any(|(name, value)| is_claude_oauth_token_name(name) && !value.is_empty()))
}

pub fn merge_request_auth_environment(host: &BTreeMap<String, String>, request: Option<&BTreeMap<String, String>>) -> BTreeMap<String, String> {
    let mut environment = host.clone();
    if let Some(request) = request
        && request.keys().any(|name| is_claude_oauth_token_name(name)) {
            environment.retain(|name, _| !is_claude_oauth_token_name(name));
            for (name, value) in request.iter().filter(|(name, _)| is_claude_oauth_token_name(name)) {
                environment.insert(name.clone(), value.clone());
            }
    }
    environment
}

pub fn strip_managed_auth_environment(parent: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    parent.iter().filter(|(name, _)| {
        !is_claude_oauth_token_name(name)
            && !name.starts_with("SENPI_")
            && !matches!(name.as_str(), "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_BASE_URL" | "ANTHROPIC_CUSTOM_HEADERS" | "CLAUDE_CODE_USE_BEDROCK" | "CLAUDE_CODE_USE_FOUNDRY" | "CLAUDE_CODE_USE_GATEWAY" | "CLAUDE_CODE_USE_VERTEX")
    }).map(|(name, value)| (name.clone(), value.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn environment(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries.iter().map(|(name, value)| (String::from(*name), String::from(*value))).collect()
    }
    #[test]
    fn only_exact_token_namespace_counts() {
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN_0", "CLAUDE_CODE_OAUTH_TOKEN_02"] {
            assert!(has_request_oauth_token(Some(&environment(&[(name, "synthetic")]))));
            assert!(!has_request_oauth_token(Some(&environment(&[(name, "")]))));
        }
        for name in ["CLAUDE_CODE_OAUTH_TOKEN_", "CLAUDE_CODE_OAUTH_TOKEN_2x", "CLAUDE_CODE_OAUTH_TOKEN_٢", "PATH"] {
            assert!(!has_request_oauth_token(Some(&environment(&[(name, "synthetic")]))));
        }
        assert!(!has_request_oauth_token(None));
    }
    #[test]
    fn request_cannot_inject_process_controls() {
        let host = environment(&[("PATH", "/usr/bin")]);
        let request = environment(&[("CLAUDE_CODE_OAUTH_TOKEN", "synthetic"), ("PATH", "/request"), ("NODE_OPTIONS", "--require=hook"), ("CLAUDE_CONFIG_DIR", "/request")]);
        assert_eq!(merge_request_auth_environment(&host, Some(&request)), environment(&[("PATH", "/usr/bin"), ("CLAUDE_CODE_OAUTH_TOKEN", "synthetic")]));
    }
    #[test]
    fn request_replaces_entire_host_token_namespace() {
        let host = environment(&[("PATH", "/usr/bin"), ("CLAUDE_CODE_OAUTH_TOKEN", "host"), ("CLAUDE_CODE_OAUTH_TOKEN_2", "host-two")]);
        assert_eq!(merge_request_auth_environment(&host, Some(&environment(&[("CLAUDE_CODE_OAUTH_TOKEN_1", "request")]))), environment(&[("PATH", "/usr/bin"), ("CLAUDE_CODE_OAUTH_TOKEN_1", "request")]));
    }
    #[test]
    fn empty_request_token_still_replaces_host_tokens() {
        let host = environment(&[("CLAUDE_CODE_OAUTH_TOKEN_2", "host")]);
        assert_eq!(merge_request_auth_environment(&host, Some(&environment(&[("CLAUDE_CODE_OAUTH_TOKEN", "")]))), environment(&[("CLAUDE_CODE_OAUTH_TOKEN", "")]));
    }
    #[test]
    fn absent_request_tokens_preserve_host_without_mutation() {
        let host = environment(&[("CLAUDE_CODE_OAUTH_TOKEN", "host")]);
        assert_eq!(merge_request_auth_environment(&host, None), host);
        assert_eq!(merge_request_auth_environment(&host, Some(&environment(&[("PATH", "/request")]))), host);
    }
    #[test]
    fn managed_lane_strips_all_auth_and_senpi_markers() {
        let mut parent = environment(&[("PATH", "/usr/bin"), ("CLAUDE_CODE_OAUTH_TOKEN_suffix", "keep"), ("OMO_MARKER", "keep")]);
        for name in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "ANTHROPIC_CUSTOM_HEADERS", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN_12", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_FOUNDRY", "CLAUDE_CODE_USE_GATEWAY", "CLAUDE_CODE_USE_VERTEX", "SENPI_PRIVATE"] {
            parent.insert(name.into(), "synthetic".into());
        }
        assert_eq!(strip_managed_auth_environment(&parent), environment(&[("PATH", "/usr/bin"), ("CLAUDE_CODE_OAUTH_TOKEN_suffix", "keep"), ("OMO_MARKER", "keep")]));
    }
}
