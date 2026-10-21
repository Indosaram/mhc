use super::configure_allowed_env_vars::{is_allowed_mcp_env_var, is_sensitive_mcp_env_var};
use crate::shared::logger::log;
use serde_json::Value;
use utils::env_expansion::{
    EnvExpansionOptions, expand_env_references, expand_env_references_in_object,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ExpandEnvVarsOptions {
    pub trusted: bool,
}

fn on_blocked(var_name: &str, _: utils::env_expansion::EnvExpansionBlockedReason) {
    let reason = if is_sensitive_mcp_env_var(var_name) {
        "sensitive variable"
    } else {
        "not in allowlist"
    };
    log(
        &format!("Blocked MCP env var expansion for {reason} \"{var_name}\""),
        Some(
            &serde_json::json!({"varName": var_name, "sensitive": is_sensitive_mcp_env_var(var_name)}),
        ),
    );
}

pub fn expand_env_vars(value: &str, options: ExpandEnvVarsOptions) -> String {
    expand_env_references(
        value,
        &EnvExpansionOptions {
            trusted: options.trusted,
            is_allowed: Some(&is_allowed_mcp_env_var),
            on_blocked: Some(&on_blocked),
            ..Default::default()
        },
    )
}

pub fn expand_env_vars_in_object(value: &Value, options: ExpandEnvVarsOptions) -> Value {
    expand_env_references_in_object(
        value,
        &EnvExpansionOptions {
            trusted: options.trusted,
            is_allowed: Some(&is_allowed_mcp_env_var),
            on_blocked: Some(&on_blocked),
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::configure_allowed_env_vars::*;
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn allowlist_expansion_blocks_secrets_supports_defaults_and_trusted_objects() {
        let env = HashMap::from([
            ("GITHUB_TOKEN".into(), "secret".into()),
            ("PROJECT_ROOT".into(), "/project".into()),
            ("HOME".into(), "/home/test".into()),
            ("TMPDIR".into(), "/tmp/omo".into()),
            ("TEMP".into(), "C:\\Temp".into()),
            ("USERPROFILE".into(), "C:\\Users\\tester".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("XDG_CONFIG_HOME".into(), "/config".into()),
        ]);
        let options = EnvExpansionOptions {
            env: Some(&env),
            is_allowed: Some(&is_allowed_mcp_env_var),
            ..Default::default()
        };
        for (input, expected) in [
            ("${GITHUB_TOKEN}", ""),
            ("${PROJECT_ROOT}", ""),
            ("${GITHUB_TOKEN:-fallback}", "fallback"),
            ("${HOME}", "/home/test"),
            (
                "${TMPDIR}|${TEMP}|${USERPROFILE}|${LANG}|${XDG_CONFIG_HOME}",
                "/tmp/omo|C:\\Temp|C:\\Users\\tester|en_US.UTF-8|/config",
            ),
        ] {
            assert_eq!(expand_env_references(input, &options), expected);
        }
        let input = json!({"url":"https://example.test/${GITHUB_TOKEN}","args":["--dir","${HOME}"],"headers":{"Authorization":"Bearer ${GITHUB_TOKEN}"}});
        assert_eq!(
            expand_env_references_in_object(&input, &options),
            json!({"url":"https://example.test/","args":["--dir","/home/test"],"headers":{"Authorization":"Bearer "}})
        );
        let trusted = EnvExpansionOptions {
            trusted: true,
            ..options
        };
        assert_eq!(
            expand_env_references_in_object(&input, &trusted)["headers"]["Authorization"],
            "Bearer secret"
        );
        assert_eq!(
            expand_env_references("${UNSET_TRUSTED_VAR:-fallback}", &trusted),
            "fallback"
        );
        set_additional_allowed_mcp_env_vars(&["GITHUB_TOKEN".into(), "GITHUB_TOKEN".into()]);
        assert_eq!(
            expand_env_vars("${GITHUB_TOKEN:-approved}", ExpandEnvVarsOptions::default()),
            std::env::var("GITHUB_TOKEN").unwrap_or_else(|_| "approved".into())
        );
        reset_additional_allowed_mcp_env_vars();
        assert!(is_sensitive_mcp_env_var("custom_api_key"));
        assert!(!is_sensitive_mcp_env_var("PROJECT_ROOT"));
    }
}
