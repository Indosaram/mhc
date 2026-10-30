//! Port of senpi `auth/catalog-identity.ts`: a local credential fingerprint for catalog cache
//! identity. Never migrates credentials, takes a lock, or refreshes authentication.
use super::context::{ServerAuthMode, resolve_auth_mode};
use super::token_store::McpTokenStore;
use crate::config_schema::McpServerConfig;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

fn fingerprint(parts: &serde_json::Value) -> String {
    format!("{:x}", Sha256::digest(parts.to_string()))
}

/// `mcpCredentialIdentity`. `agent_dir` of `None` resolves the default agent directory, like
/// upstream's `McpTokenStore` without an explicit dir.
pub fn mcp_credential_identity(
    config: &McpServerConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    match resolve_auth_mode(config) {
        ServerAuthMode::None => Some("none".to_owned()),
        ServerAuthMode::Bearer => {
            let name = config.bearer_token_env.as_deref()?;
            let token = env
                .and_then(|env| env.get(name).cloned())
                .or_else(|| std::env::var(name).ok())?;
            if token.is_empty() {
                return None;
            }
            Some(fingerprint(&serde_json::json!([
                "mcp-catalog-bearer",
                token
            ])))
        }
        ServerAuthMode::OAuth => {
            let directory = agent_dir.map_or_else(
                || std::path::PathBuf::from(maho_core::config::get_agent_dir()),
                Path::to_path_buf,
            );
            let store =
                McpTokenStore::new(&directory, server_name, config.url.as_deref().unwrap_or(""));
            let raw = std::fs::read(store.tokens_path()).ok()?;
            let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
            let object = value.as_object()?;
            let access = object.get("accessToken")?.as_str()?;
            if access.is_empty() {
                return None;
            }
            let refresh = match object.get("refreshToken") {
                None => serde_json::Value::Null,
                Some(serde_json::Value::String(token)) => serde_json::Value::String(token.clone()),
                Some(_) => return None,
            };
            // JSON.stringify drops an undefined array slot to null, matching the Null above.
            Some(fingerprint(&serde_json::json!([
                "mcp-catalog-oauth",
                access,
                refresh
            ])))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_schema::Transport;

    #[test]
    fn none_bearer_and_oauth_identities() -> std::io::Result<()> {
        let stdio = McpServerConfig {
            transport: Some(Transport::Stdio),
            command: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(
            mcp_credential_identity(&stdio, "s", None, None),
            Some("none".into())
        );

        let bearer = McpServerConfig {
            transport: Some(Transport::Http),
            url: Some("https://example.test/mcp".into()),
            bearer_token_env: Some("MCP_TEST_TOKEN_IDENTITY".into()),
            ..Default::default()
        };
        let env = BTreeMap::from([("MCP_TEST_TOKEN_IDENTITY".to_owned(), "tok".to_owned())]);
        assert_eq!(
            mcp_credential_identity(&bearer, "s", None, Some(&env)),
            Some(format!(
                "{:x}",
                Sha256::digest(r#"["mcp-catalog-bearer","tok"]"#)
            ))
        );
        let empty = BTreeMap::from([("MCP_TEST_TOKEN_IDENTITY".to_owned(), String::new())]);
        assert_eq!(
            mcp_credential_identity(&bearer, "s", None, Some(&empty)),
            None
        );

        let dir = tempfile::tempdir()?;
        let oauth = McpServerConfig {
            transport: Some(Transport::Http),
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        assert_eq!(
            mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            None
        );
        let store = McpTokenStore::new(dir.path(), "s", "https://example.test/mcp");
        std::fs::create_dir_all(store.dir())?;
        std::fs::write(store.tokens_path(), r#"{"accessToken":"a"}"#)?;
        assert_eq!(
            mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            Some(format!(
                "{:x}",
                Sha256::digest(r#"["mcp-catalog-oauth","a",null]"#)
            ))
        );
        std::fs::write(
            store.tokens_path(),
            r#"{"accessToken":"a","refreshToken":7}"#,
        )?;
        assert_eq!(
            mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            None
        );
        Ok(())
    }
}
