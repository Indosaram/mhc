//! SC-U2 stdio environment regressions: the pinned `skill-mcp-manager/env-cleaner.ts`
//! `createCleanMcpEnvironment` contract, unit-tested and driven through the production stdio
//! consumer path (`create_mcp_transport` -> `materialize_stdio`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ext_mcp::config_schema::{McpServerConfig, Transport};
use maho_ext_mcp::env_cleaner::{create_clean_mcp_environment, create_clean_mcp_environment_from, is_excluded_env_key, EXCLUDED_ENV_PATTERNS};
use maho_ext_mcp::log::McpLogger;
use maho_ext_mcp::transport::{connect_mcp_transport, create_mcp_transport, shutdown_mcp_transport};

fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
}

#[test]
fn pinned_excluded_patterns_match_the_package_manager_and_secret_conventions() {
    assert!(EXCLUDED_ENV_PATTERNS.len() >= 4);
    for key in ["OPENAI_API_KEY", "openai_api_key", "ANTHROPIC_API_KEY", "GITHUB_TOKEN", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "DATABASE_URL", "AZURE_CONFIG_DIR", "GCP_PROJECT", "VAULT_ADDR", "KUBECONFIG", "DOCKER_AUTH_CONFIG", "NPM_CONFIG_REGISTRY", "npm_config_registry", "NO_UPDATE_NOTIFIER", "MY_KEY", "SESSION_TOKEN", "DB_PASSWORD", "APP_CREDENTIAL", "APP_CREDENTIALS"] {
        assert!(is_excluded_env_key(key), "{key} must be excluded");
    }
    for key in ["PATH", "HOME", "SAFE_FLAG", "TERM", "LANG"] {
        assert!(!is_excluded_env_key(key), "{key} must be inherited");
    }
}

#[test]
fn only_ambient_secrets_are_stripped_and_declared_env_wins() {
    let ambient = map(&[("OPENAI_API_KEY", "ambient-secret"), ("SAFE_FLAG", "1"), ("PATH", "/usr/bin"), ("GITHUB_TOKEN", "ambient-token"), ("MY_KEY", "k")]);
    let declared = map(&[("OPENAI_API_KEY", "declared-secret"), ("MCP_ONLY", "1")]);
    let clean = create_clean_mcp_environment_from(&ambient, &declared);
    assert_eq!(clean["OPENAI_API_KEY"], "declared-secret");
    assert_eq!(clean["MCP_ONLY"], "1");
    assert_eq!(clean["SAFE_FLAG"], "1");
    assert_eq!(clean["PATH"], "/usr/bin");
    assert!(!clean.contains_key("GITHUB_TOKEN"));
    assert!(!clean.contains_key("MY_KEY"));
}

#[test]
fn the_pinned_entry_point_keeps_declared_env_intact() {
    let clean = create_clean_mcp_environment(&map(&[("MCP_DECLARED", "declared")]));
    assert_eq!(clean["MCP_DECLARED"], "declared");
}

#[tokio::test]
async fn stdio_children_inherit_the_cleaned_environment() {
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("env-report", root.path(), None).unwrap()));
    let config = McpServerConfig {
        transport: Some(Transport::Stdio),
        command: Some("/usr/bin/node".into()),
        args: Some(vec![format!("{}/tests/fixtures/env-report.mjs", env!("CARGO_MANIFEST_DIR"))]),
        env: Some(map(&[("MCP_ENV_DECLARED", "declared"), ("MCP_ENV_DECLARED_TOKEN", "declared-secret")])),
        connect_timeout_ms: Some(5000.0),
        ..Default::default()
    };
    let session = map(&[("MCP_ENV_SAFE", "kept"), ("MCP_ENV_SESSION_TOKEN", "ambient-secret")]);
    let transport = create_mcp_transport("env-report", &config, Some(&session), logger).unwrap();
    let client = connect_mcp_transport(&transport).await.unwrap();
    let report: serde_json::Value = serde_json::from_str(client.server_info.read().await["version"].as_str().unwrap()).unwrap();
    assert_eq!(report["MCP_ENV_SAFE"], "kept");
    assert_eq!(report["MCP_ENV_DECLARED"], "declared");
    assert_eq!(report["MCP_ENV_DECLARED_TOKEN"], "declared-secret");
    assert!(report.get("MCP_ENV_SESSION_TOKEN").is_none(), "the pinned blacklist must strip an ambient *_TOKEN key");
    shutdown_mcp_transport(&transport).await.unwrap();
}
