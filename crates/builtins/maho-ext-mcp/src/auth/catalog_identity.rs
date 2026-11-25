//! Port of senpi mcp/auth/catalog-identity.ts: which credential a cached catalog belongs to.
use super::{
    context::{ServerAuthMode, resolve_auth_mode},
    oauth_grant::{McpGrantContext, oauth_grant_identity},
    token_store::{McpStoredAuth, McpTokenStore},
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
    Some(
        if material == "none" || resolve_auth_mode(config) == ServerAuthMode::OAuth {
            material
        } else {
            crate::config_identity_key::keyed_identity(agent_dir, &material)
        },
    )
}

/// The unkeyed identity earlier builds persisted; read only to adopt a cache entry for the same credential.
pub fn legacy_mcp_credential_identity(
    config: &McpServerDeclaredConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    if resolve_auth_mode(config) == ServerAuthMode::OAuth {
        return None;
    }
    let material = credential_material(config, server_name, agent_dir, env)?;
    Some(if material == "none" {
        material
    } else {
        format!("{:x}", Sha256::digest(material))
    })
}

fn is_valid_stored_grant_shape(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object
        .get("accessToken")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return false;
    }
    for key in [
        "refreshToken",
        "issuer",
        "resource",
        "scope",
        "refreshScope",
    ] {
        if let Some(field) = object.get(key)
            && !field.is_string()
        {
            return false;
        }
    }
    if let Some(client_info) = object.get("clientInfo") {
        let Some(client_obj) = client_info.as_object() else {
            return false;
        };
        if !client_obj.get("client_id").is_some_and(Value::is_string) {
            return false;
        }
        for key in ["client_secret", "token_endpoint_auth_method", "issuer"] {
            if let Some(field) = client_obj.get(key)
                && !field.is_string()
            {
                return false;
            }
        }
    }
    if let Some(discovery_state) = object.get("discoveryState") {
        let Some(state_obj) = discovery_state.as_object() else {
            return false;
        };
        if !state_obj
            .get("authorizationServerUrl")
            .is_some_and(Value::is_string)
        {
            return false;
        }
    }
    true
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
            if !is_valid_stored_grant_shape(&value) {
                return None;
            }
            let record: McpStoredAuth = serde_json::from_value(value).ok()?;
            let context = McpGrantContext {
                server_name,
                server_url: &store.server_url,
                agent_dir: store.agent_dir(),
                client_id: config
                    .oauth
                    .as_ref()
                    .and_then(|oauth| oauth.client_id.as_deref()),
                client_metadata_url: config
                    .oauth
                    .as_ref()
                    .and_then(|oauth| oauth.client_metadata_url.as_deref()),
            };
            Some(oauth_grant_identity(&record, &context))
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
        // #2861: token-only legacy OAuth cache adoption is refused.
        assert_eq!(
            legacy_mcp_credential_identity(&oauth, "s", Some(dir.path()), None),
            None
        );
        assert!(mcp_credential_identity(&oauth, "s", Some(dir.path()), None).is_some());
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
