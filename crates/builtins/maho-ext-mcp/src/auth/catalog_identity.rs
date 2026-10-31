//! Port of senpi mcp/auth/catalog-identity.ts: which credential a cached catalog belongs to.
use super::{
    context::{ServerAuthMode, resolve_auth_mode},
    token_store::McpTokenStore,
};
use crate::config_schema::McpServerDeclaredConfig;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

/// Local snapshot only: never migrates credentials, acquires a lock, or refreshes authentication.
pub fn mcp_credential_identity(
    config: &McpServerDeclaredConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    let material = credential_material(config, server_name, agent_dir, env)?;
    Some(if material == "none" {
        material
    } else {
        crate::config_identity_key::keyed_identity(agent_dir, &material)
    })
}

/// The unkeyed identity earlier builds persisted; read only to adopt a cache entry for the same credential.
pub fn legacy_mcp_credential_identity(
    config: &McpServerDeclaredConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    let material = credential_material(config, server_name, agent_dir, env)?;
    Some(if material == "none" {
        material
    } else {
        format!("{:x}", Sha256::digest(material))
    })
}

fn credential_material(
    config: &McpServerDeclaredConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    match resolve_auth_mode(config) {
        ServerAuthMode::None => Some("none".into()),
        ServerAuthMode::Bearer => {
            let name = config.bearer_token_env.as_ref()?;
            let token = env
                .and_then(|env| env.get(name).cloned())
                .or_else(|| std::env::var(name).ok())?;
            (!token.is_empty()).then(|| json!(["mcp-catalog-bearer", token]).to_string())
        }
        ServerAuthMode::OAuth => {
            let directory = agent_dir.map_or_else(
                || std::path::PathBuf::from(maho_core::config::get_agent_dir()),
                Path::to_path_buf,
            );
            let store =
                McpTokenStore::new(&directory, server_name, config.url.as_deref().unwrap_or(""));
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(store.tokens_path()).ok()?).ok()?;
            let access = value
                .get("accessToken")?
                .as_str()
                .filter(|token| !token.is_empty())?;
            let refresh = match value.get("refreshToken") {
                None => Value::Null,
                Some(Value::String(token)) => Value::String(token.clone()),
                Some(_) => return None,
            };
            // JSON.stringify drops an undefined array slot to `null`, matching the absent refresh token.
            Some(json!(["mcp-catalog-oauth", access, refresh]).to_string())
        }
    }
}

#[cfg(test)]
// L-ext-terminal-mcp 044d529da test: the unkeyed (legacy) identity is the plain sha256 of the material.
mod tests {
    use super::*;
    use crate::config_schema::Transport;

    #[test]
    fn legacy_none_bearer_and_oauth_identities() -> std::io::Result<()> {
        let stdio = McpServerDeclaredConfig {
            transport: Some(Transport::Stdio),
            command: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(
            legacy_mcp_credential_identity(&stdio, "s", None, None),
            Some("none".into())
        );

        let bearer = McpServerDeclaredConfig {
            transport: Some(Transport::Http),
            url: Some("https://example.test/mcp".into()),
            bearer_token_env: Some("MCP_TEST_TOKEN_IDENTITY".into()),
            ..Default::default()
        };
        let env = BTreeMap::from([("MCP_TEST_TOKEN_IDENTITY".to_owned(), "tok".to_owned())]);
        assert_eq!(
            legacy_mcp_credential_identity(&bearer, "s", None, Some(&env)),
            Some(format!(
                "{:x}",
                Sha256::digest(r#"["mcp-catalog-bearer","tok"]"#)
            ))
        );
        let empty = BTreeMap::from([("MCP_TEST_TOKEN_IDENTITY".to_owned(), String::new())]);
        assert_eq!(
            legacy_mcp_credential_identity(&bearer, "s", None, Some(&empty)),
            None
        );

        let dir = tempfile::tempdir()?;
        let oauth = McpServerDeclaredConfig {
            transport: Some(Transport::Http),
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        assert_eq!(
            legacy_mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            None
        );
        let store = McpTokenStore::new(dir.path(), "s", "https://example.test/mcp");
        std::fs::create_dir_all(store.dir())?;
        std::fs::write(store.tokens_path(), r#"{"accessToken":"a"}"#)?;
        assert_eq!(
            legacy_mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
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
            legacy_mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            None
        );
        Ok(())
    }
}
